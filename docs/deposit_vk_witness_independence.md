# Deposit VK witness-independence — root cause + fix (authoritative)

> **⚠ PI count updated by Track-2 chain-binding (2026-07-23).** The
> witness-independence root-cause + fix documented below is still authoritative.
> Only the PI count / VkBlob hash have moved: circuit now exposes **12 PI**
> (adds `chainId` at slot 4, VkBlob `de1dd3ab…7dd8d1`, 5006 B). The 11-PI
> `20cf9018…` blob referenced below is the last pre-Track-2 witness-independent
> VkBlob and is preserved for historical context — the reproducibility argument
> (cap=64, witness-independent, one embedded VK per circuit shape) carries over
> unchanged to the 12-PI blob.

**Date:** 2026-06-25
**Status:** ✅ RESOLVED & validated end-to-end (production prover path).
**Supersedes:** `deposit_vk_mpt_depth_witness_dependence_2026-06-25.md` (MPT-depth
misdiagnosis) and the `b1e5ce0b` "canonical VK" / Cargo.lock-only fix in
`deposit_vk_reproducibility.md`.

## TL;DR

`USDCBridge` embeds **one** `VK_BLOB`. A deposit proof only verifies if it was
produced against the *exact* same verifying key. For months the deposit VK
"moved" between deposits, so a single embedded VK could not verify real deposits.
Two earlier diagnoses (SRS ceremony; MPT proof depth via keccak dedup) were both
**wrong**. The real cause was a single redundant line in the circuit, plus an
unpinned keccak capacity. Both are now fixed **entirely inside `deposit-prover`**
— **no `axiom-eth` fork change is needed**.

With the fix, the deposit VK is **byte-identical** across deposits of different
contract address AND different MPT proof depth:

| Deposits compared | Before fix | After fix |
|---|---|---|
| depositId=0 (5 MPT nodes, bridge `99c3…`) vs depositId=1 (3 nodes, bridge `99c3…`) | `26f29b3d…` ≠ `68b927d0…` | **both verify against one VkBlob `20cf9018…`** |
| 3-node vs 1-node, different bridge addresses | different VKs | **byte-identical `20cf9018…`** |

Canonical VkBlob (cap=64, k=18, 11 PI, v2 RLC): **`20cf9018647357576e50b3a42fbc3bb0970ca4fa6eb62090f66c8d1cbe647a39`**.

## Root cause

### 1. Contract address loaded as an in-circuit CONSTANT (the address leak)

`circuit_v2.rs` (step 12) used to do:

```rust
let expected_address = &self.inputs.event_data.contract_address; // per-deposit value!
let expected_address_bytes = expected_address
    .iter()
    .map(|&b| ctx_gate.load_constant(Fr::from(b as u64)))   // -> the single FIXED column
    .collect();
// ... constrain extracted log address == expected_address_bytes
```

`load_constant` puts values in the circuit's **fixed column**, whose KZG
commitment is part of the verifying key. The contract address is per-deposit
data, so the fixed-column commitment — and therefore the VK — depended on the
contract address. A proof for bridge A could not verify against a VK keygen'd
from a deposit to bridge B.

This was masked for a long time because the fixture used to "prove the VK was
fine" (`depositId=0` under `proof_01/`) turned out to be an **Anvil** deposit
(sender `0xf39Fd6…`, contract `0x9fe4…`) to a *different* contract — so the VK
genuinely differed, but for the address reason, not depth.

**Fix:** the binding never needed a constant. The contract address is already a
**public input** (`#3`): Phase 0 loads it as a *witness* into
`assigned_instances[0][3]`, and Phase 1 `constrain_equal`s the RLP-extracted log
address to that public instance. The AN-side `TokenBridge.finalizeDeposit` checks
that public input against the bridge's configured deposit-source address. So the
step-12 constant was **redundant**; deleting it removes the leak while keeping the
full binding. VK becomes address-independent. (Removing it is safe — a deposit to
the wrong contract still fails the on-chain public-input check.)

### 2. Keccak promise-loader capacity left dynamic (the depth leak)

Even with the same address, the keccak coprocessor capacity was sized to the
deposit's **used** capacity (which scales with receipt/MPT keccak work), so a
deeper-proof deposit got a slightly larger circuit. Concretely, without the pin:

| Deposit | `num_advice_per_phase` |
|---|---|
| 3 MPT nodes | `[11, 10]` |
| 1 MPT node  | `[10, 10]` |

Different column counts → different VK.

