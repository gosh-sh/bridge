# L2 anchoring — implementation plan

Concrete file/function-level plan expanding
[`l2_anchoring_proposal.md`](l2_anchoring_proposal.md). Assumes W=128, P=8
(see `bridge-prover-lib/src/lib.rs:46`). L1 stride = W·P = 1024.
L2 stride = W² = 16384.

## 1. Overview

Introduce an `AnchorMode { L1, L2 }` (alias: `anchor_level: u8 ∈ {1,2}`)
selector threaded through `LiveProverConfig`, `LiveProverDriver`,
`real_chain_builder`, `compute_bridge_anchors`, and both daemon CLIs.
Default stays L1; L2 is a single opt-in flag. Under L2 the driver skips
L1-only key blocks and only targets W²-aligned boundaries; the witness
builder's existing "new-layer bundle" and "same-layer L(N)" branches
(already in `real_chain_builder.rs:141-168`) fire naturally because the
node's `history_proofs` at those boundaries carries `num_layers ≥ 2`.
Circuit 2 public instances change per bundle but VK does not (circuit is
level-parametric already). **No on-chain redeploy** — only the genesis
anchor values change (and must be W²-aligned).

## 2. File-by-file change list

### 2.1 `bridge-prover-lib`

- `src/lib.rs`
  - Add `pub const BUNDLE_STRIDE_L1: u64 = HISTORY_PROOF_WINDOW_SIZE as u64 * THINNING_FACTOR_P;`
  - Add `pub const BUNDLE_STRIDE_L2: u64 = (HISTORY_PROOF_WINDOW_SIZE as u64).pow(2);` (== W², 16384)
  - Add `pub enum AnchorMode { L1, L2 }` with `stride()`, `alignment()` helpers.

- `src/live_driver/mod.rs`
  - `LiveProverConfig` grows `pub anchor_mode: AnchorMode` (default `L1`).
  - `Default for LiveProverConfig` (~L266) sets `anchor_mode: L1`.
  - `LiveProverDriver::new_inner` (~L610) validates `SeedPolicy::Explicit(n)`
    against `stride = cfg.anchor_mode.stride(W, P)`, not hard-coded `W*P`.
  - `advance_bootstrap` (~L895) uses `stride = cfg.anchor_mode.stride()`
    for Auto seed rounding.
  - `next_target_seqno_upper_bound` (~L867) uses same stride.
  - Pass `anchor_mode` into `find_next_thinned_key_block` call site (~L666).

- `src/live_driver/thinning.rs`
  - Add `pub fn find_next_bundle_boundary(last: u64, head: u64, stride: u64) -> Option<u64>`
    → `((last / stride) + 1) * stride` range-checked against head.
  - `find_next_thinned_key_block` becomes a shim calling the new fn with
    `stride = w * p`. Keep as `#[deprecated]` alias to minimise churn.

- `src/live_driver/bundle.rs`
  - No signature changes. `drive_next_bundle` reads `num_layers` from
    `block.history_proofs.len()`. At a W²-boundary the node reports
    `num_layers ≥ 2` already (see `real_chain_builder.rs:141-168`).

- `src/real_chain_builder.rs`
  - No signature change to `build_real_chain`.
  - First L2 bundle after a fresh L2 seed hits the same-layer L(N) branch
    (~L156) because seed already installs `num_active_layers = 2`.
  - Add `debug_assert!` on the L2-daemon Case D path: `chain_result.num_steps == 1`
    when running L2 mode.
  - Add `warn!` if under L2 mode `build_chain_same_layer` (~L207, L1-only
    path) ever fires — indicates driver mis-picked a target.

- `src/bootstrap.rs`
  - `BootstrapSeed` grows `#[serde(default = "default_anchor_level")] pub anchor_level: u8`
    (defaults to 1 so legacy seed files load unchanged).
  - `apply` unchanged.
  - `fetch_from_node` gains `anchor_level: u8` param; forwards to constructor.

