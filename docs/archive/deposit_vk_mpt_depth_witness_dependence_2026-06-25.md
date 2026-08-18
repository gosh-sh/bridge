> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

> # ⛔ SUPERSEDED / MISDIAGNOSIS (corrected 2026-06-25)
> This document's conclusion — "the deposit VK scales with MPT proof depth; the
> fix belongs in the `axiom-eth` fork" — is **WRONG**. The real root cause was a
> redundant `load_constant(contract_address)` in `circuit_v2.rs` (which leaked the
> per-deposit address into the fixed column) **plus** an unpinned keccak capacity.
> Both are fixed inside `deposit-prover` with **no `axiom-eth` change**. With the
> fix, deposits of depth 1/3/5 produce a **byte-identical** VK (`20cf9018…`).
> The "depositId=0" fixture used as evidence below was an **Anvil** deposit to a
> *different* contract (addr `9fe4…`), not a real same-bridge deposit — its VK
> differed for the address reason, not depth.
> **See `docs/deposit_vk_witness_independence.md` for the authoritative root cause,
> fix, and validation.** Retained only for history.

# Deposit VK is witness-dependent on MPT proof depth (reproducer for Alina)

**Date:** 2026-06-25
**Repo:** `acki-nacki-bridge` @ `f6d52ff611b1`
**Circuit:** `deposit-prover/src/circuit_v2.rs` (`DepositEventCircuitV2`, `EthCircuitImpl<Fr, _>`, 11 PI)
**axiom-eth pin:** `github.com/gosh-sh/axiom-eth` branch `gosh-stable-rlcmanager-assignment` @ `1d61be0`

## TL;DR

The deposit circuit's **verifying key changes with the number of MPT proof nodes**
(receipt-trie depth) of the deposit's block. Two *real* Sepolia deposits produce
**byte-different VkBlobs** even though every circuit parameter is identical
(`k=18`, `num_advice_per_phase=[11,10]`, same `EthCircuitParams`).

Because the on-chain `USDCBridge` embeds **one** `VK_BLOB`, and the receipt-trie
depth is a function of how many transactions are in the deposit's block
(uncontrollable per-deposit), **a single embedded VK cannot verify arbitrary real
deposits.** This is the only thing blocking live ETH→AN deposit finalization.

We could **not** fix this in our `circuit_v2.rs` wrapper — it already fixed-pads
the one input it controls (`block_header_rlp` → `MAX_BLOCK_HEADER_BYTES`). The
receipt + MPT proof are handed to axiom-eth, which pads the *witness vectors*
(`nodes.resize(max_depth-1, dummy)`, constraint loop `for idx in 0..max_depth`),
yet the VK still moves with depth. The residual shape-dependency is inside the
axiom-eth fork — most likely the **keccak promise-call count for hashing MPT
nodes scales with the actual depth** rather than `max_depth`. That belongs to the
fork owner (you).

## Evidence

### 1. Two real Sepolia deposits → two different VKs

Same bridge `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`, same circuit, same config:

| Deposit | tx | block | tx_index | log_index | receipt RLP | **MPT nodes** | header | `num_advice` | **VkBlob SHA-256** |
|---|---|---|---|---|---|---|---|---|---|
| depositId=0 | `0x9ac341…2adf` | 11025192 | 137 | 2 | 839 B | **5** | 569 B | `[11,10]` | `26f29b3d…035d5311` |
| depositId=1 | `0x7f2b37…112c` | 11115635 | 49 | 2 | 839 B | **3** | 574 B | `[11,10]` | `68b927d0…97b9f9a1` |

Both VkBlobs are 3725 B with a **byte-identical `EthCircuitParams` JSON**; only the
serialized `VerifyingKey` bytes differ. Keygen is deterministic (re-running each
input reproduces its hash bit-for-bit).

### 2. Isolation — node count is the *only* driver

Starting from depositId=1 (`68b927d0`, 3 nodes), mutate **one field at a time** and
re-run keygen (`export_vk_blob`; keygen ignores value-consistency, only shape matters):

