# Acki Nacki → Ethereum Bridge: Halo2 Prover & Verifier

Off-chain daemons that produce and locally verify the halo2 KZG proofs the
Ethereum bridge contract consumes. Three circuits are exercised:

| Circuit | K | Role |
|---|---|---|
| 1A — Primary BLS Attestation | 20 | ≥ ⌈2n/3⌉ BLS signers from the current BK set sign a block; binds `block_id` to `bk_set_poseidon`. |
| 2 — Layer Historical Hashes  | 17 | Open the L0 Poseidon preimage in the `block_id` Merkle tree; advance `GlobalHistoryData` layer windows through a dense Poseidon chain (`MAX_CHAIN_LEN = 11`). |
| 4 — Bridge Event Prover      | 19 | Hash a `WithdrawalInitiated` event BOC, bind it to a `Poseidon96` block leaf, climb the dense chain, and publish a single `final_root` as a public input — the verifier checks it off-circuit against its mirror of `layer_windows`. |

Theory, circuit witnesses, contract sketch: see `README.md` in the sibling `crates/bridge-circuits/` sub-workspace (vendored alongside this repo). This README covers **off-chain operation**: daemons, IPC, state, runbooks for the two supported networks.

> **Notation:** `W` ≡ `HISTORY_PROOF_WINDOW_SIZE`, `P` ≡ `THINNING_FACTOR_P`. Bundle width = `W·P` source blocks.

---

## Networks supported

| Network | What can be exercised |
|---|---|
| **Local devnet** (`make run` of `acki-nacki/`, GQL at `http://localhost/graphql`) | **Full E2E** — Circuits 1A + 2 (bundle proving) **and** Circuit 4 (per-event proving via the Python orchestrator). |
| **Shellnet** (`https://shellnet.ackinacki.org/graphql`) | **Full E2E** — same circuits, same orchestrator, selected via `MODE=shellnet`. |

Same binaries for both networks. Endpoint switched via `BRIDGE_GQL_ENDPOINT`; the orchestrator selects per-network parameters via `MODE` (local | shellnet).

---

## Table of Contents