- `src/bin/compute_bridge_anchors.rs`
  - Add `--level {1,2}` flag (default 1). Env: `BRIDGE_ANCHOR_LEVEL`.
  - `BUNDLE_BOUNDARY` becomes level-dependent: L1 → `W*P` (1024, unchanged),
    L2 → `W*W` (16384).
  - Seed alignment check (~L188) uses the level-dependent stride.
  - `--at-head` snapping rounds to nearest stride below head.
  - Emits `GENESIS_LAST_SEEN_BLOCK_SEQNO` at level-aligned boundary.
  - Emits `GENESIS_ANCHOR_LEVEL={1|2}` (new env line for cross-check).
  - Under `--level 2`, picks the L2 root from `seed.layer_hashes`:
    replaces hard-coded `layer == 1` (~L226) with `layer == level`.
  - Hard `assert!(seed.layer_hashes.iter().any(|(_, l)| *l == 2))` under `--level 2`.

### 2.2 `bridge-prover-daemon`

- `src/main.rs`
  - Add `--anchor-level` (env `BRIDGE_ANCHOR_LEVEL`) CLI flag. Default 1.
  - Plumb into `LiveProverConfig.anchor_mode`.
  - Startup drift check: refuse to run if persisted `prover_state.json`
    has a different `anchor_level` (see §4).

### 2.3 `bridge-relayer-daemon`

- `src/bin/relayer.rs`
  - Add `--anchor-level` (env `BRIDGE_ANCHOR_LEVEL`) to `Cmd::DaemonLive`
    (~L355) and `Cmd::WithdrawE2E` (~L442). Default 1. **Independent of
    `--anchor-layer`** (that flag drives event-witness escalation; §7).
  - `run_daemon_live` (~L2011): construct `LiveProverConfig` with
    `anchor_mode: AnchorMode::from_u8(args.anchor_level)?`.
  - Seed validation (~L2118): `bootstrap_seqno % stride(level) == 0`.
  - Startup drift audit (~L2154): log `daemon_anchor_level`; refuse if
    on-chain `storedLastSeenBlockSeqNo % daemon_stride != 0`.

- `src/state.rs`
  - `RelayerState` grows `#[serde(default)] pub anchor_level: u8`.
    Legacy `0` → treated as 1 for backward compatibility.
  - Persist `anchor_level` on every `record_progress`.

- `src/withdraw_e2e/driver.rs`
  - `WithdrawE2EConfig` gains `pub anchor_level: u8` (default 1). Forwards
    to enricher's anchor-probing budget.

### 2.4 `bridge-verifier-daemon`

- Mirrors `bridge_state`. No serialization schema change strictly needed,
  but drift-detection must include level. Cross-check identical to daemon.

### 2.5 `bridge-event-witness` / `bridge-event-prover-lib` / `bridge-event-halo2-prover`

- **No production-code changes.** User already added L2+ anchoring here.
  See §7 smoke-test checklist.

## 3. State migration

Files under `state/`:
- `prover_state.json` — `BridgeState` (already tolerates missing fields per
  `legacy_state_file_deserializes_with_defaults` at `bridge_state.rs:555`)
- `bootstrap_seed.json` — `BootstrapSeed`
- `prover_bk_set.json` — pubkey table (unchanged)

**Extensions:**
- `BootstrapSeed.anchor_level: u8` (default 1). Bump `SEED_SCHEMA_VERSION`
  1 → 2 to force migration prompt on legacy seeds.
- `BridgeState.anchor_level: u8` (default 0 = "unknown / legacy").

**Startup rules:**
- `state.anchor_level == 0` + `cfg == L1` → treat as L1 (backward compat).
- `state.anchor_level == 0` + `cfg == L2` → **refuse**; nuke + rebootstrap.
- `state.anchor_level != cfg.anchor_mode as u8` → **refuse**; no auto-migrate.
  Matches existing "startup drift" discipline (`relayer.rs:2169`).

**Manual migration** (surfaced in the daemon error message):
```
mv state state.pre_L2_$(date +%Y%m%d_%H%M%S)
mv relayer-state.json relayer-state.pre_L2_$(date +%Y%m%d_%H%M%S).json
# re-run with BRIDGE_ANCHOR_LEVEL=2 and a W²-aligned BRIDGE_BOOTSTRAP_SEQNO
```

