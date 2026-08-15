# TD-43 — MockProver green ≠ SHPLONK / AN opcode triple (DEP-MOCK-VS-REAL)

PoC: `td_43_mock_vs_shplonk.rs`, `src/opcode_triple_verify.rs`.  
Cross-ref: TD-03–22 (MockProver PoCs), TD-42 (VkBlob pin), `examples/verify_opcode_triple.rs`.

## Verification layers

| Layer | What it checks | Proxy for AN opcode? |
|-------|----------------|----------------------|
| MockProver (`test_circuit_mock`) | Constraint satisfaction on witness | **No** — no SHPLONK bytes |
| `prover::verify_proof` | bincode `Snark` deserialize + PI field match | **No** — no crypto / wrong transcript wire |
| `verify_deposit_opcode_triple` | VkBlob + Blake2b SHPLONK + 12×32 B PI | **Yes** — mirrors opcode handler |
| AN VM `ZKHALO2VERIFYWITHVK` | On-chain triple | Production gate |

## Wire-format asymmetry (bincode Snark ≠ Blake2b)

| Artifact | Encoding | Accepted by |
|----------|----------|-------------|
| `generate_proof` output | bincode `Snark` (Poseidon transcript path) | `verify_proof` struct only |
| `export_blake2b_deposit_triple` / fixture `proof.bin` | Blake2b SHPLONK wire | opcode triple + AN VM |
| Random bytes | — | **fail** all crypto paths |

A proof that passes MockProver can use **either** wire internally in tests; only the Blake2b export matches what `USDCBridge.finalizeDeposit` verifies.

## RLC VkBlob — not `Halo2TvmOperands`

Deposit fixtures use **RLC** VkBlob (`deposit_vk_blob.bin`, 5006 B, pin `9dacd998…`, TD-42).

| Helper | VkBlob shape | Deposit fixtures? |
|--------|--------------|-------------------|
| `Halo2TvmOperands::verify` | Base only | **No** — use opcode triple |
| `verify_deposit_opcode_triple` | RLC (deposit pin) | **Yes** |
| `examples/verify_opcode_triple.rs` | RLC (CLI) | **Yes** |

## Asymmetry regression matrix

| Scenario | MockProver | `verify_proof` struct | SHPLONK opcode triple | td_43 test |
|----------|------------|----------------------|------------------------|------------|
| proof_00 witness | pass | pass (after `generate_proof`) | pass (Blake2b export) | `td_43_proof_00_mock_prover_satisfied`, `td_43_real_proof_00_passes_opcode_triple` |
| Random `proof.bin` bytes | pass (witness unrelated) | — | **fail** | `td_43_mock_green_random_proof_fails_shplonk` |
| 1-byte flip in real proof | pass | — | **fail** | `td_43_corrupted_real_proof_fails_shplonk` |
| Blake2b wire on opcode path | — | **fail** (not bincode Snark) | pass | `td_43_verify_proof_struct_not_crypto` |
| bincode Snark from `generate_proof` | pass | pass | **fail** (Poseidon ≠ Blake2b wire) | (documented; covered by struct vs opcode in same test) |

## CI hooks (TD-43 smoke wired)

| Hook | When | Notes |
|------|------|-------|
| `scripts/check_mock_vs_shplonk_smoke.sh` | standalone / aggregator | `cargo test --test td_43_mock_vs_shplonk` |
| `scripts/check_deposit_audit_gates.sh` | after `check_vk_srs_pin.sh` (TD-42) | TD-68 → TD-42 → **TD-43** → TD-04 |
| `make audit-deposit-relayer-test` | pre `cargo test` | full gate bundle |
| `.gitlab-ci.yml` `test:deposit-relayer:audit` | F10 job | gates + paths for td_43 / opcode_triple_verify |

### Smoke skip policy

Skip (exit 0) when **both** missing:

1. Fixture `deposit-prover/fixtures/deposit_10proofs/proof_00/{proof.bin,public_inputs.bin}` (384 B PI)
2. Hermez SRS `deposit-prover/data/kzg_params_18.srs` (or `DEPOSIT_KZG_SRS`)

With fixture present, smoke runs without SRS (loads Blake2b triple from disk). Without fixture, tests call `export_blake2b_deposit_triple` (needs SRS). Bootstrap: `scripts/bootstrap_hermez_srs_k18.sh`.

**Runtime:** ~5–7 min (SHPLONK verify in-process) — acceptable for deposit-relayer audit job, not unit-fast.

## Verdict: **partial META / QC — doc + CI smoke closed**

Documented asymmetry in PROJECT_FACTS / runbooks; CI smoke proves MockProver / struct verify insufficient for AN acceptance. Remaining gap: live AN opcode on shellnet (ops / TD-53).

## Commands

    bash scripts/check_mock_vs_shplonk_smoke.sh
    bash scripts/check_deposit_audit_gates.sh
    cd deposit-prover && cargo test --test td_43_mock_vs_shplonk -- --nocapture
    cd deposit-prover && cargo run --release --example verify_opcode_triple -- \
      --vk-blob fixtures/deposit_10proofs/deposit_vk_blob.bin \
      --proof fixtures/deposit_10proofs/proof_00/proof.bin \
      --pubin fixtures/deposit_10proofs/proof_00/public_inputs.bin --degree 18
