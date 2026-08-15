# TD-21 — `promiseCommit` slot 11

PoC: `tests/td_21_promise_commit.rs`, `test_circuit_mock_with_pi_corruption`, relayer `td_21_promise_commit.rs`.

## Layout (12 PI)

| Index | Name | Set by |
|-------|------|--------|
| 0–10 | depositId … blockHashLow | `virtual_assign_phase0` |
| **11** | **promiseCommit** | **EthCircuitImpl** (keccak coprocessor commitment) |

`promiseCommit` binds all in-circuit `keccak256` / MPT paths via the Poseidon promise loader (see `docs/operations/verifying_eth_proof_on_an.md`).

## Verdict: **OK**

1. Baseline `proof_00` slot 11 non-zero and stable.
2. Flip slot 11 (or slots 0, 9, 10 control) → MockProver **reject**.
3. Relayer `check_binds_to` does **not** compare `promiseCommit` to event — **QC** (AN opcode + VK verify the full 12-scalar cell).

**Not BC:** corrupted `promiseCommit` does not satisfy circuit.

## Commands

    cd deposit-prover && cargo test td_21 -- --nocapture
    cd deposit-prover && cargo test
