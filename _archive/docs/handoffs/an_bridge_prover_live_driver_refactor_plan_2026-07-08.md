> **ARCHIVED** — historical handoff; see [docs/README.md](../../docs/README.md) for current docs.

# `an-bridge-prover` — Live-driver refactor plan

**Date:** 2026-07-08
**Author:** Alina
**Scope:** Refactor `crates/an-bridge-prover/{bridge-prover-daemon,bridge-prover-lib}` so the AN → live-data → private-witness → 1A/1B + 2 (+ BK-set rotation) orchestration is reusable by an external consumer — specifically Sergey's `crates/bridge-relayer-daemon`, which today has no AN GraphQL client and no witness assembler. Circuit 4 (event / withdraw) is **out of scope** for this round.

---

## 1. Why we need this refactor

Sergey's `bridge-relayer-daemon` today can talk to the on-chain Ethereum bridge (`AckiNackiBridge.sol`) but not to Acki Nacki. Its `live_source.rs` (409 LOC) is a Phase-5.2 stub — trait definitions + `LiveBlockSource<P, G>` composition, no implementations. Its only AN-facing surface is `acki-nacki-interface::BkSetClient` (REST `/v2/bk_set{_update}`), which is sufficient for the `BkSetSentry`'s early-warning role but nowhere near enough to build a real witness for Circuits 1A/1B/2. Everything else is file-driven (`ProverProofsBlockSource` reads `proof_<seqno>.json` files written by *our* daemon).

Meanwhile our `bridge-prover-daemon` has the entire live pipeline working end-to-end — it just doesn't expose that pipeline as a library seam. All 850 LOC of genuinely reusable orchestration is inlined in `main.rs` next to ~350 LOC of daemon-only plumbing (CLI parsing, filesystem paths, ctrl-C handler, IPC write/wait, `self-verify` feature gate).

The gap is closed by a small extraction: promote the orchestration inlined in `main.rs` into a `bridge-prover-lib` module (`live_driver`), keeping the daemon's `main.rs` as a thin ~200-LOC harness (CLI + IPC + persistence). Sergey then wires his `RawBlockProvider` + `BoundProofGenerator` traits to call into that same module via a dep on `bridge-prover-lib`, and `LiveBlockSource` starts producing real `AnBlockData` without duplicating any GraphQL or witness-assembly code.

---

## 2. Ground truth (post-Explore-agent dive)

### 2.1 `bridge-prover-lib` — already-reusable inventory

Every module below is standalone, zero daemon coupling, already public.

| Module | LOC | Role in the live pipeline |
|---|---:|---|
| `gql_client` | 725 | Thin GraphQL wrapper. Domain-typed responses (`GqlProofBlock`, `ParsedAttestation`, `BkSetUpdateWithAttestations`). Methods: `query_latest_blocks`, `query_proof_block_by_seqno`, `query_attestation_envelopes`, `query_bk_set_updates_light`, `query_bk_set_updates_last`, `query_block_metadata`. |
| `attestation_fetcher` | 205 | High-level `fetch_attestation_evidence(gql, seq_no) → AttestationEvidence::{Primary,Fallback}`. Structural PRIMARY-only vs. PRIMARY+FALLBACK classifier — no heuristics. |
| `bk_set_fetcher` | 289 | `fetch_bk_set`, `next_update_after` (cursor walk over `bkSetUpdates`), `parse_bk_set_changes_pub` (apply adds/removes → new pubkey table). |
| `real_chain_builder` | 434 | `build_real_chain(gql, state, history_proofs, target_seqno, W) → RealChainResult` — reconstructs Poseidon-Merkle chains per layer via multiple GQL fetches. |
| `chain_proof_builder` | 168 | Stateless per-layer sibling assembly used by `real_chain_builder`. |
| `block_id_tree` | 103 | 8-leaf SHA-256 Merkle tree of block-id. `from_leaves`, `siblings_for_l0`, `build_layer_hashes_preimage`. |
| `prover` | 393 | `generate_primary_proof`, `generate_fallback_proof` + `_with_transcript` siblings. |
| `layer_prover` | 289 | `generate_layer_proof`, `generate_layer_proof_with_input` + `_with_transcript` siblings. |
| `verifier` | 251 | `verify_primary_proof`, `verify_layer_proof`, `verify_kzg_proof_with_transcript`. Only referenced from daemon's `self-verify` mode + verifier-daemon; live-driver doesn't need it. |
| `keys` | 957 | Per-circuit `KeyManager` split (Primary K=20, Fallback K=21, Layer K=17, Event K=19). Shared SRS anchored at max K (21). On-demand `load_/unload_*_pk`. |
| `bridge_state` | 499 | Persistent mirror of `AckiNackiBridge.storedGlobalHistoryData`. `append_bundle`, `apply_bk_set_update`, `save`, `load`. |
| `prover_bk_set` | 277 | Prover-private pubkey table (`signer_index → 48-byte BLS pubkey`). `rotate`, `save`, `load`. |
| `bootstrap` | 200 | `BootstrapSeed` (seed seqno + block hash); pinning, wait-for-chain, persist. |
| `transcript` | 623 | Blake2b + Poseidon Fiat–Shamir transcripts. Reused across prover/verifier. |
| `poseidon` + `poseidon_dense` | 179 | Poseidon primitives, `HISTORY_PROOF_WINDOW_SIZE = 128`, layer-tree constants. |
| `ipc` | 396 | File-based verifier IPC (`ProofRequest`, `BkUpdateRequest`, `wait_for_result`). Called from our daemon only — Sergey's daemon doesn't need it. |
| `types` | 104 | Domain types (`AccountRouting`, `ThreadIdentifier`, …). |

