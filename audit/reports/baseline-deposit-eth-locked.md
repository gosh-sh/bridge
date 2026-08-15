# Baseline locked — ETH deposit (closed by tests)

**Не вопросы к автору** — регрессионные якоря. Обновлять только при изменении кода/инварианта.

| Invariant / ID | Test file | Key test |
|----------------|-----------|----------|
| DEP-1 zero `anAccount` | `DepositNegative.t.sol` | `test_deposit_zeroAnAccount_reverts` |
| DEP-2 zero / over max amount | `DepositNegative.t.sol` | `test_deposit_zeroAmount_*`, `test_deposit_exceedsMax_*` |
| DEP-2 exact max | `DepositEdgeCases.t.sol` | `test_deposit_exactMaxAmount_succeeds` |
| DEP-3 treasury += amount | `FuzzDepositToken.t.sol` | `testFuzz_deposit_exactTransferFromAmount` |
| DEP-4 nonReentrant | `DepositEdgeCases.t.sol` | `test_deposit_reentrantToken_reverts` |
| DEP-5 counter / event id | `DepositCounter.t.sol`, `InvariantsDeposit.t.sol` | monotonic + handler |
| DEP-6 isolation from VB state | `DepositIsolation.t.sol` | `test_deposit_doesNotMutateVerifyBlockOrWithdrawState` |
| QC-A1-1 cap policy | `DepositWhaleCap.t.sol` | aggregate unbounded at `uint64.max` |
| A4-Q2 no pause | `DepositEdgeCases.t.sol` | `test_deposit_noPauseGate_alwaysCallable` |
| TR-3 donation | `DepositEdgeCases.t.sol` | `test_directUsdcTransfer_doesNotCreditTreasury` |
| TR-4 exact transfer | `FuzzDepositToken.t.sol` | fuzz |
| QC-OFF-07 full bind | `types.rs` | `binding_check_rejects_*`, `bundle_binding_check` |
| QC-OFF-08 deployment bind | `state.rs` | `deployment_mismatch_rejected`, `state_lock_blocks_second_writer` |
| QC-OFF-12 ABI | `f10_e_abi.rs` | stale ABI inverted |
| QC-PROV-03 key len 3 | `padding_mutation_poc.rs` | max_key_byte_len=3 path |
| QC-PROV-01 12 PI | `f10a_binding.rs`, `circuit_v2.rs` | twelve public inputs |
| QC-AN-J4 workchain ignored on AN | `DepositWorkchain.t.sol` | any `int8` emitted on L1 |
| QC-ETH-DEP-01 sender binding | `DepositEdgeCases.t.sol` | `sender` = `msg.sender`; no third-party pull |