## 4. `compute_bridge_anchors` — L2 math

CLI: `compute_bridge_anchors --level 2 [--at-head | --seed-seqno N]`

- Boundary: `stride = W² = 16 384` when `--level 2`.
- `--at-head`: `seed_seqno = (head_seqno / stride) * stride`.
- `--seed-seqno N`: require `N % stride == 0` and `N ≤ head_seqno`.
- Genesis anchor selection:
  - `top_layer = level as u8` (2 for L2)
  - Pick `seed.layer_hashes.iter().find(|(_, l)| *l == top_layer)`
- Emit:
  - `GENESIS_BK_SET_COMMITMENT=…` (unchanged)
  - `GENESIS_PREV_MAX_LEVEL_LAYER_HASH=…` (**L2 root** under `--level 2`)
  - `GENESIS_LAST_SEEN_BLOCK_SEQNO=<seed_seqno>` (W²-aligned)
  - `GENESIS_ANCHOR_LEVEL=2` (new; contract does not read it)
  - Optional `GENESIS_L1_LAYER_HASH=<L1 root at seed>` for auditing.

## 5. Daemon CLI + env summary

New knobs (both daemons):
- CLI: `--anchor-level 1|2` on `bridge-prover-daemon` and on
  `bridge-relayer-daemon`'s `daemon-live` and `withdraw-e2e` subcommands.
- Env: `BRIDGE_ANCHOR_LEVEL` (default 1); clap `env =` picks it up.
- Cross-check: `bootstrap_seqno % stride(level) == 0`.

Startup drift (extended):
- Log `daemon_anchor_level`.
- Derive on-chain stride from `storedLastSeenBlockSeqNo`:
  `on_chain.last_seen_block_seq_no % daemon_stride == 0` — else refuse
  (existing bail path at `relayer.rs:2169`).

## 6. Circuit 4 (event) smoke-test checklist

Verification-only — user already added L2+ support.

- `bridge-event-witness/src/enrich.rs`: grep for `AnchorLayerMode`, `escalate`,
  `probe`. Verify `enrich_witness` (~L206) forwards `anchor_layer` to
  `real_chain_builder::build_event_anchor_chain` for `n ≥ 2`. Auto-escalation
  loop cap `MAX_LAYERS` (~L422) — L2 window has entries after first W²
  bundle lands.
- `bridge-event-witness/src/bin/build.rs`: `--anchor-layer 2` exercised
  in wide-net tests.
- `bridge-event-prover-lib/`: no changes expected — circuit level-parametric.
- `bridge-event-halo2-prover/`: `--anchor-layer 2` runs green in golden suite.
- Pure-math helpers already covered: `real_chain_builder::l2_anchor_boundaries`
  (~L788), `l_n_anchor_boundaries` (~L824), tests at `real_chain_builder.rs:1046+`.

## 7. Testing plan

**Unit (`bridge-prover-lib`):**
- `live_driver::thinning::find_next_bundle_boundary`: L1 and L2 strides;
  L1 shim byte-identical to old behaviour.
- `LiveProverConfig` default is L1; `AnchorMode::L2.stride(W, P) == W²`.
- `real_chain_builder::l2_anchor_boundaries` already covered.

**Integration (no live GQL):**
- Extend driver mock harness with a canned fixture: block at
  `seq_no = W² = 16 384` carrying two-layer `history_proofs`. Verify:
  - `LiveProverDriver::poll_next_bundle` yields a `Bundle` after seeding.
  - `chain_result.num_steps == 1`, `num_layers == 2`.
  - `layer_hash_frs[0]` and `layer_hash_frs[1]` populated.
  - `prev_max_level_layer_hash_fr` matches prior L2 root (or zero on the
    first L2 bundle after fresh seed).
- Mock-verifier round-trip through `bridge-prover-daemon`.

