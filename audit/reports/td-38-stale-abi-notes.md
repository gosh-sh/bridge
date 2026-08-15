# TD-38 — Stale finalize ABI / PI byte reorder

PoC: `td_38_abi_pi_reorder.rs`.  
Cross-ref: QC-OFF-12, TD-02 layout, `f10_e_abi.rs`, TD-01 bind mutations.

## Mutation → bind / submit outcome

| Mutation | `check_binds_to` | `submit` (mock) |
|----------|------------------|-----------------|
| Canonical operand + `parsed` | ok | `Finalized` |
| Swap PI slots 0↔2, re-decode `parsed` | fail (depositId/amount) | `Err` (no mint) |
| Swap operand bytes only (`parsed` stale) | ok (QC gap) | would send drifted bytes |
| Permuted `encode_finalize` scalars | ≠ `parsed` fields | n/a (diagnostic) |
| ABI: `finalizeDeposit` 2×`bytes` | — | matches `build_finalize_deposit_params` |

Relayer binds via decoded `parsed` struct, not a second pass over `public_inputs` bytes before submit. Operand/`parsed` drift without re-decode is **QC** — AN opcode rejects mismatched witness (not BC).

Legacy `confirmDeposit` (6 args) ≠ `finalizeDeposit(proof, publicInputs)`.

## Verdict: **QC** (QC-OFF-12 partial OK)

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_38 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_e_abi -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
