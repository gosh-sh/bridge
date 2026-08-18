# Live E2E plan — AN → Halo2 → gnark → AckiNackiBridge.sol

Draft plan (status: pending review). Goal: a single `bridge-relayer-daemon`
binary that pulls live data from an Acki Nacki node, generates Circuit 1A/1B
+ Circuit 2 (and later Circuit 4) Halo2 proofs, wraps via gnark, and submits
to a real deployed `AckiNackiBridge.sol`. The existing two-process
file-based deployment keeps working in parallel.

Cross-repo scope:
- `acki-nacki-to-eth-bridge-halo2-prover` — `bridge-prover-lib`,
  `bridge-prover-daemon`, `bridge-event-prover-lib`, `bridge-event-witness`
- `bridge-EVM` — `bridge-relayer-daemon`, `bridge-snark-utils`
  (gnark-wrappers/*), new `bridge-gnark-wrap` crate
- `acki-nacki-to-eth-bridge-halo2-circuits` — no changes; consumed via the
  prover-lib as today

## 1. Architectural decision

**`bridge-prover-lib` becomes the proving SDK. `bridge-prover-daemon` becomes
a thin shell over it. `bridge-relayer-daemon` adds an optional `live-prover`
feature that links `bridge-prover-lib` directly.**

Justification:
- `bridge-snark-utils/Cargo.toml:10-18` already proves the Cargo
  graph resolves with both circuit crates and `bridge-prover-lib` linked
  together on `bump-halo2-lib-v0.4.1`. The relayer adopting the same deps
  is mechanically feasible.
- Production may still want process separation (prover on isolated heavy
  host, signer on tiny box). Feature-gate the in-process path so both
  deployment modes share one library.
- The gnark wrap step stays as a Go subprocess invoked from Rust — a
  small new crate `bridge-gnark-wrap` packages the existing
  `gnark-wrappers/circuit-*/prove` binaries with a clean Rust API. No
  port to Rust now.

End-state Cargo graph:

```
bridge-relayer-daemon
  ├─ alloy + tvm-sdk metadata               (always)
  └─ [feature live-prover]
        ├─ bridge-prover-lib                (Circuits 1A/1B/2 prove fns + pipeline)
        ├─ bridge-event-prover-lib          (Circuit 4 prove fn + pipeline)
        ├─ bridge-event-witness             (witness builders)
        └─ bridge-gnark-wrap                (NEW: Rust shim over gnark-wrappers/*)
```

## 2. What actually lives in `bridge-prover-daemon/src/main.rs`

This is the load-bearing observation behind the plan: the daemon is **not**
just glue. It contains the live-data witness-recovery pipeline that any
other consumer (the relayer, future audit tools, batch backfillers) needs.

| Concern | Where in `main.rs` | What it does | Lib home |
|---|---|---|---|
| **Thinning schedule** | `find_next_thinned_key_block` (~1066-1082) | W·P-alignment rule that picks the next target seqno. Single source of truth for "when to prove". | `bridge-prover-lib::schedule` |
| **Circuit 2 witness recovery** | `generate_layer_proof_for_key_block` (~1093-1195) | GQL fetch → parse `history_proofs` + `block_merkle_tree_leaves` → build `layer_hashes_preimage` → 8-leaf SHA-256 tree + L0 siblings → bk_set freshness check vs `leaves[2]` → walk back W blocks, reconstruct the Poseidon trees the node produced (`real_chain_builder::build_real_chain`) → call `layer_prover::generate_layer_proof`. | `bridge-prover-lib::layer_pipeline` |
| **Circuit 1A/1B evidence classification + dispatch** | inline in main loop (~712-815) | `attestation_fetcher::fetch_attestation_evidence` → classify Primary vs Fallback; defensive BK-set membership filter; on-demand PK load/unload for the ~3.7 GB single-PK memory envelope; call `prover::generate_{primary,fallback}_proof`. | `bridge-prover-lib::attestation_pipeline` |
| **BK-set update interleaving (Circuit 3 feed)** | ~410-700 | Detect pending `BkUpdateRequest`, query the update block, apply to in-memory `bk_set`, recompute Poseidon commitment via `poseidon::compute_bk_set_poseidon`, yield back to the key-block loop. | `bridge-prover-lib::bk_update_pipeline` |
| **`KeyManager` PK lifecycle** | used throughout | Memory-aware load/unload of Circuit 1A, 1B, 2 PKs. Not derivable from individual sub-crates. | `bridge-prover-lib::key_manager` (likely already exists — surface it as public API) |
| **BK-set acquisition with fallback** | `load_bk_set` (~1197-1207) | GQL primary, config-file fallback. | `bridge-prover-lib::bk_set_fetcher` (already there; promote the wrapper itself) |
| Stats accumulation | throughout | Counters. | Stays in daemon — telemetry. |
| State persistence (`state.json` r/w) | throughout | Last seen seqno, last_known_bk_set_update_seq_no, etc. | Borderline. Lib **consumes** state by `&mut`; daemon owns the JSON file. |
| Filesystem `proof_<seqno>.json` emission | after each proof | Hex-encode, serialize JSON. | Stays in daemon — wire format for file-based deployment. Lib returns in-memory `{ proof_bytes, public_inputs }`. |
| IPC wait for `result_<seqno>.json` | after write | Polls for verifier-daemon ACK. | Stays in daemon — deployment-specific trust pattern, not a proving concern. |
| Tokio main + retry loop + SIGTERM | `main`, top loop | Async runtime, sleep, shutdown. | Stays in daemon. |

The `bridge-prover-lib` subcrates today (`block_id_tree`, `real_chain_builder`,
`layer_prover`, `prover`, `attestation_fetcher`, `bk_set_fetcher`, `poseidon`)
are correctly factored but **too low-level on their own**. The daemon is the
only place that composes them. That composition is what we're lifting.

## 3. Proposed lib API surface

Two layers, both in `bridge-prover-lib`.

### Low-level (already exists — kept as-is)
`block_id_tree`, `real_chain_builder`, `layer_prover`, `prover`,
`attestation_fetcher`, `bk_set_fetcher`, `poseidon`, `key_manager`.

### High-level (NEW)

```rust
// bridge-prover-lib/src/pipeline.rs

pub struct ProverConfig {
    pub window_size: u64,            // W
    pub thinning_factor: u64,        // P
    pub history_window_size: u64,    // chain-proof depth for Circuit 2
    pub bk_set_config_path: PathBuf, // fallback for bk_set_fetcher
}

pub struct ProverState {
    pub stored_last_seen_block_seq_no: u64,
    pub stored_last_known_bk_set_update_seq_no: u64,
    pub bk_set: HashMap<u16, Vec<u8>>,
    pub bk_set_commitment: Fr,
}

pub struct BlockProofBundle {
    pub seq_no: u64,
    pub attestation_circuit_tag: AttestationCircuit, // Primary | Fallback
    pub primary_proof: Vec<u8>,                       // Circuit 1A or 1B
    pub primary_public_inputs: Vec<Fr>,
    pub layer_proof: Vec<u8>,                         // Circuit 2
    pub layer_public_inputs: Vec<Fr>,
}

impl ProverPipeline {
    pub fn new(cfg: ProverConfig, key_manager: KeyManager) -> Self;

    /// Lifted from `find_next_thinned_key_block`.
    pub fn next_target(&self, state: &ProverState, latest_seqno: u64) -> Option<u64>;

    /// Composes: bk-update interleave + Circuit 1A/1B dispatch + Circuit 2
    /// witness recovery + proof gen. The relayer (or daemon) calls this
    /// per key block. Mutates `state` (advances seqno, applies bk-updates).
    pub async fn prove_block(
        &mut self,
        gql: &GqlClient,
        state: &mut ProverState,
        target_seqno: u64,
    ) -> Result<BlockProofBundle>;

    /// Bootstrap: load bk_set (GQL + config fallback), compute initial
    /// commitment, set seed seqno.
    pub async fn bootstrap(
        &self,
        gql: &GqlClient,
        bootstrap_seqno: Option<u64>,
    ) -> Result<ProverState>;
}
```

Analogous for Circuit 4 in `bridge-event-prover-lib`:

```rust
pub struct EventProofBundle {
    pub event_id: H256,
    pub proof: Vec<u8>,            // Circuit 4 Halo2 bytes
    pub public_inputs: Vec<Fr>,    // 10 PIs: tokenId..finalRoot
}

impl EventProverPipeline {
    pub async fn prove_event(
        &mut self,
        gql: &GqlClient,
        event: WithdrawalInitiatedEvent,
    ) -> Result<EventProofBundle>;
}
```

## 4. Phase plan

### Phase 0 — Scout (½ day, read-only)

1. Read `bridge-prover-daemon/src/main.rs` end-to-end. Produce a function-
   by-function move-list with line numbers and the exact signatures the
   new lib API needs to expose.
2. Read `bridge-snark-utils`'s gnark-wrappers invocation code.
   Document the stdin/stdout contract — that's what `bridge-gnark-wrap`
   re-packages.
3. Confirm on-disk shape of `proof_<seqno>.json` and `proof_event_<seqno>.json`
   matches what `bridge-relayer-daemon`'s `ProverProofsBlockSource` /
   `PartnerWithdrawalProof` expects today.
4. Pick the e2e target: Anvil + freshly deployed `AckiNackiBridge.sol`
   first; Sepolia gated on funded signer + reliable RPC.

Deliverable: one-page Phase 0 note listing exact functions + new lib API
shape.

### Phase 1 — Lib extraction (load-bearing)

1. Create `bridge-prover-lib/src/pipeline.rs` and `schedule.rs`. Move
   `find_next_thinned_key_block` into `schedule`.
2. Move `generate_layer_proof_for_key_block` body verbatim into
   `pipeline::prove_layer_circuit_2`. Already only calls into lib
   sub-crates — cut/paste with `KeyManager`/`GqlClient`/`BridgeState`
   demoted from globals to method args / receiver fields.
3. Lift Circuit 1A/1B dispatch (~712-815) into
   `pipeline::prove_attestation_circuit_1`. Move PK load/unload with it —
   the memory-budget protocol is a proving invariant, not a daemon
   concern.
4. Lift bk-update interleave (~410-700) into
   `pipeline::apply_pending_bk_update`. Returns `Updated(new_commitment)`
   | `NoUpdate`. Daemon decides whether to persist state.
5. Add `ProverPipeline::prove_block` composing (3) + (4) + (2) in the
   same order the daemon does today.
6. Rewrite `bridge-prover-daemon/src/main.rs` as a thin loop:
   ```rust
   loop {
       let target = pipeline.next_target(&state, latest_seqno)?;
       let bundle = pipeline.prove_block(&gql, &mut state, target).await?;
       write_proof_json(&bundle, proofs_dir);
       wait_for_result_json(&bundle, proofs_dir, timeout);
       state.save(STATE_FILE)?;
   }
   ```
7. **Regression gate**: run rewritten daemon against a recorded AN fixture
   and assert byte-identical `proof_<seqno>.json` to the pre-rewrite
   daemon. If diff → Phase 1 not done.

Same surgery on `bridge-event-prover-lib` for Circuit 4 (likely smaller —
verify in Phase 0).

Phase 1 acceptance: lib drives the same flow the daemon drove, end-to-end,
zero behavioural change.

### Phase 2 — `bridge-gnark-wrap` Rust shim

New crate `bridge-EVM/crates/bridge-gnark-wrap/`.

```rust
pub enum CircuitTag { Primary1a, Fallback1b, LayerHashes2, Event4 }

pub struct WrappedProof {
    pub groth16_bytes: [u8; 256],
    pub public_inputs: Vec<Fr>,   // re-encoded as verifier expects
}

pub async fn wrap(
    tag: CircuitTag,
    halo2_proof: &[u8],
    public_inputs: &[Fr],
) -> Result<WrappedProof>;
```

Internals: spawn `gnark-wrappers/circuit-<tag>/prove` (Go binary) via
`tokio::process::Command`; stdin = JSON `{ proof_hex, public_instances_hex }`
matching today's orchestrator format; stdout = 256-byte Groth16 + PI order.

`build.rs` builds the Go binaries on first compile (calls `go build`),
stores under `target/<profile>/gnark-wrappers/`. Cached for CI.

Optional: switch `bridge-snark-utils` to use this same crate
instead of inlining the subprocess calls — reduces two call sites to one.

Acceptance: round-trip test in `bridge-gnark-wrap/tests/` — record a
Halo2 proof bundle in a fixture, wrap, verify the 256-byte output against
the deployed `*Groth16VerifierGenerated.sol` on local Anvil. Fixtures
reusable from `bridge-snark-utils/fixtures/`.

### Phase 3 — `live-prover` feature on `bridge-relayer-daemon`

1. `bridge-relayer-daemon/Cargo.toml`: add optional feature `live-prover`
   pulling `bridge-prover-lib`, `bridge-event-prover-lib`,
   `bridge-event-witness`, `bridge-gnark-wrap`.
2. New `BlockSource` impl in `src/live_source.rs` (file already exists as
   placeholder):
   ```rust
   pub struct LiveAnBlockSource {
       gql: GqlClient,
       pipeline: ProverPipeline,
       state: RwLock<ProverState>,
   }
   impl BlockSource for LiveAnBlockSource {
       async fn fetch(&self, expected_seqno: u64) -> Result<Option<Block>> {
           let latest = self.gql.latest_seqno().await?;
           let Some(target) = self.pipeline.next_target(&*self.state.read(), latest) else {
               return Ok(None);
           };
           if target != expected_seqno { return Ok(None); }

           let bundle = self.pipeline
               .prove_block(&self.gql, &mut *self.state.write(), target)
               .await?;

           let primary_g16 = bridge_gnark_wrap::wrap(
               CircuitTag::from(bundle.attestation_circuit_tag),
               &bundle.primary_proof, &bundle.primary_public_inputs).await?;
           let layer_g16 = bridge_gnark_wrap::wrap(
               CircuitTag::LayerHashes2,
               &bundle.layer_proof, &bundle.layer_public_inputs).await?;

           Ok(Some(Block {
               attestation_proof: primary_g16.groth16_bytes.into(),
               layer_hashes_proof: layer_g16.groth16_bytes.into(),
               ..Block::from_public_inputs(&bundle.layer_public_inputs)
           }))
       }
   }
   ```
3. New CLI subcommand on `relayer.rs`:
   `relayer daemon-live --an-node-url ... --rpc-url ... --bridge-address ... --bk-set bk_set.json`
   Plus the existing backoff / sentry / state flags.
4. **`spawn_blocking` for proving**: each `prove_*` holds a CPU for
   seconds. Wrap each in `tokio::task::spawn_blocking` so the alloy
   reactor stays responsive.
5. Keep `daemon-prover` (file-based) untouched. Both paths share
   `Relayer::run_until_shutdown`; only the `BlockSource` differs.

Acceptance: `relayer daemon-live` against local AN devnet + Anvil +
freshly deployed bridge lands a real `BlockVerified` event for one key
block.

### Phase 4 — E2E test (Circuits 1+2)

Add `bridge-relayer-daemon/tests/e2e_live.rs`, gated behind
`--features live-prover` and an env var:

```rust
#[tokio::test]
#[ignore = "requires AN_NODE_URL + ANVIL_RPC + deployed bridge address"]
async fn e2e_circuits_1_and_2() {
    // 0. Spawn anvil; deploy AckiNackiBridge + verifiers from contracts/.
    // 1. Build LiveAnBlockSource against env!("AN_NODE_URL").
    // 2. Build EthBridgeClient against anvil + funded signer.
    // 3. Drive Relayer::tick once. Assert TickOutcome::Verified.
    // 4. Read storedLastSeenBlockSeqNo back, assert it advanced.
    // 5. (Optional) decode BlockVerified event from receipt logs.
}
```

Budget for: gnark binary build time on first run, halo2 keygen latency
(cache to disk like `historical-layer-hashes-movement-checker-circuit/
test_cache_real_prover/`), AN devnet flakiness (retry guard or stable
recorded fixture devnet).

### Phase 5 — Circuit 4

Extension, not redesign:

1. New `LiveAnEventSource` mirroring `LiveAnBlockSource`, tailing
   `WithdrawalInitiated` events. Uses `bridge-event-witness` for the
   witness, `EventProverPipeline::prove_event` for the proof,
   `bridge_gnark_wrap::wrap(Event4, …)` for the 256-byte output.
2. CLI: `relayer daemon-withdraw-live`.
3. Submits via existing `EthBridgeClient::submit_withdraw` →
   `withdrawByProof(...)`.
4. E2E: deposit on AN → withdraw event observed → proof → tx →
   ERC20 balance moves on Eth side.

Higher risk than Phases 3-4: Circuit 4 has nullifier + dappFr/accFr
identity invariants + depth-8 Poseidon path. Phase 1 proof-shape
regressions surface here if anywhere.

### Phase 6 — Hardening / decision point

Two viable production deployments using the same lib:
- **Single-binary**: `relayer daemon-live` does everything. Default for
  small-team operators, dev/test, this e2e plan.
- **Two-binary**: keep `bridge-prover-daemon` on a prover host; run
  `relayer daemon-prover` on a signer host. Production where the signer
  box must not host halo2/tvm-sdk code.

Both first-class. Default in CI / docs is single-binary because it's the
live-test mode.

## 5. Risks & invariants to preserve

1. **Toolchain alignment** — `bridge-prover-lib` requires
   `bump-halo2-lib-v0.4.1` pinned across the whole graph (see
   `acki-nacki-to-eth-bridge-halo2-prover/Cargo.toml:18-31` + patch
   table). When this lands in `bridge-relayer-daemon`, those pins come
   with it. Verify no conflict with alloy + tvm-sdk-3.0.0
   (`full_dex_and_bridge_test_with_final_halo2_circuit` branch) before
   committing.
2. **bk_set freshness** — prover-lib's freshness check (`leaves[2] ==
   Poseidon(bk_set)`) is the only thing that catches stale `bk_set.json`
   before Circuit 1A's pairing constraints blow up. Preserve it through
   the move. Make `LiveAnBlockSource` read bk_set from the pipeline's
   `ProverState`, not from a static file — closes the same window
   `bridge_bk_set_sync.md` describes.
3. **gnark wrapper status (R15/Phase 8)** — wrapped Groth16 proofs are
   identity-stub today. E2E exercises plumbing (wire format, gas,
   anchor advancement) but **does not** prove the bridge is
   cryptographically sound. Document this in test docstrings.
4. **Proof-gen time vs source rate** — Circuit 2 ~10s per block at
   `num_layers=10`; AN devnet observed ~3 b/s
   (`bridge_devnet_source_rate.md`). Thinning factor P
   (`BRIDGE_PROVER_THINNING_SPEC.md`) is the right knob; the live source
   must use it.
5. **`real_chain_builder::build_real_chain` walks W blocks of history
   per Circuit 2 proof** — at `HISTORY_WINDOW_SIZE = 128` that's 128
   GQL fetches per key block. Today's daemon absorbs the latency
   because it processes one key block at a time. In
   `LiveAnBlockSource::fetch` this is the dominant cost; budget for a
   GQL response cache keyed by seqno (both in lib and in relayer).
6. **`KeyManager` is stateful** (loads/unloads PKs based on active
   circuit, ~3.7 GB single-PK budget). When the relayer holds the
   pipeline across many ticks, load/unload must survive concurrent
   ticks if ever parallelised. Document the single-threaded contract
   or add a mutex.
7. **State-file ownership** — `bridge-prover-daemon`'s state.json and
   `bridge-relayer-daemon`'s state.json are independent today. In
   single-binary mode one process owns both. Decide: keep two state
   files (clearer audit trail) or merge.
8. **Two sources of bk_set state** — `BkSetSentry` independently watches
   AN for rotations. In single-binary mode the pipeline owns bk_set;
   sentry should *read* from it, not maintain its own. Cleaner than
   today.

## 6. Order of attack

1. **First**: Phase 0 + Phase 1 — library extraction is the highest-
   leverage move; everything downstream gets simpler.
2. **Next**: Phase 2 — `bridge-gnark-wrap` is small and self-contained;
   good onboarding task.
3. **Then**: Phase 3 + Phase 4 — first live `verifyBlock` lands.
4. **Finally**: Phase 5 (Circuit 4), once 1+2 lane is green for a few
   days running.

## 7. Out of scope (explicit)

- **R15 / Phase 8** (real Halo2-on-EVM verification via
  snark-verifier-sdk Yul) is a separate workstream tracked in
  `r15_snark_verifier_roadmap.md` and the `bridge-evm-aggregator`
  spike. Lands inside `EthBridgeClient::submit_block` regardless of
  whether binaries are combined first.
- **Porting gnark wrappers to Rust** — keep Go subprocess.
- **Deleting the file-based two-process deployment** — keep it
  first-class.