Total: **~5600 LOC** of already-reusable library. Nothing else needs to move.

### 2.2 `bridge-prover-daemon/src/main.rs` — coupling breakdown (1207 LOC)

**Reusable (≈850 LOC)** — inlined orchestration that belongs in `bridge-prover-lib`:

- `find_next_thinned_key_block` (lines 1066–1082) — pure fn, computes `((last_seen / (W·P)) + 1) * W·P`.
- Bootstrap flow (lines 298–359) — seed pinning, wait-for-chain, apply seed.
- Phase-3 bk-update drain loop (lines 418–709) — the whole thing.
- Attestation fetch + classify + retry (lines 712–743).
- 1A/1B proof generation with on-demand PK load/unload (lines 746–814).
- Circuit 2 proof via `generate_layer_proof_for_key_block` (lines 816–855 + helper at 1093–1195).
- State advance after verifier ACK (lines 1024–1035).

**Daemon-only (≈350 LOC)** — stays in `main.rs`:

- Env-var parsing (`BRIDGE_GQL_ENDPOINT`, `BRIDGE_BOOTSTRAP_SEQNO`).
- Ctrl-C handler + graceful-shutdown flag.
- Filesystem paths (`./state/`, `./proofs/`, `./params/`, `./bk_set.json`).
- Poll interval / verifier timeout constants.
- Log heartbeat formatting.
- `self-verify` feature-gate branch (an alternate path to file-based verifier IPC).
- `ipc::write_combined_proof` + `wait_for_result` calls.
- `state.save` + `prover_bk_set.save` after each acked event.

### 2.3 Sergey's side — what he has, what he doesn't

| Has | Doesn't have |
|---|---|
| `BlockSource::fetch(target) → Option<AnBlockData>` trait + file/fixture impls | Any AN block/attestation/history GraphQL fetcher |
| `BkUpdateSource::fetch(target) → Option<BkSetUpdateData>` trait + file impl | Any AN bk-set-update-blob parser |
| `EthBridgeClient::{submit_block, submit_bk_set_update, submit_withdraw, read_state}` | Any Circuit 1A/1B/2 proof generator |
| `BkSetSentry` polls `acki-nacki-interface::BkSetClient` for early rotation warning | Any way to build a 1A/1B witness against the OLD BK-set (for the update-block proof) |
| `live_source.rs` trait scaffold: `RawBlockProvider` + `BoundProofGenerator` + `LiveBlockSource<P, G>` — 409 LOC of stubs | Any live implementation of those two traits |
| `types::{AnBlockData, BkSetUpdateData, FinalizationType}` — his shape (U256 fields, `[u8; 32]` siblings, `Bytes` proofs) | The witness-to-proof plumbing that turns GQL data into `AnBlockData` |

He does **not** import `bridge-prover-lib` or `bridge-prover-orchestrator` today. `aggregator.rs` and Circuit 4 handling talks to subprocesses (`bridge-event-halo2-prover` + gnark), also file-driven for BK updates via `ProverProofsBlockSource` / `BkUpdateProofsSource`.

---

## 3. Proposal — extract `live_driver` into `bridge-prover-lib`

### 3.1 New module: `bridge_prover_lib::live_driver`

Single new module inside the existing lib (not a new sibling crate — the lib is already ~6000 LOC and Sergey will add a dep on it anyway). Roughly ~700 LOC lifted verbatim from `main.rs` and wrapped in a struct.

```
bridge-prover-lib/src/
  live_driver/
    mod.rs           // public API: LiveProverDriver, config, output payloads
    thinning.rs      // find_next_thinned_key_block + related pure fns
    bk_update.rs     // Phase-3 drain loop, ported from main.rs
    bundle.rs        // 1A/1B + Circuit 2 bundle assembly, ported from main.rs
    bootstrap.rs     // seed pinning + wait-for-chain, ported from main.rs
                     //   (or fold into existing bridge_prover_lib::bootstrap)
```

### 3.2 Public API surface

