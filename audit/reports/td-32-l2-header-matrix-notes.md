# TD-32 — Per-L2 header shape matrix (717B, 16–21 fields)

PoC: `deposit-prover/tests/td_32_l2_header_matrix.rs`, fixtures `deposit-prover/fixtures/headers/`.

## Matrix (live `eth_getBlockByNumber` JSON, minus tx/withdrawal lists)

| Shape | Fixture | Fields | RLP bytes | ≤717 |
|-------|---------|--------|-----------|------|
| Arbitrum One (pre-Shanghai) | `arbitrum_one.json` | 16 | 551 | yes |
| Mainnet Shanghai | `mainnet_shanghai.json` | 17 | 574 | yes |
| OP Ecotone | `op_ecotone.json` | 20 | 584 | yes |
| Sepolia Prague | `sepolia_prague.json` | 21 | 644 | yes |
| OP Isthmus | `op_isthmus.json` | 21 | 635 | yes |

`HEADER_SHAPE_SAMPLES` in `rlp_utils.rs` exports the same five rows for unit + integration tests.

## Positive path

For each shape: `verify_block_header_rlp` → canonical RLP; `keccak256(rlp) == block.hash`; field count matches; encoded length ≤ `MAX_BLOCK_HEADER_BYTES` (717).

## Negative path (no silent pass)

| Mutation | Result |
|----------|--------|
| Prague fixture, drop `requestsHash` (20 fields) | `verify_block_header_rlp` rejects (`does not reproduce the block hash`) |
| Shanghai fixture, inject 32-byte `requestsHash` | Reject (hash mismatch) |

Truncated encoding hashes to a different value than `block.hash` — not accepted.

## BC-D06 / envelope QC

`BLOCK_HEADER_MAX_FIELD_LENS[21]` and `MAX_BLOCK_HEADER_BYTES=717` are compile-time coupled (`circuit_v2.rs`).

Integer slots 8 (`number`), 10 (`gasUsed`), 11 (`timestamp`) are widened to 8 bytes alongside slot 9 (`gasLimit`) so Arbitrum One `gasLimit` ≈ 2^50 (7 bytes) does not create a liveness cliff where `gasUsed`/`timestamp` decode fails while `gasLimit` fits.

**Not BC** — no shape outside the 717B / 21-field envelope passes `verify_block_header_rlp` or MockProver silently.

## Verdict: **QC**

Envelope and hash binding are correct for all five live shapes. Residual risk is operational/liveness (narrowing a slot below live chain data) — documented as BC-D06 QC, not a soundness break.

Sepolia Prague fixture closes the §3 “Sepolia fixture” gap for header shapes.

## Commands

    cd deposit-prover && cargo test td_32 -- --nocapture
    cd deposit-prover && cargo test header_samples -- --nocapture
    cd deposit-prover && cargo test
