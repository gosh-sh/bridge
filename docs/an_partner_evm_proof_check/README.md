# EVM Proof Check on the Acki Nacki Side — Specification Bundle

**Owner:** Bridge integration team (Pruvendo)
**Status:** v1, ready for AN node-team implementation
**Linear:** NODE-3406
**Last updated:** 2026-05-14

This bundle is the canonical specification and integration package for verifying
ZK proofs of Ethereum deposit events on the Acki Nacki side. It is the deliverable
that lets the AN node-team add a single TVM opcode (`gosh.zkhalo2verify` or
equivalent) and the `TokenBridge` Solidity contract (PR 2112) plug into it with
no further design back-and-forth.

The full pipeline is already implemented and exercised end-to-end on our side
(`deposit-prover/`, `deposit-prover/gnark-wrapper/`, `Groth16Verifier.sol`,
`Groth16DepositVerifier.sol`); this bundle distils the wire-level format and the
verification algorithm into a self-contained spec that does not require reading
our Rust or Go codebases.

## Contents

| File | Purpose |
|---|---|
| `README.md` | This file: scope, glossary, hand-off checklist. |
| [`01_spec.md`](./01_spec.md) | The **canonical specification**: curve choice, proof byte layout, verification-key byte layout, public-input semantics, full verification algorithm in pseudocode. This is the document the AN node-team should treat as the source of truth. |
| [`02_test_vectors.md`](./02_test_vectors.md) | Concrete byte-level test vectors (one positive, several negatives) plus instructions to regenerate them from our pipeline. Drop directly into TVM opcode unit tests. |
| [`03_integration.md`](./03_integration.md) | How the new TVM opcode plugs into `contracts/exchange/TokenBridge.sol::finalizeDeposit` (PR 2112). Replaces the current `proof; // ignored` stub with a real verify call. |
| [`04_reference_impl.md`](./04_reference_impl.md) | Pointers to our existing Rust/Solidity reference verifier code with line-level cross-references, in case any test vector disagrees with this spec and a tie-break is needed. |
| `fixtures/` | Binary artefacts referenced by `02_test_vectors.md`: verification key, sample proof, public inputs JSON. |

## TL;DR for the AN Node Team

You need to add **one capability** to the TVM:

> Given a `vk_bytes` blob (556 bytes, fixed for the lifetime of the bridge), a
> `proof_bytes` blob (256 bytes, per-deposit), and 7 `uint256` public inputs,
> compute whether this is a valid **Groth16 proof on BN254**. Return a bool.

That's it. Everything else — fetching events, deriving public inputs, anti-replay,
minting — is already on the contract side and works regardless of how this
opcode is implemented.

`01_spec.md` lays out the byte layout and verification math. `02_test_vectors.md`
gives you ground truth to test against. `03_integration.md` shows the contract
binding.

## Glossary

| Term | Meaning |
|---|---|
| **Groth16** | The pairing-based ZK-SNARK system from [Groth, 2016](https://eprint.iacr.org/2016/260). Used here in standard form, no Plonk/SHPLONK on the verifier side. |
| **BN254** | The Barreto-Naehrig pairing-friendly curve at ~128-bit security; the curve Ethereum precompiles `0x06/0x07/0x08` work over. Also called `alt_bn128`. |
| **Fr** | Scalar field of BN254. Order `r = 21888242871839275222246405745257275088548364400416034343698204186575808495617`. Public inputs live in Fr. |
| **Fp** | Base field of BN254. Curve coordinates live in Fp (G1) or Fp² (G2). |
| **G1** | The "small" subgroup; points are `(x, y) ∈ Fp × Fp`. Used for proof points A and C, and for the public-input MSM. Encoded as 64 bytes uncompressed (`x ‖ y`, big-endian). |
| **G2** | The "large" subgroup; points are `(x, y) ∈ Fp² × Fp²`. Used for proof point B and VK constants β, γ, δ. Encoded as 128 bytes (`x₁ ‖ x₀ ‖ y₁ ‖ y₀`, big-endian). |
| **VK** (verification key) | The proof-system-side constants: `α ∈ G1`, `β ∈ G2`, `γ ∈ G2`, `δ ∈ G2`, plus 8 IC points (one per public input + 1) in G1. Fixed at trusted setup time; per-deposit it does **not** change. |
| **gnark** | The Go ZK library from Consensys that produced our Groth16 wrapper. Binary VK/proof layouts in this spec follow gnark's conventions; the reference implementation in `04_reference_impl.md` cites gnark line numbers. |
| **`promise_commit`** | The 7th public input; the Poseidon commitment from the Keccak coprocessor inside the Halo2 deposit circuit. Opaque to the verifier — just one more 32-byte field element. |
| **Halo2** | The proving system used internally by `deposit-prover/`. The AN-side verifier **does not** need to know anything about Halo2: we wrap the Halo2 proof inside Groth16, so the verifier only sees Groth16 bytes. This is intentional — Halo2 verification is ~1.2M gas in Solidity, Groth16 is ~250k. |
| **Keccak coprocessor** | An off-circuit pattern we use inside the Halo2 prover to handle keccak256 cheaply via Poseidon promises. Irrelevant to verification (the commitment is just one of the 7 public inputs); for reference see `docs/keccak_coprocessor_flowchart.mmd`. |

## Hand-off Checklist (AN node-team)

When you start implementing, this is the minimum useful loop:

1. Read `01_spec.md` end to end (≈ 30 min).
2. Take `fixtures/positive_v1/{vk.bin, proof.bin, public_inputs.json}` and write
   the smallest possible unit test in your TVM-runner that calls
   `verify(vk, proof, public_inputs)` and asserts `true`.
3. Take `fixtures/negative_v1_flipped_bit/{proof.bin}` (same VK, same inputs,
   one bit flipped in proof) and assert `false`.
4. Take `fixtures/negative_v1_wrong_input/{public_inputs.json}` (same VK, same
   proof, `depositId` incremented) and assert `false`.
5. Once those three are green, the opcode is functionally correct. Wire it into
   the contract per `03_integration.md` and add the negative tests from §M1–M3
   of `01_spec.md` (input out of field, malformed point, etc.).

## Hand-off Checklist (bridge integration team — us)

What we ship along with this bundle:

- [x] The 4 spec/integration documents.
- [x] One positive fixture (`fixtures/positive_v1/`): real proof generated from
  a real Sepolia deposit event, 7 real public inputs, the VK that was used.
- [x] Two negative fixtures derived from the positive one.
- [x] Pointers to our Rust reference verifier in `deposit-prover/examples/`
  that the AN team can run locally to cross-check ambiguous edge cases.
- [ ] **Open follow-up:** if AN settles on a curve other than BN254 or a proof
  system other than Groth16 (unlikely — see `01_spec.md §0`), this bundle would
  need a refresh. Confirm BN254/Groth16 before they cut the TVM opcode work.

## Compatibility Statement

The Groth16 proof layout in this spec is the **gnark standard** layout (256-byte
uncompressed for G1+G2+G1). It matches the Ethereum precompile `0x08` layout
byte-for-byte for the pairing step. The VK layout is gnark's binary export
format, which matches the canonical Groth16 VK structure published in the
[Groth16 paper](https://eprint.iacr.org/2016/260.pdf) Section 3.1.

Any compliant Groth16 verifier (libsnark, snarkjs, gnark, Rust arkworks,
constantine) will accept the proofs in this spec. The AN-side opcode does not
need to be tied to any specific library — it just needs to implement the
verification equation correctly.