- [Architecture](#architecture)
- [Binary roles & state ownership](#binary-roles--state-ownership)
- [Repository Layout](#repository-layout)
- [Prerequisites](#prerequisites)
- [KZG SRS provisioning (Hermez PPoT)](#kzg-srs-provisioning-hermez-ppot)
- [Configuration (env vars)](#configuration-env-vars)
- [Per-run hygiene (read before every E2E run)](#per-run-hygiene-read-before-every-e2e-run)
- [Bootstrap behavior](#bootstrap-behavior)
- [BK set rotation](#bk-set-rotation)
- [Runbook — local devnet (full E2E with Circuit 4)](#runbook--local-devnet-full-e2e-with-circuit-4)
- [Runbook — full E2E via bundled `python/` orchestrator](#runbook--full-e2e-via-bundled-python-orchestrator)
- [Runbook — bundle-only (Circuits 1A + 2, local or shellnet)](#runbook--bundle-only-circuits-1a--2-local-or-shellnet)
- [IPC, State, and On-disk Artifacts](#ipc-state-and-on-disk-artifacts)
- [Performance](#performance)
- [Integration Tests](#integration-tests)
- [Troubleshooting](#troubleshooting)
- [Future work](#future-work)

---

## Architecture

```
              ┌──────────────────────────┐
              │  Acki Nacki node(s)      │
              │  local devnet  OR        │
              │  shellnet                │
              └────────────┬─────────────┘
                           │ GQL (BRIDGE_GQL_ENDPOINT)
       ┌───────────────────┼────────────────────────────┐
       ▼                                                ▼
┌─────────────────────┐                  ┌────────────────────────────┐
│ bridge-prover-daemon│   proofs/        │ bridge-verifier-daemon     │
│  • Circuit 1A       │ ───────────────► │  • Reads proof_*.json      │
│  • Circuit 2        │  proof_NNN.json  │  • Verifies 1A + 2         │
│  • One bundle per   │ ◄─────────────── │  • Advances layerWindows   │
│    thinned KB       │  result_NNN.json │  • Watches proof_event_*   │
│    (W·P blocks)     │                  │  • Verifies Circuit 4      │
└─────────────────────┘                  └────────────────────────────┘
                           ▲                       ▲
                           │ proof_event_NNN.json  │
┌──────────────────────────┴────────────────┐      │
│ Per WithdrawalInitiated event:            │      │
│   bridge-event-private-witness-export ─►  │      │
│   bridge-event-witness-builder        ─►  │      │
│   bridge-event-halo2-prover --fixture ─────┘     │
│ (driven by the Python E2E orchestrator)          │
└──────────────────────────────────────────────────┘
```

Both halves of the system are **file-based**: `proofs/proof_NNN.json` is the prover→verifier channel; `proofs/result_NNN.json` (or `proof_event_NNN.result.json` for Circuit 4) is the verifier→prover ACK. `state/verifier_state.json` is the off-chain twin of the Ethereum contract's `layerWindows` storage.

**On-demand PK loading.** Each proving key is ~3 GB. The prover loads one circuit's PK, generates a proof, then unloads before loading the next — peak RSS stays around 14 GB instead of 22+.

**Path selection (Primary 1a vs Fallback 1b).** Acki Nacki's consensus has two finalisation paths — Primary (≥2N/3 signers within β blocks) and Fallback (two ≥N/2+1 attestations over the same `block_id`, used when the primary deadline is missed). The prover daemon classifies each key block's `Block.attestations[]` GraphQL response by structure:

- 1 entry with `target_type == PRIMARY` → Circuit 1a (`generate_primary_proof`).
- 2 entries (`PRIMARY` prefinalization + `FALLBACK` target proof, same `block_id`) → Circuit 1b (`generate_fallback_proof`).

Classification is **structural**, not heuristic — the threshold checks (≥2N/3 vs >N/2) are enforced in-circuit. Both circuits emit the same 4-public-instance shape `[block_id, bk_set_commitment, block_seq_no, last_seen]`; the `attestation_circuit` tag in `proof_NNN.json` tells the verifier which VK to use. The full classifier rules are the code: `BundleFinalizationType` in [`bridge-prover-lib/src/live_driver/bundle.rs`](bridge-prover-lib/src/live_driver/bundle.rs). (A `docs/fallback_path.md` write-up was linked here until it was deleted as superseded in `a69ba36`.)

---

## Binary roles & state ownership

Five executables, two long-running daemons that own the state files, plus three one-shot CLIs the Python orchestrator chains per `WithdrawalInitiated` event.

**Daemons (long-running, own `state/`):**

| Binary | Role | Owns |
|---|---|---|
| `bridge-prover-daemon` | Polls the chain via GQL, generates Circuit 1A + 2 proofs per thinned key-block (W·P-block bundle). Writes `proofs/proof_NNN.json`. | `state/prover_state.json` — its mirror of the L1+ history windows and the next bundle to prove. |
| `bridge-verifier-daemon` | Verifies every `proof_*.json` the prover drops, advances the layer-hash mirror, picks up `proof_event_*.json` and verifies Circuit 4. Writes `result_NNN.json` / `proof_event_*.result.json`. | `state/verifier_state.json` — off-chain twin of the Ethereum contract's `layerWindows`, i.e. the authoritative source for which layer hash anchors each key block. |

**Event-side CLIs (one-shot, called per event):**

| Binary | Reads | Writes | Touches daemon state / GQL? |
|---|---|---|---|
| `bridge-event-private-witness-export` | Event BOC + block context from CLI flags only. | `partial.json` (decoded `WithdrawalInitiated` + `ext_msg_leaf` ingredients). | **No.** Pure local decode — safe to run before the enclosing bundle is even proved. |
| `bridge-event-witness-builder` | `partial.json`, **`state/verifier_state.json`** (for the anchor layer hash), GQL (for `tracked_ext_out_messages` and the L1 tree shape). | `witness.json` — the complete Circuit-4 `PrivateWitness` (`events_tree_proof`, `block_tree_proof`, `anchor`). | **Yes — the only one of the three.** Glue step between the live world and the prover. |
| `bridge-event-halo2-prover` | `witness.json` + `./params/` (SRS + Circuit 4 PK/VK). | `proofs/proof_event_NNN.json` (self-verified before exit). | **No.** Pure cryptographic step — replayable offline against a frozen `witness.json`. |

**Why three event binaries instead of one.** Each step has a different failure mode and a different dependency surface, so isolating them keeps the deterministic pieces deterministic:

- Export (1) needs only raw block data → usable in hermetic unit tests.
- Builder (2) is the only piece that has to talk to the live daemon + GQL → keep its blast radius small.
- Prover (3) is CPU-bound and depends only on a frozen fixture → re-runnable without re-doing the network round-trips.

Mirrors the dex-tooling convention (`acki-nacki/tests/dex/...`) of one Rust binary per artefact.

**Per-event flow:** Python orchestrator → (1) `partial.json` → (2) `witness.json` (reads `verifier_state.json` + GQL) → (3) `proof_event_NNN.json` → `bridge-verifier-daemon` picks it up → `proof_event_NNN.result.json`.

---

## Repository Layout

```
acki-nacki-to-eth-bridge-halo2-prover/
├── Cargo.toml  Cargo.lock                 # workspace root (6 members below)
├── bridge-prover-lib/                     # shared library
├── bridge-prover-daemon/                  # bin "bridge-prover-daemon" (Circuits 1A + 2)
├── bridge-verifier-daemon/                # bin "bridge-verifier-daemon" (all three circuits)
├── bridge-event-prover-lib/               # shared Circuit 4 prover/verifier library
├── bridge-event-halo2-prover/             # bin "bridge-event-halo2-prover" (Circuit 4, one-shot)
├── bridge-event-witness/                  # 2 bins: "bridge-event-private-witness-export" (dump
│                                          #         PartialPrivateWitness from a block) and
│                                          #         "bridge-event-witness-builder" (enrich it
│                                          #         via GQL + verifier state)
├── bk_set.local.json                      # local-devnet BLS pubkeys (materialized by orchestrator; daemon default)
├── bk_set.shellnet.json                   # shellnet genesis BLS pubkeys (manual; transcribed from partner-posted keys_config.json — see Shellnet BK-set section)
├── bk_set.json.poseidon_dex_local.bak     # legacy backup for the acki-nacki `poseidon_dex` branch
├── scripts/run-bridge-test.sh             # launcher (wipes state, builds, starts both daemons)
├── scripts/stop-bridge-test.sh
├── params/   state/   proofs/   logs/     # gitignored; created on demand
└── .cargo/config.toml                     # --cfg tokio_unstable (required, do not remove)
```

`HISTORY_WINDOW_SIZE` is driven by `bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE` (currently `128`) — node and prover therefore cannot disagree on `W` at the constant level. `THINNING_FACTOR_P` (currently **`8`**, `bridge-prover-lib/src/lib.rs:46`) lives in `bridge-prover-lib/src/lib.rs`, so the bundle width is `W·P = 1024`. It was `4` — bundle width 512 — until `a69ba36` raised it for the 1024-aligned genesis anchors Deploy #7/#8 required.

### Consumers of `bridge-prover-lib`

The library is designed to serve two independent binaries:

1. **This repo's `bridge-prover-daemon`** — the reference consumer, drives Circuits 1A/1B + 2 + bk-updates against a real AN node and hands proofs to the paired `bridge-verifier-daemon` over IPC.
2. **`crates/bridge-relayer-daemon` (Sergey's ETH-side relayer)** — a separate crate outside this workspace, adds `bridge-prover-lib` as a dep and drives the same [`LiveProverDriver`](bridge-prover-lib/src/live_driver/mod.rs) API against the ETH-side `AckiNackiBridge.sol` contract. That integration landed on 2026-07-30; the API surface listed above is the contract, and the relayer's own sources are the reference for how it polls and acks. Note that since `a69ba36` the relayer submits to the Solidity contract directly — it no longer waits on this workspace's `.result.json` ACK files, which remain the prover↔verifier-daemon channel only.

The public API is stable at:
- `LiveProverDriver::{new, poll_next_bundle, poll_next_bk_update, ack_bundle, ack_bk_update, snapshot_state, snapshot_prover_bk_set, snapshot_bootstrap_seed, key_manager_ref, record_self_verify_result}`
- `LiveProverConfig`, `SeedPolicy`, `LiveBundleEvent`, `LiveBkUpdateEvent`, `BundleProofArtifacts`, `BkUpdateProofArtifacts`, `BundleFinalizationType`, `DriverError`, `DriverResult`
- `LiveProverDriver::new(gql, key_manager, state, prover_bk_set, cfg)` — as of 2026-07-27 refactor, the ctor takes a `ProverBkSet` (the persisted prover-private pubkey table) directly, not a decoded `HashMap<u16, Vec<u8>>`. Pubkeys are read internally via `driver.prover_bk_set().pubkeys()?`. The old `bk_set: HashMap<u16, Vec<u8>>` parameter is gone.
- `bk_set_fetcher` primitives for producing a `ProverBkSet` at daemon startup:
  - `load_bk_set_from_config(path)` — read a genesis snapshot from JSON (the default cold-boot seed source).
  - `bk_set_at_height(client, genesis, target_height)` — fold `bkSetUpdates` (height ≤ target) onto a genesis snapshot, for cold-start against a rotating chain long past genesis. Enabled by `BRIDGE_BK_SET_BOOTSTRAP=fold_at_height`.
  - `next_update_after(client, cursor)` — cursor-walk the delta log for post-startup rotations (used by `poll_next_bk_update`).
  - `fold_bk_updates` / `parse_bk_set_changes_pub` / `normalize_bk_set_pubkeys` — lower-level helpers exposed for daemons that manage the BK set themselves.
  - The old `fetch_bk_set` was disabled 2026-07-22 (replayed the delta log from ∅, missing the un-emitted genesis committee); do not resurrect without changing AN's protocol to emit synthetic genesis events.

Public method failures are surfaced as [`DriverError`](bridge-prover-lib/src/live_driver/mod.rs) — a structured enum (`GqlTransient`, `GqlSchema`, `ProofGen`, `StateInconsistent`, `Bootstrapping`, `Other`). Consumers who want to keep using `anyhow::Result<T>` at their call sites don't need to change anything — the blanket `impl<E: Error+Send+Sync+'static> From<E> for anyhow::Error` in `anyhow` auto-converts `DriverError` and `?` continues to work.

---

## Prerequisites

- **Rust nightly** (release builds).
- **~13 GB free disk** under `params/` (Hermez KZG SRS at K=17/19/20/21 + four PKs).
- **~16 GB RAM** during proof generation.
- **Docker / docker compose** for the local 5-node Acki Nacki cluster (local devnet only).
- Sibling checkout of `gosh-sh/acki-nacki` (private repository) on branch **`poseidon_dex`** (local devnet only).
- Python 3 + `tvm-cli` on PATH (local devnet only — orchestrator).
- Sibling sub-workspaces (both live under `crates/`):

| Path | Role |
|---|---|
| `crates/bridge-prover-libraries/` (this crate) | Daemons + prover lib |
| `crates/bridge-circuits/` | Halo2 circuits (path-dep, resolved via this Cargo.toml) |

---

## KZG SRS provisioning (Hermez PPoT)

The four circuits use the community-audited **Hermez Perpetual Powers of Tau** ceremony as their KZG trust root. The runtime SRS loader in `bridge_prover_lib::keys::common::assert_hermez_ceremony` rejects anything whose `s_g2` head is not `928fafb3d0cc…` — that includes the legacy Acki Nacki chain-ceremony blobs (`c6028acf…`) and any synthetic `gen_srs` trapdoor. Before your first run on a fresh checkout, materialize the four SRS files under `params/` with the in-tree provisioning binary:

```bash
cargo build --release --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs           # provisions K=17, 19, 20, 21
```

What this does:

1. Ensures `powersOfTau28_hez_final_20.ptau` (~1.2 GB, SnarkJs format) at `$HOME/.cache/halo2-kzg-srs/` — auto-downloads from the Polygon zkEVM GCS mirror on cache miss.
2. For each of K=17, 19, 20: reads the K=20 ptau, verifies the K=20 raw-SRS SHA-256 trust anchor (single-sourced from `gosh-zk-snark-halo2-utils::ptau`), and downsizes to the requested degree.
3. For K=21: reads the separate `powersOfTau28_hez_final_21.ptau` (~2.4 GB — **not** auto-downloaded; the utils crate's shared trust anchor is K=20-capped, so K=21 uses a byte-level Hermez `s_g2`-head check instead).
4. Byte-level asserts `s_g2` head = `928fafb3d0cc` on every file before writing → any file this binary writes is guaranteed to pass the runtime loader.

Outputs:

| File | Size | Used by |
|---|---|---|
| `params/kzg_bn254_17.srs` | ~16 MB  | Circuit 2 (layer, K=17) proving |
| `params/kzg_bn254_19.srs` | ~64 MB  | Circuit 4 (event, K=19) proving |
| `params/kzg_bn254_20.srs` | ~128 MB | Circuit 1A (primary, K=20) proving + all keygen |
| `params/kzg_bn254_21.srs` | ~256 MB | Circuit 1B (fallback attestation, K=21) proving |

### K=21 ptau — one-time manual download

The K=21 ceremony ptau is not fetched by the binary. If the default cache path is empty, `bootstrap_hermez_srs --k 21` will print the exact `curl` command; it is:

```bash
mkdir -p ~/.cache/halo2-kzg-srs
curl -L --fail --progress-bar \
  https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau \
  -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau
```

Then re-run `./target/release/bootstrap_hermez_srs`.

### CLI flags

```
--params-dir PATH        Where to write kzg_bn254_N.srs
                         (default: <bridge-prover-lib>/../params)
--k N                    Circuit K to materialize (repeatable, 1..=21)
                         (default: --k 17 --k 19 --k 20 --k 21)
--wipe-cached-keys       Delete {primary,layer,event,fallback}_{vk,pk,config_params}.*
                         (VK commitments embed s_g2 — mandatory after any SRS swap)
--ptau PATH              Override K=20 ptau path
--ptau21 PATH            Override K=21 ptau path
```

### When to re-run

- **First checkout** — one-off before the first daemon start.
- **After changing the SRS backend** (e.g. rotating away from a legacy `gen_srs`-provisioned `params/`) — always add `--wipe-cached-keys`, since existing `{primary,layer,event,fallback}_vk.bin` embed the old ceremony's `s_g2`.
- **Otherwise, never.** The Hermez ceremony is fixed; on subsequent runs the four `kzg_bn254_*.srs` files stay valid indefinitely.

---

## Configuration (env vars)

| Env var | Used by | Default | Meaning |
|---|---|---|---|
| `BRIDGE_GQL_ENDPOINT` | prover, verifier | `http://localhost/graphql` | Acki Nacki GraphQL URL. Used for bundle fetch, attestation polling and `bkSetUpdates` diffs at runtime. **Not** consulted for the BK set at bootstrap — startup is file-first (`state/prover_bk_set.json` → `BRIDGE_BK_SET_CONFIG` on cold boot). |
| `BRIDGE_GQL_FAILOVER_ENDPOINTS` | relayer `daemon-live` only | unset | Comma-separated failover GraphQL URLs. Every request starts at `BRIDGE_GQL_ENDPOINT`, retries it `BRIDGE_GQL_RETRIES_PER_ENDPOINT` (3) times `BRIDGE_GQL_RETRY_DELAY_MS` (1000) apart, then moves down this list and cycles until one attempt succeeds (`BRIDGE_GQL_MAX_ROUNDS` unset = forever). Timeouts: `BRIDGE_GQL_REQUEST_TIMEOUT_SECS` (30), `BRIDGE_GQL_CONNECT_TIMEOUT_SECS` (10). The prover/verifier daemons keep single-attempt semantics. |
| `RELAYER_METRICS_ADDR` | relayer `daemon-live` only | unset | Bind address of the Prometheus text exporter (`GET /metrics`), e.g. `0.0.0.0:9464`. Exposes `relayer_gql_requests_total`, `relayer_gql_errors_total{kind}`, `relayer_gql_failovers_total`, `relayer_gql_full_rounds_total`, `relayer_gql_request_duration_seconds`. |
| `BRIDGE_BK_SET_CONFIG` | prover, verifier | `./bk_set.local.json` | Path to the per-network genesis BK-set JSON. Set to `./bk_set.shellnet.json` when pointing daemons at shellnet. Consulted only on cold boot (as the seed for `state/prover_bk_set.json`) and by the startup guard for commitment-mismatch detection. See [Startup guard & cold-boot mismatch](#startup-guard--cold-boot-mismatch-post-2026-07-27) and [Shellnet BK-set — manual maintenance](#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson). |
| `BRIDGE_BOOTSTRAP_SEQNO` | prover only | unset → auto | Explicit seed seqno. Must be `> 0` and divisible by `W·P` (= 1024 at `P=8`), else the daemon refuses to start (`bridge-prover-daemon/src/main.rs:297-303`). |
| `RUST_LOG` | both | `info` | Standard env_logger spec. |

All other constants (poll intervals, file paths, `THINNING_FACTOR_P`) are hard-coded; see `bridge-prover-daemon/src/main.rs` and `bridge-verifier-daemon/src/main.rs` if you need to change them.

---

## Per-run hygiene (read before every E2E run)

Three cleanups the orchestrator does **not** enforce itself. Each surfaces as an opaque error deep in `tvm-cli` or witness-builder output, and each has cost hours in the past when skipped. Run them before every E2E:

1. **Rebuild all six binaries whenever `bridge-prover-lib` changed.** Six binaries share `bridge-prover-lib`: `bridge-prover-daemon`, `bridge-verifier-daemon`, `bridge-event-halo2-prover`, `bridge-event-witness-builder`, `bridge-event-private-witness-export`, `bridge-event-halo2-selftest`. Partial rebuilds leave stale binaries embedding old assertions (e.g. a pre-migration binary still says `"block_merkle_tree_leaves must have 8 entries"` after the depth-3 → depth-4 migration, whereas the current source emits `"must have 16 entries"`). Quick check:

   ```bash
   strings target/release/bridge-event-witness-builder | grep -E 'must have [0-9]+ entries'
   # should match the current assertion in bridge-prover-lib/src/gql_client.rs
   cargo build --release   # or rebuild the specific stale bin
   ```

2. **Wipe the multisig keys file between local-devnet runs:**

   ```bash
   rm -rf work-local/msig_withdrawals_e2e.keys.json work-local/msig_deploy/
   ```

   The file is persisted → same multisig address → same `deployx` message hash → the node's `feedback_registry` TTL cache returns `DUPLICATE_MESSAGE`. Symptom looks like a giver / stateInit bug but is purely cache pollution.

3. **Wipe daemon state after any cluster restart:**

   ```bash
   rm -rf state/ proofs/
   ```

   `bootstrap_seed.json` pins the daemons to old chain state. On a fresh cluster the pinned seed does not exist → prover polls forever, verifier waits for a bundle that never anchors.

Local-devnet only: `ACKI_NACKI_ROOT` must point at the sibling `acki-nacki/` checkout (default `../../../../acki-nacki`) so `materialize_bk_set_from_node_config` finds `contracts/` and `config/`. If it's wrong you get `FileNotFoundError` before any tvm-cli call runs.

**Not on this list (local devnet only): BK-set resync.** For local devnet you do **not** need to manually refresh `bk_set.local.json` after a cluster rebuild — the orchestrator regenerates it automatically on every run, and the daemons prefer GraphQL anyway. See [BK set rotation](#bk-set-rotation) for the full two-layer picture. **Shellnet is different** — see [Shellnet BK-set — manual maintenance](#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson).

---

## Bootstrap behavior

The prover writes `state/bootstrap_seed.json` once on first start and the verifier mirrors from it. There are two modes:

- **Auto (`BRIDGE_BOOTSTRAP_SEQNO` unset)** — at startup, the prover reads the current chain head, pins the seed at the next `W·P` boundary strictly past it, then polls until the chain reaches that seqno. The seed is **pinned once** and never recomputed. Works for fresh devnet *and* mid-chain shellnet.
- **Explicit (`BRIDGE_BOOTSTRAP_SEQNO=N`)** — uses `N` directly. `N` must be `> 0` and `N % (W·P) == 0`. Use this for reproducibility, or to skip the auto-mode wait by pinning to a known-good seed already past chain head.

After first init the seed file is the single source of truth — the verifier does not re-read it, and the prover does not re-pick. If you ever need to re-seed (e.g. switching networks), **wipe `state/` on both daemons together** — otherwise the verifier keeps its stale persisted state while the prover writes a fresh seed, and they silently diverge.

---

## BK set rotation

The BK set is a **circuit witness**, not a circuit constant — only `MAX_SIGNERS = 300` (compile-time, `bridge-prover-lib/src/keys.rs`) shapes the keys. **Rotation does not require regenerating `primary_*.bin` / `layer_*.bin`** as long as the new set still fits ≤ `MAX_SIGNERS`.

Post 2026-07-27 refactor there is a **single source of truth** — `state/prover_bk_set.json` — driven by two paths:

- **At bootstrap:** file-first startup (see [Startup guard & cold-boot mismatch](#startup-guard--cold-boot-mismatch-post-2026-07-27) below). If `state/prover_bk_set.json` already exists, it wins outright; `BRIDGE_BK_SET_CONFIG` is only read on cold boot (empty `state/`).
- **At runtime:** the driver consumes GQL `bkSetUpdates` via `poll_next_bk_update` + `ack_bk_update`, which rewrites `state/prover_bk_set.json` and refreshes the cached Poseidon Fr. On shellnet the diff stream is currently empty (rotation OFF per Sehor 2026-07-08); on local devnet each fresh chain regenerates keys, but the runtime-diff path never fires because the daemon is restarted alongside the chain.

**Runtime rotation (both daemons kept running):** the driver ingests the GQL diff and rewrites `state/prover_bk_set.json`; **do NOT** stop daemons or wipe `state/`.

**Restart after a chain-side key rotation:** stop both daemons, refresh the genesis file selected by `BRIDGE_BK_SET_CONFIG` (`bk_set.local.json` or `bk_set.shellnet.json`), restart both. The startup guard will catch a stale-`state/` mismatch at seq 0 and tell you to `rm -rf ./state/`. See the next subsection.

### Startup guard & cold-boot mismatch (post 2026-07-27)

The daemon runs `verify_prover_bk_set_matches_config_file` at startup: if `BRIDGE_BK_SET_CONFIG` file exists, its Poseidon commitment is compared to `state/prover_bk_set.json.commitment`. Four outcomes:

| Case | State cursor | Config file | Commitment | Action |
|---|---|---|---|---|
| A — fresh chain + stale state | `last_applied_update_seq_no == 0` | present | mismatch | **BAIL** with "wipe `./state/`" message. Fresh chain regenerated the BK set; `state/` from prior chain instance is stale. Fix: `rm -rf ./state/` → cold boot re-seeds from the config file. |
| B — chain rotated past genesis snapshot | cursor > 0 | present | mismatch | Info log only, continue. Expected on shellnet after any partner re-genesis; `state/` is the authoritative post-rotation truth. |
| C — config file absent | any | missing | n/a | Silent pass. Warm restarts without the seed file present are legal. |
| D — match | any | present | equal | OK. |

**Rules of thumb for the operator (developer running `cargo run`):**

- **Case A (seq-0 bail on local devnet):** `rm -rf ./state/` and restart. The seed file (`bk_set.local.json`) is already correct — the orchestrator regenerated it via `materialize_bk_set_from_node_config` at the start of the E2E script. No JSON edits needed.
- **Case A on shellnet:** shellnet is Case A only if the seed file was rebuilt from the wrong source (e.g., `zs_bk_set` instead of `keys_config.json.bk_nodes[i].bls_pubkey`). Fix the seed file, restart. Do **not** wipe state.
- **Wrong network's seed file selected:** fix `BRIDGE_BK_SET_CONFIG`, restart. No JSON edits, no state wipe.
- **Never hand-edit `state/prover_bk_set.json`** — it is driven exclusively by the cold-boot seed and runtime GQL diffs.

The two JSONs (`state/prover_bk_set.json` and `bk_set.*.json`) are **not** kept in sync after cold boot. The seed file is a bootstrap input; the state file is the working record.

### BK-set resync on cluster rebuild — automatic (local devnet only)

A common concern for local-devnet operators: *after `make stop && make run` the node images may regenerate BLS keys — do I need to hand-edit `bk_set.local.json` to match?* **No — for local devnet.** The orchestrator handles it:

**Orchestrator side (config-file prep).** Before starting anything else, `python/generate_withdrawals_with_live_event_proving.py` calls `materialize_bk_set_from_node_config` (line 258+). This function enumerates live `*-nodeN-*` docker containers, reads each `$ACKI_NACKI_ROOT/config/block_keeperN_bls.keys.json`, and rewrites `<PROVER_DIR>/bk_set.local.json` from scratch. Both daemons then load this file unconditionally on startup.

The committed `bk_set.local.json` in this repo is therefore just a placeholder / documentation snapshot — it is **overwritten before every local-devnet run**.

*(Historical note: prior to 2026-07-22 the daemons preferred a GraphQL `bkSetUpdates` replay over the file. That path was disabled after it was found to reconstruct an incorrect set on any chain with rotations — see the shellnet section below. The 2026-07-27 refactor further inverted the startup: `state/prover_bk_set.json` is now the single source of truth, and `BRIDGE_BK_SET_CONFIG` is consulted only on cold boot and by the startup guard. `bk_set_at_height` remains the correct primitive for distant-block cold starts.)*

**Shellnet is different — no auto-resync.** Shellnet has BK-set rotation *disabled* (Sehor confirmed 2026-07-08 for the `poseidon_dex@7ffec27` deployment): the genesis committee is fixed for the life of the chain and the GraphQL `bkSetUpdates` stream stays empty forever. The fetcher therefore always falls through to `bk_set.shellnet.json`, which is **hand-maintained** by transcribing the partner-posted `keys_config.json` (specifically the `bk_nodes[i].bls_pubkey` fields). There is no orchestrator materialiser for it. See [Shellnet BK-set — manual maintenance](#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson) for the full picture.

---

## Runbook — local devnet (full E2E with Circuit 4)

### Step 1 — Start the cluster

```bash
cd /path/to/acki-nacki                   # checked out to branch `poseidon_dex`
cargo clean && cargo update              # force rebuild — node code or tvm-sdk dep may have changed
make generate_zerostate                  # first time only
docker builder prune -af                 # purge stale Docker caches before rebuild
make run                                  # kill + build_node + run_silent
docker ps                                 # expect node{0..4}, q_server0, block_manager, nginx0, aerospike — all healthy
curl -s -X POST -H 'Content-Type: application/json' \
     -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
     http://localhost/graphql            # should return a seq_no
```

First-ever build: 10–20 min. Incremental: seconds-to-minutes via Docker cache. Skip `cargo clean`/cache purge only if you're sure neither the node nor `tvm-sdk` has changed since last `make run`.

### Step 2 — Sync `bk_set.local.json` to the cluster's BLS keys

The daemons cold-boot from `BRIDGE_BK_SET_CONFIG` (default `./bk_set.local.json`) when `state/prover_bk_set.json` is absent, and the startup guard also compares the two on every restart. The file must match `acki-nacki/config/block_keeper{0..4}_bls.keys.json`. If you've run before on the same chain branch, just restore the backup:

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cp bk_set.json.poseidon_dex_local.bak bk_set.local.json   # if backup exists & branch unchanged
```

Otherwise build it fresh:

```bash
python3 -c '
import json
out = {}
for i in range(5):
    with open(f"/path/to/acki-nacki/config/block_keeper{i}_bls.keys.json") as f:
        out[str(i)] = json.load(f)[0]["public"]
print(json.dumps(out, indent=2))
' > bk_set.local.json
```

(You can normally skip this step — the orchestrator regenerates `bk_set.local.json` on every run via `materialize_bk_set_from_node_config`. See [BK-set resync on cluster rebuild](#bk-set-resync-on-cluster-rebuild--automatic-local-devnet-only).)

### Step 3 — Keys (first run only)

Before any key generation runs, materialize the Hermez PPoT KZG SRS files under `params/` (one-off; see [KZG SRS provisioning (Hermez PPoT)](#kzg-srs-provisioning-hermez-ppot) for details):

```bash
cargo build --release --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs           # provisions K=17, 19, 20, 21
```

Then generate the per-circuit VKs / PKs:

| Circuit | Files produced under `params/` | Command |
|---|---|---|
| 4 — Event Prover (K=19) | `event_pk.bin`, `event_vk.bin`, `event_config_params.json` | `cargo run --release --bin bridge-event-halo2-prover -- --selftest` (~5 min). **Run this first** — the verifier bails on missing `event_vk.bin`. |
| 1A — Primary (K=20) | `primary_pk.bin`, `primary_vk.bin` | generated by `bridge-prover-daemon` on first start (next step). |
| 1B — Fallback (K=21) | `fallback_pk.bin`, `fallback_vk.bin` | same — generated by `bridge-prover-daemon` on first start. |
| 2 — Layer (K=17) | `layer_pk.bin`, `layer_vk.bin` | same — generated by `bridge-prover-daemon` on first start. |

### Step 4 — Start prover + verifier

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
rm -rf state/ proofs/                                      # wipe both together
scripts/run-bridge-test.sh                                  # builds, launches both, writes logs/
# Or manually (e.g. for restart-resume testing):
#   ./target/release/bridge-verifier-daemon &
#   ./target/release/bridge-prover-daemon &
```

No env vars needed — defaults give `BRIDGE_GQL_ENDPOINT=http://localhost/graphql` and auto-mode bootstrap.

Tail:
```bash
tail -f logs/verifier_output.log logs/prover_output.log
```

Expect within ~3 min: prover logs `auto-mode: chain head at seq_no=N, pinned seed seq_no=M` then `seed key block available ... seed written`, verifier logs `bootstrapping from seed at ./state/bootstrap_seed.json`.

### Step 5 — Run the orchestrator

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
NETWORK=localhost python3 python/generate_withdrawals_with_live_event_proving.py
```

Phases (`[T+MM:SS]`):
1. Deploy multisig wallet, fund via GiverV3.
2. Send `WithdrawalInitiated` via `TokenBridge`.
3. Poll GraphQL for the ExtOut message; capture `(block_seq_no, block_height, envelope_hash, account_dapp_id, account_id)`.
4. Compute `thinned_kb_seq = ((event_seq // (W·P)) + 1) · W·P` and wait for the verifier state to advance to it.
5. Run `bridge-event-private-witness-export` → `bridge-event-witness-builder` → `bridge-event-halo2-prover --fixture <enriched.json> --out-dir proofs/`.
6. Wait for `proofs/proof_event_NNN.result.json` from the verifier daemon.
7. Assert `verified == true && anchor_matched == true && proof_valid == true`. Exit 0.

### Step 6 — Inspect

```bash
ls proofs/
# proof_001536.json  result_001536.json
# proof_event_000000.json  proof_event_000000.result.json
cat proofs/proof_event_000000.result.json
```

### Stop

```bash
scripts/stop-bridge-test.sh             # SIGINT both, SIGKILL after 30s if needed
cd /path/to/acki-nacki && make stop     # stops + removes docker volumes
```

---

## Runbook — full E2E via bundled `python/` orchestrator

Same flow as above, but with the orchestrator and all its Python-side
dependencies bundled under [`python/`](./python) — no acki-nacki checkout needed
to drive the event side. Useful when the node is already running somewhere
(local devnet, shellnet, ops cluster) and the consumer only has this repo.

**What's bundled**

| Path | Purpose |
|---|---|
| `tvm-cli` (on `PATH`, or `CLI_NAME`) | Used to encode message bodies / read accounts. Not shipped in-tree — it is platform-specific, and a committed binary shadowed working system installs via `PATH` injection. The helper tries each candidate and takes the first that answers `version`; `CLI_NAME` overrides. |
| `python/contracts/{TokenBridge,UpdateCustodianMultisigWallet,GiverV3}.*` | ABIs / TVC / GiverV3 keys the orchestrator deploys & calls. |
| `python/helper/common.py` | Verbatim `tests/helper/common.py` from acki-nacki — `tvm-cli` wrapper, GQL, deploy helpers. |
| `python/generate_withdrawals_with_live_event_proving.py` | The orchestrator itself; all artefact paths are anchored to `__file__`, so CWD doesn't matter. |

**Prerequisites beyond a running node**

Same as Steps 1–4 of the local-devnet runbook with two adjustments:

- The node still has to come from somewhere (e.g. acki-nacki `make run` for local devnet) — `python/` packaging only covers the orchestrator side.
- A working `python3` (no extra packages — only stdlib is used).

**Run**

The orchestrator is a single script driven by `MODE` (`local` (default) | `shellnet`); MODE selects per-network defaults (`NETWORK`, `GRAPHQL_URL`, `WORK_DIR`, USDC-bridge owner key path, timeouts, GQL User-Agent).

**Local devnet** (default):
```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
# defaults: NETWORK=http://127.0.0.1:80, GRAPHQL_URL=http://localhost/graphql,
#          PROVER_DIR=<this repo>, WORK_DIR=<repo>/work-local
python3 python/generate_withdrawals_with_live_event_proving.py
```

**Shellnet** — set `MODE=shellnet` and the matching `BRIDGE_GQL_ENDPOINT` on the daemons (Step 4 of the local runbook → use `BRIDGE_GQL_ENDPOINT="https://shellnet.ackinacki.org/graphql" scripts/run-bridge-test.sh`):

```bash
MODE=shellnet \
BRIDGE_GQL_ENDPOINT="https://shellnet.ackinacki.org/graphql" \
    python3 python/generate_withdrawals_with_live_event_proving.py
```

`MODE=shellnet` defaults: `NETWORK=shellnet.ackinacki.org`, `GRAPHQL_URL=https://shellnet.ackinacki.org/graphql`, `WORK_DIR=<repo>/work-shellnet`, bridge-owner key from `python/contracts/USDCBridge.shellnet.keys.json`, custom GQL User-Agent (the shellnet reverse proxy rejects default `urllib`), longer timeouts. Override any of `NETWORK` / `GRAPHQL_URL` / `WORK_DIR` / `CLI_NAME` / `PROVER_DIR` to deviate.

Wall-clock on a fresh start against shellnet: **~10 min** end-to-end (daemon bootstrap + first bundle ~5 min, event capture + verifier catch-up to next bundle ~4 min, Circuit 4 prove ~3 min). The orchestrator deploys a fresh multisig funded by `GiverV3` (canonical 10 NACKL + 100T ECC[2]), mints ECC[3] (USDC) via `USDCBridge.mintAndSend` signed by the bundled bridge-owner key, fires `TokenBridge.initiateWithdrawal`, and chains the three event binaries once the verifier has crossed the event's anchor.

Phases printed are identical to Step 5 of the local-devnet runbook; exit code 0 only if the daemon's `result.json` shows `verified && anchor_matched && proof_valid`.

**Shellnet-specific gotchas:**
- `COMPUTE_SKIPPED: empty balance` during multisig deploy — faucet amounts in the orchestrator are canonical (`acki-nacki/tests/test_multisig.py`); do not shrink them.
- `Resource not found` polling the multisig — the multisig has its own dapp_id, so its address is the self-dapp `acc::acc` form, not `0:acc` or zero-dapp. The orchestrator already uses self-dapp; preserve that in extensions.
- `403` from `https://shellnet.ackinacki.org/graphql` — the reverse proxy rejects the default Python `urllib` User-Agent. The orchestrator overrides it; custom GQL callers must do the same.
- Verifier never reaches the event's anchor seq_no — almost always means the seed sits far behind chain head. Wipe + restart (`stop-bridge-test.sh` → `run-bridge-test.sh`) so auto-seed re-anchors at the next `W·P` boundary past current head.
- `BRIDGE_BOOTSTRAP_SEQNO=<n> must be > 0 and divisible by W*P=<bundle_size>` — explicit seeds must be `W·P`-aligned; drop the override or pick a valid boundary (`echo $((N - N % 1024))` at the current `P=8`). The daemon prints the live `bundle_size`, so trust that number over any written here.

### Shellnet key-refresh checklist (run this after every shellnet redeploy)

Unlike local devnet — where `python/generate_withdrawals_with_live_event_proving.py` auto-materialises everything from the sibling `acki-nacki/config/` — shellnet has **no** auto-refresh path. The shellnet cluster is a shared partner-run environment; when it is redeployed, the on-chain contract owner pubkeys change, the bundled key snapshot in this repo goes stale, and every `USDCBridge.mintAndSend` from this repo silently bounces (`exit_code=209`, "auth check failed").

**Symptom of stale bundled key:** `USDCBridge.mintAndSend` returns `exit_code=209` with no ECC[3] credit — the mint transaction reaches the contract but the compute phase rejects the pubkey check `msg.pubkey() == m_ownerPubkey`.

**Detection — one-liner:**

```bash
# Compare bundled shellnet key vs live on-chain owner pubkey
LOCAL=$(jq -r .public python/contracts/USDCBridge.shellnet.keys.json)
CHAIN=$(./tvm-cli -j runx --abi python/contracts/USDCBridge.abi.json \
        --addr 0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a \
        -m getOwnerPubkey | jq -r .value0 | sed 's/^0x//')
[ "$LOCAL" = "$CHAIN" ] && echo "OK — shellnet key current" || echo "STALE — refresh required"
```

**Where the truth lives.** The partner posts the fresh `config/` directory alongside every shellnet redeploy (e.g. `HALO2_TVM_EXPERIMENTS/config-2/` on 2026-07-08 for shellnet deployed from `acki-nacki@7ffec27` on branch `poseidon_dex`). Only one file matters for us:

```bash
cp <config-N>/USDCBridge.keys.json python/contracts/USDCBridge.shellnet.keys.json
```

**What else in `config-*/` do we consume?** Nothing. Explicit inventory:

| File in partner `config-*/` | Consumed by shellnet path? | Why / Why not |
|---|---|---|
| `USDCBridge.keys.json` | **YES** — the only file that must be refreshed | Owner pubkey pinned in the deployed contract; must match to sign `mintAndSend`. |
| `USDCToken.keys.json` | **NO** | `USDC_TOKEN_ID = 3` is a bare ECC currency ID; USDCBridge mints ECC[3] directly, no `USDCToken` contract call in this pipeline. Ignore this file. |
| `keys_config.json` | **YES — must be transcribed into `bk_set.shellnet.json`** | Holds `bk_nodes[i].bls_pubkey` — the *actual* BLS pubkeys the running shellnet nodes hold the secret halves for. This is the source of truth for the genesis committee. See "Shellnet BK-set — manual maintenance" below. |
| `block_keeperN_bls.keys.json` | **NO** (equivalent — same values as `keys_config.json.bk_nodes[N].bls_pubkey`) | Each file's `[0].public` is identical to `keys_config.json`'s corresponding `bls_pubkey`. Prefer `keys_config.json` because it is a single flat file. |
| `zs_bk_set` | **NO — do NOT use** | Historical artifact: on the 2026-07-08 shellnet snapshot the `zs_bk_set` posted alongside `SHHH_config/` holds a **different keypair set** than the running nodes (verified 2026-07-08 — using it produced ~96 BLS pairing equality failures in Circuit 1A). It appears to be a stale template from a previous deployment. Always cross-check `zs_bk_set.current[i].pubkey` against `keys_config.json.bk_nodes[i].bls_pubkey` before touching. |
| Everything else (Exchange, DappRoot, AiSuperRoot, PMPRoot, EccRoot, LicenseRoot, block_manager*, MobileVerifiers*, ip_config, blockchain.conf, zerostate) | **NO** | Unrelated to the bridge orchestrator. |

**Giver key.** Bundled `python/contracts/GiverV3.keys.json` is the well-known local pubkey; matches shellnet giver's on-chain data BOC (`128a5586045a9a3c…`). Only needs refresh if the partner rotates the shellnet giver too — usually not.

### Shellnet BK-set — manual maintenance of `bk_set.shellnet.json`

**Unlike local devnet, the shellnet BK-set cannot be fetched from GraphQL and must be maintained by hand as `bk_set.shellnet.json`. Rebuild it whenever the partner re-genesises shellnet from a new `zerostate`.**

**Why this is manual (empirically verified 2026-07-08 against `https://shellnet.ackinacki.org/graphql`):**

- Sehor confirmed 2026-07-08 that **shellnet BK-set rotation is disabled** for the current deployment (`poseidon_dex@7ffec27`). The genesis committee is fixed for the life of the chain.
- `blockchain.bkSetUpdates` is a delta log (add/remove rotation events); with rotation off it stays empty forever, and the genesis committee is never emitted as a synthetic "Added" event either.
- The GraphQL schema has **no** `currentBkSet` / snapshot query. Full type scan for `bk|committee|signer|validator|zerostate|keeper` returns only the three `BlockchainBkSetUpdate*` types — none of which expose the current active set.
- `Block` carries only `gen_validator_list_hash_short` (a hash, not the pubkeys).

Consequence: on shellnet (and on any AN chain — see 2026-07-22 note below) the daemon loads its initial BK set from the file at `BRIDGE_BK_SET_CONFIG`. **The historical `bk_set_fetcher::fetch_bk_set` GraphQL path was disabled 2026-07-22** — it replayed the `bkSetUpdates` delta log from ∅, but AN does not emit genesis as a synthetic `Added` event, so it returned an empty set on fresh chains and a diff-from-genesis (i.e. wrong) set on rotating ones. For a cold start against a rotating chain long past genesis, use `BRIDGE_BK_SET_BOOTSTRAP=fold_at_height` (+ `BRIDGE_BK_SET_TARGET_SEQNO`); this loads the JSON as the *genesis* snapshot and folds all `bkSetUpdates` up to the target height via `bk_set_fetcher::bk_set_at_height`. If that file is stale or wrong, either Circuit 1A fails with ~96 BLS pairing equality-constraint violations, or Circuit 2 aborts with `loaded BK set Poseidon commitment (…) does not match block.leaves[2] (…)`. The only working source of shellnet's genesis committee is the partner-posted `keys_config.json` — specifically its `bk_nodes[i].bls_pubkey` fields, which are the pubkeys the running nodes actually hold the secret halves for. **Do NOT use the `zs_bk_set` file** posted alongside — on the 2026-07-08 snapshot it holds a *different* keypair set than the running chain (stale template from a prior deployment).

**Producing `bk_set.shellnet.json` from `keys_config.json`:**

```bash
jq '.bk_nodes | to_entries | map({key: .key, value: .value.bls_pubkey}) | from_entries' \
   /path/to/SHHH_config/keys_config.json \
   > bk_set.shellnet.json
```

That produces the `{"0":"<48-byte-hex>", "1":"…", …}` shape the daemon's loader (`bk_set_fetcher::load_bk_set_from_config`) expects. Sanity-check the file has exactly the number of entries listed in `keys_config.json.bk_nodes` (5 for the 2026-07-08 shellnet from `acki-nacki@7ffec27`) and that each value is a 48-byte compressed BLS12-381 G1 pubkey (96 hex chars).

**Optional cross-check against `zs_bk_set`:** if the partner-posted `zs_bk_set` is *not* stale, then for every `i` it should hold that `zs_bk_set.current[i].pubkey == keys_config.json.bk_nodes[i].bls_pubkey`. On the 2026-07-08 snapshot they diverge for all five indices — `zs_bk_set` is stale — so it must not be used.

**When to update:** only after a shellnet **re-genesis** (partner ships a new `keys_config.json`). Ordinary shellnet redeploys of individual contracts do NOT rotate the BK-set and do NOT require this refresh. Currently there is no in-repo automation for this. If AN ever enables rotation on shellnet and publishes it via `bkSetUpdates`, the daemon will automatically fold updates on top of the genesis file (`current_set = bk_set.shellnet.json ⊕ replay(bkSetUpdates)`) with no manual bookkeeping required.

**State cleanup when `bk_set.shellnet.json` changes.** Because `state/prover_bk_set.json` and `state/prover_state.json` pin the previous BK-set Poseidon commitment, changing the file *requires* `rm -rf state/ proofs/` before restart — otherwise the daemon bails with `prover_bk_set.json commitment X disagrees with prover_state.json Y — delete BOTH files or restore them from a paired backup` (see `bridge-prover-daemon/src/main.rs` around line 222). This is only relevant on shellnet since local devnet regenerates the file every run.

**Sanity check after refresh** — the daemon prints `BK set: N signers, commitment=0x…` at startup. Bring one shellnet block up, take its `boc.leaves[2]`, and confirm both match; if they don't, `keys_config.json` was for a different chain snapshot than the running shellnet.

**Validation shortcut (recommended).** Instead of the full E2E, run the prover daemon with the `self-verify` feature — it produces and inline-verifies both circuits without the verifier daemon or Python orchestrator. If Circuit 1A and Circuit 2 both self-verify on the first bundle (~15 min wall clock), the BK-set is correct:

```bash
rm -rf state/ proofs/                                          # required after BK-set change
cargo build --release --bin bridge-prover-daemon --features bridge-prover-daemon/self-verify
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.json \
    ./target/release/bridge-prover-daemon > logs/prover_selfverify.log 2>&1 &
tail -f logs/prover_selfverify.log     # expect Circuit 1a/2 self-verify OK per bundle
```

Empirical result 2026-07-08: 5 consecutive bundles self-verified (`processed=5, verify_ok=5, fail=0`) with the corrected file (commitment `0x13a1dfbb…`).

**Post-refresh smoke test.** Run the isolated contract-only test (adds ~90 s) before spending 15 min on the full E2E:

```bash
MODE=shellnet python3 python/test_deploy_and_withdraw_only.py
```

Only if that PASSes, proceed to the full runbook above.

---

## Runbook — bundle-only (Circuits 1A + 2, local or shellnet)

For exercising the bridge's state-update path (Circuits 1A + 2) without event catching — useful on either local devnet (skip the Step-5 orchestrator) or live shellnet. Same binaries as the full runbook; only `BRIDGE_GQL_ENDPOINT` differs.

Pick the endpoint:

| Target | `BRIDGE_GQL_ENDPOINT` |
|---|---|
| Local devnet | `http://localhost/graphql` (default — env var can be omitted) |
| Shellnet | `https://shellnet.ackinacki.org/graphql` |

### Step 1 — Build binaries + provision Hermez SRS

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cargo build --release --bin bridge-prover-daemon --bin bridge-verifier-daemon
```

**First run only** — materialize the four Hermez PPoT KZG SRS files under `params/`:

```bash
cargo build --release --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs           # provisions K=17, 19, 20, 21
```

See [KZG SRS provisioning (Hermez PPoT)](#kzg-srs-provisioning-hermez-ppot) for what this does, when to add `--wipe-cached-keys`, and how to fetch the K=21 ptau (not auto-downloaded).

The verifier loads all four VKs at startup, so Circuit 4 keys must exist on disk even when no event will be proven. If `params/event_*.bin` are absent:
```bash
cargo run --release --bin bridge-event-halo2-prover -- --selftest
```

### Step 2 — Sync the per-network BK-set file (safety net)

The verifier prefers GQL and falls back to this file only if the startup race loses. For local devnet, follow Step 2 of the full runbook (copy from `acki-nacki/config/block_keeper*_bls.keys.json`). For shellnet the GQL fetch always returns empty (rotation is disabled and `bkSetUpdates` is a delta log — see [Shellnet BK-set — manual maintenance of `bk_set.shellnet.json`](#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson)), so the file **is** the source of truth: point the daemon at `bk_set.shellnet.json` (built from the partner-posted `keys_config.json.bk_nodes[i].bls_pubkey`) via `BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.json`.

### Step 3 — Wipe state, start both daemons

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
rm -rf state/ proofs/ logs/ && mkdir -p logs

# Omit BRIDGE_GQL_ENDPOINT for local devnet; set it for shellnet.
export BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql   # shellnet only

nohup ./target/release/bridge-verifier-daemon > logs/verifier.log 2>&1 &
echo "verifier_pid=$!" > logs/pids.txt

nohup ./target/release/bridge-prover-daemon > logs/prover.log 2>&1 &
echo "prover_pid=$!" >> logs/pids.txt
```

Optionally pin the seed for reproducibility via `BRIDGE_BOOTSTRAP_SEQNO=<N>` (must be `> 0` and `% (W·P) == 0`).

### Step 4 — Watch first bundle land

```bash
tail -f logs/verifier.log logs/prover.log
ls proofs/                              # proof_<seed+W·P>.json + result_<seed+W·P>.json
cat proofs/result_<seed+W·P>.json       # { "primary_verified": true, "layer_verified": true, "error": null }
```

Expected wall-clock from prover start to first verified bundle: ~12 min (wait for chain to cross seed + ~5 min Circuit 1A + ~3 min Circuit 2).

### Stop

```bash
kill $(cat logs/pids.txt | cut -d= -f2)
```

---

## IPC, State, and On-disk Artifacts

### `proofs/proof_NNN.json` (block bundles, written by the prover)

```json
{
  "schema_version": 6,
  "block_seq_no": 1536,
  "last_seen_block_seqno": 1024,
  "block_id_hex": "…",   // raw 32-byte BE chain hash = uint256(bytes32(blockId))
  "attestation_circuit": "primary",   // or "fallback" — picks the VK (1a vs 1b)
  "attestation_proof_hex": "…",   "attestation_proof_gen_ms": 102392,
  "layer_proof_hex":   "…",   "layer_proof_gen_ms":   137310,
  "bk_set_poseidon_hash_hex": "…",
  "num_layers": 2,
  "layer_hash_frs_hex": ["…", "…", "0", … (MAX_LAYERS = 10 entries)],
  "prev_max_level_layer_hash_hex": "…"
}
```

Since v5 (2026-07-22, Circuit 1 byte-order fix), `block_id_hex` is a *single* field shared as Circuit 1 and Circuit 2 public instance [0] — both circuits emit `block_id_fr = uint256(bytes32(root))`. The pre-v5 `layer_block_id_hex` sibling field has been removed as redundant.

Since v6 (2026-07-23), `block_id_hex` carries the **raw 32-byte BE chain hash** (= `Solidity uint256(bytes32(blockId))`), not the `Fr::to_repr()` LE bytes of the reduced public instance. This preserves the top 2 bits when the chain hash exceeds the Fr modulus (~3/4 of blocks). The Fr the Rust verifier consumes is derived on demand via `ipc::hash_hex_to_fr` (inner-product fold of reversed bytes, matching `attestation_bls_checker_circuit::attestation_data_parser::compute_block_id_fr`); the on-chain Halo2Verifier Yul does the equivalent via `mod(calldataload, f_q)`. Same convention applies to `BkUpdateRequest.block_id_hex`; the pre-v6 sibling `block_id_hash_hex` was dropped since the two fields were derivable from each other.

`attestation_circuit` is the **path-selection tag** (`BundleFinalizationType`, `bridge-prover-lib/src/live_driver/bundle.rs`). The 4-public-instance layout is identical for 1a and 1b; only the verifying key differs. Schema v3 added this tag; legacy v2 files deserialise as `"primary"`.

### `proofs/proof_event_NNN.json` (Circuit 4, local devnet only)

```json
{
  "schema_version": 1,
  "seq_no": 0,
  "proof_hex": "…",
  "public_instances_hex": ["…", … (10 entries; slot 9 = final_root)],
  "self_verified": true,
  "event_proof_gen_ms": 152166
}
```

### `proofs/result_NNN.json` (bundle ACK) / `proofs/proof_event_NNN.result.json` (event ACK)

```json
{ "block_seq_no": 1536, "primary_verified": true, "layer_verified": true, "error": null }
```

```json
{ "verified": true, "anchor_matched": true, "proof_valid": true, "prover_self_verified": true, "verified_at_block_seq_no": 1536, "event_public_instances_hex": [...], "error": null }
```

### `state/prover_state.json` and `state/verifier_state.json`

Persisted bridge state — layer hashes per active layer (1..`num_active_layers`), the last relayed key-block seqno/height, BK set commitment. The verifier file is the canonical off-chain mirror of the Ethereum contract's `layerWindows` storage.

### `state/bootstrap_seed.json`

Written by the prover on first run from the seed key block's envelope; consumed by the verifier on startup so both halves agree on initial `(bk_set, height, last_seen)`. Never re-read after first init.

---

## Performance

Measured 2026-05-23, release profile, 5-node local Acki Nacki devnet (~3 b/s), `W=128, P=4`.

### Per-proof generation

| Circuit | K | Range |
|---|---|---|
| 1A (Primary BLS) | 20 | ~5 min |
| 2 (Layer Historical Hashes) | 17 | ~3 min |
| 4 (Bridge Event Prover) | 19 | ~5 min |

Verify times: ~5 ms (1A), ~3 ms (2), ~110 ms (4). All constant-time.

### Whole E2E cycle

| Scenario | Wall-clock (orchestrator T+) |
|---|---|
| Local devnet, daemons started before event (one bundle of catch-up) | **~10:30** |
| Shellnet, prover start → first verified bundle | **~12:00** |

### Cached cryptographic artifacts

| File | Size |
|---|---|
| `params/kzg_bn254_17.srs` | ~16 MB |
| `params/kzg_bn254_19.srs` | ~64 MB |
| `params/kzg_bn254_20.srs` | ~128 MB |
| `params/kzg_bn254_21.srs` | ~256 MB |
| `params/primary_pk.bin` | ~3.5 GB |
| `params/fallback_pk.bin` | ~3.5 GB |
| `params/layer_pk.bin` | ~2.7 GB |
| `params/event_pk.bin` | ~2.65 GB |

Each circuit K keeps its own degree-matched SRS slice on disk (halo2-axiom requires `params.n() == 1 << circuit.k()`). All four SRS files come from the same Hermez ceremony — see [KZG SRS provisioning (Hermez PPoT)](#kzg-srs-provisioning-hermez-ppot). Peak RSS stays around the largest of the four PKs (load-on-demand).

---

## Integration Tests

All require the local Acki Nacki cluster up on `http://localhost/graphql`.

```bash
cargo test -p bridge-prover-lib --test live_attestation_test    -- --nocapture  # BLS verify, ~1 s
cargo test -p bridge-prover-lib --test live_proof_test          -- --nocapture  # Circuit 1A, ~2-3 min
cargo test -p bridge-prover-lib --test both_circuits_test       -- --nocapture  # 1A + 2, ~2.5 min
cargo test -p bridge-prover-lib --test live_10_blocks_test      -- --nocapture  # 10× Circuit 1A, ~20 min
cargo test -p bridge-prover-lib --test tree_reconstruction_test -- --nocapture
cargo test -p bridge-prover-lib --test shellnet_bk_set_test     -- --nocapture  # BK set extraction (shellnet)
cargo test -p bridge-event-prover-lib --test event_prover       -- --nocapture  # Circuit 4 standalone
```

---

## Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| Daemon panics with `SRS … is not Hermez Perpetual Powers of Tau (s_g2 head … expected 928fafb3d0cc)` | `params/kzg_bn254_*.srs` was written by legacy `gen_srs` or the Acki Nacki chain ceremony (head starts with `c6028acf…`). Wipe stale artifacts and re-provision from Hermez: `./target/release/bootstrap_hermez_srs --wipe-cached-keys`. See [KZG SRS provisioning (Hermez PPoT)](#kzg-srs-provisioning-hermez-ppot). |
| Daemon panics with `no Hermez Perpetual Powers of Tau SRS (≥ k=21) under ./params` | K=21 SRS missing (needed by Fallback / Circuit 1B, eagerly constructed at `KeyManager::new`). Run `./target/release/bootstrap_hermez_srs --k 21`; if the K=21 ptau isn't cached, the binary will print the `curl` command to fetch it. |
| Verifier exits with `"primary VK not found"` / `"layer VK not found"` / `"fallback VK not found"` | Run `bridge-prover-daemon` first — it generates 1A/1B/2 keys on initial start (~10 min). |
| Verifier exits with `"event VK not found"` | Run `cargo run --release --bin bridge-event-halo2-prover -- --selftest` once. |
| Prover auto-mode never starts proving — seed seqno keeps moving | Should not happen (bugfix landed 2026-05-23: seed is pinned once at startup). If observed, file an issue. As a workaround, pin via `BRIDGE_BOOTSTRAP_SEQNO=<next W·P boundary past chain head>`. |
| Circuit 1A fails with ~96 BLS pairing equality constraint violations | Genesis BK-set file stale (path selected by `BRIDGE_BK_SET_CONFIG`). Local devnet: re-sync `bk_set.local.json` from `acki-nacki/config/block_keeper*_bls.keys.json` (see local Step 2) or trust the GQL fetch by deleting the stale file. Shellnet: rebuild `bk_set.shellnet.json` from the partner-posted `keys_config.json` (`bk_nodes[i].bls_pubkey`) and `rm -rf state/ proofs/` before restart — see [Shellnet BK-set — manual maintenance](#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson). Do **not** use `zs_bk_set` on the 2026-07-08 snapshot; it is stale. |
| Circuit 2 fails with `loaded BK set Poseidon commitment (…) does not match block.leaves[2] (…)` | Same root cause as the row above — daemon loaded a stale/wrong genesis BK-set file. Check the log line `trying config file fallback: <path>` to confirm which file the daemon actually consumed, then refresh that file. |
| Verifier state file shows old `last_key_block` after restart with new network | Wipe `state/` on **both** daemons together before re-seeding. The verifier never re-reads `bootstrap_seed.json` after first init. |
| Orchestrator hits `VERIFIER_STATE_TIMEOUT_S` | Confirm prover is producing bundles (`logs/prover_output.log` should show `=== Processing key block at height ===` every ~3 min). |
| `non-monotone height` in verifier log | Cluster was restarted (chain reset) without wiping prover/verifier `state/`. Wipe both, re-bootstrap. |
| `spawned_tasks_count not found` / tokio errors | `--cfg tokio_unstable` missing. Restore `.cargo/config.toml`. |

---

## Future work

Pending items the current production path is missing — none block today's E2E happy path, but each will be needed before the bridge is real-world hardened.

### 1. Circuit 3 (BK Set Update) wiring

Likewise not on the daemon path yet. Circuit 3 proves that applying the on-chain "effective changes" to the old BK-set Poseidon commitment (L2) yields the new one (L3), both sitting under `H_1` of the block-id Merkle tree. K=16, public instances `[block_id, poseidon_old, poseidon_new]`. Must run **on every key block whose BK set differs from the previous one** — otherwise the verifier's `stored_bk_set_commitment` cannot advance and the next Circuit 1A/1B will fail on commitment mismatch. Wiring: detect BK-set delta in the prover daemon, generate the proof alongside the 1A+2 bundle, extend the verifier-daemon to consume it and roll `storedBkSetCommitment` forward.

### 2. Event anchoring beyond nearest L1

**Current state — confirmed.** `bridge-event-witness-builder` hard-codes `layer_idx = 0` and rejects anything else (`main.rs` line ~223). The Python orchestrator's `target_seq = thinned_kb_seq` math is the matching client-side consequence: a withdrawal must wait for the **next thinned L1 key block past the event** to be relayed, then is bound directly to that L1 layer hash. `bridge-event-prover-lib` and `bridge-prover-lib` together produce a Circuit 4 witness whose `dense_chain` carries **only inactive padding** to `MAX_CHAIN_LEN = 11` — i.e. zero hops up to a higher layer; the L1 root *is* the anchor.

What this means in practice:
- **Liveness coupling.** A user withdrawal cannot be proved until the bundle covering its key block has been relayed (one bundle = `W·P` blocks, 1024 at W=128 and the current P=8, ≈ minutes on devnet, longer on shellnet).
- **No cross-layer compression.** Even when an event sits inside an L2/L3/… aggregation that the prover *is* relaying, the witness still has to anchor against the L1 cell. There is no escalation logic.

Future enhancements (all already sketched in `bridge-event-witness/src/bin/build.rs` as `TODO(L1→L5 escalation)`):

- **L1→Ln escalation.** When the event's bundle has rolled out of the L1 rolling window, walk up: find the parent L2 key block in `state.layer_windows[1]`, append one active `dense_chain` link to bridge L1→L2 (or further). The in-circuit `verify_chain_of_dense_proofs` already supports up to 11 hops; only the witness builder needs work. Production-shape `real_chain_builder::build_layer_n_tree` is the reference layout.
- **Wait-for-L2 (or higher) by default.** Shellnet and the intended mainnet profile already anchor at L2 (`BRIDGE_ANCHOR_LEVEL=2`). L1 remains the local/CI `AnchorMode` default and this CLI's compile-time default.
- **Anchor randomization / batching.** When multiple withdrawals fall under the same layer-N root, the submitter could randomize which of the layer's child roots each proof binds to (anonymity-set behaviour the dropped `circuit4-single-final-root` design used to provide in-circuit). Same goes for amortising several proofs under a shared anchor: pick the highest layer that still covers the freshest event.
- **Anchor recency policy.** Once L1→Ln escalation lands, the bridge contract needs a rule for the maximum staleness it accepts. Probably exposed as a contract parameter so it can be tightened/loosened without redeploying.

---
