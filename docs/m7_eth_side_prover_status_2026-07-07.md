# M7 ETH-side prover + runtime aggregation — status (2026-07-07)

Closes the novel half of R15/M7: **runtime aggregation** of a Poseidon inner
snark into the EVM calldata the *already-deployed* SHPLONK aggregator verifier
accepts on-chain, driven from **real Acki Nacki data**.

## What is DONE and proven

### 1. Runtime aggregation binary — `aggregate-proof`
`crates/bridge-evm-aggregator/src/bin/aggregate_proof.rs`

- Input: a Poseidon inner `.snark`. Output: EVM calldata `instances ‖ proof`
  (22 instances = 12 KZG accumulator limbs + 10 Circuit-4 public inputs; 3616 B).
- **Self-check**: regenerates the aggregator Yul `.bin` for the supplied inner
  snark and refuses to emit calldata unless it is **byte-identical** to the
  committed `contracts/ethereum/verifiers/<name>.bin`. VK drift ⇒ hard error.

### 2. ETH-side Circuit-4 prover (`our_side_reprove`)
`crates/bridge-prover-orchestrator/src/bin/export_c4_poseidon_snark.rs`

- New `--fixture <PrivateWitness.json>`: re-proves a **real** withdrawal witness
  (from the live `bridge-event-witness-builder`) with a **Poseidon** transcript
  and exports the aggregator inner `.snark`. The AN-side default is Blake2b; the
  aggregator only consumes Poseidon — so the ETH leg re-proves on our side.
- `--seed` retained for synthetic distinct-PI snarks (universality checks).

### 3. Relayer wiring — `crates/bridge-relayer-daemon/src/aggregator.rs`
- `Circuit4SnarkProver` (+ `SubprocessCircuit4SnarkProver` → `export-c4-poseidon-snark --fixture`, + `MockCircuit4SnarkProver`).
- `ProofAggregator` (+ `SubprocessAggregator` → `aggregate-proof`, + `MockAggregator`).
- `Circuit4ShplonkPipeline`: witness → Poseidon snark → aggregate → calldata,
  with `calldata_binds_instances` cross-check (calldata words 12..21 BE must equal
  the ten LE public inputs) before surfacing the proof.
- CLI: `relayer prove-withdraw-shplonk` → writes a `proof_event.json` whose
  `proof_hex` is the aggregator calldata; `submit-withdraw` / `daemon-withdraw` /
  `daemon-bridge` consume it unchanged (validation already accepts ≥704 B calldata).
- 56 relayer unit tests green (6 new), clippy + fmt clean.

### 4. Proven on-chain with REAL shellnet data (n14, 2026-07-07)
Real withdrawal witness from ursus
(`work-shellnet/event_000000/witness.json`, live shellnet `WithdrawalInitiated`):

| Step | Result |
|------|--------|
| `export-c4-poseidon-snark --fixture real_witness.json` | Poseidon SELF_VERIFY PASS, 10 PIs |
| Our 10 PIs vs partner `proof_event_000000.json` | **all 10 MATCH** byte-for-byte |
| `aggregate-proof` | VK match: regenerated == committed `BridgeWithdrawalAggregatorVerifier.bin` (20987 B) |
| Foundry `AckiNackiBridgeProductionWithdrawByProof` on the real calldata | **3/3 PASS** — deployed real verifier accepts; tamper/mismatch rejected |

Universality (2 distinct inner snarks, different PIs) also confirmed earlier:
both regenerate the identical committed `.bin` and both calldata verify on-chain.

## 1A / 1B / 2 live → Poseidon — DONE (not blocked)

> **Correction (2026-07-08).** An earlier revision of this doc claimed the 1A/1B/2
> live→Poseidon leg was blocked because `bridge-prover-daemon` is a binary-only
> crate. **That was wrong.** The live-witness code is *not* private to the daemon —
> it lives in **Alina's `bridge-prover-lib`** and every module is `pub`:
> `gql_client`, `attestation_fetcher`, `bk_set_fetcher`, `real_chain_builder`,
> `block_id_tree`, `chain_proof_builder`, `bridge_state`, `bootstrap`. The daemon's
> `main.rs` is **pure orchestration** over those public functions (it `use`s
> `bridge_prover_lib::{attestation_fetcher, gql_client, real_chain_builder, ...}`).
> So the ETH leg reuses the same lib exactly like Circuit 4 — no partner refactor
> needed.

### ETH-side 1A/1B/2 prover — `export-1a1b2-poseidon-snark`
`crates/bridge-prover-orchestrator/src/bin/export_1a1b2_poseidon_snark.rs`

Mirrors `export-c4-poseidon-snark` for the `verifyBlock` leg. Live fetch + prove
via `bridge-prover-lib`'s public API, Poseidon transcript, snark-verifier `.snark`
export for the R15 aggregator:

- `--circuit primary|fallback|layer|auto` (auto = classify the attestation
  evidence: `[PRIMARY]` → 1A, `[PRIMARY, FALLBACK]` → 1B).
- **1A/1B**: `gql_client::create_client` → `attestation_fetcher::fetch_attestation_evidence`
  + `bk_set_fetcher::fetch_bk_set` → `prover::generate_{primary,fallback}_proof_with_transcript(Poseidon)`
  → native Poseidon self-verify → `export_poseidon_snark` (`primary`/`fallback` VK+config).
