# TD-65 — G3 live `is_finalized` / Revert loop (QC-OFF-06)

PoC: `td_65_g3_revert_loop.rs` (8 tests).  
Cross-ref: `f10_interface_reverted.rs`, `f10_competing_submit.rs`, TD-26 HOL, TD-37 competing finalize, TD-53 recovery runbook, `phase-g-status.md` G3.

Shellnet live E2E — **deferred** (E-AN-01).

## Exit-code matrix (recap)

| AN receipt | exit_code | `SubmitOutcome` | Relayer `tick` | Cursor |
|------------|-----------|-----------------|----------------|--------|
| `Confirmed` | — | `Finalized` | `Finalized` | advances |
| `Reverted` | none / ≠51 | `Rejected` | `AnRejected` | HOL (same id) |
| `Reverted` | 51 (`EXIT_CONSTRUCTOR_ALREADY_CALLED`) | `AlreadyFinalized` | `AlreadyFinalized` | advances |
| `Reverted` | 220 etc. | `Rejected` (+ ops hint) | `AnRejected` | HOL |
| `call_contract` abort | 51 in message | `AlreadyFinalized` | `AlreadyFinalized` | advances |
| `is_finalized` stub | — | skip prove if true | `AlreadyFinalized` | advances |

Exit 51 = duplicate voucher constructor — **not** generic nullifier string; unmapped duplicate reverts stay `Rejected` → QC-OFF-06 grief loop.

## `is_finalized` stub (QC-OFF-05)

`AnInterfaceSubmitter::is_finalized` always `Ok(false)` — no `usedDepositIds` read on `IAckiNacki`. After successful finalize, stub stays false → relayer always proves on forward path (gas waste, not double-mint). `MockAnSubmitter` tracks nullifier mirror for tests.

## Cross-refs

| TD / doc | Link |
|----------|------|
| TD-37 | Competing `finalize-one` / `AlreadyFinalized` unlock |
| TD-26 | HOL blocking when generic `Rejected` |
| TD-53 | Operator recovery: prove-one → finalize-one → restart |
| `f10_interface_reverted.rs` | Generic Revert → `Rejected` (included in smoke) |

## CI hooks (TD-65 mock smoke wired)

| Hook | When |
|------|------|
| `scripts/check_td65_g3_smoke.sh` | standalone / aggregator |
| `scripts/check_deposit_audit_gates.sh` | after TD-53 (RELAYER block) |
| `make audit-deposit-relayer-test` | full gate bundle |
| `.gitlab-ci.yml` `test:deposit-relayer:audit` | gates + td_65 paths |

**Runtime:** ~1s (`td_65` 8 tests + optional `f10_interface_reverted`).

Env `TD65_SKIP_F10_INTERFACE=1` skips f10 cross-ref (CI runs full smoke by default).

## Verdict: **partial META / QC — mock CI smoke wired**

Exit 51 → `AlreadyFinalized` path gated in CI; generic Revert → HOL documented. Live `is_finalized` / shellnet = ops.

## Commands

    bash scripts/check_td65_g3_smoke.sh
    cd crates/deposit-relayer-daemon && cargo test td_65 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test --test f10_interface_reverted -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test f10_competing -- --nocapture
