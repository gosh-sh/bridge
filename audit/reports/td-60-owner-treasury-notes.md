# TD-60 — Owner paths do not reduce treasury without withdraw proof

PoC: `DepositOwnerTreasuryGuard.t.sol`, `OwnerPrincipal.t.sol`, `InvariantsOwner.t.sol` + `OwnerOpsHandler`.

## Owner ops matrix

| Function | `treasuryBalance` | Notes |
|----------|-------------------|-------|
| `supplyToAave` | unchanged | moves liquid → AAVE book |
| `withdrawFromAave` | unchanged | AAVE → liquid |
| `emergencyWithdrawAll` | unchanged | principal book cleared; liquid may include yield |
| `harvestYield` | unchanged | yield → `yieldRecipient` only |
| `skimExcessUsdc` | unchanged | skims `excessUsdc()` above ledger |
| `setAaveEnabled` | unchanged | config |
| `setLiquidReserveBps` | unchanged | config |
| `setYieldRecipient` | unchanged | config |
| `transferOwnership` | unchanged | config |

Principal decrease: **only** `withdrawByProof` (valid Circuit 4 proof + nullifier). No owner `transfer` of user principal.

## Skim / donation path

Donation → `excessUsdc()`; `skimExcessUsdc` reduces liquid USDC, not `treasuryBalance`. `skim > excess` → `NoExcessUsdc`.

## Fuzz cross-ref

`InvariantsOwner.t.sol` — `invariant_A4INV1_ownerOpsNeverReduceTreasury`.  
`OwnerOpsHandler` — includes `skimExcessMax` (TD-60 extension).

## Verdict: **INV/OK**

Owner centralization: yield skim + AAVE ops only; not BC (no treasury theft without proof).

## Commands

    cd audit/spec/ethereum && forge test --match-contract OwnerPrincipal -vv
    cd audit/spec/ethereum && forge test --match-path '*OwnerTreasury*' -vv
    cd audit/spec/ethereum && forge test --match-contract InvariantsOwner -vv --fuzz-runs 64
