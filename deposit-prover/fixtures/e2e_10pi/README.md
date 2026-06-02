# Deposit opcode e2e fixtures — 10 public inputs (AN-recipient bound)

Canonical, version-controlled `(vk_blob, public_inputs, proof)` triple for the
EVM→AN deposit proof, consumed by the AN-side `ZKHALO2VERIFYWITHVK`
(`0xC7 0x4A`) opcode via the isolated `an_rlc_verify` verifier.

This is the **10-public-input** layout (AN recipient bound in-circuit):

| # | field            | source                                   |
|---|------------------|------------------------------------------|
| 0 | depositId        | Deposit event topic[1]                   |
| 1 | sender           | Deposit event topic[2]                    |
| 2 | amount           | Deposit event data word 0                 |
| 3 | anWorkchain      | Deposit event data word 1 (`int8`)        |
| 4 | anAccountHigh    | Deposit event data word 2, high 128 bits  |
| 5 | anAccountLow     | Deposit event data word 2, low 128 bits   |
| 6 | contractAddress  | log address                               |
| 7 | blockHashHigh    | keccak(block header) high 128 bits        |
| 8 | blockHashLow     | keccak(block header) low 128 bits         |
| 9 | promiseCommit    | keccak coprocessor Poseidon promise commit|

## Files

| file                              | bytes | notes                                       |
|-----------------------------------|-------|---------------------------------------------|
| `deposit_vk_blob.bin`             | 3597  | v2 RLC VkBlob (k=18), opcode-readable        |
| `deposit_public_inputs.bin`       | 320   | 10 × 32-byte LE Fr                           |
| `deposit_proof_blake2b.bin`       | 8000  | Blake2b-transcript SHPLONK proof             |
| `deposit_eth_circuit_params.json` | —     | EthCircuitParams embedded in the VkBlob      |
| `deposit_proof_input.json`        | —     | DepositProofInput that produced the triple   |

## Provenance

Generated from a real `deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)`
call against a local Anvil deployment of the updated `AckiNackiBridge` (amount
= 50 USDT, anWorkchain = 0, anAccount = 0x…deadbeef), then:

```bash
cd deposit-prover
# 1. VkBlob (keygen, k=18 — matches the opcode's hardcoded --degree 18 SRS)
cargo run --release --example export_vk_blob -- \
  --input fixtures/e2e_10pi/deposit_proof_input.json \
  --output fixtures/e2e_10pi/deposit_vk_blob.bin \
  --config-out fixtures/e2e_10pi/deposit_eth_circuit_params.json \
  --degree 18 --max-data-byte-len 256 --max-log-num 20

# 2. proof + public inputs
cargo run --release --example export_blake2b_proof -- \
  --input fixtures/e2e_10pi/deposit_proof_input.json \
  --proof-out fixtures/e2e_10pi/deposit_proof_blake2b.bin \
  --pubin-out fixtures/e2e_10pi/deposit_public_inputs.bin \
  --degree 18 --max-data-byte-len 256 --max-log-num 20

# 3. verify via the exact binary the opcode spawns
cargo build --release --bin an_rlc_verify
./target/release/an_rlc_verify \
  --vk-blob fixtures/e2e_10pi/deposit_vk_blob.bin \
  --proof fixtures/e2e_10pi/deposit_proof_blake2b.bin \
  --pubin fixtures/e2e_10pi/deposit_public_inputs.bin \
  --degree 18 --srs-dir data --expect-pi 10   # exit 0 = ACCEPT
```

The SRS used for both keygen and verify is `deposit-prover/data/kzg_bn254_18.srs`
(the 0x4A RLC path is process-isolated and reads the SRS from `--srs-dir`; there
is no separately-embedded ceremony, unlike the in-process Dark DEX `0x49` opcode).

Consumed by tvm-sdk PR #248 `tvm_vm/src/tests/test_halo2.rs`
(`test_zkhalo2_with_vk_deposit_10pi_triplet`); override the dir with the
`DEPOSIT_E2E_DIR` env var.
