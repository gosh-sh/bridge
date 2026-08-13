> **ARCHIVED** — historical handoff; see [docs/README.md](../../docs/README.md) for current docs.

# `bridge-relayer-daemon` — live AN-side integration plan (for Sergey)

**Date:** 2026-07-09
**Author:** Alina
**Scope:** Follow-on to the live-driver refactor plan (`an_bridge_prover_live_driver_refactor_plan_2026-07-08.md`) and the M7 status doc (`docs/m7_eth_side_prover_status_2026-07-07.md`). Concrete file-level guidance for wiring `crates/bridge-relayer-daemon` to consume `crates/an-bridge-prover/bridge-prover-lib` and start driving the deployed `AckiNackiBridge.sol` from real Acki-Nacki live data — same way our `bridge-prover-daemon` already does on shellnet.

**Non-goals for this round:**
* Circuit 4 / withdrawals — `aggregator.rs`, `withdraw_prover.rs`, `withdrawal.rs`, `Circuit4ShplonkPipeline` stay exactly as-is.
* Retiring `acki-nacki-interface::BkSetClient`. We recommend migration path (§7), but the delete lands whenever Sergey wants — not required for the live-block lane to work.
* Changing anything about `bridge.rs` / `daemon.rs` / `guarded_relayer.rs` / `state.rs` / `types.rs` / `error.rs` / `proof_validation.rs`. Only two files really change on Sergey's side: `live_source.rs` (rewritten) and `relayer.rs` (small two-phase extension). Plus `Cargo.toml` (one dep) and `bin/relayer.rs` (constructor wiring).

