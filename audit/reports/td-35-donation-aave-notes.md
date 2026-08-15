# TD-35 — Donation → AAVE → skim vs treasury ledger

PoC: `DepositDonationAavePath.t.sol`.

Cross-ref: **TD-14** (`DepositTR1Interleaved.t.sol`), **TR-3**, `EmergencyYield.t.sol` (QC-A1-3).

## Step semantics (DEPOSIT=10 USDC, DONATION=2 USDC, 10% reserve)

| Step | treasuryBalance | liquid USDC | suppliedPrincipal | excessUsdc |
|------|-----------------|-------------|-------------------|------------|
| After `deposit()` | +10 | +10 | 0 | 0 |
| After direct donation | unchanged (+10) | +12 | 0 | 2 |
| After `supplyToAave(max)` | +10 | reserve (~1) | ~11 | 0 |
| After accrue + `harvestYield` | +10 | reserve | ~11 | 0 |
| After post-harvest donation + `skimExcessUsdc` | +10 | ~10 | ~11 | 0 (donation → yieldSink) |

Reserve = `treasuryBalance × liquidReserveBps / 10000` (default 10%). Supplyable liquid includes donation above reserve — donation can become AAVE principal without ledger credit.

## Verdict: **QC** (TR-3 / QC-A1-3)

1. Direct transfer never credits `treasuryBalance` (TR-3).
2. Donation may be supplied to AAVE or skimmed as `excessUsdc` to `yieldRecipient` — **not** user withdrawable principal.
3. **Not BC** — `treasuryBalance` unchanged by donation/skim; no user pull of donated funds as deposit refund.

Ops: operators must treat unsolicited USDC as yield/excess, not depositor liability on ledger.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*DonationAave*' -q
    cd audit/spec/ethereum && forge test --match-contract 'InvariantsTreasury' -q
