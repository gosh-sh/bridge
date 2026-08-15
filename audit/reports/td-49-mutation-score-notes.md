# TD-49 — Mutation testing score (DEP-MUTATION-TESTING / D-25)

PoC: `td_49_mutation_kill_matrix.rs`, `DepositMutationKill.t.sol`, `deposit-prover/tests/td_49_mutation_kill.rs`.  
Cross-ref: TD-01 (partial PI bind), TD-05 (dappId), TD-21 (promiseCommit), TD-25 (reentrancy), TD-31 (anWorkchain).

## Pinned score (META)

**KILLED 13/13 (100%)** on the documented kill set.  
**Survivors: 3 QC (intentional)** — not counted against score; not audit gaps.

| Survivor | Why QC | Cross-ref |
|----------|--------|-----------|
| `dappId` hi/lo flip | Config/AN-bound, not L1 event field | TD-05 |
| `promiseCommit` flip | Not in relayer `check_binds_to` | TD-21 |
| `anWorkchain` event flip | Not PI-bound at relayer | TD-31 / TD-44 |

## Mutant → test (kill / survive)

| Mutant | Layer | Killing test | Result |
|--------|-------|--------------|--------|
| flip `depositId` PI | relayer | `td_49_kill_matrix_each_bound_field_flip_rejected` | kill |
| flip `sender` | relayer | same | kill |
| flip `amount` | relayer | same | kill |
| flip `contractAddress` | relayer | same | kill |
| flip `chainId` | relayer | same | kill |
| flip `anAccount` | relayer | same | kill |
| flip `blockHash` | relayer | same | kill |
| flip `dappId` hi/lo | relayer | `td_49_survivor_dapp_id_flip_not_event_bound` | survive QC |
| flip `promiseCommit` | relayer | `td_49_survivor_promise_commit_flip_not_event_bound` | survive QC |
| flip `anWorkchain` event | relayer | `td_49_survivor_an_workchain_not_in_bind` | survive QC |
| skip `transferFrom` | L1 | `test_td49_mutant_no_transfer_treasury_and_counter_unchanged` | kill |
| returnless transfer | L1 | `test_td49_mutant_returnless_transfer_kill` | kill |
| missing `Deposit` emit | L1 overlay | `test_td49_mutant_missing_emit_breaks_overlay` | kill |
| remove `nonReentrant` | L1 | `test_td49_mutant_reentrancy_guard_reference_td25` | kill (TD-25) |
| happy treasury growth | L1 TR-1 | `test_td49_happy_path_treasury_matches_transfer` | control |
| flip PI `depositId` | circuit | `td_49_circuit_kill_deposit_id_flip` | kill |
| flip PI `amount` | circuit | `td_49_circuit_kill_amount_flip` | kill |

Kill set count: **7 relayer bound + 4 L1 + 2 circuit = 13 killed**.

## Regression cross-ref

| TD | Link to TD-49 |
|----|----------------|
| TD-25 | `nonReentrant` removal mutant — `test_td49_mutant_reentrancy_guard_reference_td25` |
| TD-55 | Failed deposit ledger frozen — `DepositMutationKill` counter/treasury freeze mutants |
| TD-61 | Allowance/dust — separate from kill matrix; L1 transfer mutants overlap TR-1 overlay |

## CI hooks (TD-49 smoke wired)

| Hook | When |
|------|------|
| `scripts/check_mutation_kill_smoke.sh` | standalone / aggregator |
| `scripts/check_deposit_audit_gates.sh` | after TD-43 mock≠SHPLONK (META block) |
| `make audit-deposit-relayer-test` | full gate bundle |
| `.gitlab-ci.yml` `test:deposit-relayer:audit` | gates + td_49 paths |

**Runtime:** relayer + forge ~20s; circuit MockProver kills ~3 min (acceptable in F10 job).

## Verdict: **partial META / QC — score pinned + CI smoke**

13/13 kill set documented and gated; 3 survivors are QC by design (not event-bound at relayer).

## Commands

    bash scripts/check_mutation_kill_smoke.sh
    bash scripts/check_deposit_audit_gates.sh
    cd crates/deposit-relayer-daemon && cargo test td_49 -- --nocapture
    cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test --match-path '*MutationKill*' -q
    cd deposit-prover && cargo test td_49 -- --nocapture