```rust
// bridge-prover-lib/src/live_driver/mod.rs

/// Configuration for the live driver. No filesystem paths, no CLI knobs.
/// Timeouts + intervals are policy the *caller* owns; the driver exposes
/// non-blocking `poll_*` methods.
pub struct LiveProverConfig {
    pub thinning_factor_p: u64,           // default THINNING_FACTOR_P (4)
    pub history_window_size: u64,         // default HISTORY_PROOF_WINDOW_SIZE (128)
    pub max_bk_updates_per_iter: usize,   // safety cap, default 8
    pub seed_policy: SeedPolicy,
}

pub enum SeedPolicy {
    /// Pin a specific seqno. Must be > 0 and divisible by (W·P).
    Explicit(u64),
    /// Auto-select: next (W·P)-aligned seqno past current chain head.
    Auto,
    /// Resume from a pre-existing BridgeState (already bootstrapped).
    Resume,
}

/// The live driver. Owns GQL client + KeyManager + mutable state.
/// Not persistence-aware — persistence stays with the caller.
pub struct LiveProverDriver {
    gql: GqlClient,
    key_manager: KeyManager,
    state: BridgeState,
    prover_bk_set: ProverBkSet,
    bk_set: HashMap<u16, Vec<u8>>,
    bk_set_commitment: Fr,
    cfg: LiveProverConfig,
    stage: DriverStage,
}

enum DriverStage {
    Bootstrapping { seed_seqno: u64, seed_block_id: Option<[u8; 32]> },
    Steady,
}

impl LiveProverDriver {
    /// Construct. Caller provides already-loaded state (or fresh from disk),
    /// pubkey table, KeyManager, and GQL endpoint.
    pub fn new(
        gql: GqlClient,
        key_manager: KeyManager,
        state: BridgeState,
        prover_bk_set: ProverBkSet,
        bk_set: HashMap<u16, Vec<u8>>,
        cfg: LiveProverConfig,
    ) -> Result<Self, DriverError>;

    /// Poll for the next BK-set rotation proof (Circuits 1A/1B against OLD set +
    /// 3-hop SHA-256 Merkle path). Must be drained *before* the next bundle can
    /// advance past it — if a pending rotation exists, `poll_next_bundle` will
    /// return `Nothing { blocked_by_pending_bk_update: true, .. }` until the
    /// caller acks the rotation.
    ///
    /// Non-blocking; returns immediately.
    pub async fn poll_next_bk_update(
        &mut self,
    ) -> Result<LiveBkUpdateEvent, DriverError>;

    /// Poll for the next bundle (Circuit 1A/1B + Circuit 2) for the next thinned
    /// key-block. Non-blocking.
    pub async fn poll_next_bundle(
        &mut self,
    ) -> Result<LiveBundleEvent, DriverError>;

    /// Acknowledge a bundle was successfully verified/submitted downstream.
    /// Idempotent by seq_no: no-op if the driver's cursor is already past
    /// `artifacts.block_seq_no`. Advances BridgeState in-memory.
    pub fn ack_bundle(
        &mut self,
        artifacts: &BundleProofArtifacts,
    ) -> Result<(), DriverError>;

    /// Acknowledge a bk-set update was successfully applied downstream.
    /// Idempotent by seq_no. Advances BridgeState + ProverBkSet in-memory.
    pub fn ack_bk_update(
        &mut self,
        artifacts: &BkUpdateProofArtifacts,
    ) -> Result<(), DriverError>;

    /// Read-only snapshots for persistence. Caller saves after every ack.
    pub fn snapshot_state(&self) -> &BridgeState;
    pub fn snapshot_prover_bk_set(&self) -> &ProverBkSet;
}

pub enum LiveBundleEvent {
    Bootstrapping { seed_seqno: u64, chain_head_seqno: u64 },
    Nothing {
        next_target_seqno: u64,
        chain_head_seqno: u64,
        /// True if the driver has a pending bk-update the caller hasn't acked
        /// yet — the block cursor cannot advance until `poll_next_bk_update`
        /// yields the rotation and the caller acks it.
        blocked_by_pending_bk_update: bool,
    },
    Bundle(BundleProofArtifacts),
}

pub enum LiveBkUpdateEvent {
    Bootstrapping { seed_seqno: u64, chain_head_seqno: u64 },
    Nothing,
    BkUpdate(BkUpdateProofArtifacts),
}
```

**Why two methods instead of one.** Sergey's `LiveBlockSource<P, G>` composes his `BlockSource` and `BkUpdateSource` traits — both single-method `fetch(target) → Option<T>` shapes. A two-method driver API maps 1:1:

```rust
impl BlockSource for LiveBlockSource { async fn fetch(&self, tgt) → … {
    self.driver.lock().await.poll_next_bundle().await …
}}
impl BkUpdateSource for LiveBlockSource { async fn fetch(&self, tgt) → … {
    self.driver.lock().await.poll_next_bk_update().await …
}}
```

No `PendingEvent` state machine, no routing logic inside `LiveBlockSource`. The "bk-update blocks bundle advance" invariant lives entirely inside the driver, surfaced via the `blocked_by_pending_bk_update` bit in `LiveBundleEvent::Nothing`.

### 3.3 Output payloads — shape once, consume twice

`BundleProofArtifacts` and `BkUpdateProofArtifacts` must be shaped so that **both** our IPC verifier (`ipc::ProofRequest`) **and** Sergey's `AnBlockData` / `BkSetUpdateData` can be built from them with a trivial `From` on each caller's side.

**No `Fr` in the public payload.** Everything is `[u8; 32]` big-endian. Rationale: keeping `Fr` (halo2 field element) out of the payload means Sergey's `bridge-relayer-daemon` does not transitively pull the halo2 dep chain (`halo2-axiom`, `halo2curves`, gate hierarchy). Our IPC verifier reconstructs `Fr` from the 32-byte views in ~5 LOC per field — it already does this from IPC JSON strings today. This costs nothing on our side and buys Sergey a cleaner dep graph.

