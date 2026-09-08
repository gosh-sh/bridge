# TD-23 — Fee-on-transfer in stateful TR-1 invariant

PoC: `audit/spec/ethereum/DepositFoTInvariant.t.sol`, handler `FoTTreasuryHandler`.

## Assumption gate

`PROJECT_FACTS.md`: bridge assumes standard ERC-20 — exact `transferFrom` credit. ETH-11 enforces that: a fee-on-transfer or rebasing token reverts `TransferAmountMismatch` instead of crediting `treasuryBalance`.

## Verdict: **QC closed fail-closed** (ETH-11)

| Check | Outcome |
|-------|---------|
| FoT `deposit` | **Reverts** `TransferAmountMismatch` |
| `treasuryBalance` / custody after revert | Both stay 0 |
| TR-1 after FoT attempts | **Holds** (nothing credited) |
| FoT-aware accounting (credit net) | Still out of scope |

## Stateful campaign

`FoTTreasuryHandler.depositFoT` attempts a FoT deposit and expects revert; invariant `invariant_TR1_FoT_deposit_does_not_credit` asserts empty ledger.

Fee bps in campaign: 5% (`FOT_FEE_BPS=500`); unit tests cover 1% and 10%.

## Commands

    cd audit/spec/ethereum && forge test --match-contract InvariantsTreasury -q
    cd audit/spec/ethereum && forge test --match-path '*FoT*' -q
    cd audit/spec/ethereum && forge test --match-contract 'Deposit' -q