**Sepolia smoke (fresh Deploy #12 pinned to L2 genesis):**
1. Pick shellnet W²-boundary `S` with `S ≤ head − W²`.
2. `compute_bridge_anchors --level 2 --seed-seqno S` → copy env values
   into `contracts/ethereum/.env.shellnet`.
3. Deploy `AckiNackiBridge` (level-opaque; unchanged).
4. Nuke `state/`, `relayer-state.json`.
5. `bridge-relayer-daemon daemon-live --anchor-level 2 --bootstrap-seqno S`.
6. Wait for first `verifyBlock` at `S + W²`. Record gas + timing.
7. One shellnet burn; `withdraw-e2e --anchor-level 2 --anchor-layer 2`.
   Confirm withdraw succeeds within ~2 h ceiling (§ proposal wait-time table).

## 8. Rollout order (small → medium)

1. **[S] `bridge-prover-lib` types + config** — `AnchorMode`,
   `LiveProverConfig` field, `find_next_bundle_boundary` shim. Default L1;
   full test suite stays green.
2. **[S] `compute_bridge_anchors --level`** — additive; L1 output byte-identical.
3. **[M] `LiveProverDriver` L2 dispatch** — thread `anchor_mode` into
   seed-alignment, `find_next_bundle_boundary`, `advance_bootstrap`.
   Confirm L1 mode still yields byte-identical bundles.
4. **[S] Daemon CLI/env plumbing** — both daemons. Default L1.
5. **[S] State-schema extension** — `anchor_level` in `BridgeState` +
   `BootstrapSeed`. Bump `SEED_SCHEMA_VERSION`. No auto-migrate.
6. **[M] Sepolia Deploy #12 dry-run** — fresh deploy at live L2 boundary;
   end-to-end withdraw.
7. **[S] Docs** — update `TECHNICAL_README.md` with mode and W² cadence.

L1 keeps working through stages 1–5 (every new field defaults to L1).

## 9. Known risks / open questions

- **Case D branch (`real_chain_builder.rs:156`)** — under L2 the cursor
  advances by exactly W² each bundle, so `target_seqno % W² == 0` always
  holds and `num_layers == prev_num_layers == 2`. Verified against
  dispatch logic; no refactor.
- **`find_next_thinned_key_block` rename** — "thinned" references the L1
  P-thinning. Suggest `find_next_bundle_boundary`; keep old name as
  `#[deprecated]` shim.
- **First-L2-bundle path** — the L2 seed installs `num_active_layers = 2`,
  so `prev_num_layers = 2` on the first `poll_next_bundle`. First bundle
  hits Case D (steady-state path). Verify with smoke run.
- **`build_chain_same_layer` under L2** — L1-only same-layer path (~L207)
  must never fire under L2. Add `debug_assert!` or `warn!`.
- **On-chain level opacity** — `AckiNackiBridge.verifyBlock` reads
  `_readInstance(proof, 15)` as bytes. As long as genesis anchor is the
  L2 root at a W²-aligned block and every subsequent bundle advances that
  anchor consistently, contract doesn't need to know the level.
- **Anchor selection in `compute_bridge_anchors`** — must pick L2 root, NOT
  L1 root, at the W²-boundary. Single most error-prone change. Hard
  `assert!` guards it.

**Grep queries already run:**
- `Grep "target_layer|anchor_level|thinning_factor|num_layers|MAX_LAYERS"`
  in `bridge-event-witness/` → `enrich.rs` and `bin/build.rs` already
  parametric on `--anchor-layer auto|N`. Confirms Circuit 4 side is done.
- `Grep "num_layers"` in `real_chain_builder.rs` → dispatch reads
  `target_history_proofs.len()` (~L117); no per-caller field to plumb.
- `Grep "AnchorLayerMode|anchor_layer"` in `bridge-relayer-daemon/` →
  `withdraw_e2e/driver.rs:24,68,197` — withdraw pipeline already routes
  `AnchorLayerMode` (event-anchoring layer; distinct from bundle
  anchoring level added here).

## Critical files

- `bridge-prover-lib/src/live_driver/mod.rs`
- `bridge-prover-lib/src/live_driver/thinning.rs`
- `bridge-prover-lib/src/real_chain_builder.rs`
- `bridge-prover-lib/src/bootstrap.rs`
- `bridge-prover-lib/src/bin/compute_bridge_anchors.rs`
- `bridge-relayer-daemon/src/bin/relayer.rs`
- `bridge-relayer-daemon/src/state.rs`
