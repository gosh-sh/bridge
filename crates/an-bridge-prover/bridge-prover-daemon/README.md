# bridge-prover-daemon

Long-running daemon that produces halo2 KZG proofs for the bridge's
**state-update path** — Circuit 1A *or* 1B (BLS Attestation, K=20) and
Circuit 2 (Layer Historical Hashes, K=17). Pairs with
`bridge-verifier-daemon` over the filesystem (`proofs/` ↔ `state/`).

The daemon classifies each key block as **Primary** (one ≥2N/3
attestation → Circuit 1A) or **Fallback** (paired ≥N/2+1 PRIMARY +
FALLBACK attestations over the same `block_id` → Circuit 1B). See
[`docs/fallback_path.md`](../docs/fallback_path.md) for the classifier
contract and operational notes.

This README is a standalone runbook for exercising the prover/verifier
pair **without event proving** (Circuit 4) — useful for testing the
bundle path on either a local devnet or live shellnet. For full E2E
(Circuit 4 + Python orchestrator) see the workspace-level
[`TECHNICAL_README.md`](../TECHNICAL_README.md).

> **Notation:** `W = HISTORY_PROOF_WINDOW_SIZE` (128), `P = THINNING_FACTOR_P` (4). Bundle width = `W·P = 512` source blocks.

---

## What this is for

The Acki Nacki → Ethereum bridge needs the Ethereum-side contract to
trust a compact summary of Acki Nacki's block history, so that later
event proofs (e.g. `WithdrawalInitiated`) can be verified against it.
The two daemons in this repo are the off-chain machinery that keeps
that summary current.

### Bridge state — `GlobalHistoryData`

The node maintains, per thread and per layer `L ∈ {0..10}`, a
fixed-size circular buffer of `W = 128` Poseidon hashes (see
[`GLOBAL_HISTORY_DATA_SPEC.md`](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits/blob/main/GLOBAL_HISTORY_DATA_SPEC.md)).
Layer 0 holds per-block leaves `Poseidon(block_id ‖ envelope_hash ‖
ext_out_messages_root)` — appended every finalized block. Layer
`L ≥ 1` holds the Poseidon-Merkle root of the previous full layer-`L-1`
window — appended only at heights where `h % W^L == 0`. Each layer's
window is overwritten in place; only the last `W` hashes per layer
remain. Top-layer reach: `W^{MAX_LAYERS+1} = 128^{11} ≈ 9·10^{23}`
blocks.

A **key block** here is a finalized block whose height is a multiple of
`W` — i.e. the block that *publishes* a new batch of layer roots in its
`common_section.history_proofs` (layer-`L` roots ride on heights where
`h % W^L == 0`, so a single key block can carry multiple layers at once).
The bridge state advances only at key blocks: the daemons reconstruct
`GlobalHistoryData` by **proving the contents of key blocks** —
Circuit 1A pins the key block's `block_id` to the live BK set, Circuit 2
opens its layer-historical-hash payload and walks them into the layer
windows. **The "bridge state" is precisely that mirror of layer
windows** — small in absolute size but spanning the whole chain through
the layer hierarchy. The verifier daemon keeps the full mirror in
`state/verifier_state.json`; the Ethereum contract will hold the same
shape, just on a thinned cadence.

![Layer hashes movement — chain-length cases and inclusion proofs at varying bridge maturity](docs/layer-hashes-movement.png)

The diagram above sketches the three primitives Circuit 2 uses to keep
the bridge mirror in sync with `GlobalHistoryData`. **Top strip:** the
layer hierarchy (Level 1 / Level 2 / L_3) over consecutive blocks.
**Left column — layer-hash movement cases**, the three regimes the
prover must handle when a new key block arrives:

- **Case A** — chain length in the bridge and in the incoming key block
  are equal; replace `L_T_prev` with `L_T_new` via a single layer-`T`
  path of length up to 10.
- **Case B** — the bridge is *behind* (its top layer is lower than the
  one the new key block carries); extend by climbing one or more layers
  in the same proof chain.
- **Case C** — the bridge is *ahead* (already has a higher top layer
  than the new key block); the convergence target sits inside an
  already-mirrored higher window, so the chain is shorter on the
  bridge side and `L_{T-1}_new` is required to converge into
  `L_{T-1}_prev + 1`.

**Right column — anonymous block-inclusion proofs** the same primitives
power for event proving (Circuit 4): at each bridge-maturity level
(`L_2` not built / `L_2` built / `L_3` built / …) we can prove any
user's block/event up to the corresponding reach (`128 ≈ 10²`,
`128·128·9 ≈ 10⁵`, …). Same chain primitives, different starting
position.

### What each daemon does

