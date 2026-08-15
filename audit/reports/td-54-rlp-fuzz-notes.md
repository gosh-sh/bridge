# TD-54 — RLP non-canonical fuzz (`rlp_utils` vs alloy)

PoC: `tests/td_54_rlp_noncanonical_fuzz.rs`.

## Artifact → encoder → attack → outcome

| Artifact | Canonical encoder | Non-canonical attack | Outcome |
|----------|-------------------|----------------------|---------|
| Block header (TD-32 matrix) | `encode_block_header` (ethers `rlp` 0.5.2) | truncate tail / inflate list prefix | `keccak ≠ block.hash` |
| Block header Prague | `verify_block_header_rlp` | drop `requestsHash` from `other` | **Err** «does not reproduce the block hash» |
| Block header Shanghai | `verify_block_header_rlp` | forged `requestsHash` in `other` | **Err** hash mismatch |
| `proof_00` header witness | pinned bytes in fixture | truncate (implicit) | MPT/circuit reject (TD-09) |
| `proof_00` receipt witness | on-chain typed `0x02` | truncate receipt RLP | circuit reject (TD-09) |
| Receipt trie key | `encode_tx_index` (`alloy_rlp`) | flip key byte | different trie path (TD-22) |
| `encode_tx_index` | `alloy_rlp::Encodable` | — | **parity** exact bytes |
| `typed_tx_chain_id` | manual decode | truncated `0x02` prefix | **Err** (BC-D09, unit in `rlp_utils`) |

Note: block headers intentionally use ethers `rlp` crate (axiom-eth parity), not `alloy_rlp`. Canonical check = `verify_block_header_rlp` (`keccak(encoded) == block.hash`).

## Parity pass count (PoC)

| Suite | Pass count |
|-------|------------|
| `HEADER_SHAPE_SAMPLES` verify + byte identity | 5 |
| `proof_00` header witness well-formed | +1 |
| `proof_00` receipt typed + tx-index alloy parity | +1 |
| **Total canonical parity assertions** | **7** |

## Cross-refs

| TD | Link |
|----|------|
| TD-32 | Header shape matrix |
| TD-09 | MPT / receipt RLP truncation |
| TD-22 | Tx index trie coupling |
| TD-64 | Full chain corpus (open) |

## Verdict: **META/QC**

Canonical samples agree on hash-bound path; non-canonical mutations do not silently produce alternate canonical hashes at `verify_block_header_rlp`. No BC silent-accept in PoC scope.

## Commands

    cd deposit-prover && cargo test td_54 -- --nocapture
    cd deposit-prover && cargo test td_32_l2_header_matrix -- --nocapture
    cd deposit-prover && cargo test td_12_eip1559_only -- --nocapture
