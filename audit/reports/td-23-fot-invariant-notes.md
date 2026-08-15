# TD-23 — Fee-on-transfer in stateful TR-1 invariant

PoC: `audit/spec/ethereum/DepositFoTInvariant.t.sol`, handler `FoTTreasuryHandler`.

## Assumption gate

`PROJECT_FACTS.md`: bridge assumes standard ERC-20 — exact `transferFrom` credit. FoT tokens violate TR-1:

`liquid + principal < treasuryBalance` after `deposit(amount)`.

## Verdict: **QC** (QC-A1-2 / DEP-FOT-ASSUME)

| Check | Outcome |
|-------|---------|
| TR-1 with FoT deposits | **Fails** (documented inverted invariant + unit asserts) |
| Nominal `treasuryBalance` vs custody | Gap = `amount * feeBps / 10000` |
| TR-1 green under FoT | **No** → not BC |
| Production USDC fix | Out of scope — trust assumption |

## Stateful campaign

`FoTTreasuryHandler.depositFoT` in fuzz; inverted invariant `invariant_TR1_FoT_solvency_gap_after_deposit` passes when gap exists.

Fee bps in campaign: 5% (`FOT_FEE_BPS=500`); unit tests cover 1% and 10%.

## Commands

    cd audit/spec/ethereum && forge test --match-contract InvariantsTreasury -q
    cd audit/spec/ethereum && forge test --match-path '*FoT*' -q
    cd audit/spec/ethereum && forge test --match-contract 'Deposit' -q