**Fix:** pin the keccak promise-loader capacity to a fixed worst case,
`PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY)`, in **every**
build of the circuit (keygen, prover, VK export). With the pin, `num_advice` is
stable (`[12, 10]`) regardless of depth.

`FIXED_KECCAK_CAPACITY = 64` — chosen worst case:
- measured `used_capacity`: 1-node deposit = 11, 3-node = 21 (~5 / real node);
- realistic Sepolia receipt-proof depths (≤ ~6 nodes) → ≤ ~40, so 64 is ~1.5–3×
  headroom;
- the circuit's hard cap is `RECEIPT_PF_MAX_DEPTH = 10`; an all-max-size 10-node
  proof is ~55–60, still under 64 (thin but positive). If a deposit ever exceeds
  the pinned capacity, proof generation **fails safe** (errors at prove time —
  never produces an unsound proof). If that happens, raise the constant and
  re-keygen + redeploy the VK.

## What is NOT the cause (corrected)

- **SRS ceremony / `b1e5ce0b`.** `deposit_vk_reproducibility.md` proposed
  `b1e5ce0b…` as the "reproducible canonical VK". That VK was keyed on the Hermez
  SRS and was rejected by the opcode; the SRS-prioritisation fix in `prover.rs`
  (chain ceremony first) is correct and retained, but `b1e5ce0b` is **not** the
  production VK. Tracking `Cargo.lock` (also done) is good hygiene but did not fix
  witness-dependence.
- **MPT proof depth / keccak request dedup.** `deposit_vk_mpt_depth_…md` claimed
  the VK scaled with MPT depth and asked the `axiom-eth` fork owner to issue a
  constant number of node-keccak calls. Proven false: with the address constant
  removed and capacity pinned, deposits of depth 1, 3 and 5 produce a
  byte-identical VK. The `axiom-eth` fork is **pristine / upstream** — the
  attempted distinct-padding change was reverted and is unnecessary.

## The fix (files)

All in `deposit-prover` (against **upstream** `gosh-sh/axiom-eth`, no `[patch]`):

- `src/circuit_v2.rs` — delete the step-12 `load_constant(contract_address)` +
  byte-equality loop (binding preserved via public input #3).
- `src/prover.rs` — `pub const FIXED_KECCAK_CAPACITY: usize = 64;`;
  `get_or_create_proving_key` + `generate_proof` build the circuit with
  `EthCircuitImpl::new_impl(.., PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY))`
  and `mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY))`.
- `examples/export_vk_blob.rs`, `examples/export_deposit_proof_set.rs`,
  `examples/export_blake2b_proof.rs` — same pinned construction (so the exported
  VK and the proofs all agree).
- `Cargo.toml` — no `[patch]` for `axiom-eth` (uses the upstream git pin).

## Validation (reproducible)

```bash
cd deposit-prover
# A) Same bridge, different MPT depth (5 vs 3 nodes) — the exact counterexample
#    from the superseded MPT-depth doc — keygen once, prove both:
./target/release/examples/export_deposit_proof_set --set-dir <set> --degree 18
#  proof_00 (5n) VERIFIED + proof_01 (3n) VERIFIED against one VkBlob 20cf9018…

# B) Full production path: VK from deposit A, proof from deposit B (different
#    address AND depth) via the library prover, cross-verified through the
#    opcode handler path:
./target/release/examples/export_vk_blob       --input A.json --output vk.bin  --degree 18
./target/release/examples/export_blake2b_proof --input B.json --proof-out p.bin --pubin-out pi.bin --degree 18
./target/release/examples/verify_opcode_triple --vk-blob vk.bin --proof p.bin --pubin pi.bin --degree 18
#  -> PASS — AN opcode will accept (vk_blob, public_inputs, proof)
```

`circuit_v2` unit tests pass; clippy clean on the changed file.

## Production VK + redeploy

The production VkBlob to embed in `USDCBridge.sol` is the **`20cf9018…`** family
(cap=64, k=18, 11 PI, v2 RLC), reproducible from any real deposit input via
`export_vk_blob`. Redeploy steps unchanged from
`shellnet_usdcbridge_deposit_vk_redeploy.md` §5–§6, but embed `20cf9018…` (NOT the
deployed `147efe14…`, which is the under-provisioned synthetic-fixture VK that
cannot verify real deposits, and NOT `b1e5ce0b…`). After redeploy, the
already-mined Sepolia `depositId=1` can be proven and finalised live.
