# Acki Nacki → Ethereum Bridge: Halo2 Prover & Verifier

Off-chain daemons that produce and locally verify the halo2 KZG proofs the
Ethereum bridge contract consumes. Three circuits are exercised:

| Circuit | K | Role |
|---|---|---|
| 1A — Primary BLS Attestation | 20 | ≥ ⌈2n/3⌉ BLS signers from the current BK set sign a block; binds `block_id` to `bk_set_poseidon`. |
| 2 — Layer Historical Hashes  | 17 | Open the L0 Poseidon preimage in the `block_id` Merkle tree; advance `GlobalHistoryData` layer windows through a dense Poseidon chain (`MAX_CHAIN_LEN = 11`). |
| 4 — Bridge Event Prover      | 19 | Hash a `WithdrawalInitiated` event BOC, bind it to a `Poseidon96` block leaf, climb the dense chain, and publish a single `final_root` as a public input — the verifier checks it off-circuit against its mirror of `layer_windows`. |

Theory, circuit witnesses, contract sketch: see [`acki-nacki-to-eth-bridge-halo2-circuits/README.md`](../acki-nacki-to-eth-bridge-halo2-circuits/README.md). This README covers **off-chain operation**: daemons, IPC, state, runbooks for the two supported networks.

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
- [Configuration (env vars)](#configuration-env-vars)
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

Classification is **structural**, not heuristic — the threshold checks (≥2N/3 vs >N/2) are enforced in-circuit. Both circuits emit the same 4-public-instance shape `[block_id, bk_set_commitment, block_seq_no, last_seen]`; the `attestation_circuit` tag in `proof_NNN.json` tells the verifier which VK to use. See [docs/fallback_path.md](docs/fallback_path.md) for the full classifier rules and operational notes.

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
├── bk_set.json                            # local BLS pubkeys fallback (see Troubleshooting)
├── bk_set.json.poseidon_dex_local.bak     # snapshot for the acki-nacki `poseidon_dex` branch
├── scripts/run-bridge-test.sh             # launcher (wipes state, builds, starts both daemons)
├── scripts/stop-bridge-test.sh
├── params/   state/   proofs/   logs/     # gitignored; created on demand
└── .cargo/config.toml                     # --cfg tokio_unstable (required, do not remove)
```

`HISTORY_WINDOW_SIZE` is driven by `bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE` (currently `128`) — node and prover therefore cannot disagree on `W` at the constant level. `THINNING_FACTOR_P` (currently `4`) lives in `bridge-prover-lib/src/lib.rs`.

---

## Prerequisites

- **Rust nightly** (release builds).
- **~10 GB free disk** under `params/` (KZG SRS + three PKs).
- **~16 GB RAM** during proof generation.
- **Docker / docker compose** for the local 5-node Acki Nacki cluster (local devnet only).
- Sibling checkout of [`acki-nacki`](https://github.com/gosh-sh/acki-nacki) on branch **`poseidon_dex`** (local devnet only).
- Python 3 + `tvm-cli` on PATH (local devnet only — orchestrator).
- These two repos pinned to the matching branches:

| Repo | Branch |
|---|---|
| `gosh-sh/acki-nacki-to-eth-bridge-halo2-prover` (this repo) | `main` |
| `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits` | `main` (pinned via this repo's `Cargo.toml`) |

---

## Configuration (env vars)

| Env var | Used by | Default | Meaning |
|---|---|---|---|
| `BRIDGE_GQL_ENDPOINT` | prover, verifier | `http://localhost/graphql` | Acki Nacki GraphQL URL. Verifier uses it for BK-set fetch (falls back to `./bk_set.json` on failure). |
| `BRIDGE_BOOTSTRAP_SEQNO` | prover only | unset → auto | Explicit seed seqno. Must be `> 0` and divisible by `W·P` (= 512), else the daemon refuses to start. |
| `RUST_LOG` | both | `info` | Standard env_logger spec. |

All other constants (poll intervals, file paths, `THINNING_FACTOR_P`) are hard-coded; see `bridge-prover-daemon/src/main.rs` and `bridge-verifier-daemon/src/main.rs` if you need to change them.

---

## Bootstrap behavior

The prover writes `state/bootstrap_seed.json` once on first start and the verifier mirrors from it. There are two modes:

- **Auto (`BRIDGE_BOOTSTRAP_SEQNO` unset)** — at startup, the prover reads the current chain head, pins the seed at the next `W·P` boundary strictly past it, then polls until the chain reaches that seqno. The seed is **pinned once** and never recomputed. Works for fresh devnet *and* mid-chain shellnet.
- **Explicit (`BRIDGE_BOOTSTRAP_SEQNO=N`)** — uses `N` directly. `N` must be `> 0` and `N % (W·P) == 0`. Use this for reproducibility, or to skip the auto-mode wait by pinning to a known-good seed already past chain head.

After first init the seed file is the single source of truth — the verifier does not re-read it, and the prover does not re-pick. If you ever need to re-seed (e.g. switching networks), **wipe `state/` on both daemons together** — otherwise the verifier keeps its stale persisted state while the prover writes a fresh seed, and they silently diverge.

---

## BK set rotation

The BK set is a **circuit witness**, not a circuit constant — only `MAX_SIGNERS = 300` (compile-time, `bridge-prover-lib/src/keys.rs`) shapes the keys. **Rotation does not require regenerating `primary_*.bin` / `layer_*.bin`** as long as the new set still fits ≤ `MAX_SIGNERS`.

Both daemons fetch the set **once at startup** and cache it for the whole run — there is no `bkSetUpdates` subscription. If the on-chain set rotates mid-run, the prover will silently skip key blocks signed by indices it doesn't recognise (`signers [k] not in BK set, skipping`).

On rotation: **stop both daemons, refresh `bk_set.json` if you rely on the GQL-failure fallback, restart both — do NOT wipe `state/`** (`stored_bk_set_commitment` is overwritten on the next bundle). The only case that needs `rm -rf state/` is a rotation that happens during the bootstrap key block itself, since `bootstrap_seed.json` would then encode the stale set.

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

### Step 2 — Sync `bk_set.json` to the cluster's BLS keys

The verifier falls back to `./bk_set.json` if the GQL `bkSetUpdates` race loses at startup. The file must match `acki-nacki/config/block_keeper{0..4}_bls.keys.json`. If you've run before on the same chain branch, just restore the backup:

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cp bk_set.json.poseidon_dex_local.bak bk_set.json   # if backup exists & branch unchanged
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
' > bk_set.json
```

### Step 3 — Keys (first run only)

| Circuit | Files produced under `params/` | Command |
|---|---|---|
| 4 — Event Prover (K=19) | `event_pk.bin`, `event_vk.bin`, `event_config_params.json` | `cargo run --release --bin bridge-event-halo2-prover -- --selftest` (~5 min). **Run this first** — the verifier bails on missing `event_vk.bin`. |
| 1A — Primary (K=20) | `primary_pk.bin`, `primary_vk.bin` | generated by `bridge-prover-daemon` on first start (next step). |
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
| `python/bin/tvm-cli` | Used to encode message bodies / read accounts. Picked up via `PATH` injection unless `CLI_NAME` is set. |
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
- `BRIDGE_BOOTSTRAP_SEQNO must be a multiple of 512` — explicit seeds must be `W·P`-aligned; drop the override or pick a valid boundary (`echo $((N - N % 512))`).

---

## Runbook — bundle-only (Circuits 1A + 2, local or shellnet)

For exercising the bridge's state-update path (Circuits 1A + 2) without event catching — useful on either local devnet (skip the Step-5 orchestrator) or live shellnet. Same binaries as the full runbook; only `BRIDGE_GQL_ENDPOINT` differs.

Pick the endpoint:

| Target | `BRIDGE_GQL_ENDPOINT` |
|---|---|
| Local devnet | `http://localhost/graphql` (default — env var can be omitted) |
| Shellnet | `https://shellnet.ackinacki.org/graphql` |

### Step 1 — Build binaries

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cargo build --release --bin bridge-prover-daemon --bin bridge-verifier-daemon
```

The verifier loads all three VKs at startup, so Circuit 4 keys must exist on disk even when no event will be proven. If `params/event_*.bin` are absent:
```bash
cargo run --release --bin bridge-event-halo2-prover -- --selftest
```

### Step 2 — Sync `bk_set.json` (safety net)

The verifier prefers GQL and falls back to this file only if the startup race loses. For local devnet, follow Step 2 of the full runbook (copy from `acki-nacki/config/block_keeper*_bls.keys.json`). For shellnet the chain is always live so the GQL fetch normally wins; watch startup log for `loaded BK set from GraphQL: N signers`.

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
  "schema_version": 3,
  "block_seq_no": 1536,
  "last_seen_block_seqno": 1024,
  "block_id_hex": "…",
  "attestation_circuit": "primary",   // or "fallback" — picks the VK (1a vs 1b)
  "primary_proof_hex": "…",   "primary_proof_gen_ms": 102392,
  "layer_proof_hex":   "…",   "layer_proof_gen_ms":   137310,
  "layer_block_id_hex": "…",
  "bk_set_poseidon_hash_hex": "…",
  "num_layers": 2,
  "layer_hash_frs_hex": ["…", "…", "0", … (MAX_LAYERS = 10 entries)],
  "prev_max_level_layer_hash_hex": "…"
}
```

`attestation_circuit` is the **path-selection tag** (see [docs/fallback_path.md](docs/fallback_path.md)). The 4-public-instance layout is identical for 1a and 1b; only the verifying key differs. Schema v3 added this tag; legacy v2 files deserialise as `"primary"`.

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
| `params/kzg_bn254_20.srs` | ~128 MB (K=17/19/20 share via degree truncation) |
| `params/primary_pk.bin` | ~3.5 GB |
| `params/layer_pk.bin` | ~2.7 GB |
| `params/event_pk.bin` | ~2.65 GB |

Peak RSS stays around the largest of the three (load-on-demand).

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
| Verifier exits with `"primary VK not found"` / `"layer VK not found"` | Run `bridge-prover-daemon` first — it generates 1A/2 keys on initial start (~10 min). |
| Verifier exits with `"event VK not found"` | Run `cargo run --release --bin bridge-event-halo2-prover -- --selftest` once. |
| Prover auto-mode never starts proving — seed seqno keeps moving | Should not happen (bugfix landed 2026-05-23: seed is pinned once at startup). If observed, file an issue. As a workaround, pin via `BRIDGE_BOOTSTRAP_SEQNO=<next W·P boundary past chain head>`. |
| Circuit 1A fails with ~96 BLS pairing equality constraint violations | `bk_set.json` stale — re-sync from `acki-nacki/config/block_keeper*_bls.keys.json` (see local Step 2) or trust the GQL fetch by deleting the stale file. |
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
- **Liveness coupling.** A user withdrawal cannot be proved until the bundle covering its key block has been relayed (one bundle ≈ `W·P = 512` blocks ≈ minutes on devnet, longer on shellnet).
- **No cross-layer compression.** Even when an event sits inside an L2/L3/… aggregation that the prover *is* relaying, the witness still has to anchor against the L1 cell. There is no escalation logic.

Future enhancements (all already sketched in `bridge-event-witness/src/bin/build.rs` as `TODO(L1→L5 escalation)`):

- **L1→Ln escalation.** When the event's bundle has rolled out of the L1 rolling window, walk up: find the parent L2 key block in `state.layer_windows[1]`, append one active `dense_chain` link to bridge L1→L2 (or further). The in-circuit `verify_chain_of_dense_proofs` already supports up to 11 hops; only the witness builder needs work. Production-shape `real_chain_builder::build_layer_n_tree` is the reference layout.
- **Wait-for-L2 (or higher) by default.** Today the prover anchors at the *nearest* L1 because that's the soonest. A later policy could prefer a higher layer when latency budget allows, to amortise verifier gas across many events under one anchor.
- **Anchor randomization / batching.** When multiple withdrawals fall under the same layer-N root, the submitter could randomize which of the layer's child roots each proof binds to (anonymity-set behaviour the dropped `circuit4-single-final-root` design used to provide in-circuit). Same goes for amortising several proofs under a shared anchor: pick the highest layer that still covers the freshest event.
- **Anchor recency policy.** Once L1→Ln escalation lands, the bridge contract needs a rule for the maximum staleness it accepts. Probably exposed as a contract parameter so it can be tightened/loosened without redeploying.

---