- **Circuit 2**: replays the daemon's `generate_layer_proof_for_key_block`
  (`query_proof_block_by_seqno` → `block_id_tree::{build_layer_hashes_preimage,BlockIdMerkleTree}`
  → `real_chain_builder::build_real_chain` over a daemon `state.json`) then
  `layer_prover::generate_layer_proof_with_transcript(Poseidon)`. Needs the
  daemon's persisted `--state state.json` (the running chain anchor) — that's the
  same real input the Blake2b daemon consumes, not a mock.

Downstream is identical to C4: feed the `.snark` to `aggregate-proof --name
{PrimaryAggregatorVerifier|FallbackAggregatorVerifier|LayerHashesAggregatorVerifier}`
→ EVM calldata, self-checked byte-for-byte against the committed verifier `.bin`.

Builds/runs on n14 (halo2-axiom git deps, `cargo +nightly`). Attestation (1A/1B)
is stateless (attestation bytes + bk_set + last_seen scalar); Circuit 2
additionally consumes the daemon `state.json`.

### Live n14 run (2026-07-08) — tool works, blocked on shellnet genesis BK set

Ran `export-1a1b2-poseidon-snark --circuit auto` against **live shellnet**
(`https://shellnet.ackinacki.org/graphql`), real finalized key blocks
(~seq 1,982,532 / 1,983,571). The full path executed on real data:

| Step | Result |
|------|--------|
| `gql_client` + `attestation_fetcher::fetch_attestation_evidence` | live PRIMARY evidence, 5 signers — **OK** |
| `KeyManager` load primary VK/PK (3.7 GB, K=20) + SRS downsize 21→20 | **OK** |
| `prover::generate_primary_proof_with_transcript(Poseidon)` | proof generated — **OK** |
| `verifier::verify_primary_proof_with_transcript(Poseidon)` | **SELF_VERIFY FAIL** — tool refuses to export (correct safety gate) |

**Root cause — pinpointed, not the code**: the BK set used to check the BLS
signatures is stale. Added a fast pre-flight `--check-bk-set` (fetch the block's
`block_merkle_tree_leaves[2]` = the node's BK-set Poseidon commitment, compare to
`compute_bk_set_poseidon(loaded_set)`; ~1 s, no proving). Both cached configs
mismatch the live node:

```
block.leaves[2] (shellnet, current)                 = 5f434241f8bfd6b3f8364117de3d6583290be7adb3fe99596685e667bbdfa113
compute_bk_set_poseidon(prover/bk_set.json)         = e62d8b5f… → MISMATCH
compute_bk_set_poseidon(an-bridge-prover/bk_set.json)= 93f04b1d… → MISMATCH
```

**Why we can't self-serve the set**: the current shellnet BK set was established
at **genesis** and shellnet's public API does not expose it —
`blockchain.bkSetUpdates` is empty (`edges: []`; updates only record *changes*),
there is no `/v2/bk_set` REST (404), and `BlockchainQuery` has no validators/BK
field carrying pubkeys. The two cached `bk_set.json` on n14 are both from the
local `poseidon_dex` stand (see the `*.poseidon_dex_local.bak` siblings), not
shellnet.

**Partner ask (unblocks 1A/1B/2 live)**: provide the shellnet genesis BK-set
pubkeys — the 5 × 48-byte BLS keys (index → pubkey) whose
`compute_bk_set_poseidon` equals `5f434241f8bfd6b3f8364117de3d6583290be7adb3fe99596685e667bbdfa113`.
Drop it as `bk_set.json` and re-run:

```bash
# verify first (1 s, no proving):
export-1a1b2-poseidon-snark --endpoint https://shellnet.ackinacki.org/graphql \
  --seqno <recent> --circuit primary --check-bk-set --bk-set-config bk_set.json
# then prove for real (expect SELF_VERIFY PASS):
export-1a1b2-poseidon-snark --endpoint https://shellnet.ackinacki.org/graphql \
  --seqno <recent> --circuit auto --params-dir ../../params \
  --snark-dir <out> --name circuit1a --bk-set-config bk_set.json
```

> Note this is orthogonal to Circuit 4 (withdrawal), which needs **no** BK set —
> which is exactly why the C4 M7 leg is green on real shellnet data while the
> verifyBlock leg waits on this one genesis artifact. It's also independent of
> the pending 16-leaf `block_id` / PR#3 re-keygen (`node16leaf` / `regen-c2-wrap`):
> the FAIL here is proven to be the BK set, not circuit shape.

## D — shellnet withdraw-bridge redeploy (dry-run green, broadcast gated)

`script/DeployShellnetE2EBridge.s.sol` deploys all four real verifiers
(1A/1B/2 + C4) + bridge in one batch. Local simulate (dummy env) succeeds,
gas ≈ 21.3M, EIP-170 OK. Real broadcast recipe (gated — needs correct shellnet
genesis anchor + real `WITHDRAW_ACC_FR` + deploy key + spends Sepolia ETH +
changes the canonical withdraw-bridge address):

```bash
cd contracts/ethereum
PRIVATE_KEY=<deploy key> \
GENESIS_BK_SET_COMMITMENT=<shellnet anchor> \
GENESIS_PREV_MAX_LEVEL_LAYER_HASH=<shellnet anchor> \
WIRE_WITHDRAW_BY_PROOF=true WITHDRAW_ACC_FR=<real accFr, PI[7]> \
START_PAUSED=true \
forge script script/DeployShellnetE2EBridge.s.sol \
  --rpc-url <alchemy sepolia> --broadcast
```

After deploy: update `ubuntu@ursus-tools.dev:config/bridge-relayer.env`
`BRIDGE_ADDRESS`, unpause after forgery + E2E sign-off.