| Mutation | Resulting VK | Verdict |
|---|---|---|
| `tx_index` 49 → 137 (RLP key path_len 1 → 2) | `68b927d0…` (unchanged) | **no effect** |
| `block_header_rlp` → depositId=0's (569 B) | `68b927d0…` (unchanged) | **no effect** |
| MPT node count 3 → 5 (the real depositId=0) | `26f29b3d…` | **flips the VK** |

### 3. The synthetic fixtures corroborate it

The deployed `147efe14` VK was keygen'd from 10 **synthetic** fixtures
(`deposit-prover/fixtures/deposit_10proofs/`). All ten share:
`tx_index=0, log_index=0, **nodes=1**, receipt ≈ 523 B` — i.e. identical structure
→ **one** VK covers all ten (the `tvm_vm` opcode test verifies all 10 against
`147efe14`, and we just finalized all 10 live on shellnet, see below). They differ
from real USDC deposits, which have **3 logs (log_index=2), 839 B receipts, and
≥3 MPT nodes**, hence shape `[11,10]` and a depth-dependent VK.

## Where we believe the fix lives

`axiom-eth/src/mpt/mod.rs` (`@1d61be0`):
- `parse_mpt_inclusion_phase0/1` correctly pad `proof.nodes` to `max_depth-1` and
  iterate `for idx in 0..max_depth` — **constraint count is constant**.
- But `let depth = proof.len()` (`mpt/types/input.rs:82`) captures the *real* depth,
  and the keccak/`mpt_hash` work for the nodes appears to be issued for the real
  nodes only, not for all `max_depth` slots. That makes the keccak promise-call
  count (and therefore the main circuit's advice fill / `break_points` /
  permutation argument / VK commitment) a function of the actual depth.

**Ask:** make the deposit-shaped circuit's VK independent of the MPT proof depth —
i.e. issue a **constant `max_depth`** number of node-keccak promise calls (hash the
dummy-padded slots too), so every receipt-proof of a given `max_depth` yields the
same `VerifyingKey`. (If there's a config knob or an existing "fixed-depth" mode we
should be setting from the deposit circuit, point us at it and we'll wire it.)

## Context: the rest of the pipe is already green

On live shellnet (`https://shellnet.ackinacki.org`), against the deployed
`USDCBridge` (code-hash `b38e934a…898e154`, embedded VK `147efe14`), all **10/10**
synthetic deposit proofs were finalized end-to-end:

- `finalizeDeposit(proof, publicInputs)` → opcode `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`,
  RLC `circuit_shape=1`) → mint out-message. Every tx `aborted=false, exit_code=0`.
- e.g. proof_00 tx `6794cf7980117e2263b69020d712157d8beafccdc115ae6510e54836877665e4`.

So the opcode, the node build (`tvm-sdk @ full_dex_and_bridge…`), the VkBlob v2 RLC
reader, the contract, and the mint are all confirmed working. **The single
remaining blocker for real deposits is the depth-dependent VK above.**

## Reproduce

```bash
cd deposit-prover
# 1. Fetch the two real deposits (Sepolia RPC in ETH_RPC_URL)
./target/release/examples/fetch_deposit_data --tx-hash 0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf \
  --contract 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 --log-index 2 --output out/dep0.json
./target/release/examples/fetch_deposit_data --tx-hash 0x7f2b376dfdae0af77a626d1d14585d5f517c0e072e8c90f93f455a1a6057112c \
  --contract 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 --log-index 2 --output out/dep1.json

# 2. Keygen each → different VkBlob (same [11,10], same EthCircuitParams JSON)
for t in dep0 dep1; do
  ./target/release/examples/export_vk_blob --input out/$t.json --output out/vk_$t.bin \
    --degree 18 --max-data-byte-len 256 --max-log-num 20
  sha256sum out/vk_$t.bin
done
# dep0 -> 26f29b3d…   dep1 -> 68b927d0…
```

Artifacts attached alongside this doc: `dep0.json`, `dep1.json`, `vk_dep0.bin`,
`vk_dep1.bin` (under `dist/deposit_vk_mpt_depth_repro_2026-06-25/`).
