# `ZKHALO2VERIFYWITHVK` — TVM opcode reference

**Status**: LANDED on `tvm-sdk` 2026-05-22 (PR [#240](https://github.com/tvmlabs/tvm-sdk/pull/240), branch `serhii/verhalo2shplonk-real-impl` → merged into `serhii/node-3406-vergrth16-with-vk`).
**Dispatch byte**: `0xC7 0x4A`
**Mnemonic**: `ZKHALO2VERIFYWITHVK`
**Compiler builtin** (pending in TVM-Solidity-Compiler): `gosh.zkHalo2VerifyWithVK(bundle_cell)`
**Feature gate**: `--features gosh` (same as `ZKHALO2VERIFY`, `VERGRTH16`, `POSEIDON`, `CHKHISTPROOF`)

This document is the **integration reference** for anyone wiring `ZKHALO2VERIFYWITHVK` into an AN-side contract (typically `TokenBridge.finalizeDeposit(...)`) or auditing the opcode. The companion design memo `docs/zk_halo2_an_side_design.md` covers *why* the opcode exists; this one covers *how to call it correctly*.

---

## 1. What the opcode does

Verify a Halo2 SHPLONK proof for a **caller-supplied verifying key** against a **caller-supplied vector of public inputs**, on the BN254 curve. It is the per-VK generalisation of Serhii's `ZKHALO2VERIFY` (which hard-codes the DarkDex W=8 VK) — the same structural pivot that `VERGRTH16` → `VERGRTH16WITHVK` made on the Groth16 side, but applied natively to Halo2 SHPLONK so the bridge no longer needs a Groth16 wrapper for the ETH→AN deposit path.

**One-line summary**: pop a `Halo2TvmBundle` cell, verify, push a boolean.

---

## 2. Stack ABI

```
ZKHALO2VERIFYWITHVK:
  Pops (top → bottom):
    bundle_cell : Cell  — TVM cell whose flat byte payload is a Halo2TvmBundle
                          (see §3 for the wire format).

  Pushes:
    accept : Boolean    — true  if the SHPLONK verifier accepts the proof
                          false if the proof is well-formed but invalid

  Throws FatalError on structural errors only:
    - bundle magic mismatch (not b"HALO2TVM")
    - bundle version mismatch (not 0x01)
    - unknown transcript_kind byte
    - chunk length runs past the end of the bundle
    - bundle size exceeds 16 MiB (DoS guard)
    - config_json fails to deserialise as BaseCircuitParams
    - vk_bytes fails to deserialise as VerifyingKey<G1Affine>
      (this includes the curve-membership check; soundness-critical)
    - instances_bytes length is not a multiple of 32
    - a 32-byte instance chunk is >= the Fr modulus
    - bundle.config.k disagrees with vk.get_domain().k() (defence-in-depth
      against a malicious header that lies about k)
    - BaseCircuitBuilder::new(config) panics on internally-inconsistent
      params (e.g. lookup_bits >= k) — caught via std::panic::catch_unwind
      and converted to a structural FatalError
```

**Cryptographic rejection vs structural rejection.** A *well-formed* proof that just doesn't satisfy the relation is reported as `Boolean::false` on the stack — never as an exception. Only structural problems with the **bundle / VK / instances / config** raise `FatalError`. A malformed `proof_bytes` that fails SHPLONK deserialisation **also** collapses into `Boolean::false` via the handler's `.is_ok()` mapping over `verify_proof`'s `Result` — matching the convention of the existing `VERGRTH16` opcode (every verifier error is a cryptographic reject, never an exception).

**One operand, not three.** During the skeleton phase the design memo sketched a 3-operand ABI (`vk_cell`, `pub_inputs_cell`, `proof_cell`). The real implementation collapsed this to a single self-describing bundle, because:

- The VK *needs* the circuit's `BaseCircuitParams` to deserialise, and putting params on a fourth stack slot was uglier than embedding them in the bundle.
- The on-curve check is required on the VK, but ⇒ then you need a per-VK cache, but ⇒ caching by anything other than the VK bytes themselves was fragile.
- The producer (`crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`) was already shipping the four components as one blob over the wire to Ethereum-side tooling, so the single-cell ABI matched what was already on disk.

---

## 3. `Halo2TvmBundle` wire format

### 3.1 Byte layout

```text
  off  size  field
  ───  ────  ─────────────────────────────────────────────────────────
    0     8  magic           = b"HALO2TVM"   (ASCII, no NUL)
    8     1  version         = 0x01
    9     1  transcript_kind = 0x00          (Blake2b; 0x01 reserved for Keccak)
   10     6  reserved        = 0 × 6         (must be zero, ignored on read)
   16     4  config_len   (u32 LE)
   20  cl    config_json  (UTF-8 serde_json of `BaseCircuitParams`)
  ...     4  vk_len       (u32 LE)
  ...  vl    vk_bytes     (`VerifyingKey::write(SerdeFormat::RawBytes)`)
  ...     4  instances_len (u32 LE; must be a multiple of 32)
  ...  il    instances_bytes (N × 32-byte LE `Fr::to_repr()`, strict)
  ...     4  proof_len    (u32 LE)
  ...  pl    proof_bytes  (SHPLONK proof with Blake2b transcript)
```

All length prefixes are `u32` little-endian (bundles are always under 4 GiB; the consumer caps at 16 MiB).

### 3.2 Header fields in detail

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 8 | `magic` | `b"HALO2TVM"` (`0x48 0x41 0x4C 0x4F 0x32 0x54 0x56 0x4D`). Distinguishes the bundle from any other TVM cell payload. |
| 8 | 1 | `version` | `0x01` today. Bump on any breaking change to the byte layout. The consumer rejects every other value with `FatalError`. |
| 9 | 1 | `transcript_kind` | `0x00 = Blake2b SHPLONK` (the only flavour the AN-side handler speaks today — `verify_proof::<KZGCommitmentScheme<Bn256>, VerifierSHPLONK, Challenge255, Blake2bRead, SingleStrategy>`). `0x01 = Keccak` is **reserved** for a future variant — emitting `0x01` today raises `FatalError`. |
| 10..15 | 6 | `reserved` | Must be zero on write. The consumer does not enforce zero on read (future format extensions may repurpose these bytes). |

### 3.3 Payload chunks

Each chunk is a `u32` little-endian length followed by exactly that many bytes. Order is fixed: **config_json, vk_bytes, instances_bytes, proof_bytes**.

| Chunk | Producer-side serialisation | Consumer-side deserialisation |
|---|---|---|
| `config_json` | `serde_json::to_vec(&BaseCircuitParams)` | `serde_json::from_slice::<BaseCircuitParams>(..)` |
| `vk_bytes` | `vk.write(&mut buf, SerdeFormat::RawBytes)` | `VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(.., SerdeFormat::RawBytes, config)` |
| `instances_bytes` | `for fr in instances: out.extend_from_slice(fr.to_repr())` (strict LE; no u64 shortcut) | per-32-byte `Fr::from_repr(..)`; `>= modulus` ⇒ `FatalError` |
| `proof_bytes` | bytes written by `Blake2bWrite<_, _, Challenge255<_>>` during proving | passed to `Blake2bRead::<_, _, Challenge255<_>>::init(..)` for verification |

### 3.4 Sizing reference (DarkDex W=8 L0 fixture)

| Chunk | Size | Notes |
|---|---|---|
| `config_json` | ~125 B | `BaseCircuitParams` serialised by `serde_json` (compact form, no whitespace). |
| `vk_bytes` | 842 B | `K=19` DarkDex W=8 circuit, 4 advice + 1 fixed + 1 lookup column, 1 instance column. |
| `instances_bytes` | `5 × 32` = 160 B | 5 public inputs for DarkDex W=8 layer 0 (matches the checked-in `dark_dex_w8_L0_instances.bin` fixture). The bridge's deposit-prover circuit has 7. |
| `proof_bytes` | 2 080 B | SHPLONK + Blake2b transcript at `K=19` for this specific fixture. Larger circuits or different parameter sets yield proofs of a few KB to ~10 KB. |
| **Header + length prefixes** | 32 B | 8 + 1 + 1 + 6 + 4 × 4 |
| **Total bundle** | **~3.2 KB** | Well under the 16 MiB cap. |

The **16 MiB cap** (`MAX_BUNDLE_BYTES` in `tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs`) is the only DoS guard; without it a pathological `u32::MAX` length prefix would let a hostile producer ask the consumer to allocate 4 GiB.

### 3.5 Why `SerdeFormat::RawBytes` and not `RawBytesUnchecked`

`SerdeFormat::RawBytesUnchecked` — the variant historically used by the `gosh-zk-snark-halo2-utils` test helper `io::read_vk` — **skips** curve-membership checks on every group element of the verifying key. This is fine when the VK comes from your own trusted prover. It is **not** fine when the VK is supplied by the caller of an on-chain opcode: a maliciously-crafted off-curve point in the VK lets the attacker produce verifications that don't actually correspond to any valid proof.

The opcode handler therefore uses `SerdeFormat::RawBytes`, which runs the on-curve check on every group element. The producer side mirrors this byte layout (see `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs::from_native`) so producer and consumer agree.

This costs a few hundred milliseconds per first-time VK deserialisation. The per-VK cache (§5) amortises that across calls.

---

## 4. KZG SRS handling

The verifier needs `ParamsKZG<Bn256>`. For SHPLONK verification (not proving) only three points from the universal SRS are actually used:

| Point | Symbol | Role |
|---|---|---|
| First G1 point | `g[0]` | Anchor for opening proof commitments. |
| G2 generator | `g2` | Pairing base. |
| `[s]_2` (G2) | `s_g2` | Trapdoor-shifted G2 generator. |

These three points are **embedded in `tvm_vm` as constants** (`KZG_G0_BYTES`, `KZG_G2_BYTES`, `KZG_S_G2_BYTES` in `tvm_vm/src/executor/zk_halo2_utils.rs`) and `build_shared_kzg_params(k)` (in `zk_halo2_with_vk.rs`) reconstructs a `ParamsKZG<Bn256>` for any `k` at runtime via `from_parts`. **No on-disk SRS file is required** for verification.

The points were extracted from the KZG setup used by the bridge's Halo2 prover (`gosh-zk-snark-halo2-utils`-tested), which in turn uses the Hermez Phase 1 ceremony as its trust root. The bridge inherits that trust assumption transparently — no separate Phase 2 ceremony is needed for SHPLONK verification because SHPLONK is non-universal-but-trapdoor-free at verification time.

---

## 5. Per-VK cache

The handler keeps a bounded-FIFO `HashMap<Vec<u8>, Arc<CachedVk>>` keyed by `bundle.vk_bytes`, capacity 8. A `CachedVk` holds:

- The deserialised `VerifyingKey<G1Affine>`.
- The reconstructed `ParamsKZG<Bn256>` for the VK's `k = vk.cs.degree`.

**Why 8.** Empirical lower bound: a deposit-prover contract calls with exactly one VK; a multi-circuit AN-side dApp realistically holds 2–4 VKs at once. The capacity of 8 covers the common cases without unbounded memory growth.

**Cache semantics.**
- **Hit**: ~0.5–1 ms warm-path. The handler reuses both `vk` and `params` directly.
- **Miss**: first call against a new VK pays VK deserialisation (curve-membership check on every group element) + `ParamsKZG` reconstruction. ~1–3 s on x86-64 at `k=19`, dominated by the EvaluationDomain precomputation in `VerifyingKey::read`.
- **Eviction**: oldest entry by insertion order. There is no LRU-on-access update — the cache is FIFO for predictability.

`Arc<CachedVk>` is used so that two concurrent calls with the same VK don't hold separate copies of the heavy `ParamsKZG`.

---

## 6. Gas model

Currently a placeholder: `ZKHALO2_VERIFY_WITH_VK_GAS_PRICE = 5_000` (see `tvm_vm/src/executor/zk_halo2_with_vk.rs`). This is a structural guess scaled from `VERGRTH16_GAS_PRICE = 2_380`:

- Halo2 SHPLONK verification at `k=19` is ~3–5× the wall-clock of one Groth16 BN254 pairing check on the same machine.
- VK bytes are kilobytes (vs 192 B for Groth16), so the deserialisation step is heavier.
- Cache amortises the heavy work; the gas number should reflect *amortised* cost or `tvm-vm` will overcharge most invocations.

**Re-bench TODO** (before mainnet):

1. Measure cold-cache verify (single VK, no prior calls). Use this as the `gas_price` upper bound.
2. Measure warm-cache verify (FIFO hit). This is what most calls will pay.
3. Decide whether to charge a flat amortised number or to split cold/warm via two-tier pricing.
4. Re-run with the bridge's actual deposit-prover VK (not just DarkDex W=8) once the circuit lands.

See `tvm-sdk/docs/zkhalo2verifywithvk_design.md` §5 (Q-GAS-1) for the open benchmarking checklist.

---

## 7. Producer side (Rust, in this repository)

The bridge ships a producer that emits exactly the bytes `ZKHALO2VERIFYWITHVK` consumes:

| Path | Role |
|---|---|
| `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` | `Halo2TvmBundle::from_native(..)` / `write(..)` / `read(..)` / `verify(..)` — the producer-side type. The consumer-side decoder in `tvm-sdk/tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs` is byte-for-byte compatible. |
| `crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs` | Round-trip test: prove with `FallbackKeyManager`, serialise via `Halo2TvmBundle::write`, deserialise via `Halo2TvmBundle::read`, verify against the reconstructed `(vk, instances, proof)` triple. Green; this is the ground truth for the format. |

### 7.1 Reference encoder snippet

```rust
use bridge_prover_orchestrator::halo2_tvm_bundle::Halo2TvmBundle;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Fr, G1Affine},
    plonk::VerifyingKey,
};

fn build_bundle_bytes(
    config: &BaseCircuitParams,
    vk: &VerifyingKey<G1Affine>,
    instances: &[Fr],
    proof_bytes: Vec<u8>,
) -> anyhow::Result<Vec<u8>> {
    let bundle = Halo2TvmBundle::from_native(config, vk, instances, proof_bytes)?;
    let mut out = Vec::with_capacity(32 * 1024);
    bundle.write(&mut out)?;
    Ok(out)
}
```

The returned `Vec<u8>` is what you pack into the TVM cell that goes onto the stack as the opcode's single operand.

### 7.2 Strictness checks on the producer side

The producer is expected to be self-consistent with the consumer, but the consumer trusts nothing:

| Check | Producer behaviour | Consumer behaviour |
|---|---|---|
| `instances_bytes.len() % 32 == 0` | `Halo2TvmBundle::from_native` always encodes `Fr::to_repr()` (32 bytes each) | `FatalError` if the chunk length is not a multiple of 32 |
| `Fr::from_repr(chunk).is_some()` | `Fr::to_repr` is canonical and always produces an in-range encoding | `FatalError` if any 32-byte chunk decodes ≥ modulus |
| VK on-curve | `SerdeFormat::RawBytes` write ⇒ encodes canonical compressed form | `SerdeFormat::RawBytes` read ⇒ runs `is_on_curve` on every group element |
| Proof structural shape | Producer uses the matching `Blake2bWrite` | Consumer uses `Blake2bRead::init`; mismatched transcript ⇒ verification returns `false`, no exception |

---

## 8. Test surface (consumer side, `tvm-sdk`)

| Test (location: `tvm_vm/src/tests/test_halo2_with_vk.rs`) | What it covers |
|---|---|
| `round_trip_dark_dex_w8_l0_valid_proof_returns_true` | Positive: real Halo2 SHPLONK proof for DarkDex W=8 L0 → bundle → opcode → `true`. Warm-path ~0.6 s. |
| `flipped_proof_byte_rejected_as_false` | Negative: flip a byte in `proof_bytes` → opcode returns `Ok(false)` (handler maps every `verify_proof` error via `.is_ok()`). |
| `tweaked_instance_fr_rejected_as_false` | Negative: flip the low bit of the first public-input `Fr` (stays in-modulus) → opcode returns `Ok(false)`. |
| `bad_magic_returns_fatal_error` | Negative: clobber the 8-byte magic → opcode raises `FatalError`. |
| `corrupt_vk_byte_never_verifies_as_true` | Negative: flip a byte inside the VK G1 region → either `FatalError` (curve-membership) or `Ok(false)` (deserialised but cryptographically wrong) — never `Ok(true)`. |
| `instance_ge_modulus_returns_fatal_error` | Negative: set first 32 instance bytes to `0xFF…FF` (> Fr modulus) → `FatalError` via strict `Fr::from_repr`. |
| `instance_count_mismatch_rejected_as_false` | Negative: drop the last 32-byte instance Fr from a valid bundle → `verify_proof` sees N-1 columns instead of N → `Ok(false)`. |
| `empty_proof_rejected_as_false` | Negative: zero-length `proof_bytes` → `Ok(false)`. |
| `malformed_config_json_returns_fatal_error` | Negative: replace `config_json` with `b"this is not json"` → `FatalError` in `Halo2TvmBundle::parse`. |
| `config_k_mismatch_returns_fatal_error` | Negative: mutate `"k":19` → `"k":18` in JSON; either our `config.k != vk.domain().k()` guard fires, or `BaseCircuitBuilder::new` panics on `lookup_bits=18` against `k=18` and our `catch_unwind` wrapper surfaces it as `FatalError`. |
| `fifo_cache_reused_across_two_invocations` | Smoke: same VK across two calls, second is fast (warm cache). |

Bundle decoder tests in `tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs` (`tests::` module): negative tests covering magic / version / transcript / chunk-length / trailing-garbage / instance-multiple-of-32 / oversized-bundle DoS guard.

Total `tvm_vm --features gosh --lib`: **116 tests passed** (110 baseline + 11 new opcode + the bundle decoder tests are counted under module `zk_halo2_with_vk_bundle::tests`).

Assembler round-trip test in `tvm_assembler/src/lib.rs::gosh_zk_opcode_tests::zk_opcode_bytes_round_trip` ensures the mnemonic `ZKHALO2VERIFYWITHVK` assembles to exactly `0xC7 0x4A`.

---

## 9. Call site sketch (Solidity-on-TVM, pseudocode)

The compiler-side builtin `gosh.zkHalo2VerifyWithVK(bundle)` is **pending** in TVM-Solidity-Compiler at the time of writing. The expected call site looks like:

```solidity
// inside the AN-side TokenBridge contract
contract TokenBridge {
    TvmCell public depositProofBundleTemplate;  // bundle skeleton; VK + config baked in at deploy time
    mapping(uint256 => bool) public usedDepositIds;

    function finalizeDeposit(
        uint256 depositId,
        uint256 sender,           // public input #1 (ETH address as Fr)
        uint256 amount,
        uint256 contractAddress,
        uint256 blockHashHigh,
        uint256 blockHashLow,
        uint256 promiseCommit,
        TvmCell proofBundle       // full Halo2TvmBundle from the off-chain prover
    ) external {
        require(!usedDepositIds[depositId], "deposit already finalized");
        require(
            gosh.zkHalo2VerifyWithVK(proofBundle),
            "halo2 verification failed"
        );

        // ... cross-check public-input bindings between bundle and call args ...
        // ... credit recipient, emit event ...
        usedDepositIds[depositId] = true;
    }
}
```

The contract is responsible for ensuring that the 7 public inputs encoded *inside* the bundle match the values it intends to act on (`depositId`, `sender`, etc.). The opcode itself only checks that the proof is valid for *whatever* public inputs the bundle carries.

---

## 10. Failure-mode cheat sheet

| Symptom (caller view) | Likely cause | Where to look |
|---|---|---|
| `FatalError` immediately, before any verification work | Bundle bytes don't start with `b"HALO2TVM"`, or `version != 0x01`, or `transcript_kind != 0x00`, or a chunk length runs past the end | Producer-side encoding bug; check `Halo2TvmBundle::write` succeeded against a fresh `Vec<u8>` |
| `FatalError` with "instances chunk length .. is not a multiple of 32" | Producer concatenated public inputs with the wrong stride (e.g. u64 shortcut, or non-32-byte `Fr` encoding) | Use `encode_instances(&[Fr])` from `bridge_prover_orchestrator::halo2_tvm_bundle` |
| `FatalError` with "Fr::from_repr rejected" | Producer passed a value ≥ Fr modulus as a public input | Reduce the value mod p on the producer side; use `Fr::from(value).to_repr()` |
| `FatalError` deserialising VK | VK bytes were written with the wrong `SerdeFormat`, or VK was for a different curve, or an off-curve point was injected | Re-emit VK via `vk.write(&mut buf, SerdeFormat::RawBytes)` |
| `false` on the stack (no exception) | The proof is well-formed but doesn't satisfy the relation. Either the prover was given the wrong witness, or the public inputs in the bundle don't match what the proof was generated against, or VK ↔ proof drift between deploys | Re-run the producer-side `Halo2TvmBundle::verify` locally — if it returns `false` there too, it's a prover-side issue, not an opcode issue |
| `false` on the stack but local `Halo2TvmBundle::verify` returns `true` | `ParamsKZG` mismatch — producer used different SRS bytes than the consumer reconstructs from the three embedded points | Verify the producer uses the same KZG points as `tvm_vm/src/executor/zk_halo2_utils.rs::KZG_*_BYTES`; if you're using a custom SRS, that needs an explicit `transcript_kind` extension and a wire-format bump |
| `FatalError` "bundle byte length .. exceeds MAX_BUNDLE_BYTES" | Bundle > 16 MiB | Almost certainly a producer-side bug emitting a giant length prefix; check that `proof_bytes` is reasonable (~10 KB for the bridge's circuit) |

---

## 11. Cross-references

### `tvm-sdk` side (consumer)
- `tvm_vm/src/executor/zk_halo2_with_vk.rs` — opcode handler.
- `tvm_vm/src/executor/zk_halo2_with_vk_bundle.rs` — bundle decoder + unit tests.
- `tvm_vm/src/executor/zk_halo2_utils.rs` — embedded KZG points + `build_shared_kzg_params(k)`.
- `tvm_vm/src/tests/test_halo2_with_vk.rs` — integration tests (DarkDex W=8 L0 round-trip + 4 negatives).
- `tvm_assembler/src/{simple,lib}.rs` — mnemonic + assembler round-trip test.
- `tvm_vm/src/executor/engine/handlers.rs` — dispatch entry at `0xC7 0x4A`.
- `tvm_vm/src/executor/gas/gas_state.rs` — `Gas::{zkhalo2_verify_with_vk_price, consume_zkhalo2_verify_with_vk}`.
- `docs/zkhalo2verifywithvk_design.md` — frozen design memo.

### Bridge side (producer)
- `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` — producer-side `Halo2TvmBundle` type (encoder + decoder + verifier).
- `crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs` — full round-trip ground-truth.
- `crates/bridge-prover-orchestrator/src/{prover,verifier,keys}.rs` — the fallback prover that emits the proof bytes embedded in the bundle.

### Pull requests
- [`tvm-sdk` PR #240 — ZKHALO2VERIFYWITHVK real implementation](https://github.com/tvmlabs/tvm-sdk/pull/240) — landed 2026-05-22.
- [`tvm-sdk` PR #242 — Remove VERGRTH16WITHVK + nightly fmt](https://github.com/tvmlabs/tvm-sdk/pull/242) — retirement of the per-VK Groth16 path.

### Design / decision-log
- `docs/zk_halo2_an_side_design.md` — full design memo with Q-WIRE-1..5 rationale.
- `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 + 2026-05-22 — phase-by-phase reasoning behind the pivot from Groth16 wrappers to native Halo2.
- `docs/verifying_eth_proof_on_an.md` — end-to-end deposit-proof flow that `ZKHALO2VERIFYWITHVK` lives in.
- `docs/audit_trail_v2.md` K-13 — trust assumption tied to the opcode's soundness.

---

## 12. Things explicitly out of scope for this opcode

- **Multi-VK proof aggregation.** One opcode call ⇒ one (VK, proof, instances) triple. Aggregation (recursion / IVC) lives at a higher layer.
- **Cross-curve proofs.** BN254 only. BLS12-381 verification is `ZKHALO2VERIFY`'s territory (and even there it's curve-fixed at the VK level).
- **Producer-side proving from inside TVM.** Proving is many orders of magnitude more expensive than verification and is never run on-chain.
- **Calldata-level VK rotation.** The VK rotation policy (when a contract decides to accept proofs against a new VK) is application-level, not VM-level. The opcode just verifies whatever bundle it's given.
- **Replay protection.** The opcode doesn't track which proofs it has accepted before. Contracts must maintain their own nullifier / `usedDepositIds` map (see §9).
