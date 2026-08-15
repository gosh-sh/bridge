# TD-58 — Pause asymmetry: docs vs code (#20)

PoC: `audit/spec/ethereum/DepositPauseAsymmetry.t.sol`, `DepositEdgeCases::test_deposit_noPauseGate_alwaysCallable`.

## Source truth matrix (`AckiNackiBridge.sol`)

| Function | Bridge pause gate? | External USDC pause? |
|----------|-------------------|----------------------|
| `deposit` | **No** (`nonReentrant` only) | Yes — `transferFrom` reverts (TD-24) |
| `verifyBlock` | **No** | N/A |
| `withdrawByProof` | **No** | N/A (payout `transfer`) |
| `applyBkSetUpdate` | **No** | N/A |
| `supplyToAave` | **No** (`onlyOwner`) | N/A |
| `withdrawFromAave` | **No** (`onlyOwner`) | N/A |
| `emergencyWithdrawAll` | **No** (`onlyOwner`) | N/A |
| `harvestYield` | **No** (`onlyOwner`) | N/A |

No `pause()`, `unpause()`, `whenNotPaused`, or `BridgePaused` in current bytecode (#20).

## Doc fixes (QC sweep)

| File | Change |
|------|--------|
| `docs/integration/an_partner_integration_plan.md` | Emergency pause changelog → OBSOLETED #20 |
| `docs/operations/testnet_security_status.md` | Removed `pause()` deploy default; deposit always callable |
| `audit/PROJECT_FACTS.md` | Bridge pause fact added |
| `audit/reports/manual-audit/A1-deposit-treasury.md` | Pause checklist → #20 |
| `audit/reports/manual-audit/A3-withdraw.md` | WD-10 / pause refs updated |
| `audit/reports/manual-audit/A4-aave-ac-verifiers.md` | Pause scope → removed |
| `audit/reports/eth-audit-plan.md` | Pause test matrix updated |

`AckiNackiBridgePause.t.sol` — **not in repo** (removed with #20); do not cite as existing.

## Verdict: **META/QC**

Doc drift fixed; behaviour intentional (no bridge pause gate). Token pause = external ops risk, not BC.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*Pause*' -vv
    cd audit/spec/ethereum && forge test --match-test test_deposit_noPauseGate -vv
    bash scripts/check_pi_count_docs.sh

From `contracts/ethereum`, use audit overlay root:

    cd contracts/ethereum && forge test --root ../../audit/spec/ethereum --match-path '*PauseAsymmetry*' -vv