**Status of our side today (verified 2026-07-09):**
* `bridge-prover-lib::live_driver` is landed: `mod.rs`, `bk_update.rs`, `bundle.rs`, `thinning.rs`. Public API matches plan §3.2 (see §2.1 below for what's actually shipped).
* `bridge-prover-daemon/src/main.rs` is 601 LOC of pure harness (down from ~1207) and drives the driver via `poll_next_bk_update` → ack → persist → `poll_next_bundle` → ack → persist. Runbook lives in `TECHNICAL_README.md`.
* Full E2E (`Circuit 1A + 2 + 4`) verified on shellnet (memory: `bridge_shellnet_circuit4_e2e_2026_07_08.md`, ~7 min wall time, daemon verdict `verified=true, anchor_matched=true, proof_valid=true` at seq_no 1905664).
* **Blockers we ship before Sergey merges:** three tiny things, see §3.

---

## 1. Sergey's ask (paraphrased)

> "1A/1B/2 live → Poseidon — blocked: `bridge-prover-daemon` is binary-only (no `lib.rs`), live-witness code isn't reusable. Circuit 4 had a clean seam (`bridge-event-witness` + `PrivateWitness`); 1A/1B/2 don't. Need a partner refactor (extract witness-fetch into a lib, or add a Poseidon-emit mode to the daemon). Documented. Voice tomorrow morning."

Answer, short: **we chose option 1 and it's landed.** `bridge-prover-lib::live_driver` is the seam. This plan is the recipe for consuming it, plus the three tiny gaps we still owe him.

---

## 2. Ground truth

### 2.1 `bridge-prover-lib` — what Sergey gets

Public surface (all in `crates/an-bridge-prover/bridge-prover-lib/src/`):

| Module | Sergey uses it for |
|---|---|
| `live_driver::LiveProverDriver` | The main orchestrator: `new`, `poll_next_bundle`, `poll_next_bk_update`, `ack_bundle`, `ack_bk_update`, `snapshot_state`, `snapshot_prover_bk_set`, `snapshot_bootstrap_seed`. All async, `!Sync`. Returns `anyhow::Result`. |
| `live_driver::{LiveProverConfig, SeedPolicy}` | `SeedPolicy::{Explicit(u64), Auto, Resume}`. Config also carries `thinning_factor_p (4)`, `history_window_size (128)`, `max_bk_updates_per_iter (8)`. |
| `live_driver::LiveBundleEvent` | `Bootstrapping { seed_seqno, chain_head_seqno } \| Nothing { next_target_seqno, chain_head_seqno, blocked_by_pending_bk_update } \| Bundle(BundleProofArtifacts)`. |
| `live_driver::LiveBkUpdateEvent` | `Bootstrapping {…} \| Nothing \| BkUpdate(BkUpdateProofArtifacts)`. |
| `live_driver::BundleProofArtifacts` | All bytes (no `Fr`) — `block_seq_no, block_height, block_id_be, layer_block_id_be, fin_type (BundleFinalizationType), bk_set_commitment_be, num_layers, layer_hashes_be: [[u8;32]; 10], prev_max_level_layer_hash_be, attestation_proof: Vec<u8>, layer_hashes_proof: Vec<u8>`. Plus diagnostic fields `primary_proof_gen_ms`, `layer_proof_gen_ms`, `last_seen_block_seq_no`, `state_layer_hashes` (used internally by `ack_bundle`). |
| `live_driver::BkUpdateProofArtifacts` | `block_seq_no, block_id_be, fin_type, old_bk_set_commitment_be, new_bk_set_commitment_be, merkle_sibling_h0_be, merkle_sibling_h23_be, attestation_proof: Vec<u8>, new_pubkeys: HashMap<u16, Vec<u8>>` (post-rotation table — not on-chain, needed for Sergey's local mirror if he keeps one). |
| `gql_client::GqlClient` | Thin GraphQL wrapper. `query_latest_blocks`, `query_bk_set_updates_light/_last/_first`, `query_block_metadata`, `query_proof_block_by_seqno`, `query_attestation_envelopes` (returns the full `[PRIMARY]` or `[PRIMARY, FALLBACK]` row), etc. Missing three "current-BK-set" shortcuts — see §3.1. |
| `bk_set_fetcher` | `fetch_bk_set`, `next_update_after`, `parse_bk_set_changes_pub`, `normalize_bk_set_pubkeys`, `load_bk_set_from_config`. |
| `bridge_state::BridgeState` | Persistent mirror of `AckiNackiBridge.storedGlobalHistoryData`. Fields: `window_size, layer_windows: Vec<HistoryWindow>, stored_bk_set_commitment, stored_last_seen_block_seq_no, stored_last_seen_block_height, initialized, schema_version (=4), stored_last_bk_set_update_seq_no`. Methods: `new`, `append_bundle`, `apply_bk_set_update`, `save`, `load`, `is_empty`, `flatten_layer_hashes`, `num_active_layers`, `prev_max_level_layer_hash_for`. |
| `bootstrap::BootstrapSeed` | `apply(&BridgeState)`, `save`, `load`, `fetch_from_node`. `DEFAULT_SEED_PATH = "./state/bootstrap_seed.json"`. |
| `prover_bk_set::ProverBkSet` | Prover-private pubkey table. `rotate`, `save`, `load`. |
| `keys::KeyManager` | On-demand PK load/unload. **NB: needs the SRS + PKs on disk under `./params/`**. |

Constants that matter for Sergey:
* `THINNING_FACTOR_P = 4` (bundles every `W·P = 128·4 = 512` blocks)
* `HISTORY_PROOF_WINDOW_SIZE = 128`
* `MAX_LAYERS = 10`

Nothing that Sergey needs is behind a Cargo feature; everything above is unconditionally public.

### 2.2 `bridge-relayer-daemon` — what Sergey has today

| File | LOC | What it does |
|---|---:|---|
| `src/live_source.rs` | 409 | **Stub.** Traits `RawBlockProvider`, `BoundProofGenerator`, `LiveBlockSource<P, G>`, `RawBlockWitness`, `BoundProofArtifacts`. Only in-memory + stub impls compiled today. No live implementations wired. |
| `src/source.rs` | 842 | File-driven `BlockSource` + `BkUpdateSource` traits and their `ProverProofsBlockSource` + `BkUpdateProofsSource` disk-scanning impls. Reads `proof_<seqno>.json` and `bkupd_<seqno>.json` written by our current `bridge-prover-daemon`. |
| `src/bridge.rs` | 668 | `BridgeClient` trait, `EthBridgeClient<P, N>` alloy binding. `sol!` binds `verifyBlock`, `applyBkSetUpdate`, `withdrawByProof`, `is_nullifier_used`, all `stored*` readers. Also `MockBridgeClient` for tests. `BridgeOnChainState` currently has `last_seen_block_seq_no`, `bk_set_commitment`, `prev_max_level_layer_hash` — **missing `stored_last_bk_set_update_seq_no`** (getter exists in `sol!`, not wired into `read_state()`). |
| `src/relayer.rs` | 528 | `Relayer<S, B>::tick()`: reads bridge, computes `target = last_seen + 1`, `source.fetch(target)`, shape-validates, submits. **Currently only handles blocks — no BK-update lane inside the loop.** |
| `src/daemon.rs` | 726 | `run_until_shutdown`, `BackoffConfig`, `RelayerMetrics`. Wraps single-source `tick` in a loop. |
| `src/guarded_relayer.rs` | 553 | Composes `Relayer + BkSetSentry`; pauses on rotation, needs manual `resume()`. Does **not** submit `applyBkSetUpdate` — that's off-band today. |
| `src/bk_set_sentry.rs` | 526 | Uses `acki-nacki-interface::BkSetTracker` (REST `/v2/bk_set_update`) to detect rotation and emit `SentryStatus::RotationDetected`. No proof generation. |
| `src/state.rs` | 126 | `RelayerState` = `{ last_processed_seqno, last_attempt_seqno, attempts_since_progress }`. Atomic-write JSON persistence. |
| `src/types.rs` | 120 | `AnBlockData` (see §2.3), `BkSetUpdateData` (see §2.3), `FinalizationType = Primary \| Fallback`, `MAX_LAYER_HASHES = 10`. |
| `src/{aggregator,withdraw_prover,withdrawal}.rs` | ~2000 | Circuit 4 pipeline. **Out of scope.** |
| `src/bin/relayer.rs` | ~400 | CLI: `smoke-fixture`, `sentry-watch`, `daemon`, `verify-fixture`. Today `daemon` still uses `FixturesBlockSource` (Phase 5.1 placeholder). |

Zero halo2 in Sergey's dep graph today. Zero AN GraphQL client. Only AN-facing crate is `acki-nacki-interface` (REST only).

### 2.3 The `From` seam — payload shapes line up cleanly

Sergey's `AnBlockData` (from `src/types.rs`):
```rust
pub struct AnBlockData {
    pub fin_type: FinalizationType,          // Primary | Fallback
    pub block_id: U256,
    pub bk_set_commitment: U256,
    pub block_seq_no: u64,
    pub num_layers: u8,                       // 1..=10
    pub layer_hashes: [U256; MAX_LAYER_HASHES],  // fixed len 10, tail-zero
    pub prev_max_level_layer_hash: U256,
    pub attestation_proof: Bytes,
    pub layer_hashes_proof: Bytes,
}
```

Our `BundleProofArtifacts` (§2.1) is `[u8; 32]`-native. Conversion is boilerplate on Sergey's side:
```rust
impl From<&BundleProofArtifacts> for AnBlockData {
    fn from(b: &BundleProofArtifacts) -> Self {
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        for (i, h) in b.layer_hashes_be.iter().enumerate().take(b.num_layers as usize) {
            layer_hashes[i] = U256::from_be_bytes(*h);
        }
        AnBlockData {
            fin_type: b.fin_type.into(),
            block_id: U256::from_be_bytes(b.block_id_be),
            bk_set_commitment: U256::from_be_bytes(b.bk_set_commitment_be),
            block_seq_no: b.block_seq_no,
            num_layers: b.num_layers,
            layer_hashes,
            prev_max_level_layer_hash: U256::from_be_bytes(b.prev_max_level_layer_hash_be),
            attestation_proof: Bytes::from(b.attestation_proof.clone()),
            layer_hashes_proof: Bytes::from(b.layer_hashes_proof.clone()),
        }
    }
}
```

Sergey's `BkSetUpdateData` (from `src/types.rs`):
```rust
pub struct BkSetUpdateData {
    pub fin_type: FinalizationType,
    pub block_id: U256,
    pub block_seq_no: u64,
    pub old_commitment_l2: U256,
    pub new_commitment_l3: U256,
    pub sibling_h0: [u8; 32],
    pub sibling_h23: [u8; 32],
    pub attestation_proof: Bytes,
}
```

Note: Sergey's `BkSetUpdateData` deliberately drops the `new_pubkeys` map — the ETH contract doesn't consume it. Sergey doesn't need it either unless he starts caching the pubkey table for his sentry (§7). Recommendation: don't map `new_pubkeys` into `BkSetUpdateData`; keep it available on `BkUpdateProofArtifacts` for whoever wants it.

Conversion:
```rust
impl From<&BkUpdateProofArtifacts> for BkSetUpdateData {
    fn from(u: &BkUpdateProofArtifacts) -> Self {
        BkSetUpdateData {
            fin_type: u.fin_type.into(),
            block_id: U256::from_be_bytes(u.block_id_be),
            block_seq_no: u.block_seq_no,
            old_commitment_l2: U256::from_be_bytes(u.old_bk_set_commitment_be),
            new_commitment_l3: U256::from_be_bytes(u.new_bk_set_commitment_be),
            sibling_h0: u.merkle_sibling_h0_be,
            sibling_h23: u.merkle_sibling_h23_be,
            attestation_proof: Bytes::from(u.attestation_proof.clone()),
        }
    }
}
```

`FinalizationType` on both sides is a two-variant enum (Primary / Fallback) — trivial `From`. Ours is called `BundleFinalizationType` in the driver; that's the whole delta.

### 2.4 The real gap — Sergey's `Relayer::tick()` today handles only blocks

Reading `src/relayer.rs` line-by-line: `tick()` calls `bridge.read_state()` → computes `target = last_seen + 1` → `source.fetch(target)` → validates → `bridge.submit_block(block)`. There is **no** BK-update code path in the main loop. `BkUpdateSource` exists (as a trait + `BkUpdateProofsSource` impl in `source.rs`), but nobody calls it.

The rotation lane is entirely off-band today: `BkSetSentry` detects a rotation, `SentryGuardedRelayer` sets `paused=true`, and an operator manually submits `applyBkSetUpdate` before calling `resume()`. That's fine for smoke tests but not for a live shellnet daemon.

**This is the biggest concrete change on Sergey's side beyond just wiring `LiveBlockSource`.** Detailed in §5.

Consequence for prioritisation: **the live-blocks lane (Circuits 1A/1B/2 without rotation) is what Sergey can ship first**, standalone, without touching the BK-update path. The user's note "now BK-set is fixed" (2026-07-09) means we can defer rotation-in-loop until later. Landing the block lane is what unblocks Sergey's E2E on the deployed bridge.

---

## 3. Blockers on our side (bridge-prover-lib) — ship before Sergey merges

Three tiny things. All small, all deferred from the July-08 plan pending Sergey's actual needs. All can go in one PR.

### 3.1 Three `gql_client` shortcut queries (~120 LOC total)

> **SUPERSEDED (2026-07-30).** Phase A landed the shared `bridge_prover_lib::bk_set_bootstrap::load_bk_set` helper, which both `bridge-prover-daemon` and `bridge-relayer-daemon::run_daemon_live` call directly (see `src/bin/relayer.rs:1799`). Sergey no longer needs the three shortcut queries described below, and `acki-nacki-interface::BkSetClient` has already been deleted (Phase D, commit `3d98215`). The `bk_set_fetcher::load_bk_set_from_config` + `bk_set_at_height` primitives that the bootstrap helper composes are already public. Kept below for historical context only.

Add to `crates/an-bridge-prover/bridge-prover-lib/src/gql_client.rs`. Purpose: single AN-facing surface for both daemons — Sergey can retire `acki-nacki-interface::BkSetClient` when he's ready, without waiting for anything from us later.

| Signature | Composes from |
|---|---|
| `pub async fn query_current_bk_set_compact(&self) -> Result<CompactBkSetSnapshot>` | `bk_set_fetcher::fetch_bk_set` + `query_latest_blocks(1)` for seq_no witness. Return `{ seq_no, entries: Vec<{ node_id, node_owner_pk, epoch_start_seq_no }> }`. |
| `pub async fn query_current_bk_set_full(&self) -> Result<FullBkSetSnapshot>` | Same walk, richer entry (pubkey_48, signer_index, epoch_finish_seq_no, address, stake, owner_address, owner_pubkey, wait_step). |
| `pub async fn fetch_bk_set(&self) -> Result<HashMap<u16, Vec<u8>>>` (in `bk_set_fetcher`) | Reconstructs current set from `bkSetUpdates` history (`normalize_bk_set_pubkeys` applied internally). Sergey's sentry uses this shape directly. |

Signer-index width: `u16` (matches `bridge-prover-lib` internal convention). Sergey's current REST returns `u32` for backwards compatibility, but the underlying `signer_index` never exceeds 65535 — the widening happens in `acki-nacki-interface::BkSetClient::fetch_signer_index_bk_set`. Documented in the new function's doc-comment.

### 3.2 Structured `DriverError` variants (~40 LOC)

Today `LiveProverDriver` returns `anyhow::Result<T>`. Sergey's `RelayerError` (in `src/error.rs`) is a tagged enum with variants like `BlockNotYetAvailable`, `BridgeRejected`, `AckiNacki`. To wrap driver errors cleanly he needs to know whether an error is `GqlTransient` (retry) vs `ProofGen` (fatal for this seqno) vs `StateInconsistent` (wipe + rebootstrap).

Concrete shape (add to `live_driver/mod.rs`):
```rust
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("GraphQL transient: {0}")]
    GqlTransient(#[source] anyhow::Error),
    #[error("GraphQL schema/shape error: {0}")]
    GqlSchema(#[source] anyhow::Error),
    #[error("proof generation failed for seq_no {seq_no}: {source}")]
    ProofGen { seq_no: u64, #[source] source: anyhow::Error },
    #[error("in-memory state inconsistent, wipe + rebootstrap: {0}")]
    StateInconsistent(#[source] anyhow::Error),
    #[error("bootstrap in progress: seed={seed_seqno} head={chain_head_seqno}")]
    Bootstrapping { seed_seqno: u64, chain_head_seqno: u64 },
    #[error("other: {0}")]
    Other(#[source] anyhow::Error),
}
```

Sites we thread these through inside `bk_update.rs` + `bundle.rs`: GQL failures (classify by reqwest kind), `KeyManager::load_*` failures (ProofGen), `state.append_bundle`/`state.apply_bk_set_update` failures (StateInconsistent). Everything else keeps flowing as `Other`.

Public API becomes `Result<LiveBundleEvent, DriverError>` and `Result<LiveBkUpdateEvent, DriverError>`. Our own daemon updates its call sites — `?` still works via `From<DriverError> for anyhow::Error`. Zero behaviour change; strictly additive.

### 3.3 `LiveBlockSource::driver_snapshot()` accessor (~15 LOC)

Sergey's consistency check (§5.4) needs read-only access to `driver.snapshot_state()` from outside the trait-impl call sites. Ship this on `LiveBlockSource`:

```rust
impl LiveBlockSource {
    /// Return a cloned snapshot of the driver's post-ack BridgeState. Cheap
    /// (a Clone of ~10 × 128 × 32 bytes ≈ 40 KiB). Sergey's Relayer::tick
    /// calls this immediately after ack_last_* to cross-check against
    /// `EthBridgeClient::read_state()` — see §5.4.
    pub async fn driver_snapshot(&self) -> BridgeState {
        self.driver.lock().await.snapshot_state().clone()
    }
    pub async fn driver_prover_bk_set_snapshot(&self) -> ProverBkSet {
        self.driver.lock().await.snapshot_prover_bk_set().clone()
    }
}
```

`BridgeState` and `ProverBkSet` already derive `Clone`. Zero API change on the driver itself.

### 3.4 Documented "no-halo2 for read-only paths" pattern

`LiveProverDriver::new` takes a `KeyManager`, which forces halo2-axiom into Sergey's transitive dep graph the moment he pulls `bridge-prover-lib`. This is the trade-off the July-08 plan accepted (§Decisions #5, no `Fr` in payloads keeps his *runtime* code halo2-clean; the *compile-time* cost of the transitive dep is fine). We document this explicitly in `TECHNICAL_README.md` and in the `LiveProverDriver::new` doc-comment so nobody re-litigates it.

If Sergey pushes back and wants a hard split (e.g. run the ETH-side relayer as a strict wasm-friendly crate), fallback = spool `BundleProofArtifacts` as JSON to disk via a small `bridge-prover-driver-cli` binary, matching his existing `ProverProofsBlockSource` schema (July-08 plan §5.2). Not shipped now; would take ~200 LOC.

**These four items are the entirety of the prover-lib delta.** Any additional field Sergey needs surfaces later as its own shortcut — same pattern as §3.1.

---

## 4. Sergey's side — what changes, file by file

Assumes §3 is landed and Sergey has picked up the tag.

### 4.1 `Cargo.toml`

```toml
[dependencies]
bridge-prover-lib = { path = "../an-bridge-prover/bridge-prover-lib" }
```

Also add `thiserror` if not already there (for `DriverError` re-mapping). Everything transitively pulled by `bridge-prover-lib` (`halo2-base`, `halo2-ecc`, `pse-poseidon`, etc.) becomes indirect; no new API breakage for downstream consumers of `bridge-relayer-daemon`.

Deletion candidate later: `acki-nacki-interface = { path = "../acki-nacki-interface" }`. Keep for now — sentry migration is a separate landable step (§7).

### 4.2 `src/live_source.rs` — full rewrite (~200 LOC)

Delete the entire 409-LOC Phase-5.2 stub. Replace with:

```rust
use bridge_prover_lib::live_driver::{
    LiveProverDriver, LiveProverConfig, SeedPolicy, DriverError,
    LiveBundleEvent, LiveBkUpdateEvent,
    BundleProofArtifacts, BkUpdateProofArtifacts,
};
use tokio::sync::Mutex;
use std::sync::Arc;
use crate::source::{BlockSource, BkUpdateSource};
use crate::types::{AnBlockData, BkSetUpdateData};
use crate::error::RelayerError;

pub struct LiveBlockSource {
    driver: Arc<Mutex<LiveProverDriver>>,
}

impl LiveBlockSource {
    pub fn new(driver: Arc<Mutex<LiveProverDriver>>) -> Self { Self { driver } }
}

fn map_err(e: DriverError) -> RelayerError {
    use DriverError::*;
    match e {
        GqlTransient(inner) | Bootstrapping { .. } => RelayerError::AckiNacki(inner.to_string()),
        GqlSchema(inner) => RelayerError::AckiNacki(format!("gql schema: {inner}")),
        ProofGen { seq_no, source } =>
            RelayerError::Other(format!("proof-gen failed for {seq_no}: {source}")),
        StateInconsistent(inner) =>
            RelayerError::Other(format!("driver state inconsistent: {inner}")),
        Other(inner) => RelayerError::Other(inner.to_string()),
    }
}

#[async_trait::async_trait]
impl BlockSource for LiveBlockSource {
    async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
        let mut d = self.driver.lock().await;
        match d.poll_next_bundle().await.map_err(map_err)? {
            LiveBundleEvent::Bootstrapping { .. } | LiveBundleEvent::Nothing { .. } =>
                Ok(None),
            LiveBundleEvent::Bundle(b) => {
                // Fall-forward: driver's cursor is authoritative; if it produced
                // a bundle strictly before target (should not happen with a
                // correctly-initialised state), we ack and drop to keep it
                // moving. Log for observability.
                if b.block_seq_no < target {
                    tracing::warn!(
                        driver_seq_no = b.block_seq_no,
                        target,
                        "driver produced pre-target bundle — acking + dropping",
                    );
                    d.ack_bundle(&b).map_err(map_err)?;
                    return Ok(None);
                }
                let data = AnBlockData::from(&b);
                // NB: we do NOT ack here. The caller (Relayer::tick) will call
                // `LiveBlockSource::acknowledge_last_bundle(seq_no)` after the
                // ETH submit succeeds. See §5 for the ack-after-submit protocol.
                Ok(Some(data))
            }
        }
    }
}

#[async_trait::async_trait]
impl BkUpdateSource for LiveBlockSource {
    async fn fetch_bk_update(&self, target: u64) -> Result<Option<BkSetUpdateData>, RelayerError> {
        let mut d = self.driver.lock().await;
        match d.poll_next_bk_update().await.map_err(map_err)? {
            LiveBkUpdateEvent::Bootstrapping { .. } | LiveBkUpdateEvent::Nothing =>
                Ok(None),
            LiveBkUpdateEvent::BkUpdate(u) => {
                if u.block_seq_no < target {
                    tracing::warn!(driver_seq_no = u.block_seq_no, target,
                        "driver produced pre-target bk-update — acking + dropping");
                    d.ack_bk_update(&u).map_err(map_err)?;
                    return Ok(None);
                }
                Ok(Some(BkSetUpdateData::from(&u)))
            }
        }
    }
}
```

The `Arc<Mutex<LiveProverDriver>>` is required because `Relayer<S, B>` owns the source (`.source`) and calls both `BlockSource::fetch` and `BkUpdateSource::fetch_bk_update` from the same `tick()`. Both trait impls must share the same driver instance so the "bk-updates block bundles" invariant is honoured. The mutex is uncontended in the daemon (single-threaded `tick`).

**Ack-after-submit** is the non-trivial bit. `LiveBlockSource` must expose a way for `Relayer::tick` to tell the driver "the last bundle you gave me made it on-chain, advance the cursor":

```rust
impl LiveBlockSource {
    pub async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
        let mut d = self.driver.lock().await;
        // We need to look up the artifacts by seq_no. Cheapest option: keep a
        // one-slot cache of the last unacked bundle inside LiveBlockSource. The
        // driver's own cursor is our authoritative "last given"; the cache is
        // just the payload we need to hand back to ack_bundle().
        // See §5.2 for the exact struct shape.
        todo!("see §5.2 pending-bundle cache")
    }
    pub async fn ack_last_bk_update(&self, seq_no: u64) -> Result<(), RelayerError> { … }
}
```

Details in §5.

### 4.3 `src/bridge.rs` — one field addition (~5 LOC)

Add `stored_last_bk_set_update_seq_no: u64` to `BridgeOnChainState`. Wire in `EthBridgeClient::read_state`:

```rust
BridgeOnChainState {
    last_seen_block_seq_no: contract.storedLastSeenBlockSeqNo().call().await?._0.to::<u64>(),
    bk_set_commitment: contract.storedBkSetCommitment().call().await?._0,
    prev_max_level_layer_hash: contract.storedPrevMaxLevelLayerHash().call().await?._0,
    stored_last_bk_set_update_seq_no: contract.storedLastBkSetUpdateSeqNo().call().await?._0.to::<u64>(),
}
```

Same in `MockBridgeClient` (add the field to its state + mutate on `submit_bk_set_update`). Sergey's `alloy` `sol!` binding already declares the getter — verified.

### 4.4 `src/relayer.rs` — two-phase tick (~80 LOC delta)

`Relayer` today is generic over one `BlockSource`. It becomes generic over both:

```rust
pub struct Relayer<S, U, B>
where S: BlockSource, U: BkUpdateSource, B: BridgeClient
{
    config: RelayerConfig,
    state: RelayerState,
    source: S,
    bk_update_source: U,
    bridge: B,
}
```

Since `LiveBlockSource` implements both `BlockSource` and `BkUpdateSource`, callers can pass the same object twice (an `Arc<LiveBlockSource>` cloned into both slots). For file-driven tests, the pair is `(ProverProofsBlockSource, BkUpdateProofsSource)` — Sergey already has both.

`tick()` becomes:

```rust
pub async fn tick(&mut self) -> Result<TickOutcome, RelayerError> {
    let on_chain = self.bridge.read_state().await?;

    // Phase 1 — drain any pending BK-set rotation *before* trying to advance blocks.
    // Mirrors the invariant enforced inside LiveProverDriver via
    // LiveBundleEvent::Nothing { blocked_by_pending_bk_update: true }.
    let bk_target = on_chain.stored_last_bk_set_update_seq_no + 1;
    if let Some(upd) = self.bk_update_source.fetch_bk_update(bk_target).await? {
        // Cheap on-chain sanity checks against `on_chain` — mirrors existing
        // block-side checks in the current tick().
        // ... (bk_set_commitment must match, monotonic seq_no, …)
        let outcome = self.bridge.submit_bk_set_update(&upd).await?;
        match outcome {
            SubmitBkUpdateOutcome::Applied { new_state, tx_hash } => {
                self.source.ack_last_bk_update(upd.block_seq_no).await?;
                self.state.record_bk_update_progress(upd.block_seq_no);
                self.persist_state()?;
                return Ok(TickOutcome::BkUpdateApplied { seq_no: upd.block_seq_no, new_state, tx_hash });
            }
            SubmitBkUpdateOutcome::Reverted { reason } => {
                self.state.record_bk_update_attempt(upd.block_seq_no);
                self.persist_state()?;
                return Ok(TickOutcome::BkUpdateReverted { seq_no: upd.block_seq_no, reason });
            }
        }
    }

    // Phase 2 — original block-lane logic, unchanged apart from the ack call.
    let target = on_chain.last_seen_block_seq_no + 1;
    let Some(block) = self.source.fetch(target).await? else {
        // ...record_attempt + persist + return NotYetAvailable
    };
    // ... (existing shape validation + on-chain-anchor sanity checks)
    match self.bridge.submit_block(&block).await? {
        SubmitOutcome::Verified { new_state, tx_hash } => {
            self.source.ack_last_bundle(block.block_seq_no).await?;
            self.state.record_progress(block.block_seq_no);
            self.persist_state()?;
            Ok(TickOutcome::Verified { seq_no: block.block_seq_no, new_state, tx_hash })
        }
        SubmitOutcome::Reverted { reason } => {
            self.state.record_attempt(block.block_seq_no);
            self.persist_state()?;
            Ok(TickOutcome::BridgeReverted { seq_no: target, reason })
        }
    }
}
```

New enum variants on `TickOutcome`:
* `BkUpdateApplied { seq_no, new_state, tx_hash }`
* `BkUpdateReverted { seq_no, reason }`

New types alongside `SubmitOutcome`:
* `SubmitBkUpdateOutcome::{Applied { new_state, tx_hash }, Reverted { reason }}` — mirrors `SubmitOutcome` for the rotation path.

`BridgeClient` trait gains one method:
```rust
async fn submit_bk_set_update(&self, upd: &BkSetUpdateData) -> Result<SubmitBkUpdateOutcome, RelayerError>;
```
Already implemented on `EthBridgeClient` via the existing `applyBkSetUpdate` sol! binding; just needs to be surfaced on the trait + mirrored on `MockBridgeClient`.

`RelayerState` gains:
```rust
pub struct RelayerState {
    // existing:
    pub last_processed_seqno: Option<u64>,
    pub last_attempt_seqno: Option<u64>,
    pub attempts_since_progress: u32,
    // new:
    pub last_bk_update_processed_seqno: Option<u64>,
    pub last_bk_update_attempt_seqno: Option<u64>,
    pub bk_update_attempts_since_progress: u32,
}
```
Bump `schema_version` in the JSON payload; old files load with the new fields defaulting to `None`/0 — safe.

### 4.5 `src/daemon.rs` — small `TickOutcome` fanout in `LastOutcome`

`LastOutcome` gains two tags: `BkUpdateApplied { seq_no }`, `BkUpdateReverted { seq_no }`. `run_until_shutdown` treats `BkUpdateApplied` like `Verified` (reset backoff) and `BkUpdateReverted` like `Reverted` (apply backoff). No other changes.

### 4.6 `src/guarded_relayer.rs` — decide fate

Two options:

* **Option A (recommended, minimal churn).** Leave `SentryGuardedRelayer` in place as an optional additional guard. Once the two-phase `tick` handles BK-updates natively, the sentry's role narrows to "pause faster than GQL cursor catches up" (early-warning latency optimisation, ~1-2 block savings). Fine as-is.
* **Option B.** Delete `SentryGuardedRelayer` entirely once §7 migration lands and `bk_update_source` is authoritative. Cleaner but more invasive. Defer.

Recommendation: A now, B later.

### 4.7 `src/bin/relayer.rs` — constructor wiring (~60 LOC)

New `daemon-live` subcommand (or extend `daemon` behind a flag `--live`). Configuration reads new env vars:

* `BRIDGE_GQL_ENDPOINT` — same as our daemon (`http://.../graphql`).
* `BRIDGE_PARAMS_DIR` — path to shared SRS + PKs (`./params/`). Sergey's daemon needs `KeyManager::load_or_init` too.
* `BRIDGE_STATE_DIR` — `./state/`; holds `prover_state.json`, `prover_bk_set.json`, `bootstrap_seed.json`, and the existing `relayer_state.json`.
* `BRIDGE_BK_SET_CONFIG` — fallback JSON if GQL bk-set fetch fails at startup (matches our daemon).
* `BRIDGE_BOOTSTRAP_SEQNO` — optional. Unset ⇒ `SeedPolicy::Auto` on first run, `SeedPolicy::Resume` if state exists.

Wiring:
```rust
let gql = bridge_prover_lib::gql_client::create_client(&cfg.gql_endpoint);
let key_manager = KeyManager::load_or_init(&cfg.params_dir)?;
let (bk_set, _commitment) = fetch_bk_set_with_file_fallback(&gql, &cfg.bk_set_json).await?;
let state = BridgeState::load(cfg.state_dir.join("prover_state.json"), HISTORY_PROOF_WINDOW_SIZE).unwrap_or_default();
let prover_bk_set = ProverBkSet::load_or_seed(cfg.state_dir.join("prover_bk_set.json"), &bk_set)?;

let seed_policy = match cfg.bootstrap_seqno {
    Some(n) => SeedPolicy::Explicit(n),
    None if state.is_empty() => SeedPolicy::Auto,
    None => SeedPolicy::Resume,
};

let driver = Arc::new(Mutex::new(LiveProverDriver::new(
    gql, key_manager, state, prover_bk_set, bk_set,
    LiveProverConfig { seed_policy, ..Default::default() },
)?));

let source = LiveBlockSource::new(Arc::clone(&driver));
let relayer = Relayer::new(cfg.relayer, source.clone(), source, eth_bridge);
relayer.run_until_shutdown(shutdown, BackoffConfig::default(), metrics).await?;
```

Persistence at daemon-exit and after each ack is done inside `LiveBlockSource::ack_*` via `driver.snapshot_state().save(...)` + `driver.snapshot_prover_bk_set().save(...)`. Detail in §5.2.

---

## 5. The two ack protocols (this is the subtle bit)

### 5.1 Why we can't ack inside `BlockSource::fetch`

Our own `bridge-prover-daemon` does `fetch → IPC-verify → ack`. The ack is what advances the driver's cursor. If ack happened inside `fetch`, the driver would move past a bundle that the ETH bridge later rejected (rare but possible: chain reorg on a bad RPC, revert due to timing).

Sergey's original `ProverProofsBlockSource` gets away with implicit acks because its "cursor" is just "scan the directory for the next-highest seqno file". Idempotency is free.

For the live driver, we must ack *after* `submit_block` returns `Verified`. That means `LiveBlockSource` needs a "pending" slot the driver has produced but the caller hasn't confirmed yet.

### 5.2 `LiveBlockSource` internal state

```rust
pub struct LiveBlockSource {
    driver: Arc<Mutex<LiveProverDriver>>,
    pending_bundle: Arc<Mutex<Option<BundleProofArtifacts>>>,
    pending_bk_update: Arc<Mutex<Option<BkUpdateProofArtifacts>>>,
    state_paths: StatePaths,      // where to snapshot after ack
}
```

Ack semantics:
```rust
pub async fn ack_last_bundle(&self, seq_no: u64) -> Result<(), RelayerError> {
    let pending = self.pending_bundle.lock().await.take();
    let Some(b) = pending else {
        return Err(RelayerError::Other("no pending bundle to ack".into()));
    };
    if b.block_seq_no != seq_no {
        return Err(RelayerError::Other(format!(
            "pending bundle seq_no {} != ack seq_no {}", b.block_seq_no, seq_no
        )));
    }
    let mut d = self.driver.lock().await;
    d.ack_bundle(&b).map_err(map_err)?;
    d.snapshot_state().save(&self.state_paths.state_json).map_err(other)?;
    d.snapshot_prover_bk_set().save(&self.state_paths.prover_bk_set_json).map_err(other)?;
    Ok(())
}
```

Same shape for `ack_last_bk_update`.

`fetch()` becomes:
```rust
async fn fetch(&self, target: u64) -> Result<Option<AnBlockData>, RelayerError> {
    // If we already have a pending bundle from a previous fetch, that means
    // the caller (Relayer::tick) crashed between fetch and ack. Re-hand it
    // back — this is safe because the ETH bridge's storedLastSeenBlockSeqNo
    // is the authoritative cursor, and Relayer::tick recomputes target from
    // it every time.
    if let Some(p) = self.pending_bundle.lock().await.as_ref() {
        if p.block_seq_no >= target {
            return Ok(Some(AnBlockData::from(p)));
        }
        // Stale pending — drop, driver will re-poll below.
        // (This branch only happens if the operator manually rewound state,
        // which we log as a warning.)
        tracing::warn!(
            pending_seq_no = p.block_seq_no,
            target,
            "stale pending bundle; dropping without ack — driver will re-poll",
        );
    }
    // ... (original poll_next_bundle path from §4.2) ...
    // On Bundle(b): stash in pending_bundle, return AnBlockData::from(&b).
}
```

Crash recovery works because both cursors are external:
* On-chain `storedLastSeenBlockSeqNo` — authoritative for "what has been verified".
* `driver.snapshot_state().stored_last_seen_block_seq_no` — persisted after every ack.
* `RelayerState` — Sergey's local retry log.

If daemon crashes between `fetch` (pending stashed in memory, not persisted) and successful `submit_block` (which triggers `ack_bundle`), the pending is lost — but that's OK: on restart, `on_chain.last_seen_block_seq_no` is unchanged, `target` is the same, and the driver re-produces the same bundle. Zero on-chain state corruption; at worst we pay to re-prove one bundle (~75 s per memory `gosh-dark-dex-halo2-new-circuit` benchmark, ~30 s on the shellnet-sized circuit).

### 5.3 Ordering vs. `BkUpdateSource`

Because §4.4 gives us "phase 1: drain updates; phase 2: advance blocks" in `Relayer::tick`, and the driver internally emits `LiveBundleEvent::Nothing { blocked_by_pending_bk_update: true }` while a rotation is undrained, the two lanes converge naturally. On a rotation:

1. Sergey's `tick` phase 1 calls `fetch_bk_update(bk_target)` → driver returns `BkUpdate(u)` → we stash it as `pending_bk_update` → return `Some(BkSetUpdateData)`.
2. `bridge.submit_bk_set_update(&u)` hits chain, gets `Applied`.
3. `source.ack_last_bk_update(u.block_seq_no)` → driver rotates its `ProverBkSet` + `BridgeState.stored_bk_set_commitment` + saves both to disk.
4. On next tick, phase 1 returns `None` (no more pending updates); phase 2 proceeds normally with the new BK-set.

Rejects (phase-1 revert) leave the driver's `pending_bk_update` slot intact — the pending is NOT dropped on `SubmitBkUpdateOutcome::Reverted`. Next tick sees the same pending, re-tries `submit_bk_set_update`. Sergey's existing backoff kicks in via the `Reverted` branch of `run_until_shutdown`.

---

### 5.4 Contract-side global history data — Sergey collects it, holds it, checks sync

**This is a first-class relayer responsibility, not something the driver does for him.** The prover-lib `BridgeState` is a *witness-building* structure; it does not know or care what's actually on-chain. The relayer must independently poll the contract's stored anchors, persist them in relayer state, and cross-check on every ack that the driver's post-ack view matches what the chain says it stored. Any drift = halt + operator alert.

#### 5.4.1 Contract-side data Sergey collects

The AckiNackiBridge stores derived anchors only (the 10×128 hash window lives off-chain, replaced by proofs). Everything Sergey can and must see:

| Field | Contract getter (sol! binding) | Semantics |
|---|---|---|
| `stored_last_seen_block_seq_no: u64` | `storedLastSeenBlockSeqNo()` | Highest key-block seq_no verified. |
| `stored_bk_set_commitment: [u8; 32]` | `storedBkSetCommitment()` | Poseidon commitment of the current BK set. |
| `stored_prev_max_level_layer_hash: [u8; 32]` | `storedPrevMaxLevelLayerHash()` | Highest-active-layer latest hash from the last verified bundle. Feeds the next bundle's anchor. |
| `stored_last_bk_set_update_seq_no: u64` | `storedLastBkSetUpdateSeqNo()` | Highest bk-update seq_no applied via `applyBkSetUpdate`. |

Extend `BridgeOnChainState` to include all four (§4.3 covered the fourth):

```rust
// src/bridge.rs
pub struct BridgeOnChainState {
    pub last_seen_block_seq_no: u64,
    pub bk_set_commitment: U256,
    pub prev_max_level_layer_hash: U256,
    pub last_bk_set_update_seq_no: u64,   // NEW (§4.3)
}
```

That struct **IS** Sergey's mirror of the contract's global history data. No new type needed. He already reads it every tick via `bridge.read_state()`.

#### 5.4.2 Persistence — hold it across restarts

Extend `RelayerState` to remember the last observed contract state:

```rust
// src/state.rs
pub struct RelayerState {
    // existing:
    pub last_processed_seqno: Option<u64>,
    pub last_attempt_seqno: Option<u64>,
    pub attempts_since_progress: u32,
    pub last_bk_update_processed_seqno: Option<u64>,   // §4.4
    pub last_bk_update_attempt_seqno: Option<u64>,     // §4.4
    pub bk_update_attempts_since_progress: u32,        // §4.4
    // new (§5.4):
    /// Last observed on-chain global history data. Persisted so a restart
    /// can compare "what the chain said last time we successfully synced"
    /// against "what the chain says now" and detect drift caused by another
    /// relayer, manual bridge interaction, or reorg.
    pub last_observed_on_chain: Option<BridgeOnChainState>,
}
```

Schema-bump the JSON version; older files load with `last_observed_on_chain = None`.

#### 5.4.3 Sync check — run it after every ack

Cross-check has two independent flavours; both cheap and additive.

**Check A — driver-vs-contract equality after each ack.** In `Relayer::tick()`, right after `source.ack_last_bundle(seq_no).await?`:

```rust
let expected = self.source.driver_snapshot().await;          // §3.3 accessor
let actual   = self.bridge.read_state().await?;
match check_history_consistency(&expected, &actual) {
    Ok(()) => {
        self.state.last_observed_on_chain = Some(actual);
        self.persist_state()?;
    }
    Err(drift) => {
        tracing::error!(?drift, "contract/driver global-history-data drift — halting");
        return Err(RelayerError::Other(format!("drift: {drift}")));
    }
}
```

Same pattern after `ack_last_bk_update`.

**Check B — chain monotonicity across ticks.** Every `tick()`'s first `bridge.read_state()` compared against `state.last_observed_on_chain` before doing anything else. Detects:

* `actual.last_seen_block_seq_no < remembered` → chain rewound (reorg / node behind); back off, don't submit anything.
* `actual.last_seen_block_seq_no > remembered + expected_gap` → another actor advanced the bridge between our ticks (another relayer instance? manual admin? panic-hardcoded state?); halt and require operator ack.
* `actual.stored_bk_set_commitment != remembered.stored_bk_set_commitment` while `actual.last_bk_set_update_seq_no == remembered.last_bk_set_update_seq_no` → catastrophic (commitment changed without an update event); halt.

Both checks live in a small `src/history_consistency.rs` (~120 LOC). Zero dependency on `bridge-prover-lib` beyond the `BridgeState` type — pure data comparison.

#### 5.4.4 What the check function looks like

```rust
// src/history_consistency.rs
pub struct HistoryDrift {
    pub field: &'static str,
    pub expected_hex: String,
    pub actual_hex: String,
}

pub fn check_history_consistency(
    expected: &BridgeState,          // from driver.snapshot_state() post-ack
    actual:   &BridgeOnChainState,   // from bridge.read_state() post-tx
) -> Result<(), HistoryDrift> {
    let expected_prev_max = expected
        .highest_layer_latest_hash()
        .unwrap_or([0u8; 32]);

    if expected.stored_last_seen_block_seq_no != actual.last_seen_block_seq_no {
        return Err(drift("last_seen_block_seq_no",
            expected.stored_last_seen_block_seq_no.to_string(),
            actual.last_seen_block_seq_no.to_string()));
    }
    if expected.stored_bk_set_commitment != actual.bk_set_commitment.to_be_bytes() {
        return Err(drift("bk_set_commitment",
            hex::encode(expected.stored_bk_set_commitment),
            hex::encode(actual.bk_set_commitment.to_be_bytes::<32>())));
    }
    if expected_prev_max != actual.prev_max_level_layer_hash.to_be_bytes() {
        return Err(drift("prev_max_level_layer_hash",
            hex::encode(expected_prev_max),
            hex::encode(actual.prev_max_level_layer_hash.to_be_bytes::<32>())));
    }
    if expected.stored_last_bk_set_update_seq_no != actual.last_bk_set_update_seq_no {
        return Err(drift("last_bk_set_update_seq_no",
            expected.stored_last_bk_set_update_seq_no.to_string(),
            actual.last_bk_set_update_seq_no.to_string()));
    }
    Ok(())
}
```

Total: ~50 LOC of arithmetic + `hex::encode`. No cryptography. No halo2. No new deps.

#### 5.4.5 Startup drift audit

At daemon start, before entering `run_until_shutdown`:

1. Load `RelayerState` from disk. If `last_observed_on_chain = Some(remembered)`, read chain now:
2. If `bridge.read_state() != remembered`: **do not auto-recover**. Print a diff, exit non-zero. Operator decides whether to nuke `state/` and rebootstrap, or intervene.
3. Cross-check `driver.snapshot_state()` (freshly loaded from disk) against `bridge.read_state()`. Same halt-on-mismatch rule.

This is what catches the "someone else ran a relayer on the same key" scenario early — before the daemon tries to submit a stale bundle and gets a `Reverted` from the contract for the wrong reason.

#### 5.4.6 What Sergey does NOT need to duplicate

* The 10×128 `HistoryWindow` buffer. Not on-chain. Not mirrorable. Lives inside the driver, feeds witness synthesis, never leaks to the ETH side.
* `BridgeState::append_bundle` / `apply_bk_set_update` transition logic. Already runs inside the driver on ack. Sergey's job is *cross-checking the outcome against chain*, not re-implementing the transition.
* `ProverBkSet` (the 48-byte pubkey table). Off-chain, private to the prover. Not consumed by the contract; not consumed by the relayer either. Persist it (§6) so restarts work, but no consistency check because there's nothing on-chain to check against.

#### 5.4.7 Metrics + observability

Extend `RelayerMetrics`:
```rust
pub history_drift_detected_total: AtomicU64,           // Check A failures
pub chain_rewind_observed_total: AtomicU64,            // Check B: seq_no went backwards
pub last_observed_on_chain_seq_no: AtomicU64,          // gauge
```

Log every `last_observed_on_chain` update at `info` level with all four fields. This gives ops a linear ledger of "what the chain said at every observation" for offline audit.

---

## 6. Startup, persistence, filesystem layout

Merged with Sergey's existing conventions:

```
bridge-relayer-daemon runtime directory
├── params/                        # NEW: SRS + PKs (KeyManager consumes)
│   ├── kzg_bn254_20.srs
│   ├── circuit_primary_k20.pk
│   ├── circuit_fallback_k21.pk
│   ├── circuit_layer_k17.pk
│   └── circuit_event_k19.pk       # unused if C4 stays subprocess
├── state/                         # merged
│   ├── prover_state.json          # NEW: BridgeState
│   ├── prover_bk_set.json         # NEW: ProverBkSet
│   ├── bootstrap_seed.json        # NEW: BootstrapSeed
│   └── relayer_state.json         # EXISTING: RelayerState (schema bumped)
├── bk_set.json                    # NEW: fallback if GQL seed fails
├── proofs/                        # EXISTING: still used by
│                                  #   file-driven ProverProofsBlockSource
│                                  #   (dev/CI harness; live daemon skips)
└── logs/                          # EXISTING
```

Startup sequence (mirrors `bridge-prover-daemon/main.rs`):

1. `KeyManager::load_or_init(&cfg.params_dir)` — verifies SRS + PK files, loads PK metadata but keeps PKs unloaded (on-demand load per proof).
2. `create_client(&cfg.gql_endpoint)` — reqwest client + endpoint URL.
3. `fetch_bk_set_with_file_fallback(&gql, &cfg.bk_set_json)` — GQL first, JSON if GQL fails.
4. `BridgeState::load(state_dir.join("prover_state.json"), HISTORY_PROOF_WINDOW_SIZE)` — returns fresh if file missing.
5. `ProverBkSet::load_or_seed(state_dir.join("prover_bk_set.json"), &bk_set)` — fresh or on-disk.
6. Decide `SeedPolicy` (§4.7 match).
7. Construct `LiveProverDriver` → wrap in `Arc<Mutex<..>>` → construct `LiveBlockSource`.
8. Read on-chain state via `EthBridgeClient::read_state()` — informs Sergey's `Relayer::tick` cursor.
9. If `state.stored_last_seen_block_seq_no != on_chain.last_seen_block_seq_no` and neither is zero, log a warning: drift possible (another relayer / manual bridge interaction). Recommend: nuke `state/` and rebootstrap. Do NOT auto-nuke.
10. Enter `run_until_shutdown` loop.

**Divergence audit** (step 9) is safety-critical. Details: our `bridge-prover-daemon` today advances state solely on IPC-verifier ACKs, so its `stored_last_seen_block_seq_no` mirrors the verifier's cursor. Sergey's daemon advances on ETH-bridge `Verified`, which mirrors *chain* cursor. If the same driver state is reused across both binaries (bad idea) they'll disagree. Documented recommendation: **one `state/` directory per daemon instance**; do not share.

---

## 7. BK-set sentry — migration path (optional, incremental)

> **MOOT (2026-07-30).** The sentry stack was deleted outright in Phase C (commit `82ecf9a`): `bridge-relayer-daemon/src/bk_set_sentry.rs`, `guarded_relayer.rs`, and the `SentryWatch` subcommand are gone. `acki-nacki-interface::BkSetClient`/`BkSetTracker` followed in Phase D (commit `3d98215`). BK rotation is now handled by the driver's own bk-update lane (§4.4 two-phase tick), so nothing needs migrating. Section retained for historical context.

Once §3.1 GQL shortcuts land, Sergey can retire `acki-nacki-interface::BkSetClient` at his convenience. Recommended sequence (each landable independently):

### 7.a Add GQL-backed `BkSetPoller` impl in `bridge-relayer-daemon`

```rust
pub struct GqlBkSetPoller {
    gql: GqlClient,
    cached_snapshot: Option<BkSetSnapshot>,
}

#[async_trait::async_trait]
impl BkSetPoller for GqlBkSetPoller {
    async fn poll(&mut self) -> Result<BkSetChange, AckiNackiError> {
        let current = bridge_prover_lib::bk_set_fetcher::fetch_bk_set(&self.gql)
            .await
            .map_err(|e| AckiNackiError::Transient(e.to_string()))?;
        let new_snapshot = BkSetSnapshot::from_signer_index_map(&current, /*seq_no*/ 0);
        Ok(diff_against(&self.cached_snapshot, &new_snapshot))
    }
    fn latest(&self) -> Option<&BkSetSnapshot> { self.cached_snapshot.as_ref() }
    fn reset(&mut self) { self.cached_snapshot = None; }
}
```

`BkSetSentry<GqlBkSetPoller>` composes identically to today. No changes to `SentryStatus`, `MembershipDelta`, `SentryMetrics`, `SentryGuardedRelayer`.

### 7.b Cut over `sentry-watch` subcommand + `SentryGuardedRelayer`

Constructor picks `GqlBkSetPoller` instead of `BkSetTracker` under a flag; smoke-test on shellnet; flip default.

### 7.c Delete `acki-nacki-interface` dep

`sentry` shim can stay for CLI diagnostics if useful. Otherwise `cargo rm acki-nacki-interface`.

**When to do this:** low urgency now. The two-phase `tick` in §4.4 makes `SentryGuardedRelayer` an early-warning belt-and-braces, not a critical path. Ship in a follow-up PR after the block lane is live.

---

## 8. Open questions / decisions Sergey needs to weigh in on

**Q1. Params dir layout.** Our daemon expects `./params/{kzg_bn254_K.srs, circuit_*_kN.pk}`. Sergey's harness has no equivalent today (Circuit 4 params are subprocess-owned). Options:
* Symlink `./params/` from his runtime dir to our released params bundle (~2 GB) — cheapest, works for shellnet ops.
* Fetch on first-run from an S3/HTTP mirror — nice for onboarding, more infra.
* Bundle a params-download helper subcommand in `bin/relayer.rs`.

**Recommendation:** symlink for now, download helper later. Not blocking §4.

**Q2. Persistence cadence.** `ack_last_*` triggers a `save()` today. Both `BridgeState` and `ProverBkSet` do atomic write via `.tmp + rename`. This adds ~1-5 ms per ack (fs-dependent). Acceptable. If Sergey's fs is slow (network mount) we can add a `save_every_n_acks` config knob later.

**Q3. Feature-gate the live path?** Sergey's binary currently ships one CLI with multiple subcommands (`smoke-fixture`, `sentry-watch`, `daemon`). Options:
* One binary, `daemon --live` picks `LiveBlockSource`, `daemon --file` picks `ProverProofsBlockSource`. Old mode stays for CI.
* Two binaries (`relayer-live`, `relayer-file`). Cleaner, more infra.

**Recommendation:** subcommand flag now, split later if needed.

**Q4. `AckiNackiError` vs `RelayerError` mapping fidelity.** Our `DriverError::GqlTransient` should map to `RelayerError::AckiNacki(String)` (which today comes from `acki-nacki-interface`). That variant becomes the transport-independent "AN side had a transient issue" bucket. Semantically fine; the *name* becomes a mild misnomer. Optional: rename `RelayerError::AckiNacki` → `RelayerError::AnSideTransient` in a follow-up cleanup. Not blocking.

**Q5. Do we want `LiveProverDriver` to consume its own `KeyManager`?** Right now `LiveProverDriver::new` takes `KeyManager` by value. If Sergey wants to share a single `KeyManager` across (hypothetical, not-in-scope) Circuit-4-in-lib future work, we'd want `Arc<Mutex<KeyManager>>`. **Recommendation:** not now. Ship as-is; refactor later if a genuine sharing use-case surfaces.

**Q6. Bootstrap divergence on live shellnet.** If Sergey starts the live daemon on shellnet with `BRIDGE_BOOTSTRAP_SEQNO=None` and empty `state/`, the driver picks `Auto` and seeds at `next W·P boundary past chain head`. That could be minutes to hours away depending on shellnet activity. Meanwhile Sergey's `Relayer::tick` will see `LiveBundleEvent::Bootstrapping` for a long time. Options:
* Log-throttle bootstrapping outcomes (once per minute rather than every 3 s).
* Add a "wait for bootstrap complete" pre-flight in `bin/relayer.rs` before entering the main loop.

**Recommendation:** log-throttle. Documented in `run_until_shutdown`'s existing throttling pattern.

**Q7. Explicit `BundleFinalizationType → FinalizationType` conversion.** Ours is called `BundleFinalizationType` in the driver, Sergey's is `FinalizationType`. Two-variant `Primary | Fallback` on both sides. Add a `pub From<BundleFinalizationType> for FinalizationType` impl in Sergey's `types.rs`. Trivial.

---

## 9. PR sequence

**PR-A (our repo, small).** Ship §3.
* Add three `query_current_*_bk_set` shortcuts to `gql_client.rs`.
* Introduce `DriverError` enum in `live_driver/mod.rs`; thread through `bk_update.rs` + `bundle.rs`.
* Doc: update `TECHNICAL_README.md` §Runbook with an explicit "consumers of this library" subsection pointing at Sergey's plan (this doc).
* No behavior change on our daemon. Green on shellnet regression.

**PR-B (Sergey's crate).** Blocks-only lane.
* Add `bridge-prover-lib` dep.
* Rewrite `live_source.rs` per §4.2 + §5.2 (block lane only; leave `BkUpdateSource` impl as a `todo!()` stub or return `Ok(None)`).
* Add all four fields to `BridgeOnChainState` (§4.3 + §5.4.1).
* Add `src/history_consistency.rs` (§5.4.4) + wire Check A into `Relayer::tick` (§5.4.3) + Check B before each tick + startup audit (§5.4.5).
* Extend `RelayerState` with `last_observed_on_chain` (§5.4.2); bump schema.
* Extend `bin/relayer.rs` with `daemon-live` subcommand wiring (§4.7), block lane only.
* Full E2E on local devnet + shellnet Sepolia (dry-run first via `EthBridgeClient::dry_run_block`).

**PR-C (Sergey's crate).** BK-update lane.
* Extend `Relayer::tick` per §4.4 (two-phase).
* Wire `LiveBlockSource::fetch_bk_update` fully (§4.2).
* Schema-bump `RelayerState` for the new fields (§4.4).
* Test on local devnet with forced rotation.

**PR-D (Sergey's crate, optional).** Sentry migration.
* Add `GqlBkSetPoller` per §7.a.
* Cut over `sentry-watch` and `SentryGuardedRelayer`.
* Remove `acki-nacki-interface` dep.

Each PR independently landable + revertable. Circuit 4 unchanged throughout.

---

## 10. Handoff to Sergey — TL;DR

> **The seam is landed.** `bridge_prover_lib::live_driver::LiveProverDriver` gives you `poll_next_bundle` + `poll_next_bk_update` + `ack_*` + `snapshot_*`. Payload is `[u8; 32]`-BE only — trivial `From` to your `AnBlockData` / `BkSetUpdateData`.
>
> **We owe you four things:** (1) `bk_set_fetcher::fetch_bk_set` (already public) so you can migrate the sentry off REST when convenient, (2) `DriverError` enum so your `RelayerError` re-mapping is clean, (3) `LiveBlockSource::driver_snapshot()` accessor so you can pull the driver's post-ack `BridgeState` for the §5.4 consistency check, (4) a paragraph in `TECHNICAL_README.md` pointing consumers here. Shipping in PR-A this week.
>
> **Two non-trivial things on your side:** (a) your `Relayer::tick` currently handles only blocks; the BK-update lane exists as `BkUpdateSource` but nobody calls it in the loop. That has to become a two-phase tick (drain updates → advance blocks). ~80 LOC in `relayer.rs`. Detailed in §4.4 + §5.3. (b) contract global-history-data consistency is *your* responsibility — collect all four on-chain anchors every tick via `bridge.read_state()`, persist in `RelayerState`, cross-check against `driver.snapshot_state()` after every ack, halt on drift. New module `src/history_consistency.rs` (~120 LOC). Detailed in §5.4.
>
> **Ship block lane first (PR-B), rotation lane second (PR-C), sentry migration whenever (PR-D).** BK-set is fixed on the live shellnet right now, so PR-B unblocks E2E immediately.
>
> Runbook + shellnet operational conventions live in `crates/an-bridge-prover/TECHNICAL_README.md`. Copy the `./params/`, `./state/`, `./bk_set.json` layout — reuse the same env-var names for consistency across the two daemons.
>
> Voice tomorrow morning — I'll walk through §4.4, §5.2, and Q1–Q7. Everything else is code.
