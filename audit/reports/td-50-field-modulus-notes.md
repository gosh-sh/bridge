# TD-50 — Field modulus / BN254 aliasing for deposit PI

PoC: `td_50_field_modulus_pi.rs`, `td_50_pi_modulus_binding.rs`.

Cross-ref: TD-02 layout, TD-21 `promiseCommit`, L1 `BN254_R` (`blockId` reduction).

## Per-slot matrix

| Slot | Label | Encoding | Byte domain | `< r` guaranteed? | Range-check | Verdict |
|------|-------|----------|-------------|-------------------|-------------|---------|
| 0 | depositId | Horner BE 32B | topic; top byte 0 | Yes (< 2^248) | top byte = 0 | OK |
| 1 | sender | Horner BE 32B | topic; 12 zero + 20 addr | Partial | topic shape | QC |
| 2 | amount | Horner BE 32B | log word 0 | Yes (< 2^128) | high 16B = 0 | OK |
| 3 | contractAddress | Horner BE 32B | log addr padded | Partial | 12 zero pad | QC |
| 4 | chainId | tx RLP field | EIP-1559 | Small | MPT + type | OK |
| 5–6 | dappIdHigh/Low | Horner 16B halves | config witness | Half < 2^128 | **8-bit per byte** (BC-D05) | OK |
| 7–8 | anAccount* | Horner 16B halves | log word 2 | Half < 2^128 | MPT bytes | QC |
| 9–10 | blockHash* | Horner 16B halves | keccak header | Half < 2^128 | byte-wise root bind | QC |
| 11 | promiseCommit | circuit append | keccak promise | Circuit | opcode | OK |

BN254 `r` = `0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001`.

## Mutation probes

| Probe | Result |
|-------|--------|
| depositId / chainId baseline | MockProver pass, Fr < r |
| amount low-half `0xFF` (high zero) | pass; PI = Horner(amount) |
| dappId witness byte `Fr(256)` | **reject** (BC-D05 range_check) |
| block hash 16B halves | Fr < r; ~injective on half |
| Horner(`0`) == Horner(`p` BE) | same Fr = 0 (alias demo) |
| relayer amount mismatch | `check_binds_to` **reject** |
| proof_00 operand scalars | all 12 `< r` |

Relayer decodes PI as `U256::from_le_bytes` per slot — does not detect Fr-alias byte strings (QC); circuit phase-1 byte constraints limit practical aliasing on bound event fields.

## DEP-FUZZ-AMOUNT-WORD

Amount uses 32-byte Horner fold with **high 16 bytes forced zero** in phase 1 → injective below 2^128 and AN `uint128` safe. Fuzz target: low-half boundary (`0xFFFF…` in low 16 bytes).

## Verdict: **META / QC** (not BC)

No exploitable aliasing path found for bound deposit fields under current constraints. Residual QC: unsplit Horner slots (sender/contract) and relayer U256-only equality.

## Commands

    cd deposit-prover && cargo test td_50 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_50 -- --nocapture
    bash scripts/check_pi_count_docs.sh
