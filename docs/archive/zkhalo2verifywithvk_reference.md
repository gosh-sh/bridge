> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# `ZKHALO2VERIFYWITHVK` — TVM opcode reference

**Status**: LANDED on `tvm-sdk` `main` (PR [#243](https://github.com/tvmlabs/tvm-sdk/pull/243)). Subsequent commits on the same PR (2026-05-25) refactored the ABI to **3 separate stack operands** (Variant A).
**Dispatch byte**: `0xC7 0x4A`
**Mnemonic**: `ZKHALO2VERIFYWITHVK`
**Feature gate**: `--features gosh` (same as `ZKHALO2VERIFY`, `VERGRTH16`, `POSEIDON`, `CHKHISTPROOF`)

This document is the **integration reference** for anyone wiring `ZKHALO2VERIFYWITHVK` into an AN-side contract (typically `TokenBridge.finalizeDeposit(...)`) or auditing the opcode. The companion memo `tvm-sdk/docs/zkhalo2verifywithvk_design.md` covers *why* the opcode exists; this one covers *how to call it correctly*.

---

## 1. What the opcode does

Verify a Halo2 SHPLONK proof for a **caller-supplied verifying key** against a **caller-supplied vector of public inputs**, on the BN254 curve.

**One-line summary**: pop three cells (`proof`, `public_inputs`, `vk`), verify, push a boolean.

---

## 2. Stack ABI (Variant A — three operands)

```
ZKHALO2VERIFYWITHVK:
  Pops (top → bottom):
    proof_cell           : Cell  — raw SHPLONK proof bytes (no header)
    public_inputs_cell   : Cell  — raw N × 32 LE Fr (no header)
    vk_cell              : Cell  — VkBlob payload (magic-tagged, versioned)

  Pushes:
    accept : Boolean    — true  if the SHPLONK verifier accepts the proof
                          false if the proof is well-formed but invalid

  Throws FatalError on structural errors only:
    - vk_cell payload doesn't parse as VkBlob:
        * magic mismatch (not b"VKBLOB\x00\x00")
        * version mismatch (not 0x01)
        * unknown transcript_kind byte (only 0 = Blake2b accepted)
        * chunk length runs past the end of the payload
        * payload over 1 MiB (DoS guard)
        * config_json fails to deserialise as BaseCircuitParams
        * trailing garbage after vk_bytes
    - vk_bytes fail to deserialise as VerifyingKey<G1Affine>
      (this includes the curve-membership check; soundness-critical)
    - public_inputs_cell payload length is not a multiple of 32
    - public_inputs_cell payload exceeds 256 KiB
    - a 32-byte chunk in public_inputs is >= Fr modulus
    - proof_cell payload exceeds 1 MiB
    - VkBlob.config.k disagrees with vk.get_domain().k() (defence-in-depth
      against a malicious header that lies about k)
    - BaseCircuitBuilder::new(config) panics on internally-inconsistent
      params (e.g. lookup_bits >= k) — caught via std::panic::catch_unwind
      and converted to a structural FatalError
```

**Cryptographic rejection vs structural rejection.** A *well-formed* proof that just doesn't satisfy the relation is reported as `Boolean::false` on the stack — never as an exception. Only structural problems with the **VkBlob / public_inputs / config** raise `FatalError`. A malformed `proof_cell` payload that fails SHPLONK deserialisation **also** collapses into `Boolean::false` via the handler's `.is_ok()` mapping over `verify_proof`'s `Result` — matching the convention of the existing `VERGRTH16` opcode.

### Why three operands, not one bundle

| Operand               | Why a separate cell                                                  |
|-----------------------|----------------------------------------------------------------------|
| `vk_cell`             | Long-lived. A `TokenBridge` contract stores it once in `c4` / persistent storage at deploy time and re-uses it on every call. Magic + version + transcript_kind framing live here so a drifted producer / consumer is rejected loudly. |
| `public_inputs_cell`  | Computed per-call from the call arguments. Headerless so a contract can build it O(1) on the hot path (`BUILDER` / `STZEROES` / `STIR` / `ENDC`). |
| `proof_cell`          | Comes straight from off-chain prover output. Headerless so it can be stored / forwarded verbatim through messages without re-encoding. |

The previous design memo sketched a **single self-describing bundle** with all four chunks (config, vk, instances, proof) glued together; the live ABI splits that into three cells because (a) the VK + config are deploy-time constants that contracts shouldn't have to re-wrap on every call, and (b) per-call public_inputs are O(1) to construct without a length prefix.

---

## 3. `VkBlob` wire format (`vk_cell` payload)

### 3.1 Byte layout

```text
  off  size  field
  ───  ────  ─────────────────────────────────────────────────────────
    0     8  magic           = b"VKBLOB\x00\x00"  (ASCII + 2× NUL pad)
    8     1  version         = 0x01
    9     1  transcript_kind = 0x00          (Blake2b; 0x01 reserved for Keccak)
   10     6  reserved        = 0 × 6         (must be zero, ignored on read)
   16     4  config_len   (u32 LE)
   20  cl    config_json  (UTF-8 serde_json of `BaseCircuitParams`)
  ...     4  vk_len       (u32 LE)
  ...  vl    vk_bytes     (`VerifyingKey::write(SerdeFormat::RawBytes)`)
```

All length prefixes are `u32` little-endian (blobs are always under 4 GiB; the consumer caps at 1 MiB).

### 3.2 Header fields in detail

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 8 | `magic` | `b"VKBLOB\x00\x00"` (`0x56 0x4B 0x42 0x4C 0x4F 0x42 0x00 0x00`). Distinguishes the blob from any other TVM cell payload. |
| 8 | 1 | `version` | `0x01` today. Bump on any breaking change to the byte layout. The consumer rejects every other value with `FatalError`. |
| 9 | 1 | `transcript_kind` | `0x00 = Blake2b SHPLONK` (the only flavour the AN-side handler speaks today — `verify_proof::<KZGCommitmentScheme<Bn256>, VerifierSHPLONK, Challenge255, Blake2bRead, SingleStrategy>`). `0x01 = Keccak` is **reserved**. |
| 10..15 | 6 | `reserved` | Must be zero on write. The consumer does not enforce zero on read (future format extensions may repurpose these bytes). |

### 3.3 Payload chunks

Each chunk is a `u32` little-endian length followed by exactly that many bytes. Order is fixed: **config_json, vk_bytes**.

| Chunk | Producer-side serialisation | Consumer-side deserialisation |
|---|---|---|
| `config_json` | `serde_json::to_vec(&BaseCircuitParams)` | `serde_json::from_slice::<BaseCircuitParams>(..)` |
| `vk_bytes` | `vk.write(&mut buf, SerdeFormat::RawBytes)` | `VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(.., SerdeFormat::RawBytes, config)` |

### 3.4 Sizing reference (real bridge Circuit 1B fallback fixture)

| Chunk | Size | Notes |
|---|---|---|
| `config_json` | ~140 B | `BaseCircuitParams` serialised by `serde_json` (compact form). |
| `vk_bytes` | ~6 KiB | `K=20` fallback BLS attestation verifier, 44 advice + 1 fixed + 4 lookup columns. |
| **Header + length prefixes** | 24 B | 8 + 1 + 1 + 6 + 2 × 4 |
| **Total `vk_cell` payload** | **6 308 B** | Well under the 1 MiB cap. |

### 3.5 Why `SerdeFormat::RawBytes` and not `RawBytesUnchecked`

`SerdeFormat::RawBytesUnchecked` skips curve-membership checks on every group element of the verifying key. This is fine when the VK comes from your own trusted prover. It is **not** fine when the VK is supplied by the caller of an on-chain opcode: a maliciously-crafted off-curve point in the VK lets the attacker produce verifications that don't actually correspond to any valid proof.

The opcode handler uses `SerdeFormat::RawBytes`, which runs the on-curve check on every group element. The producer side mirrors this byte layout (see `crates/bridge-snark-utils/src/halo2_tvm_bundle.rs::VkBlob::from_native`) so producer and consumer agree.

This costs a few hundred milliseconds per first-time VK deserialisation. The per-VK cache (§5) amortises that across calls.

---

## 4. `public_inputs_cell` wire format

**No header.** Just a contiguous sequence of `N × 32` little-endian `Fr::to_repr()` values. The number of public inputs `N` is implied by `payload_len / 32` and is cross-checked against the VK on every verify.

### 4.1 How to assemble `public_inputs_cell`

Given `N` public inputs as TVM `int`s (each fitting in a 256-bit Fr):

1. Encode each `int` as a **32-byte little-endian** byte string and concatenate.
2. Build the resulting `N × 32`-byte buffer into a single `Cell` (`NEWC` + repeated `STSLICE` / `STZEROES` / `STIR` + `ENDC`).

For instances that fit in `u64` (common for amounts, block heights, addresses), the bottom 8 bytes are the integer encoded LE, padded with 24 bytes of zeros to reach 32 bytes. For 32-byte digests (nullifiers, commitments, hashes), the field element is exactly the digest interpreted as little-endian `Fr`.

**Worked example (3 inputs):**

```text
                   ┌──────────────────────────────────────────────┐
   instances[0]    │ block_seqno = 7  →  0x07 00..00 (32 B LE)    │
   instances[1]    │ nullifier   = 0xdead..beef (32 B LE)          │
   instances[2]    │ commitment  = 0xbabe..cafe (32 B LE)          │
                   └──────────────────────────────────────────────┘
   public_inputs_cell payload = bytes[0] ‖ bytes[1] ‖ bytes[2]
                              (96 B total, no header, no separator)
```

Out-of-range field elements (e.g. flipping a high bit so the chunk exceeds the BN254 scalar modulus) are **always** structural errors (`FatalError`), not silent verify-fails — this catches caller bugs early instead of letting them masquerade as cryptographic rejects.

### 4.2 Public-input ordering

The ordering inside `public_inputs_cell` must exactly match the circuit's instance vector. For bridge Circuit 1B (fallback BLS attestation verifier):

```
instances[0] = bk_set_poseidon_commit    (Fr — Poseidon hash of BK set)
instances[1] = envelope_hash_high        (Fr — high 16 bytes of the envelope SHA-256)
instances[2] = envelope_hash_low         (Fr — low 16 bytes of the envelope SHA-256)
instances[3] = last_seen_block_seqno     (Fr — u32 zero-padded to 32 bytes LE)
```

Contracts that re-order the inputs will produce a public_inputs cell that decodes cleanly but fails verification with `Ok(false)` — exactly the same way a wrong witness would.

---

## 5. `proof_cell` wire format

**No header.** Just the SHPLONK proof bytes emitted by `Blake2bWrite::<_, _, Challenge255<_>>::finalize()` on the producer side. The handler hands these bytes directly to `Blake2bRead::init` and runs `halo2_proofs::plonk::verify_proof`.

Hard cap: 1 MiB. SHPLONK proofs for K≤22 circuits are well under 100 KiB in practice; the cap is purely a DoS guard against a hostile contract.

---

## 6. KZG SRS handling

The verifier needs `ParamsKZG<Bn256>`. For SHPLONK verification (not proving) only three points from the universal SRS are actually used:

| Point | Symbol | Role |
|---|---|---|
| First G1 point | `g[0]` | Anchor for opening proof commitments. |
| G2 generator | `g2` | Pairing base. |
| `[s]_2` (G2) | `s_g2` | Trapdoor-shifted G2 generator. |

These three points are **embedded in `tvm_vm` as constants** (`KZG_G0_BYTES`, `KZG_G2_BYTES`, `KZG_S_G2_BYTES` in `tvm_vm/src/executor/zk_halo2_utils.rs`) and `build_shared_kzg_params(k)` (in `zk_halo2_with_vk.rs`) reconstructs a `ParamsKZG<Bn256>` for any `k` at runtime via `from_parts`. **No on-disk SRS file is required** for verification.

---

## 7. Per-VK cache

The handler keeps a bounded-FIFO `HashMap<Vec<u8>, Arc<CachedVk>>` keyed by `VkBlob.vk_bytes`, capacity 8. A `CachedVk` holds the deserialised `VerifyingKey<G1Affine>` and the reconstructed `ParamsKZG<Bn256>` for the VK's `k = vk.cs.degree`.

- **Hit**: ~0.5–1 ms warm-path.
- **Miss**: ~1–3 s on x86-64 at `k=20`, dominated by `EvaluationDomain` precomputation in `VerifyingKey::read` and the on-curve checks.
- **Eviction**: oldest entry by insertion order (FIFO, no LRU-on-access update).

`Arc<CachedVk>` is used so concurrent calls with the same VK don't hold separate copies of the heavy `ParamsKZG`.

---

## 8. Gas model

Currently a placeholder: `ZKHALO2_VERIFY_WITH_VK_GAS_PRICE = 5_000`. Treat this as a structural guess; it **must be re-benchmarked before mainnet**. See `tvm-sdk/docs/zkhalo2verifywithvk_design.md` §8 for the open Q-GAS-1 checklist.

---

## 9. Producer side (Rust, in this repository)

The bridge ships a producer that emits exactly the bytes `ZKHALO2VERIFYWITHVK` consumes:

| Path | Role |
|---|---|
| `crates/bridge-snark-utils/src/halo2_tvm_bundle.rs` | `VkBlob` (encoder + decoder) + `Halo2TvmOperands` (full three-operand bundle for round-trip testing) + `encode_instances` / `decode_instances`. |
| `crates/bridge-snark-utils/tests/halo2_tvm_bundle_round_trip.rs` | Round-trip test: prove with `FallbackKeyManager`, serialise three operand byte streams, deserialise each, verify against the reconstructed `(vk, instances, proof)` triple. Green; this is the ground truth for the format. |

### 9.1 Reference encoder snippet

```rust
use bridge_snark_utils::{Halo2TvmOperands, VkBlob};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Fr, G1Affine},
    plonk::VerifyingKey,
};

fn build_operand_bytes(
    config: &BaseCircuitParams,
    vk: &VerifyingKey<G1Affine>,
    instances: &[Fr],
    proof_bytes: Vec<u8>,
) -> anyhow::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let operands = Halo2TvmOperands::from_native(config, vk, instances, proof_bytes)?;
    Ok((operands.vk_blob, operands.public_inputs, operands.proof))
}

// vk_blob is deploy-time (write to c4); public_inputs and proof are per-call.
```

To build *just* the `VkBlob` (the deploy-time artifact):

```rust
let blob = VkBlob::from_native(&config, &vk)?;
let vk_blob_bytes = blob.to_bytes()?;
```

### 9.2 Strictness checks on the producer side

| Check | Producer behaviour | Consumer behaviour |
|---|---|---|
| `public_inputs.len() % 32 == 0` | `encode_instances` always emits `Fr::to_repr()` (32 bytes each) | `FatalError` if not a multiple of 32 |
| `Fr::from_repr(chunk).is_some()` | `Fr::to_repr` is canonical and always in-range | `FatalError` if any 32-byte chunk decodes ≥ modulus |
| VK on-curve | `SerdeFormat::RawBytes` ⇒ canonical compressed form | `SerdeFormat::RawBytes` ⇒ runs `is_on_curve` on every group element |
| Proof structural shape | Producer uses `Blake2bWrite` | Consumer uses `Blake2bRead::init`; mismatched transcript ⇒ verification returns `false`, no exception |

### 9.3 Real bridge Circuit 1B fixture

The round-trip test, when run with `EXPORT_HALO2_FIXTURE_DIR=…`, dumps the three operand byte streams from a real fallback BLS attestation proof. These files are checked in as:

- `tvm-sdk/tvm_vm/halo2_test_data/fallback_vk_blob.bin` — `VkBlob` payload (6 308 B)
- `tvm-sdk/tvm_vm/halo2_test_data/fallback_public_inputs.bin` — 4 × 32 B = 128 B
- `tvm-sdk/tvm_vm/halo2_test_data/fallback_proof.bin` — SHPLONK proof (14 784 B)

The opcode integration test `bridge_circuit_1b_fallback_real_proof_verifies` (`tvm_vm/src/tests/test_halo2_with_vk.rs`) consumes these files and asserts the handler returns `Ok(true)`.

---

## 10. Test surface (consumer side, `tvm-sdk`)

| Test (location: `tvm_vm/src/tests/test_halo2_with_vk.rs`) | What it covers |
|---|---|
| `round_trip_dark_dex_w8_l0_valid_proof_returns_true` | Positive: synthetic DarkDex W=8 L0 → 3 operands → opcode → `true`. |
| `bridge_circuit_1b_fallback_real_proof_verifies` | Positive: real bridge Circuit 1B fallback proof → 3 operands → opcode → `true`. |
| `flipped_proof_byte_rejected_as_false` | Negative: flip a byte in `proof_cell` → opcode returns `Ok(false)`. |
| `tweaked_instance_fr_rejected_as_false` | Negative: flip the low bit of the first public-input `Fr` (stays in-modulus) → opcode returns `Ok(false)`. |
| `bad_vk_blob_magic_returns_fatal_error` | Negative: clobber the 8-byte VkBlob magic → opcode raises `FatalError`. |
| `corrupt_vk_byte_never_verifies_as_true` | Negative: flip a byte inside the VK G1 region → either `FatalError` or `Ok(false)` — never `Ok(true)`. |
| `instance_ge_modulus_returns_fatal_error` | Negative: set first 32 public-input bytes to `0xFF…FF` (> Fr modulus) → `FatalError`. |
| `instance_count_mismatch_rejected_as_false` | Negative: drop the last 32-byte chunk from valid public_inputs → `Ok(false)`. |
| `empty_proof_rejected_as_false` | Negative: zero-length `proof_cell` payload → `Ok(false)`. |
| `malformed_config_json_returns_fatal_error` | Negative: replace `config_json` with `b"this is not json"` → `FatalError`. |
| `config_k_mismatch_returns_fatal_error` | Negative: mutate `"k":19` → `"k":18` in JSON; either `config.k != vk.domain().k()` guard fires, or `BaseCircuitBuilder::new` panics on `lookup_bits=18` against `k=18` and our `catch_unwind` wrapper surfaces it as `FatalError`. |
| `fifo_cache_reused_across_two_invocations` | Smoke: same VK across two calls, second is fast (warm cache). |

Total: 12 integration tests + 9 unit tests in `zk_halo2_with_vk_bundle::tests`. All green.

---

## 11. Call site sketch (Solidity-on-TVM, pseudocode)

```solidity
// inside the AN-side TokenBridge contract
contract TokenBridge {
    TvmCell public vkBlob;             // deploy-time VkBlob (immutable per circuit)
    mapping(uint256 => bool) public usedDepositIds;

    function finalizeDeposit(
        uint256 depositId,
        uint256 sender,
        uint256 amount,
        uint256 contractAddress,
        uint256 anWorkchain,        // AN destination workchain (proven, full 32-byte word)
        uint256 anAccountHigh,      // high 16 bytes of the 256-bit AN account
        uint256 anAccountLow,       // low  16 bytes of the 256-bit AN account
        uint256 blockHashHigh,
        uint256 blockHashLow,
        uint256 promiseCommit,
        TvmCell proofCell
    ) external {
        require(!usedDepositIds[depositId], "deposit already finalized");

        // Build public_inputs_cell on the fly from the call arguments.
        // Order MUST match the circuit's instance column (10 inputs):
        //   [depositId, sender, amount, contractAddress,
        //    anWorkchain, anAccountHigh, anAccountLow,
        //    blockHashHigh, blockHashLow, promiseCommit]
        // each packed as a 32-byte little-endian Fr.
        TvmBuilder pi;
        pi.storeUint(depositIdFieldLE, 256);
        pi.storeUint(senderFieldLE, 256);
        pi.storeUint(amountFieldLE, 256);
        pi.storeUint(contractAddressFieldLE, 256);
        pi.storeUint(anWorkchainFieldLE, 256);
        pi.storeUint(anAccountHighFieldLE, 256);
        pi.storeUint(anAccountLowFieldLE, 256);
        pi.storeUint(blockHashHighFieldLE, 256);
        pi.storeUint(blockHashLowFieldLE, 256);
        pi.storeUint(promiseCommitFieldLE, 256);
        TvmCell publicInputsCell = pi.toCell();

        require(
            gosh.zkHalo2VerifyWithVK(vkBlob, publicInputsCell, proofCell),
            "halo2 verification failed"
        );

        usedDepositIds[depositId] = true;

        // The recipient is a PROVEN public input — reconstruct it from the two
        // 16-byte halves and credit that AN account. An EVM address is not a
        // valid AN recipient, so the destination is bound in the proof rather
        // than trusted from a relayer hint.
        uint256 anAccount = (anAccountHigh << 128) | anAccountLow;
        // ... credit (int8(anWorkchain) : anAccount) with `amount`, emit event ...
    }
}
```

The contract is responsible for assembling `public_inputs_cell` so that its content matches the values it intends to act on, **in the circuit's instance order**. The opcode itself only checks that the proof is valid for *whatever* public inputs the cell carries — and it reads the input *count* from the VkBlob, so the 7→10 expansion (adding the AN recipient) needs no opcode change, only this call site and the VkBlob regen.

---

## 12. Failure-mode cheat sheet

| Symptom (caller view) | Likely cause | Where to look |
|---|---|---|
| `FatalError` immediately, before any verification work | `vk_cell` bytes don't start with `b"VKBLOB\x00\x00"`, or `version != 0x01`, or `transcript_kind != 0x00`, or a chunk length runs past the end of the VkBlob | Producer-side encoding bug; check `VkBlob::to_bytes()` succeeded |
| `FatalError` with "public_inputs_cell length .. is not a multiple of 32" | Contract concatenated public inputs with the wrong stride (e.g. u64 shortcut, or non-32-byte Fr encoding) | Use `encode_instances(&[Fr])` from `bridge_snark_utils` |
| `FatalError` with "Fr::from_repr rejected" | A public input is ≥ Fr modulus | Reduce the value mod p on the contract side; use canonical `Fr::from(value).to_repr()` |
| `FatalError` deserialising VK | VK bytes were written with the wrong `SerdeFormat`, or VK was for a different curve, or an off-curve point was injected | Re-emit VK via `vk.write(&mut buf, SerdeFormat::RawBytes)` |
| `false` on the stack (no exception) | The proof is well-formed but doesn't satisfy the relation. Either the prover was given the wrong witness, or the public inputs in the cell don't match what the proof was generated against, or VK ↔ proof drift between deploys | Re-run `Halo2TvmOperands::verify` locally — if it returns `false` there too, it's a prover-side issue, not an opcode issue |
| `false` on the stack but local `Halo2TvmOperands::verify` returns `true` | `ParamsKZG` mismatch — producer used different SRS bytes than the consumer reconstructs | Verify the producer uses the same KZG points as `tvm_vm/src/executor/zk_halo2_utils.rs::KZG_*_BYTES` |

---

## 13. Cross-references

### `tvm-sdk` side (consumer)
- `tvm_vm/src/executor/zk_halo2_with_vk.rs` — opcode handler (Base + Rlc VK-read branches; `rlc_branch_tests`).
- `tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs` — bundle/`VkBlob` decoder + `circuit_shape` v2 parsing + size validators + unit tests.
- `Cargo.toml` (workspace root) — `[patch]` block unifying the halo2 backend + `axiom-eth` fork pin (§15.3).
- `tvm_vm/src/executor/zk_halo2_utils.rs` — embedded KZG points + `build_shared_kzg_params(k)`.
- `tvm_vm/src/tests/test_halo2_with_vk.rs` — integration tests.
- `tvm_assembler/src/{simple,lib}.rs` — mnemonic + assembler round-trip test.
- `tvm_vm/src/executor/engine/handlers.rs` — dispatch entry at `0xC7 0x4A`.
- `tvm_vm/src/executor/gas/gas_state.rs` — `Gas::zkhalo2_verify_with_vk_price`.
- `docs/zkhalo2verifywithvk_design.md` — frozen design memo.
- `docs/vm-instructions/acki-nacki-vm-instructions.md` — canonical VM-instructions doc.

### Bridge side (producer)
- `crates/bridge-snark-utils/src/halo2_tvm_bundle.rs` — producer-side `VkBlob` + `Halo2TvmOperands` types.
- `crates/bridge-snark-utils/tests/halo2_tvm_bundle_round_trip.rs` — full round-trip ground-truth + fixture export.

### Pull requests
- [`tvm-sdk` PR #243 — ZKHALO2VERIFYWITHVK on main](https://github.com/tvmlabs/tvm-sdk/pull/243) — landed and refactored to 3-operand ABI (Variant A).

---

## 14. Things explicitly out of scope for this opcode

- **Multi-VK proof aggregation.** One opcode call ⇒ one (VK, proof, instances) triple. Aggregation (recursion / IVC) lives at a higher layer.
- **Cross-curve proofs.** BN254 only. BLS12-381 verification is `ZKHALO2VERIFY`'s territory.
- **Producer-side proving from inside TVM.** Proving is many orders of magnitude more expensive than verification and is never run on-chain.
- **VK rotation policy.** When a contract decides to accept proofs against a new VK is application-level, not VM-level. The opcode just verifies whatever operands it's given.
- **Replay protection.** The opcode doesn't track which proofs it has accepted before. Contracts must maintain their own nullifier / `usedDepositIds` map.

---

## 15. VkBlob v2 — `circuit_shape` byte + RLC / `EthCircuitImpl` support (deposit circuits)

**Status**: implemented 2026-05-29 on `tvm-sdk` branch `serhii/node-3406-vergrth16-with-vk` (single-bundle `HALO2TVM` opcode variant) and on the bridge producer (`VkBlob` `VKBLOB` variant). Built + tested on a **nightly** toolchain (see §15.3).

### 15.1 Why

Everything described above (Variant A / §3) assumes the VK was produced by a `BaseCircuitBuilder<Fr>` circuit (the DarkDex / Circuit-1B family). The **deposit-prover** is an `axiom-eth` `EthCircuitImpl<Fr, DepositEventCircuitV2>` circuit — a multi-phase **RLC + keccak-coprocessor** circuit whose constraint system is rebuilt from `EthCircuitParams`, *not* `BaseCircuitParams`. A `BaseCircuitBuilder`-only `VerifyingKey::read` cannot reconstruct that VK with the right column layout, so `TokenBridge.finalizeDeposit` could never verify a real deposit proof. (Empirically confirmed: a `BaseCircuitBuilder` read of an RLC VK either errors on EOF or silently reads a prefix that round-trips to *different* bytes — see the `base_branch_reads_fewer_points_than_rlc_vk` consumer test.)

v2 adds a one-byte **`circuit_shape`** discriminator so a single opcode covers both circuit families.

### 15.2 Wire change (both format variants)

A `circuit_shape` byte is carried at **offset 10** (the first byte of the old `reserved` region), and the layout version bumps to `0x02` when the byte is non-zero:

| `circuit_shape` | `config_json` is… | VK rebuilt with… | producer |
|---|---|---|---|
| `0x00` **Base** | `BaseCircuitParams` | `BaseCircuitBuilder<Fr>` | identical to v1 |
| `0x01` **Rlc** | `EthCircuitParams` (axiom-eth) | `EthCircuitImpl<Fr, Noop>` | deposit-prover class |

- **Backward compatible**: a v1 blob (`version = 0x01`) pins `circuit_shape = 0` and is read exactly as before. A v1 blob carrying a non-zero shape byte is rejected (a shape-tagged blob must set `version = 0x02`).
- The `Rlc` `EthCircuitParams` JSON is the value returned by `EthCircuitImpl::calculate_params()` at keygen, serialised with `serde_json`.
- `EthCircuitImpl<Fr, Noop>` uses an **empty** `EthCircuitInstructions` body: `VerifyingKey::read` rebuilds the constraint system purely from `EthCircuitParams` via `Circuit::configure_with_params`, so the instructions are never invoked on the read path. The same `Noop` type therefore covers *every* RLC/keccak circuit (deposit, future bridge circuits) — only the params differ.
- This mirrors the producer-side `VK_BLOB_VERSION_V2` / `CircuitShape { Base = 0, Rlc = 1 }` in `crates/bridge-snark-utils/src/halo2_tvm_bundle.rs`.

> **Format-variant note.** This `tvm-sdk` branch ships the original **single self-describing `HALO2TVM` bundle** opcode (`config + vk + instances + proof` in one cell — see `zk_halo2_with_vk_bundle.rs`), whereas §2/§3 above and `main`'s PR #243 describe the **3-operand Variant A** (`VKBLOB` `vk_cell` + `public_inputs_cell` + `proof_cell`). The `circuit_shape` byte and the RLC read path are defined **identically** for both; whichever variant the team settles on, the deposit shape support is the same. Reconciling the two opcode ABIs is tracked separately (out of scope for this change).

### 15.3 Build requirements (important)

The RLC path pulls `axiom-eth` (and transitively `snark-verifier-sdk`) into `tvm_vm`. This has two consequences the node build must accommodate:

1. **Nightly toolchain for the `gosh` feature.** `snark-verifier-sdk` v0.1.7-git uses the unstable `trait_alias` feature (`NativeKzgAccumulationScheme`), so the axiom-eth RLC stack only builds on nightly. The stable gosh `BaseCircuitBuilder` path is unaffected — only the new RLC capability forces nightly. This is now the agreed toolchain: `tvm-sdk/rust-toolchain.toml` pins `channel = "nightly"` (verified against rustc `1.98.0-nightly (57d06900f 2026-05-27)`), so `cargo build` / CI select it automatically without an explicit `+nightly`. Pin a specific nightly date for fully reproducible CI.
2. **`[patch]` unification of the halo2 backend.** `halo2-base` reaches the graph through three original git sources (gosh fork via `tvm_vm` + `gosh-zk-snark-halo2-utils`; axiom's `halo2-lib.git` via `axiom-eth` + `snark-verifier-sdk`). They must dedup to **one** package or the `VerifyingKey<G1Affine>` types don't match. The tvm-sdk workspace-root `[patch]` points all three sources (plus `crates.io`) at one git url+rev of the gosh fork (a shared local `path` cannot patch multiple sources — cargo keeps the original for the loser). See the `[patch]` block in `tvm-sdk/Cargo.toml`.

Two supporting fork changes:

- **gosh fork `bump-halo2-lib-v0.4.1`** (`gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381`): bumps the fork from axiom v0.4.0 → v0.4.1 / zkevm-hashes 0.2.1 (the version `axiom-eth v0.4.3` builds against), **kept stable-Rust-compatible** — `VirtualRegionManager::Assignment` keeps NO default (the upstream `= ()` default needs nightly `associated_type_defaults`), since every halo2-base impl already names it explicitly.
- **axiom-eth one-line patch** (branch `gosh-stable-rlcmanager-assignment`): `RlcManager`'s `VirtualRegionManager` impl names `type Assignment = ();` explicitly (it relied on the upstream default that the stable gosh fork drops). Pulled into `tvm_vm` via a `[patch."…/axiom-eth"]` entry.

### 15.4 Consumer-side tests (`tvm-sdk`)

- `zk_halo2_with_vk_bundle.rs::tests` — v2 parse coverage: `parse_v2_rlc_bundle_carries_opaque_eth_params`, `parse_v2_base_bundle_is_base_shape`, `parse_rejects_v1_with_nonzero_shape`, `parse_rejects_unknown_shape`, `parse_rejects_empty_rlc_config` (+ all v1 negatives still green).
- `zk_halo2_with_vk.rs::rlc_branch_tests` — keygens a real `EthCircuitImpl<Fr, KeygenProbe>` VK on the opcode's own backend, then: `rlc_branch_reconstructs_vk` (byte-for-byte round-trip through the `Rlc` reader), `base_branch_reads_fewer_points_than_rlc_vk` (Base reader does NOT faithfully round-trip an RLC VK), `rlc_branch_rejects_malformed_config_json`.
- All existing Base fixture tests (`test_halo2_with_vk.rs`, incl. the real DarkDex W=8 L0 proof) stay green.

> **Still pending (o5b / o6).** A full *valid-RLC-proof* end-to-end fixture (real deposit proof bytes + instances + RLC VkBlob) requires the deposit-prover to export on the gosh halo2-axiom backend and an agreed final opcode ABI; the consumer round-trip above proves the VK-reconstruction branch (the genuinely new logic) without it.