```rust
pub struct BundleProofArtifacts {
    // Identity
    pub block_seq_no: u64,
    pub block_id_be: [u8; 32],                      // big-endian
    pub fin_type: FinalizationType,                 // Primary or Fallback

    // Public inputs shared by Circuits 1A/1B and 2
    pub bk_set_commitment_be: [u8; 32],
    pub num_layers: u8,                             // 1..=MAX_LAYER_HASHES (10)
    pub layer_hashes_be: Vec<[u8; 32]>,             // len == num_layers; caller pads if needed
    pub prev_max_level_layer_hash_be: [u8; 32],

    // Proof bytes (Blake2b transcript — the AN-VM `ZKHALO2VERIFYWITHVK` opcode shape)
    pub attestation_proof: Vec<u8>,                 // 1A or 1B halo2 SHPLONK proof
    pub layer_hashes_proof: Vec<u8>,                // Circuit 2 halo2 SHPLONK proof
}

pub struct BkUpdateProofArtifacts {
    pub block_seq_no: u64,
    pub block_id_be: [u8; 32],
    pub fin_type: FinalizationType,
    pub old_bk_set_commitment_be: [u8; 32],
    pub new_bk_set_commitment_be: [u8; 32],
    pub sibling_h0_be: [u8; 32],                    // for on-chain 3-hop SHA-256 path
    pub sibling_h23_be: [u8; 32],
    pub attestation_proof: Vec<u8>,                 // 1A or 1B against OLD BK-set

    // Post-rotation pubkey table so caller can rotate its ProverBkSet snapshot
    // (mirrors the `ack_bk_update` in-memory rotation — caller uses this to
    // persist the *new* prover_bk_set.json)
    pub new_pubkeys: HashMap<u16, Vec<u8>>,
}
```

Both payloads are pure data — no `KeyManager`, no `GqlClient`, no `Fr`, no daemon state. Both are `Serialize` + `Deserialize` so they can be dropped into a file for cross-process IPC if a caller wants that (Option B for Sergey, §5.2 below).

### 3.4 What stays with the caller

- **Persistence.** Driver holds `BridgeState` + `ProverBkSet` in-memory only. Caller calls `driver.snapshot_state()` + `state.save(&path)` **after every ack**. Same for `prover_bk_set`. Our daemon uses `./state/*.json`; Sergey's daemon uses whatever fits his layout. No `dirty` bit / event hook on the driver — persistence cadence is entirely the caller's choice, and "save after every ack" is the safe default that works for both daemons.
- **Downstream verification.** Driver returns proofs. Our daemon writes `ipc::ProofRequest` → verifier daemon → waits. Sergey's daemon converts to `AnBlockData` → `EthBridgeClient::submit_block`. Same payload, different downstream.
- **Timeouts and retries.** Neither `poll_next_bundle` nor `poll_next_bk_update` sleeps. Caller decides poll interval + backoff. This is the right split — our daemon retries at 3 s tight loop; Sergey's daemon has `BackoffConfig` (exponential); a test harness might poll instantly.
- **Failure surface.** `DriverError` distinguishes `GqlTransient` (retry OK), `GqlSchema` (log + retry, maybe alert), `ProofGen` (fatal for that seq_no, caller decides), `StateInconsistent` (fatal, wipe + rebootstrap).

### 3.5 State ownership + concurrency

Driver is `!Sync` (owns `KeyManager` with multi-GB PKs, mutable `BridgeState`). One driver per daemon. No async cancellation safety concerns — the async points are the GQL fetches; if the future is dropped, we might lose an in-flight proof but the in-memory state hasn't advanced yet (`ack_*` is a separate call). The only cost is wasted CPU on the interrupted proof.

### 3.6 Bootstrap semantics

Three modes under `SeedPolicy`, all live in the driver so both daemons pick their strategy through the same knob:

- `SeedPolicy::Explicit(N)` — pin `N` (must be `> 0 && N % (W·P) == 0`). Emit `Bootstrapping { seed_seqno: N, chain_head_seqno }` from `poll_next_*` while `chain_head < N`. Transition to `Steady` once `chain_head >= N` and the seed block has been fetched + applied to `state`. Used for manual/reproducible starts (`BRIDGE_BOOTSTRAP_SEQNO=N`).
- `SeedPolicy::Auto` — on first `poll_*` call, pick the next `(W·P)`-aligned seqno past the current chain head, cache it in `DriverStage::Bootstrapping`, then behave like Explicit. Used for automatic starts on a long-running chain (our daemon's current default; Sergey's daemon can pick this when running on a live shellnet with no state on disk).
- `SeedPolicy::Resume` — no bootstrap phase. Assume `BridgeState` is already past the seed; jump straight to `Steady`. Used when the caller has loaded state from disk (both daemons on restart).

### 3.7 Async model

Driver is fully `async`. Our daemon's current `main.rs` already uses a Tokio runtime (for GQL). Sergey's daemon is natively async. No sync/async bridging required in either case.