- **bridge-prover-daemon** — Polls GraphQL for new key blocks. For
  each one: fetches the BLS attestation evidence, classifies it as
  Primary (one ≥⌈2n/3⌉ attestation) or Fallback (paired ≥n/2+1
  PRIMARY + FALLBACK attestations over the same `block_id`), runs
  the matching attestation circuit (**1A** Primary or **1B** Fallback,
  same K=20 shape, same 4-public-instance layout, different VK), then
  **Circuit 2** (proves via a SHA-256 Merkle path under `block_id` the
  three block fields whose Poseidon hash is the layer-0 leaf, then
  walks a dense Poseidon chain across intermediate key blocks to
  advance the layer windows consistently). Writes one
  `proofs/proof_<seq>.json` per processed key block, tagged with the
  attestation circuit used. Proving keys are loaded on demand, then
  unloaded — peak RSS stays near the largest single PK.
- **bridge-verifier-daemon** — Watches `proofs/`, KZG-verifies each
  proof against cached VKs, and on success applies the proof's layer
  transitions to its own `state/verifier_state.json` (the off-chain
  twin of the contract storage). Writes `proofs/result_<seq>.json`
  as the ACK.

### Their interaction

Both halves are **file-based** — no sockets, no shared memory. The
on-disk channel is the IPC:

```
prover → proofs/proof_NNN.json  (1A proof + 2 proof + layer hashes)
verifier ← reads, verifies, advances state
verifier → proofs/result_NNN.json  (primary_verified, layer_verified, error)
```

State files on both sides mirror the same layer windows. Both daemons
must agree on the **bootstrap seed** (`state/bootstrap_seed.json`,
pinned at first start), and they must advance in lockstep — a state
wipe must happen on both together, otherwise they diverge silently.

### Thinning — why one bundle covers `W·P` source blocks

At ~3 blk/s a key block lands every `W/3 ≈ 43 s`, but Circuit 1A alone
takes ~5 min and its cost is invariant in `W`. Without help, the prover
falls behind the chain by ~7×. **Thinning** is the fix: relay only
*every P-th* key block (`P = 4` here), so each bundle covers `W·P =
512` source blocks. The work skipped is absorbed by Circuit 2's
dense-Poseidon proof chain — up to `MAX_CHAIN_LEN = 11` links, freely
interleaving three primitives on the layer trees: *climb* (jump to a
lower layer at the same window), *forward* (advance one full window at
the current layer), and *descend* (drop to a higher layer). The producer's
emission cadence is unchanged — thinning is purely a prover-side
relay-rate change. See
[`BRIDGE_PROVER_THINNING_SPEC.md`](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits/blob/main/BRIDGE_PROVER_THINNING_SPEC.md)
for the catalogue of chain shapes and slack analysis.

---

## Prerequisites

