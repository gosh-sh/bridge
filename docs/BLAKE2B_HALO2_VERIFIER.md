# Blake2b Halo2 Proof Verification on Ethereum

## Task Description

**Goal:** Verify that a Halo2 zero-knowledge proof generated on the Acki Nacki blockchain (using Blake2b transcript) can be verified on Ethereum.

The Acki Nacki bridge enables cross-chain deposits and withdrawals between Ethereum and the Acki Nacki blockchain using zero-knowledge proofs. The existing infrastructure uses a Groth16 wrapper around Halo2 proofs for Ethereum verification (because raw Halo2 verifiers exceed Ethereum's 24KB contract size limit). However, the current flow only handles **Ethereum → Acki Nacki** deposits.

This task addresses the reverse direction: **Acki Nacki → Ethereum** — proving that a proof generated using Acki Nacki's native libraries and transcript (Blake2b) can be verified on Ethereum using a Solidity verifier that implements the same Blake2b transcript protocol via EIP-152.

### Key Constraint

The user explicitly requires:

1. **No Keccak256** in the new verification path — only Blake2b and native Acki Nacki algorithms
2. **Preserve all existing infrastructure** — the new Blake2b verifier is standalone alongside existing contracts
3. **Use Solidity inheritance** for any code shared between existing Keccak-based verifiers and the new Blake2b verifier
4. **Use standard libraries only** — no custom cryptographic implementations

---

## Development Plan

### Phase 1: Circuit & Proof Generation (✅ COMPLETE)

1. **Study Acki Nacki's proof infrastructure** — Understand `gosh_dark_dex_halo2_circuit`, TVM opcodes (`VERGRTH16`, `POSEIDON`), and the Blake2b transcript protocol
2. **Create a simple Poseidon preimage circuit** — Minimal Halo2 circuit proving knowledge of `x` such that `Poseidon(x) = h`
3. **Generate proof with Blake2b transcript** — Using standard `halo2_proofs::Blake2bWrite` (same as Acki Nacki uses)

### Phase 2: Solidity Verifier (✅ COMPLETE)

4. **Create Blake2b Solidity verifier** — Generate verifier using `halo2-solidity-verifier` as base, then replace ALL keccak256 with Blake2b via EIP-152 precompile (address `0x09`)
5. **Implement Blake2b transcript in Solidity** — Full rewrite of transcript handling to match the Blake2b protocol (not just a hash swap — the protocols are fundamentally different)

### Phase 3: On-Chain Testing (🔄 IN PROGRESS)

6. **Deploy Blake2b verifier on Sepolia** — Deploy the generated Solidity verifier
7. **Submit proof and verify on-chain** — Send the Blake2b proof to the deployed verifier
8. **Negative tests** — Confirm tampered proofs and wrong public inputs are rejected

---

## Architecture

### Existing Infrastructure (Preserved)

```
Ethereum → Acki Nacki (Deposits):
  ┌────────────────┐    ┌──────────────────────┐    ┌──────────────────┐
  │ AckiNackiBridge │───▶│ Groth16DepositVerifier│───▶│ Groth16Verifier  │
  │   (deposit())   │    │ (IAckiNackiVerifier)  │    │ (gnark-generated)│
  └────────────────┘    └──────────────────────┘    └──────────────────┘
         │                        │
         │                        ├── DummyVerifier (test)
         │                        └── Halo2Verifier (fallback, Keccak256 transcript)
         │
         └── IBlockHeaderOracle
              ├── MockBlockHeaderOracle (test)
              └── AxiomBlockHeaderOracle (production)
```

### New Blake2b Verification Path (This Task)

```
Acki Nacki → Ethereum (Withdrawals):
  ┌─────────────────────┐    ┌──────────────────────────┐
  │ poseidon-proof (Rust)│───▶│ Blake2bHalo2Verifier.sol │
  │ - Circuit definition │    │ - EIP-152 Blake2b        │
  │ - Blake2b transcript │    │ - Standalone contract    │
  │ - Proof generation   │    │ - No Keccak256           │
  └─────────────────────┘    └──────────────────────────┘
```

### Proof System Stack

| Component      | Library                               | Version           |
| -------------- | ------------------------------------- | ----------------- |
| Curve          | BN254                                 | —                 |
| Proof system   | Halo2 (KZG + SHPLONK)                 | halo2-axiom 0.5.1 |
| Transcript     | Blake2b (`Challenge255`)              | halo2_proofs      |
| Hash (circuit) | Poseidon (T=3, RATE=2, R_F=8, R_P=57) | pse-poseidon      |
| Builder        | `RangeCircuitBuilder`                 | halo2-base 0.5.1  |

---

## Completed Work

### 1. Acki Nacki Research (✅)

**Key findings from studying `../acki-nacki`, `../tvm-sdk`, and `../gosh_dark_dex_halo2_circuit`:**

- **TVM opcodes:** `VERGRTH16` (opcode `0xC731`, gas 2380) verifies Groth16 proofs with a hardcoded verifying key for ZK login. `POSEIDON` (opcode `0xC732`, gas 356) computes Poseidon hashes.
- **ZK Login flow:** OAuth → JWT → ephemeral keypairs → Groth16 proof via `https://prover.ackinacki.org/v1` → verified on-chain via `gosh.vergrth16()`
- **DarkDEX circuit:** Halo2 circuit in `gosh_dark_dex_halo2_circuit` using Blake2b transcript, KZG+SHPLONK on BN254, Poseidon hash (T=3, RATE=2, R_F=8, R_P=57)

### 2. Transcript Protocol Analysis (✅)

**Critical discovery:** Blake2b and Keccak256 transcripts are fundamentally different protocols, not just different hash functions.

| Aspect          | Blake2b Transcript                                                                | Keccak256 Transcript                              |
| --------------- | --------------------------------------------------------------------------------- | ------------------------------------------------- |
| State model     | Incremental Blake2b state machine                                                 | Buffer accumulation                               |
| Initialization  | `hash_length=64`, `personal=b"Halo2-Transcript"`                                  | Empty buffer                                      |
| Point encoding  | Little-endian coordinates with `0x01` prefix                                      | Big-endian coordinates, appended to buffer        |
| Scalar encoding | Little-endian with `0x02` prefix                                                  | Big-endian, appended to buffer                    |
| Challenge       | `0x00` prefix → clone state → finalize → 64 bytes → `from_uniform_bytes` → scalar | `keccak256(buffer)` → 32 bytes → `mod r` → scalar |
| Output size     | 64 bytes                                                                          | 32 bytes                                          |

**Implication:** A full rewrite of the Solidity transcript handling is needed, not just replacing `keccak256()` calls with Blake2b.

### 3. Poseidon Preimage Circuit (✅)

Created `poseidon-proof/` crate — a minimal Halo2 circuit proving `Poseidon(x) = h`.

**Files created:**

| File                                       | Purpose                                                           |
| ------------------------------------------ | ----------------------------------------------------------------- |
| `poseidon-proof/Cargo.toml`                | Dependencies (halo2-base 0.5.1, pse-poseidon)                     |
| `poseidon-proof/src/lib.rs`                | Module declarations                                               |
| `poseidon-proof/src/poseidon.rs`           | Native Poseidon hash function (outside circuit)                   |
| `poseidon-proof/src/circuit.rs`            | `PoseidonPreimageCircuit` struct and implementation               |
| `poseidon-proof/src/tests.rs`              | 3 tests: MockProver valid, MockProver invalid, real Blake2b proof |
| `poseidon-proof/src/bin/generate_proof.rs` | Binary to generate proof and export artifacts                     |

**Circuit structure:**

- Private witness: `preimage` (single field element)
- Public input: `hash = Poseidon(preimage)`
- Uses `PoseidonHasher::hash_var_len_array` inside the circuit
- Same Poseidon parameters as `gosh_dark_dex_halo2_circuit`

**Test results (all passing):**

```
test tests::test_mock_prover_valid ... ok
test tests::test_mock_prover_invalid ... ok
test tests::test_real_proof_blake2b ... ok     (Proof size: 1504 bytes)
```

### 4. Proof Generation (✅)

Generated proof artifacts via `cargo run --bin generate-proof`:

```
=== Poseidon Preimage Proof Generator (Blake2b transcript) ===
  Secret preimage: 42
  Poseidon hash:   0xe3c6b925b32734aa752823dabb61bbbfba3c622e750c97fa0b8db7d16fde2022
  Proof size:      1504 bytes
  Transcript:      Blake2b (Challenge255)
  Scheme:          KZG + SHPLONK on BN254
```

**Generated artifacts in `poseidon-proof/data/`:**

| File                | Size       | Description                    |
| ------------------- | ---------- | ------------------------------ |
| `vk.bin`            | —          | Halo2 verifying key (binary)   |
| `proof.bin`         | 1504 bytes | Proof bytes (binary)           |
| `proof.hex`         | 3008 chars | Proof bytes (hex-encoded)      |
| `public_inputs.bin` | 32 bytes   | Public inputs (1 × Fr element) |
| `public_inputs.hex` | 64 chars   | Public inputs (hex-encoded)    |
| `params.bin`        | —          | KZG SRS parameters (k=12)      |

**Circuit parameters:**

```
BaseCircuitParams {
    k: 12,
    num_advice_per_phase: [2],
    num_fixed: 1,
    num_lookup_advice_per_phase: [1, 0, 0],
    lookup_bits: Some(11),
    num_instance_columns: 1
}
```

---

## Remaining Work

### 5. Blake2b Solidity Verifier (✅ COMPLETE)

**Approach:**

1. Use `halo2-solidity-verifier` (PSE) to generate the initial Solidity verifier template
2. Replace the 2 `squeeze_challenge` / `squeeze_challenge_cont` functions with Blake2b equivalents
3. Rewrite the full transcript absorption flow:
   - Implement Blake2b state initialization (`hash_length=64`, `personal=b"Halo2-Transcript"`)
   - Add prefix byte handling (`0x00`=challenge, `0x01`=point, `0x02`=scalar)
   - Convert coordinate encoding from big-endian to little-endian
   - Implement 64-byte → scalar conversion via `from_uniform_bytes` equivalent
4. Use EIP-152 Blake2f precompile (address `0x09`) for Blake2b compression

**EIP-152 Blake2f precompile:**

- Address: `0x09`
- Input format: `[4 bytes rounds][64 bytes h][128 bytes m][16 bytes t][1 byte f]`
- Output: 64 bytes (updated state `h`)
- Gas cost: `rounds * 1` (typically 12 rounds for Blake2b)

**Solidity inheritance strategy:**

- Shared code: EC point operations (precompiles 0x06, 0x07), pairing (0x08), field arithmetic, batch inversion
- Transcript-specific: `squeeze_challenge`, `read_ec_point` (endianness), challenge derivation

### 6. Deploy on Sepolia (⬜ NOT STARTED)

Deploy the Blake2b verifier, submit the proof from step 4, and confirm it verifies correctly.

### 7. Negative Tests (⬜ NOT STARTED)

Test that tampered proofs and wrong public inputs are rejected.

---

## Technical Reference

### Blake2b Transcript Protocol (from `halo2_proofs::transcript`)

```rust
// Initialization
Blake2bParams::new()
    .hash_length(64)
    .personal(b"Halo2-Transcript")
    .to_state()

// Absorb point (common_point)
state.update(&[0x01]);                    // prefix byte
state.update(coords.x().to_repr());      // x coordinate, little-endian
state.update(coords.y().to_repr());      // y coordinate, little-endian

// Absorb scalar (common_scalar)
state.update(&[0x02]);                    // prefix byte
state.update(scalar.to_repr());          // scalar, little-endian

// Squeeze challenge
state.update(&[0x00]);                    // prefix byte
let hash = state.clone().finalize();      // 64-byte output
Challenge255::new(&hash)                  // from_uniform_bytes → Fr
```

### EIP-152 Blake2f Input Format

```
┌──────────┬────────┬──────────┬────────┬──────┐
│ rounds   │ h      │ m        │ t      │ f    │
│ 4 bytes  │ 64 B   │ 128 B    │ 16 B   │ 1 B  │
│ big-end  │ LE u64 │ LE bytes │ LE u128│ 0/1  │
└──────────┴────────┴──────────┴────────┴──────┘
Total: 213 bytes input → 64 bytes output
```

### Poseidon Parameters

| Parameter | Value    | Description     |
| --------- | -------- | --------------- |
| T         | 3        | State size      |
| RATE      | 2        | Absorption rate |
| R_F       | 8        | Full rounds     |
| R_P       | 57       | Partial rounds  |
| Field     | BN254 Fr | Scalar field    |

### Related Files in Repository

| File                                                | Role                                             |
| --------------------------------------------------- | ------------------------------------------------ |
| `poseidon-proof/`                                   | New crate: Poseidon preimage circuit + proof gen |
| `contracts/ethereum/src/IAckiNackiVerifier.sol`     | Verifier interface                               |
| `contracts/ethereum/src/Halo2Verifier.sol`          | Existing Halo2 verifier (Keccak256)              |
| `contracts/ethereum/src/DummyVerifier.sol`          | Test verifier (accepts any proof)                |
| `contracts/ethereum/src/Groth16DepositVerifier.sol` | Production deposit verifier                      |
| `contracts/ethereum/src/AckiNackiBridge.sol`        | Bridge contract                                  |
| `../gosh_dark_dex_halo2_circuit/`                   | Reference Halo2 circuit (Acki Nacki)             |
