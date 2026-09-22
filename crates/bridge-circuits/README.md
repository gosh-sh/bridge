# ZK Bridge: Acki Nacki → Ethereum

A zero-knowledge proof system bridging Acki Nacki blockchain state to Ethereum. An Ethereum contract verifies halo2 KZG proofs that establish two facts:

1. **Block finalization** — Acki Nacki blocks were signed by a supermajority of validators (Circuits 1A/1B + 2).
2. **Withdrawal authenticity** — a `WithdrawalInitiated` event was committed to a finalized block (Circuit 4).

> **BK-set updates are tracked off-circuit.** Rotations of the validator set are handled in the open: the verifier opens the L2/L3 leaves of the block-id Merkle tree (old/new BK-set Poseidon commitments) with two SHA-256 siblings against the `block_id` already verified by Circuit 1A/1B, then advances `storedBkSetCommitment` directly. No Circuit 3 is in the production path — see `bridge/crates/bridge-prover-libraries/bridge-prover-lib/docs/bk_set_update_no_circuit3_plan.md` for the design.

Block-finalization proofs feed the contract a rolling commitment to Acki Nacki history (the layer-hash windows of `GlobalHistoryData`). Withdrawal proofs publish a single `final_root` that the contract matches against any cell in those windows.

> **Status — measured end-to-end 2026-05-23, `W = 128 / P = 4`:**
> - Mode B (full user cycle inc. Circuit 4) on local devnet: **10:27 wall-clock**.
> - Mode A (bridge state transition, 1A + 2) on shellnet: first verified bundle in **11:45**.
>
> Both modes run on either network — see [E2E Test Runbook](#e2e-test-runbook).
>
> **Notation:** `W` ≡ `HISTORY_PROOF_WINDOW_SIZE`, `P` ≡ `THINNING_FACTOR_P`. Bundle width = `W·P` source blocks.

---

## Table of Contents

- [Architecture](#architecture)
- [The Circuits](#the-circuits)
- [Thinning (W, P)](#thinning-w-p)
- [Block ID Merkle Tree](#block-id-merkle-tree)
- [Ethereum Contract](#ethereum-contract)
- [Measured Performance](#measured-performance)
- [E2E Test Runbook](#e2e-test-runbook)
- [Build & Tests](#build--tests)
- [Companion Specs](#companion-specs)

---

## Architecture

```
       ACKI NACKI NODE                            OFF-CHAIN PROVER
 ┌──────────────────────────┐         ┌──────────────────────────────┐
 │ block, BK set,           │  GQL    │  bridge-prover-daemon        │
 │ attestations,            │ ───────▶│   ├─ Circuit 1A (BLS)        │
 │ GlobalHistoryData,       │         │   └─ Circuit 2 (layers)      │
 │ WithdrawalInitiated BOC  │         │  bridge-event-prove          │
 │                          │         │   └─ Circuit 4 (event)       │
 │ block_id = 16-leaf       │         │  bridge-verifier-daemon      │
 │ depth-4 SHA-256 root     │         │   └─ verifies proofs locally │
 └──────────────────────────┘         └──────────────┬───────────────┘
                                                     │ proofs/*.json
                                                     ▼
                                       ETHEREUM SMART CONTRACT
                                       (verifyBlock, proveWithdrawal)
```

- **Per key block (relay):** one bundle = Circuit 1A (or 1B) + Circuit 2. On BK-set transitions the verifier additionally opens L2/L3 of the block-id tree off-circuit (see banner above). With thinning, one bundle covers `W·P` source blocks (§Thinning).
- **Per withdrawal:** one Circuit 4 proof, admissible only once the bundle covering its key block has been relayed.

---

## The Circuits

| # | Name | Job | Public instances | K |
|---|------|-----|-----|---|
| 1A | Primary Attestation | Verify ≥⌈2n/3⌉ BLS supermajority for a single attestation | `[block_id, bk_poseidon, block_seq_no, last_seen]` | 20 |
| 1B | Fallback Attestation | Verify two >50% attestations (Primary + Fallback type) referencing the same `block_id` | same as 1A | 20 |
| 2 | Layer Historical Hashes | Open L0 (Poseidon layer-hash preimage) + SHA-256 Merkle path to `block_id`; verify Poseidon dense chain (`MAX_CHAIN_LEN = 11`) for layer-hash progression | `[block_id, bk_poseidon, num_layers, layer_hash_frs[0..9], prev_max_level_layer_hash]` (14 instances) | 17 |
| 4 | Bridge Event Prover | Re-hash event BOC cells; bind `tokenId/dapp/account` to a `Poseidon96` block leaf; climb the dense chain to a layer root and publish it as `final_root`; publish the anchor's 1-indexed `anchor_layer` for on-chain slot dispatch | `[tokenId, amount, recipient_hi, recipient_lo, dst_chain_id, sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root, anchor_layer]` (`TOTAL_PUBLIC_INPUTS = 11`) | 19 |

### Circuit 4 — single-final-root design

The bridge uses ZK proofs to spare the Ethereum contract from implementing Acki Nacki's crypto primitives natively — BLS-12-381 pairings on the supermajority, the dense Poseidon chain over layer hashes, SHA-256 block-ID Merkle paths — none of which are cheap (or, in BLS-12-381's case, possible without a precompile) in Solidity. There is **no anonymity goal**: every public input the circuits expose is a value the contract already needs to mirror Acki Nacki's `GlobalHistoryData`. So Circuit 4 publishes exactly what the verifier needs to recognise the event, and nothing more.

Concretely, Circuit 4 publishes a single `final_root` (slot 9 of the 11 public instances) — the layer-hash it reaches by climbing the dense chain from the event's block leaf. Slot 10 carries the 1-indexed `anchor_layer` (range-checked `1..=MAX_ANCHOR_LAYER = 10`) so the on-chain verifier knows which layer window to check `final_root` against.

Recognising `final_root` is the verifier's job, off-circuit. Any entity that mirrors Acki Nacki's `GlobalHistoryData` (the Ethereum contract via `verifyBlock` → `appendLayer`, the off-chain verifier daemon via its own state) holds the same layer-hash windows the producer emits. Verification reduces to: **does `final_root` appear among the layer hashes in that mirrored history?** Membership in that set is what proves the event was committed to a finalised block.

The public-instance vector is fixed at `11` and the Circuit 4 VK is **independent of `W`** — no per-`W` feature flag, no separate test/prod shapes.

---

## Thinning (W, P)

Full design in [`BRIDGE_PROVER_THINNING_SPEC.md`](./BRIDGE_PROVER_THINNING_SPEC.md). Quick summary:

- `W` ≡ `HISTORY_PROOF_WINDOW_SIZE` (producer-side: layer-`L` hashes emitted every `W^L` blocks).
- `P` ≡ `THINNING_FACTOR_P` (prover-side: relay one bundle per `P` key blocks; bundle covers `W·P` source blocks).
- The prover's witness builder fills `verify_chain_of_dense_proofs`' up-to-11 slots by interleaving the three primitive moves on Acki Nacki's layer-`L` Poseidon trees (**climb** / **forward** / **descend** — §2.3 of the spec). No circuit-code changes.
- Worst-case chain length: `max(P, L_max − 1)`. At `P = 4, L_max ≤ 10`: chain ≤ 9, safely under `MAX_CHAIN_LEN = 11`.

### Current landed configuration

| Parameter | Value | Where |
|---|---|---|
| `W` | **128** | `bridge_prover_lib::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE` (vendored in the prover repo; must match the producer-side constant on the chain) |
| `P` | **4** | `bridge_prover_lib::THINNING_FACTOR_P` |
| Bundle width | `W·P = 512` blocks | ~3 min wall-clock per bundle on the 3 b/s devnet |

### Practical finding: drop the safety margin

The E2E orchestrator originally waited `thinned_kb_seq + (W·P)` ("one bundle past, for safety"). That margin is unnecessary: the L1 root committed by Circuit 2 at `thinned_kb_seq` *is* Circuit 4's anchor. Dropping it cut wall-clock significantly. The Python orchestrator now uses `target_seq = thinned_kb_seq` directly.

---

## Block ID Merkle Tree

Depth-4 binary SHA-256 tree with 16 leaves; `block_id` = root. This is the canonical shape from the `poseidon_profile_new` branch of `acki-nacki`. Combine rule at every internal node: `SHA-256(left_32B || right_32B)`.

```
                             Root
                    /                     \
                H_0..7                    H_8..15
             /          \              /            \
          H_01         H_23         H_45           H_67
          /  \         /  \         /  \           /  \
        H_0  H_1     H_2  H_3     H_4  H_5       H_6   H_7
        / \  / \     / \  / \     / \  / \       / \   / \
       L0 L1 L2 L3  L4 L5 L6 L7  L8 L9 L10 L11  L12 L13 L14 L15
```

| Leaf | Content | Hash | Used by |
|------|---------|------|---------|
| L0 | Layer-hash preimage (Poseidon) | Poseidon | Circuit 2 |
| L1 | SHA-256(bincode CommonSection) | SHA-256 | — |
| L2 / L3 | Old / new BK set commitments (siblings under H_1 so the off-circuit BK-set opener needs only 2 SHA-256 siblings — `H_0`, `H_23`) | Poseidon | Circuits 1A/1B; off-circuit BK-set opener |
| L4 | TVM Block representation hash | identity | — |
| L5 | SHA-256(durable_state_update) | SHA-256 | — |
| L6 | SHA-256(tx_cnt) | SHA-256 | — |
| L7 | Poseidon Merkle root of `[parent_block_id, refs…]` | Poseidon | — (opaque here) |
| L8 | `tracked_ext_out_messages_root` | Poseidon | Circuit 4 (event-binding target) |
| L9…L15 | Protocol-fixed `[0u8; 32]` padding | — | — |

**Circuit-optimized leaves** (L0, L2, L3, L8) carry only what circuits consume, in the format they consume it (Poseidon Fr elements). **Data-integrity leaves** (L1, L4..L7) commit everything else; no circuit ever opens them — they ensure the root binds to the whole block. **Padding leaves** (L9..L15) are protocol-fixed zeros so the tree stays balanced at depth 4.

### Which circuits open which leaves

- **Circuit 2** opens **L0 only**: re-derives `L0 = Poseidon(layer_hashes_preimage)` in-circuit, then walks the depth-4 path to `block_id` using **4 opaque 32-byte siblings** (`L1`, `sha_pair(L2,L3)`, `subtree(L4..L7)`, `subtree(L8..L15)`). Total in-circuit SHA-256 cost: **4 compressions**.
- **Circuit 4** binds the event to **L8** (`tracked_ext_out_messages_root`), not verified in Circuit 2.
- **Off-circuit BK-set opener** reveals L2/L3 with only 2 SHA-256 siblings (`H_0`, `H_23`).
- **Circuits 1A/1B** take `block_id` as an opaque input carried by the attestation payload — they do not walk the tree.

---

## Ethereum Contract

The contract:

1. **`verifyBlock`** — accepts a (Circuit 1A/1B + Circuit 2) bundle per thinned key block; verifies each proof; cross-checks shared `block_id` and `bk_set_poseidon_hash`; calls `appendLayer(L, root, blockHeight)` for each layer the bundle publishes; advances `storedLastSeenBlockSeqNo` and `storedLastSeenBlockHeight`. BK-set rotations are applied separately via `applyBkSetUpdate(L2, L3, …)`, which the verifier daemon (and eventually the contract) calls with the two open SHA-256 siblings against an already-verified `block_id` — `storedBkSetCommitment` is rolled forward there, not in `verifyBlock`.

2. **`withdrawByProof`** — accepts one Circuit 4 proof plus its 11 public-instance Frs. The contract verifies the SNARK, range-checks `pub[10]` (the 1-indexed `anchorLayer`) to `1..=MAX_LAYER_HASHES`, then checks that `pub[9]` (the `final_root`) appears in **only** `layerWindows[anchorLayer].data[i]` — a single-window scan (see ETH-15 fix). On success: consumes `pub[8]` (the `nullifier`), pays out to `recipient`, and emits the payout event.

   ```solidity
   function withdrawByProof(bytes calldata proof4, uint256[11] calldata pub4) external {
       require(verifier.verify(vk4, proof4, pub4), "proof");
       uint8 anchorLayer = uint8(pub4[10]);
       require(anchorLayer != 0 && anchorLayer <= MAX_LAYER_HASHES, "anchorLayer");
       bytes32 finalRoot = bytes32(pub4[9]);
       HistoryWindow storage w = layerWindows[anchorLayer];
       bool found = false;
       for (uint256 i = 0; i < w.dataLen; i++) {
           if (w.data[i] == finalRoot) { found = true; break; }
       }
       require(found, "anchor not in layerWindows");
       // …consume nullifier pub4[8], pay out, emit
   }
   ```

   The off-chain prover computes `final_root` directly from its dense-chain climb, and the contract spends an O(MAX_LAYERS·W) loop to recognise it against the layer-hash windows it already maintains. The off-chain verifier daemon performs the same membership check before approving the proof.

### State (per thread)

```solidity
storedBkSetCommitment        // uint256 — Poseidon hash of current BK set
storedLastSeenBlockSeqNo     // uint256 — monotone key-block seqno
storedLastSeenBlockHeight    // uint256 — last relayed key-block height
storedNumLayers              // uint256 — 1..MAX_LAYERS
layerWindows[L]              // L ∈ {1..MAX_LAYERS}: circular buffer
                             //    (data[W], heights[W], dataLen, writeCursor, lastHeight)
proven[bytes32]              // mapping(pubHash → bool) — placeholder
```

Total storage ≈ **81 KB / thread** at `W = 128, MAX_LAYERS = 10`. Layer 0 is **never** stored — re-derived by the off-chain prover.

---

## Measured Performance

Measured 2026-05-23 on a 5-node local Acki Nacki devnet (~3 b/s, dev profile, `opt-level = 3`).

### End-to-end test (W = 128, P = 4)

| Mode | Network | Scenario | Wall-clock |
|---|---|---|---|
| B (full user cycle) | Local devnet | event at block 1414, bundle 1536 | **10:27** |
| A (state transition) | Shellnet | first verified bundle from mid-chain bootstrap (seed 364544, bundle 365056) | **11:45** |

### Per-circuit proof generation

Each `proof_*.json` carries `*_proof_gen_ms` fields.

| Circuit | K | Mean | Range |
|---|---|---|---|
| 1A (Primary BLS) | 20 | ~5 min | 102 – 144 s |
| 2 (Layer Historical Hashes) | 17 | ~3 min | 103 – 137 s |
| 4 (Bridge Event Prover) | 19 | ~5 min | n=1 (~152 s) |

---

## E2E Test Runbook

The test surface has two orthogonal axes — **mode** (what gets proved) and **network** (where the chain runs). Same binaries, same configuration knobs across all four combinations.

| Mode | Circuits exercised | What it proves |
|---|---|---|
| **A — Bridge state transition** | 1A + 2 (+ 3 on BK-set changes) | Per thinned key block, the prover emits and the verifier accepts a bundle that advances `GlobalHistoryData` state. |
| **B — Full user cycle** | A + Circuit 4 | A `WithdrawalInitiated` event emitted by `TokenBridge` is proved against the bundle that anchors its key block. |

| Network | Endpoint | Setup |
|---|---|---|
| **Local devnet** | `http://localhost/graphql` | 5-node Docker cluster via `make run` of `gosh-sh/acki-nacki`. Cluster ships GiverV3, so Mode B's orchestrator can fund a fresh multisig automatically. |
| **Shellnet** | `https://shellnet.ackinacki.org/graphql` | Live network. For Mode B the operator supplies a pre-funded wallet that can call `TokenBridge` (no in-cluster giver). |

Mode is selected by **whether you run the event-emitting orchestrator** after the bundle stream is up. Network is selected by `BRIDGE_GQL_ENDPOINT`. There are no `if local`/`if shellnet` branches in the binaries.

### Prerequisites

- **Rust nightly** (release builds).
- **~10 GB free disk** under `acki-nacki-to-eth-bridge-halo2-prover/params/` for the KZG SRS + three PKs.
- **~16 GB RAM** during proof generation (load-on-demand keeps peak around the largest of `primary_pk.bin` ≈ 3.5 GB, `layer_pk.bin` ≈ 2.7 GB, `event_pk.bin` ≈ 2.65 GB).
- **Docker / docker compose** — local devnet only.
- **Python 3 with `tvm-cli` on PATH** — Mode B (orchestrator) on either network.
- Sibling repos pinned to these branches:

| Repo | Branch |
|---|---|
| `gosh-sh/acki-nacki` (chain) | `poseidon_profile_new` (canonical 16-leaf `block_id`; local devnet & shellnet) |
| Prover daemons | Local sibling `bridge/crates/bridge-prover-libraries/` |
| Circuits (this crate) | Local sibling `bridge/crates/bridge-circuits/` |

### Config (env vars consumed by both daemons)

| Env var | Used by | Default | Meaning |
|---|---|---|---|
| `BRIDGE_GQL_ENDPOINT` | prover, verifier | `http://localhost/graphql` | Acki Nacki GraphQL URL. Verifier uses it for the BK-set fetch and falls back to `./bk_set.json` on failure. |
| `BRIDGE_BOOTSTRAP_SEQNO` | prover only | unset → auto | Explicit seed seqno. Must be `> 0` and divisible by `W·P` (= 512) or the daemon refuses to start. |
| `RUST_LOG` | both | `info` | Standard env_logger spec. |

**Bootstrap behavior.** The prover writes `state/bootstrap_seed.json` once on first start; the verifier mirrors from it on its first init and never re-reads it. Auto-mode pins the seed at the next `W·P` boundary strictly past chain head **once** (bug fixed 2026-05-23 — it used to chase the head). When re-seeding (e.g. switching networks) wipe `state/` on **both** daemons together, otherwise the verifier sticks with its stale persisted state and the two halves diverge silently.

---

### Mode A — Bridge state transition (1A + 2 + optional 3)

Observational bundle proving and verification. No event emission, no orchestrator. Runs identically on local devnet and shellnet; the only difference is the GraphQL endpoint and the source of BK-set keys.

#### Step 1 — Prepare the chain endpoint

**Local devnet:**

```bash
cd /path/to/acki-nacki                   # checked out to branch `poseidon_profile_new`
make run                                  # kill + build_node + run_silent
docker ps                                 # expect node{0..4}, q_server0, block_manager, nginx0, aerospike
curl -s -X POST -H 'Content-Type: application/json' \
     -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
     http://localhost/graphql            # should return a seq_no
```

First-ever build: 10–20 min. Incremental: seconds-to-minutes via Docker cache.

**Shellnet:**

```bash
curl -s -X POST -H 'Content-Type: application/json' \
     -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
     https://shellnet.ackinacki.org/graphql
```

#### Step 2 — Sync `bk_set.json` to the chain's BLS pubkeys

The verifier prefers GQL `bkSetUpdates` but falls back to `./bk_set.json` when the startup race loses. Mismatched keys are the #1 cause of Circuit 1A failing with ~96 BLS-pairing constraint violations.

**Local devnet** — match the cluster's `block_keeper{0..4}_bls.keys.json`. If the chain branch is unchanged since your last run, restore the backup:

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
cp bk_set.json.poseidon_profile_new_local.bak bk_set.json
```

Otherwise rebuild from the live cluster's config:

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

**Shellnet** — GQL almost always wins (it's a live chain), so `./bk_set.json` is a safety net only. Sanity-check the endpoint:

```bash
curl -s -X POST -H 'Content-Type: application/json' \
     -d '{"query":"{ bkSetUpdates(last:1){edges{node{height nodeId}}}}"}' \
     https://shellnet.ackinacki.org/graphql
```

If you have a shellnet-specific backup, copy it in; otherwise let the daemon fetch over GQL and watch for `loaded BK set from GraphQL: N signers` in the startup log.

#### Step 3 — Generate keys (first run only)

The verifier loads **all three** circuit VKs at startup, so `event_*.bin` must exist on disk even in Mode A. Build it once:

| Circuit | Files produced under `params/` | Command |
|---|---|---|
| 4 — Event Prover (K=19) | `event_pk.bin`, `event_vk.bin`, `event_config_params.json` | `cargo run --release --bin bridge-event-prove -- --selftest` (~5 min). **Run this first** — `bridge-verifier` bails at startup on missing `event_vk.bin`. |
| 1A — Primary BLS (K=20) | `primary_pk.bin`, `primary_vk.bin` | Generated by `bridge-prover` on its first start (Step 4). |
| 2 — Layer Historical Hashes (K=17) | `layer_pk.bin`, `layer_vk.bin` | Same — generated by `bridge-prover` on its first start. |

#### Step 4 — Start prover + verifier

**Local devnet** (defaults to `BRIDGE_GQL_ENDPOINT=http://localhost/graphql`, auto-mode bootstrap):

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
rm -rf state/ proofs/                                      # wipe both together
scripts/run-bridge-test.sh                                  # builds, launches both, writes logs/
# Manual equivalent (e.g. for restart-resume testing):
#   ./target/release/bridge-verifier &
#   ./target/release/bridge-prover &
```

**Shellnet** (set `BRIDGE_GQL_ENDPOINT` explicitly):

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
rm -rf state/ proofs/ logs/ && mkdir -p logs

BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
    nohup ./target/release/bridge-verifier > logs/verifier.log 2>&1 &
echo "verifier_pid=$!" > logs/pids.txt

BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
    nohup ./target/release/bridge-prover  > logs/prover.log   2>&1 &
echo "prover_pid=$!" >> logs/pids.txt
```

Optionally pin the seed (`W·P` multiple `> 0`, past chain head — auto-mode prints the value it picks):

```bash
BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
BRIDGE_BOOTSTRAP_SEQNO=365056 \
    nohup ./target/release/bridge-prover > logs/prover.log 2>&1 &
```

Tail logs and watch for: prover `auto-mode: chain head at seq_no=N, pinned seed seq_no=M` → `seed key block available ... seed written`; verifier `bootstrapping from seed at ./state/bootstrap_seed.json`.

#### Step 5 — Watch the first bundle land

```bash
tail -f logs/*verifier*.log logs/*prover*.log
ls proofs/                              # proof_<seed+W·P>.json + result_<seed+W·P>.json
cat proofs/result_<seed+W·P>.json
# {"block_seq_no":<seed+W·P>,"primary_verified":true,"layer_verified":true,"error":null}
```

Measured 2026-05-23: shellnet first verified bundle in **11:45** wall-clock (seed 364544, first bundle 365056).

#### Stop

**Local devnet:**

```bash
cd /path/to/acki-nacki-to-eth-bridge-halo2-prover
scripts/stop-bridge-test.sh             # SIGINT both daemons, SIGKILL after 30 s if needed
cd /path/to/acki-nacki && make stop     # stops + removes docker volumes
```

**Shellnet:**

```bash
kill $(grep -oE '[0-9]+$' logs/pids.txt)
```

---

### Mode B — Full user cycle (Mode A + Circuit 4)

Mode B builds on Mode A: complete Steps 1–4, confirm the bundle stream is healthy, then trigger a withdrawal and prove it.

#### Step 6 — Emit the withdrawal event and prove it

The Python orchestrator drives the cycle: deploy/identify a wallet, call `TokenBridge`, poll for the ExtOut message, wait for the anchoring bundle, run the event-prove pipeline, await the verifier's verdict.

**Local devnet** (uses GiverV3 to fund a fresh multisig):

```bash
cd /path/to/acki-nacki
NETWORK=localhost python3 tests/exchange/generate_withdrawals_with_live_event_proving.py
```

**Shellnet** (supply a pre-funded wallet — no giver). Configure the orchestrator to use an operator-controlled keypair with enough balance to call `TokenBridge`; the rest of the pipeline is unchanged.

Phases (`[T+MM:SS]` timestamps in stdout):
1. Deploy / identify the wallet (on local devnet: deploy multisig and fund via GiverV3 `sendCurrencyWithFlag`).
2. Send `WithdrawalInitiated` through `TokenBridge`.
3. Poll GraphQL for the ExtOut message; capture `(block_seq_no, block_height, envelope_hash, account_dapp_id, account_id)`.
4. Compute `thinned_kb_seq = ((event_seq // (W·P)) + 1) · W·P` and wait until the verifier's state advances to it.
5. Run `bridge-event-private-witness-export` → `bridge-event-witness-builder` → `bridge-event-prove --fixture <enriched.json> --out-dir proofs/`.
6. Wait for `proofs/proof_event_NNN.result.json` from `bridge-verifier`.
7. Assert `verified == true && anchor_matched == true && proof_valid == true`. Exit 0.

#### Step 7 — Inspect

```bash
ls proofs/
# proof_001536.json  result_001536.json          (bundle)
# proof_event_000000.json  proof_event_000000.result.json
cat proofs/proof_event_000000.result.json
# {"verified":true,"anchor_matched":true,"proof_valid":true,"prover_self_verified":true,
#  "verified_at_block_seq_no":1536, ...}
```

Measured 2026-05-23 (local devnet, Mode B): full cycle in **10:27** wall-clock at `W = 128, P = 4` (event at block 1414, bundle 1536, Circuit 1A ~110 s, Circuit 2 ~115 s, Circuit 4 ~152 s).

Stop the same way as Mode A.

---

### Faster iteration

If only the prover binaries or the orchestrator changed, **skip `make run`** — the Docker cluster keeps running and the prover daemons re-bootstrap from `state/bootstrap_seed.json` on restart. Only re-run `make run` when chain-side Rust sources have changed and need to ship inside the Docker image.

### Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| `Connection refused` on GraphQL (local) | Cluster not fully up yet — wait until `docker ps` shows all containers healthy and the `seq_no` query in Step 1 returns. |
| Verifier exits at boot with `"event VK not found"` | Run `cargo run --release --bin bridge-event-prove -- --selftest` once (Mode A Step 3 — the verifier always loads all three VKs at startup). |
| Verifier exits at boot with `"primary VK not found"` / `"layer VK not found"` | Start `bridge-prover` first — it generates Circuits 1A/2 keys on its initial run (~10 min). |
| Circuit 1A fails with ~96 BLS-pairing equality constraint violations | `bk_set.json` stale relative to chain — re-sync from `acki-nacki/config/block_keeper{0..4}_bls.keys.json` (local) or delete the file and let GQL fetch take over (shellnet). |
| Prover seed seqno never reached (auto-mode) | Was a real bug, fixed 2026-05-23: the seed is now pinned **once** at startup. If you still hit it, pin explicitly via `BRIDGE_BOOTSTRAP_SEQNO=<next W·P boundary past chain head>`. |
| Re-seeding silently ignored — verifier keeps old `last_key_block` | The verifier never re-reads `bootstrap_seed.json` after first init. Wipe `state/` on **both** daemons together before re-seeding. |
| `target_seq` never reached (orchestrator) | Confirm the prover is producing bundles: `logs/prover_output.log` should show `=== Processing key block at height ===` every ~3 min. |
| `non-monotone height` in verifier log | Cluster was reset (chain rewound) without wiping prover/verifier `state/`. Wipe both, re-bootstrap. |
| `spawned_tasks_count not found` / tokio errors | `--cfg tokio_unstable` missing. Restore the prover repo's `.cargo/config.toml`. |

---

## Build & Tests

All commands from this repo root.

```bash
cargo build                      # all 3 circuit crates + helpers
cargo test                       # MockProver tests for every circuit

cargo test -p attestation-bls-checker-circuit
cargo test -p historical-layer-hashes-movement-checker-circuit
cargo test -p bridge-event-prove-circuit
```

Circuit 4's VK is now **W-independent** (single `final_root` public input), so no per-`W` Cargo features.

`opt-level = 3` is set in dev profile — debug builds compile slowly but run near release speed (necessary for halo2 operations).

---

## Companion Specs

- [`BRIDGE_PROVER_THINNING_SPEC.md`](./BRIDGE_PROVER_THINNING_SPEC.md) — full thinning design (the math behind `P`, chain-shape catalogue, throughput analysis, open issues).
- [`GLOBAL_HISTORY_DATA_SPEC.md`](./GLOBAL_HISTORY_DATA_SPEC.md) — reverse-engineered `GlobalHistoryData` spec; what the bridge contract mirrors.