- **Rust nightly** (release builds).
- **~14 GB disk** under `params/` (KZG SRS + four PKs: Circuit 1A primary, 1B fallback, Circuit 2 layer, Circuit 4 event — the latter's PK is needed only at keygen, but its VK must exist for the verifier).
- **~16 GB RAM** at proof generation.
- For local devnet only:
  - **Docker / docker compose**.
  - Sibling checkout of [`gosh-sh/acki-nacki`](https://github.com/gosh-sh/acki-nacki) on branch `poseidon_dex`.

---

## Pick your network

| Target | `BRIDGE_GQL_ENDPOINT` |
|---|---|
| Local devnet | `http://localhost/graphql` *(default, env var optional)* |
| Shellnet | `https://shellnet.ackinacki.org/graphql` |

The same binaries work for both. Endpoint is the only switch.

---

## Step 1 — (local devnet only) Start the cluster

Skip this section for shellnet.

```bash
cd /path/to/acki-nacki                  # branch: poseidon_dex
cargo clean && cargo update             # if node or tvm-sdk dep changed since last run
make generate_zerostate                 # first time only
docker builder prune -af                # purge stale Docker caches before rebuild
make run                                # kill + build_node + run_silent
docker ps                               # node{0..4}, q_server0, block_manager, nginx0, aerospike — all healthy
curl -s -X POST -H 'Content-Type: application/json' \
     -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
     http://localhost/graphql           # should return a seq_no
```

First-ever build: 10–20 min. Incremental: seconds-to-minutes via Docker cache.

---

## Step 2 — Build the daemons

All commands below run from the workspace root (`acki-nacki-to-eth-bridge-halo2-prover/`).

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cargo build --release --bin bridge-prover-daemon --bin bridge-verifier-daemon
```

The verifier loads **all three VKs** at startup (1A, 2, 4), so Circuit 4's VK must exist on disk even though we won't fire that circuit. If `params/event_*.bin` are absent:

```bash
cargo run --release --bin bridge-event-halo2-prover -- --selftest    # ~5 min, first run only
```

`primary_*.bin` / `layer_*.bin` are generated by `bridge-prover-daemon` itself on its first run (~10 min added to the first bundle).

---

## Step 3 — BK set fallback file

The verifier prefers GraphQL but falls back to `./bk_set.json` if the startup `bkSetUpdates` race loses.

**Local devnet** — must match `acki-nacki/config/block_keeper{0..4}_bls.keys.json`:

```bash
# If the bundled backup matches your current chain branch:
cp bk_set.json.poseidon_dex_local.bak bk_set.json

# Otherwise rebuild from the cluster's actual keyfiles:
python3 -c '
import json
out = {}
for i in range(5):
    with open(f"/path/to/acki-nacki/config/block_keeper{i}_bls.keys.json") as f:
        out[str(i)] = json.load(f)[0]["public"]
print(json.dumps(out, indent=2))
' > bk_set.json
```

**Shellnet** — the GQL race normally wins (the chain is always live). Watch the startup log for `loaded BK set from GraphQL: N signers`; only refresh `bk_set.json` if you need the fallback.

> Rotation: if the BK set changes mid-run the prover silently skips affected key blocks (`signers [k] not in BK set, skipping`). Recovery is "restart both daemons" — keys do **not** need to be regenerated. See `BK set rotation` in `../TECHNICAL_README.md`.

---

## Step 4 — Wipe state, start both daemons

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
rm -rf state/ proofs/ logs/ && mkdir -p logs

# Shellnet only — omit for local devnet:
export BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql

nohup ./target/release/bridge-verifier-daemon > logs/verifier.log 2>&1 &
echo "verifier_pid=$!" > logs/pids.txt

nohup ./target/release/bridge-prover-daemon > logs/prover.log 2>&1 &
echo "prover_pid=$!" >> logs/pids.txt
```

Optional: pin the seed for reproducibility via `BRIDGE_BOOTSTRAP_SEQNO=<N>` (must be `> 0` and `% (W·P) == 0`, i.e. multiple of 512). Otherwise the prover auto-pins at the next `W·P` boundary past chain head.

For local devnet, `scripts/run-bridge-test.sh` does the wipe+build+launch in one shot.

---

## Step 5 — Watch the first bundle land

```bash
tail -f logs/verifier.log logs/prover.log
```

Expected startup sequence (~3 min after the prover starts polling):

```
prover  : auto-mode: chain head at seq_no=N, pinned seed seq_no=M
prover  : seed key block available ... seed written
verifier: bootstrapping from seed at ./state/bootstrap_seed.json
```

First verified bundle arrives at `M + W·P`:

```bash
ls proofs/                                  # proof_<seed+512>.json + result_<seed+512>.json
cat proofs/result_<seed+512>.json
# { "block_seq_no": ..., "primary_verified": true, "layer_verified": true, "error": null }
```

Wall-clock from prover start to first verified bundle: **~12 min** (chain catch-up + ~5 min Circuit 1A + ~3 min Circuit 2).

---

## Stop

```bash
kill $(cat logs/pids.txt | cut -d= -f2)
# Local devnet also:
cd /path/to/acki-nacki && make stop          # stops + removes docker volumes
```

If you launched via `scripts/run-bridge-test.sh`, use `scripts/stop-bridge-test.sh` instead (graceful SIGINT, SIGKILL after 30s).

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| Verifier exits with `"event VK not found"` | Run Step 2's `--selftest` command once. |
| Verifier exits with `"primary VK not found"` / `"fallback VK not found"` / `"layer VK not found"` | Let `bridge-prover-daemon` finish its first start — it generates 1A/1B/2 keys on disk (~15 min, all four PKs sized). |
| Circuit 1A fails with ~96 BLS pairing equality constraint violations | `bk_set.json` is stale relative to the live chain. Re-sync (Step 3) or delete the file to force a GQL fetch. |
| Prover never starts proving — seed seqno keeps moving | Should not happen post-2026-05-23 (seed is pinned once at startup). Workaround: pin manually via `BRIDGE_BOOTSTRAP_SEQNO=<next W·P boundary past chain head>`. |
| Verifier state shows old `last_key_block` after switching networks | Wipe `state/` on **both** daemons together — the verifier never re-reads `bootstrap_seed.json` after first init. |
| `spawned_tasks_count not found` / tokio errors | `--cfg tokio_unstable` missing. Restore `.cargo/config.toml` at the workspace root. |

For IPC schemas (`proof_NNN.json`, `result_NNN.json`, `state/*.json` layout) and performance numbers, see [`../TECHNICAL_README.md`](../TECHNICAL_README.md).
