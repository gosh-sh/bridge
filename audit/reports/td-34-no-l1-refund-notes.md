# TD-34 — No L1 refund for stuck deposits

PoC: `DepositNoL1Refund.t.sol`.

Cross-ref: Phase 4.3 (`bridge_verification.md` §4), **TD-12/13** (unprovable envelope), **TD-26** (park/skip — no L1 USDC return).

## Recovery paths

| Path | User recoverable on L1? | Notes |
|------|---------------------------|-------|
| `deposit()` | — (funds in) | `treasuryBalance` += amount; USDC custody on bridge |
| Legacy `withdraw(depositId,…)` | **No** | Retired Phase 4.3 — selector call fails |
| User `withdrawByProof` | **No** (different flow) | AN→ETH payout; not deposit refund |
| Prove fail / AN reject | **No** | USDC stuck; relayer park/skip (TD-26) does not unwind L1 |
| Owner `emergencyWithdrawAll` | **No** (not selective) | Pulls AAVE → bridge wallet; not per-depositor refund |
| AN `finalizeDeposit` (later) | **AN side** | User recovery is cross-chain mint, not L1 pull |

## Scenario (TD-13 matrix class)

Deposit inside envelope → prove/MockProver or AN path fails → `treasuryBalance` and custody remain ≥ deposit amount; depositor balance unchanged at 0 after deposit.

Ops: manual `finalize-one`, fix proof, skip parked id (TD-26), or owner emergency for protocol custody — **not** end-user L1 refund.

## Verdict: **QC** (documented design + ops)

**Not BC** — PoC found no hidden user pull that returns deposit USDC to `msg.sender` without AN mint or owner global emergency.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*NoL1Refund*' -q
    cd audit/spec/ethereum && forge test --match-contract 'Deposit' -q
