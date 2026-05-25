# Bridge Circuit 1B (Fallback BLS attestation) — `ZKHALO2VERIFYWITHVK` fixture

A real, end-to-end-verified set of operands for the AN-side
`ZKHALO2VERIFYWITHVK` opcode (`0xC7 0x4A`, frozen Variant A — see
`docs/zkhalo2verifywithvk_reference.md`).

Use this fixture to:
- Smoke-test a fresh `ZKHALO2VERIFYWITHVK` deployment without re-running
  the (~2–5 min on cached keys / ~30 min from scratch) prover.
- Verify producer / consumer wire compatibility byte-for-byte.
- Sanity-check a contract's `public_inputs_cell` assembly logic against
  the same Fr values the on-chain handler will see.

## Provenance

| Artefact | Source |
|---|---|
| `fallback_vk.bin` | `crates/bridge-prover-orchestrator/params/fallback_vk.bin` — written by `FallbackKeyManager::ensure_keys(..)` via `vk.write(.., SerdeFormat::RawBytes)`. K=20. |
| `fallback_config_params.json` | `crates/bridge-prover-orchestrator/params/fallback_config_params.json` — `BaseCircuitParams` of the fallback circuit, serialised compactly via `serde_json::to_vec`. |
| `fallback_vk_blob.bin` | `VkBlob::from_native(config, vk).to_bytes()` — the exact byte payload of the `vk_cell` operand. Magic `"VKBLOB\x00\x00"`, version 1, transcript Blake2b. |
| `fallback_public_inputs.bin` | `encode_instances(&proof.instances())` — raw `N × 32` LE `Fr::to_repr()` (no header). N = 4. |
| `fallback_proof.bin` | `proof.proof_bytes` — SHPLONK proof with Blake2b transcript, K=20. |

All five files were emitted in one run of
`crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs::halo2_tvm_operands_round_trip_fallback_circuit`
with `EXPORT_HALO2_FIXTURE_DIR=…/fixtures/circuit_1b_fallback`. The
same test verifies the proof end-to-end via `verify_proof::<KZG,
VerifierSHPLONK, Challenge255, Blake2bRead, SingleStrategy>` before
exporting, so any committed bytes here have passed local verification.

The matching consumer-side fixture lives in the `tvm-sdk` repository at
`tvm_vm/halo2_test_data/fallback_{vk_blob,public_inputs,proof}.bin` and
drives the integration test
`tvm_vm/src/tests/test_halo2_with_vk.rs::bridge_circuit_1b_fallback_real_proof_verifies`
which feeds these same bytes through the live `ZKHALO2VERIFYWITHVK`
handler and asserts `Ok(true)`.

## Sizing reference

| File | Size | What it is |
|---|---|---|
| `fallback_config_params.json` | 181 B | `BaseCircuitParams { k: 20, num_advice_per_phase: [44], num_fixed: 1, num_lookup_advice_per_phase: [4, 0, 0], lookup_bits: Some(19), num_instance_columns: 1 }` |
| `fallback_vk.bin` | 6 154 B | Raw `VerifyingKey<G1Affine>` (SerdeFormat::RawBytes — curve-membership-checked on read) |
| `fallback_vk_blob.bin` | 6 308 B | `VkBlob` = 16 B header + 4 B + 181 B (config) + 4 B + 6 154 B (vk) − 51 B overhead = 6 308 B |
| `fallback_public_inputs.bin` | 128 B | 4 × 32 = `[bk_set_poseidon_commit, envelope_hash_high, envelope_hash_low, last_seen_block_seqno]` |
| `fallback_proof.bin` | 14 784 B | SHPLONK proof + Blake2b transcript at K=20 |

## Public input layout (4 Fr, 32 B LE each)

```text
   instances[0]  bk_set_poseidon_commit    — Poseidon hash of the BK set
   instances[1]  envelope_hash_high        — high 16 B of the envelope SHA-256
   instances[2]  envelope_hash_low         — low  16 B of the envelope SHA-256
   instances[3]  last_seen_block_seqno     — u32 zero-padded to 32 B LE
```

Total `public_inputs_cell` payload: 128 B, no header.

## How to consume these files

### Option 1: pre-assembled `VkBlob`

```rust
let vk_blob = std::fs::read("fallback_vk_blob.bin")?;
let public_inputs = std::fs::read("fallback_public_inputs.bin")?;
let proof = std::fs::read("fallback_proof.bin")?;

// Push as three TVM cells (top → bottom: proof, public_inputs, vk):
//
//   PUSHREF vk_cell
//   PUSHREF public_inputs_cell
//   PUSHREF proof_cell
//   ZKHALO2VERIFYWITHVK
//
// Expected stack-top after the opcode: -1 (true).
```

### Option 2: build your own `VkBlob` from the raw VK + config

```rust
use bridge_prover_orchestrator::VkBlob;
use halo2_base::gates::circuit::BaseCircuitParams;

let config: BaseCircuitParams =
    serde_json::from_str(&std::fs::read_to_string("fallback_config_params.json")?)?;
let vk_bytes = std::fs::read("fallback_vk.bin")?;

// Manual VkBlob assembly: 8 B magic + 1 B version + 1 B transcript + 6 B zero
//                       + 4 B cfg_len + cfg_json + 4 B vk_len + vk_bytes.
//
// Or via the helper:
//
//   let vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
//       &mut vk_bytes.as_slice(),
//       SerdeFormat::RawBytes,
//       config.clone(),
//   )?;
//   let blob = VkBlob::from_native(&config, &vk)?;
//   let payload = blob.to_bytes()?;   // identical to fallback_vk_blob.bin
```

## SHA-256 checksums

Run `sha256sum *.bin *.json` in this directory to verify integrity
against the values in `MANIFEST.sha256` of the partner pack zip.