### 3.8 GraphQL as authoritative source of BK-set data

Sergey's `acki-nacki-interface::BkSetClient` (REST `/v2/bk_set{_update}`) and his `BkSetSentry` become **retirement candidates** once the driver lands. GraphQL is authoritative because it is the only source that carries the `bkSetUpdates` event log the rotation proof needs (block ID + `bk_set_update_hex` blob + attestation-envelope pointers). Sergey's REST currently gives him only snapshots (`current` + `future` at a specific seqno), which is enough for an early-warning sentry but not enough to build the OLD-set attestation witness for `applyBkSetUpdate`.

To make the migration painless, `bridge-prover-lib::gql_client` should expose the small set of "current-membership" shortcuts Sergey's REST consumers currently rely on. Concrete additions (each is a thin wrapper around existing internal queries — no new schema, no new orchestration):

| Sergey's REST call today | Proposed GQL shortcut | Return type |
|---|---|---|
| `BkSetClient::fetch_bk_set()` (compact snapshot) | `gql_client::query_current_bk_set_compact(&gql) → CompactBkSetSnapshot` | `{ seq_no, entries: Vec<CompactBkEntry { node_id, node_owner_pk, epoch_start_seq_no }> }` |
| `BkSetClient::fetch_bk_set_update()` (full snapshot with 48-byte BLS pubkeys) | `gql_client::query_current_bk_set_full(&gql) → FullBkSetSnapshot` | `{ seq_no, entries: Vec<FullBkEntry { pubkey_48, signer_index, epoch_finish_seq_no, address, stake, owner_address, … }> }` |
| `BkSetClient::fetch_signer_index_bk_set()` (`HashMap<u32, Vec<u8>>`) | `gql_client::query_current_signer_index_bk_set(&gql) → HashMap<u16, Vec<u8>>` | Same shape (note: signer_index widened from `u32` → `u16` to match `bridge-prover-lib`'s existing convention; documented) |

The three shortcuts all compose from the same three primitives already inside `bridge-prover-lib`: `bk_set_fetcher::fetch_bk_set` (checked-in genesis-like snapshot as the starting point), `gql_client::query_bk_set_updates_last` (walk forward from that snapshot's seqno), `bk_set_fetcher::parse_bk_set_changes_pub` (apply each update). Total new code: ~120 LOC of format-adaptation on top of already-tested internals — no new GraphQL queries, no new witness assembly.

Sergey then rewrites `BkSetSentry` to poll `query_current_signer_index_bk_set` every N seconds and diff against the previous snapshot's membership hash — same detection logic he has today, just a different transport. His `acki-nacki-interface` crate's `BkSetClient` can be retired in the same PR (or kept as an offline debug tool if he prefers, but not wired into the sentry).

Any *extra* field Sergey needs that we don't expose today: add it as an additional GQL shortcut in the same module. The rule is: `bridge-prover-lib::gql_client` is the single authoritative AN-facing surface for both daemons.

---

## 4. Post-refactor: `bridge-prover-daemon/src/main.rs`

Target: ~200 LOC, ~85% shrinkage.

```rust
// bridge-prover-daemon/src/main.rs (sketch)

use bridge_prover_lib::{
    gql_client::create_client,
    keys::KeyManager,
    bridge_state::BridgeState,
    prover_bk_set::ProverBkSet,
    ipc,
    live_driver::{
        LiveProverDriver, LiveProverConfig, SeedPolicy,
        LiveBundleEvent, LiveBkUpdateEvent,
    },
};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> Result<()> {
    setup_tracing();
    let cfg = load_config_from_env();                    // GQL endpoint, seed override, paths
    let shutdown = install_ctrl_c_flag();

    let gql = create_client(&cfg.gql_endpoint);
    let key_manager = KeyManager::load_or_init(&cfg.params_dir)?;
    let (bk_set, bk_set_commitment) = fetch_bk_set_with_file_fallback(&gql, &cfg.bk_set_json)?;
    let state = BridgeState::load_or_default(&cfg.state_dir)?;
    let prover_bk_set = ProverBkSet::load_or_seed(&cfg.state_dir, &bk_set)?;

    let seed_policy = match cfg.bootstrap_seqno {
        Some(n) => SeedPolicy::Explicit(n),
        None if state.is_empty() => SeedPolicy::Auto,
        None => SeedPolicy::Resume,
    };

    let mut driver = LiveProverDriver::new(
        gql,
        key_manager,
        state,
        prover_bk_set,
        bk_set,
        LiveProverConfig { seed_policy, ..Default::default() },
    )?;

    while !shutdown.load(Ordering::Relaxed) {
        // Drain bk-set rotations first — bundles are blocked behind them.
        match driver.poll_next_bk_update().await? {
            LiveBkUpdateEvent::Bootstrapping { seed_seqno, chain_head_seqno } => {
                info!(target: "bootstrap", seed_seqno, chain_head_seqno, "waiting");
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            LiveBkUpdateEvent::BkUpdate(update) => {
                let req = ipc::BkUpdateRequest::from(&update);
                ipc::write_bk_update_request(&cfg.proofs_dir, &req)?;
                let result = ipc::wait_for_bk_update_result(
                    &cfg.proofs_dir, update.block_seq_no, VERIFIER_TIMEOUT,
                ).await?;
                if result.verify_ok {
                    driver.ack_bk_update(&update)?;
                    persist(&cfg, driver.snapshot_state(), driver.snapshot_prover_bk_set())?;
                    continue;
                } else {
                    error!(?result, "verifier rejected bk-update proof");
                    break;
                }
            }
            LiveBkUpdateEvent::Nothing => { /* fall through to bundle poll */ }
        }

        match driver.poll_next_bundle().await? {
            LiveBundleEvent::Bootstrapping { seed_seqno, chain_head_seqno } => {
                info!(target: "bootstrap", seed_seqno, chain_head_seqno, "waiting");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            LiveBundleEvent::Nothing { next_target_seqno, chain_head_seqno, .. } => {
                trace!(target: "poll", next_target_seqno, chain_head_seqno, "no new key block");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            LiveBundleEvent::Bundle(bundle) => {
                let req = ipc::ProofRequest::from(&bundle);
                ipc::write_combined_proof(&cfg.proofs_dir, &req)?;
                let result = ipc::wait_for_result(
                    &cfg.proofs_dir, bundle.block_seq_no, VERIFIER_TIMEOUT,
                ).await?;
                if result.primary_verified && result.layer_verified {
                    driver.ack_bundle(&bundle)?;
                    persist(&cfg, driver.snapshot_state(), driver.snapshot_prover_bk_set())?;
                } else {
                    error!(?result, "verifier rejected bundle");
                    break;
                }
            }
        }
    }
    Ok(())
}
```

`self-verify` mode gets a `verify_bundle_inline(&BundleProofArtifacts) -> Result<bool>` helper that lives next to `main.rs` and calls `verifier::verify_primary_proof` + `verify_layer_proof` directly — swaps in for the IPC branch under the feature flag.

The two `From` conversions are ~30 LOC total:

```rust
impl From<&BundleProofArtifacts> for ipc::ProofRequest { /* fill schema_v4 fields */ }
impl From<&BkUpdateProofArtifacts> for ipc::BkUpdateRequest { /* fill schema_v4 fields */ }
```

---

## 5. Recommendation for Sergey's `bridge-relayer-daemon`

We do not modify his crate; this section is the recommendation we hand him.

### 5.1 Option A (recommended) — depend on `bridge-prover-lib` directly

Sergey adds `bridge-prover-lib = { path = "../an-bridge-prover/bridge-prover-lib" }` to `bridge-relayer-daemon/Cargo.toml`. His `live_source.rs` is rewritten as (~60 LOC + ~40 LOC of `From` conversions):

```rust
// crates/bridge-relayer-daemon/src/live_source.rs

use bridge_prover_lib::live_driver::{
    LiveProverDriver, LiveProverConfig, SeedPolicy,
    LiveBundleEvent, LiveBkUpdateEvent,
    BundleProofArtifacts, BkUpdateProofArtifacts,
};

/// One driver shared by both trait impls. The driver's internal state
/// machine enforces "bk-updates drained before bundles advance", so both
/// impls can just call the matching `poll_next_*` blindly.
pub struct LiveBlockSource {
    driver: Mutex<LiveProverDriver>,
}

#[async_trait::async_trait]
impl BlockSource for LiveBlockSource {
    async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let mut driver = self.driver.lock().await;
        match driver.poll_next_bundle().await.map_err(map_err)? {
            LiveBundleEvent::Bootstrapping { .. } | LiveBundleEvent::Nothing { .. } => Ok(None),
            LiveBundleEvent::Bundle(b) if b.block_seq_no >= target => {
                Ok(Some(AnBlockData::from(&b)))
            }
            LiveBundleEvent::Bundle(b) => {
                // Fall forward like ProverProofsBlockSource — driver produced
                // a bundle past `target`, ack and skip.
                driver.ack_bundle(&b).map_err(map_err)?;
                Ok(None)
            }
        }
    }
}

#[async_trait::async_trait]
impl BkUpdateSource for LiveBlockSource {
    async fn fetch(&self, target: u64) -> Result<Option<BkSetUpdateData>, RelayerError> {
        let mut driver = self.driver.lock().await;
        match driver.poll_next_bk_update().await.map_err(map_err)? {
            LiveBkUpdateEvent::Bootstrapping { .. } | LiveBkUpdateEvent::Nothing => Ok(None),
            LiveBkUpdateEvent::BkUpdate(u) if u.block_seq_no >= target => {
                Ok(Some(BkSetUpdateData::from(&u)))
            }
            LiveBkUpdateEvent::BkUpdate(u) => {
                driver.ack_bk_update(&u).map_err(map_err)?;
                Ok(None)
            }
        }
    }
}
```

Plus two `From` impls on his side (~40 LOC total, pure byte plumbing — no `Fr`, no halo2 in his dep graph):

```rust
// in bridge-relayer-daemon/src/types.rs (or a new an_block_data_conv.rs)
impl From<&BundleProofArtifacts> for AnBlockData {
    fn from(b: &BundleProofArtifacts) -> Self {
        AnBlockData {
            fin_type: b.fin_type,
            block_id: U256::from_be_bytes(b.block_id_be),
            bk_set_commitment: U256::from_be_bytes(b.bk_set_commitment_be),
            block_seq_no: b.block_seq_no,
            num_layers: b.num_layers,
            layer_hashes: pad_to_10(&b.layer_hashes_be),
            prev_max_level_layer_hash: U256::from_be_bytes(b.prev_max_level_layer_hash_be),
            attestation_proof: Bytes::from(b.attestation_proof.clone()),
            layer_hashes_proof: Bytes::from(b.layer_hashes_proof.clone()),
        }
    }
}
impl From<&BkUpdateProofArtifacts> for BkSetUpdateData { /* similar shape */ }
```

His existing `Relayer::tick()`, `BackoffConfig`, `SentryGuardedRelayer`, `RelayerState`, `EthBridgeClient::submit_block` — **all unchanged**. Ack of a bundle/update happens automatically inside `LiveBlockSource` when the next `fetch(target)` arrives with `target > b.block_seq_no` — meaning Sergey's daemon's next `tick()` after a successful submit is what advances the driver's cursor, without either side needing to know about it. This is the same idempotent-by-seqno pattern `ProverProofsBlockSource` already uses.

**Sergey's `BkSetSentry` and `acki-nacki-interface::BkSetClient` retirement.** Per §3.8, GraphQL is now authoritative for all BK-set data. Sergey has two options:

- **5.1a (recommended long-term).** Rewrite `BkSetSentry` to poll `bridge_prover_lib::gql_client::query_current_signer_index_bk_set` and diff membership hashes — same detection logic he has today, GQL transport instead of REST. `acki-nacki-interface::BkSetClient` is deleted (or preserved as an offline debug tool, unwired). Landing in the same PR as the `live_source.rs` swap keeps his crate's AN-facing surface single-sourced.
- **5.1b (short-term, if he doesn't want the sentry churn).** Keep the REST-backed sentry as an early-warning latency optimisation (pause the ETH submit lane the moment REST sees a rotation, don't wait for the GQL cursor to catch up — typically 1-2 blocks). The driver remains authoritative for the actual rotation proof. Delete `BkSetClient` when convenient.

Either way, his `bk_set_sentry.rs` semantics stay the same; only the poller trait's backing impl changes.

### 5.2 Option B (fallback) — Sergey keeps his file-based path

If Option A is undesirable (dep-graph concerns, isolation), we can ship a `bridge-prover-driver-cli` binary in our crate that writes `BundleProofArtifacts` + `BkUpdateProofArtifacts` as JSON to a spool directory, matching Sergey's existing `ProverProofsBlockSource` / `BkUpdateProofsSource` schema. Then he changes nothing except env-configuring the spool path. This is strictly worse than Option A (extra disk hop, no shared cursor, two independent daemons), but it exists if we need to fully decouple deployment.

### 5.3 What Sergey does not need to touch (in either option)

`bridge.rs`, `daemon.rs`, `guarded_relayer.rs`, `bk_set_sentry.rs`, `bin/relayer.rs`, `relayer.rs`, `source.rs` (the trait definitions), `state.rs`, `types.rs`, `error.rs`, `proof_validation.rs` — all unchanged. Only `live_source.rs` (and `Cargo.toml`) change. He can pull our library the same day the driver lands and be end-to-end live within a small commit.

---

## 6. Resolved decisions (locked in 2026-07-08)

All eight open questions from the initial draft are now resolved. Recorded here for reviewer traceability.

1. **Extraction unit — RESOLVED: module.** `bridge_prover_lib::live_driver` as a new module inside the existing lib. No sibling crate. Sergey depends on `bridge-prover-lib` in one line of `Cargo.toml`; downstream compile-time cost is acceptable given he needs the whole surface anyway.

2. **`SeedPolicy` location — RESOLVED: in the driver, three modes.** `SeedPolicy::{Explicit, Auto, Resume}` all live in the library so both daemons pick their strategy through the same knob. Rationale: our daemon supports both manual (`BRIDGE_BOOTSTRAP_SEQNO=N`) and automatic (long-running-chain) starts today; Sergey's daemon will need the same choice when running against a live shellnet with no state on disk. Encoding the policy centrally is cheaper than making each daemon reimplement the "no state yet" branch.

3. **BK-set fetch — RESOLVED: GraphQL is authoritative.** `bridge-prover-lib::gql_client` is the single AN-facing surface for both daemons. Sergey's REST-backed `acki-nacki-interface::BkSetClient` is a retirement candidate; his consumers get everything they need from GQL, with the three `query_current_*_bk_set{,_compact,_full}` shortcuts documented in §3.8 filling the format-adaptation gap. Any additional field Sergey needs → add another GQL shortcut in the same module.

4. **Bundle-vs-update event shape — RESOLVED: two methods.** `poll_next_bundle()` + `poll_next_bk_update()`. Chosen for Sergey's convenience — his `BlockSource` and `BkUpdateSource` traits are already two independent single-method surfaces, so two driver methods map 1:1 with zero routing logic on his side. The "bk-updates block bundle advance" invariant lives inside the driver, surfaced via `LiveBundleEvent::Nothing { blocked_by_pending_bk_update, .. }`. Costs us a double-poll in `main.rs` (~15 extra LOC) — trivial.

5. **Field-element views — RESOLVED: `_be` only, no `Fr`.** Payload carries only `[u8; 32]` big-endian views. Rationale: no halo2 transitive dep leak into Sergey's crate. Our IPC verifier reconstructs `Fr` from bytes in ~5 LOC per field — it already does exactly this from IPC JSON strings today, so the cost is zero.

6. **`self-verify` feature gate — RESOLVED: keep.** Needed for our CI tests inside the Acki Nacki node. Post-refactor: `verify_bundle_inline(&BundleProofArtifacts) -> Result<bool>` sits next to `main.rs`, feature-gated as today. ~40 LOC — trivial to maintain.

7. **State-save granularity — RESOLVED: caller saves after every ack.** Driver holds `BridgeState` + `ProverBkSet` in-memory only. No `dirty` bit, no event hook. Caller pattern is `ack_* → snapshot_* → state.save(&path)`. This works for both daemons — our daemon does it today; Sergey's daemon can do the same in his `tick()`-after-submit path.

8. **Naming — RESOLVED: `live_driver`.** Module is `bridge_prover_lib::live_driver`. Public types: `LiveProverDriver`, `LiveProverConfig`, `LiveBundleEvent`, `LiveBkUpdateEvent`, `BundleProofArtifacts`, `BkUpdateProofArtifacts`, `SeedPolicy`, `DriverError`.

---

## 7. Non-goals for this round

- Circuit 4 (event / withdraw). Sergey's `aggregator.rs` + `withdraw_prover.rs` + `withdrawal.rs` stay untouched; we discuss that separately in a later round.
- Retiring `bridge-verifier-daemon` or changing the IPC schema. The verifier-daemon remains the local model of the on-chain bridge; our daemon still talks to it via the same JSON files. Sergey's daemon skips the verifier entirely and goes straight to ETH.
- Deleting `acki-nacki-interface::BkSetClient` / `BkSetSentry` in this round. Per decision 3, GraphQL is authoritative going forward and the REST client is a retirement candidate, but the actual delete can land in Sergey's PR-C (or a follow-up) — not in our PR-A/PR-B. The recommended sentry rewrite is described in §5.1a.
- Cross-crate rustdoc / architecture docs. Once the code lands, we'll add a short "consumer's guide" to `TECHNICAL_README.md` alongside the bundle-only runbook.

---

## 8. Rough shape of the PR sequence

If we go with Option A:

1. **PR-A (this repo).** Add `bridge_prover_lib::live_driver`. Port `main.rs` orchestration into it (module-per-concern: `thinning`, `bootstrap`, `bk_update`, `bundle`). No behavior change to `main.rs` yet — just adds the module + tests.
2. **PR-B (this repo).** Thin `bridge-prover-daemon/src/main.rs` to consume the new module. Verify E2E on shellnet (`bundle-only` runbook from TECHNICAL_README §Runbook). Should be a wash — same proofs, same IPC files, same verifier-daemon.
3. **PR-C (Sergey, cross-repo).** Recommend text: swap `live_source.rs` for the `LiveProverDriver`-backed impl, ~2 `From` conversions, `Cargo.toml` dep add. Verify against his mock-bridge test harness, then shellnet Sepolia.

Each PR is independently landable + revertable.

---

## Appendix — mapping table (for the implementation itself, not for review)

| `main.rs` region (lines) | Destination in `live_driver` | Notes |
|---|---|---|
| 130–172 (init: env + shutdown) | Stays in `main.rs` | Daemon-only |
| 174–276 (BK-set + KeyManager + state + prover_bk_set load) | Stays in `main.rs` | Daemon owns persistence |
| 298–359 (bootstrap seed) | `live_driver::bootstrap` | Driver owns; caller triggers via `SeedPolicy` |
| 366–401 (main loop preamble, poll latest) | Split: `poll_next_bundle` reads chain head; `poll_next_bk_update` walks `bkSetUpdates` cursor | |
| 403–709 (Phase 3 bk-update drain) | `live_driver::bk_update` | Emits `LiveBkUpdateEvent::BkUpdate` |
| 712–743 (attestation fetch + classify) | `live_driver::bundle` | Feeds proof gen |
| 746–814 (1A/1B proof) | `live_driver::bundle` | Uses `bridge_prover_lib::prover` |
| 816–855 (Circuit 2 proof) | `live_driver::bundle` | Uses `layer_prover` + `real_chain_builder` |
| 909–1022 (verify + advance) | Split: verify stays in daemon; advance is `driver.ack_bundle` |
| 1024–1035 (state save) | Stays in `main.rs` | Uses `driver.snapshot_state` |
| 1066–1082 (thinned key block helper) | `live_driver::thinning` (pure fn) |
| 1093–1195 (`generate_layer_proof_for_key_block`) | `live_driver::bundle` | Already a helper — port verbatim |
