# TD-66 — AAVE interleaving: donation + liquid reserve + supply/harvest

PoC: `audit/spec/ethereum/DepositAaveInterleaving.t.sol`.

`_amountSupplyable = balance - reserve`, `reserve = treasuryBalance × liquidReserveBps / BPS` (default 10%).

## Step table (case (a): D1=10, donate=2, D2=5 USDC units)

| Step | Action | treasury | liquid | suppliedPrincipal | amountSupplyable |
|------|--------|----------|--------|-------------------|------------------|
| 1 | deposit D1 | 10 | 10 | 0 | 9 |
| 2 | donate 2 | 10 | 12 | 0 | 11 |
| 3 | deposit D2 | 15 | 17 | 0 | 15.5 |
| 4 | supply(max) | 15 | 1.5 (reserve) | 15.5 | 0 |

Donation is liquid excess (TR-3 / A1-F4), not `treasuryBalance`; enters AAVE principal on supply.

## Other cases

| Case | Key check |
|------|-----------|
| (b) partial supply + D2 | manual `amountSupplyable` == `liquid - reserve` before second supply |
| (c) donate → deposit → harvest | treasury unchanged; yield → `yieldRecipient` |
| (d) liquid == reserve | `amountSupplyable == 0`; `supplyToAave(max)` → `NothingToSupply` |
| INV | treasury monotonic across deposit / donate / supply / harvest / skim |

## Cross-refs

| ID | Link |
|----|------|
| TD-35 | `DepositDonationAavePath.t.sol` — TR-3 donation ≠ treasury |
| TD-14 | Liquid reserve policy |
| A1-F4 | Donation locked as buffer until skim/supply |
| `EmergencyYield.t.sol` | post-emergency harvest path (orthogonal) |

## Verdict: **META/QC**

Interleaving PoC confirms reserve math and ledger isolation; not a BC (no invariant violation).

## Commands

    cd audit/spec/ethereum && forge test --match-path '*AaveInterleaving*' -q
    cd audit/spec/ethereum && forge test --match-path '*DonationAave*' -q
    cd audit/spec/ethereum && forge test --match-path '*EmergencyYield*' -q
