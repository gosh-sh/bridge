# USDC custody and accounting

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `a69ba36`, 2026-08-19.
Line references are to that file. Written by reading the source; earlier revisions of this material
were carried over from the ETH-deposit era and are superseded by it.*

What the bridge owes, what it actually holds, and the exact order of operations on each path. This
page exists because those three are easy to conflate, and the labelled rules below are what auditors
and operators quote at each other during an incident.

**Scope: the EVM side only** — `AckiNackiBridge.sol` and the AAVE position it holds. The
Acki Nacki side keeps per-chain accounting in `contracts/an/exchange/eccUSDCBridge.sol`;
its limits and deployment requirements are described in `contracts/an/README.md`. Where the two meet is the deposit
proof, covered in [`EVM-contracts-spec.md`](EVM-contracts-spec.md) §6.

## The book number is not a balance

`treasuryBalance` (`:108`) is **book value**: the sum of user principal the bridge owes. It is not a
token balance and is never reconciled against one.

It moves in four places in the whole contract (lines as of gosh-sh/bridge-audit#140):

* `+= credited` in `deposit` (`:856`)
* `-= pub.amount` in `withdrawByProof` (`:1587`)
* `+= amount` in `fundTreasury` (`:1807`), owner or `predecessor` only, for USDC it pulls from the
  caller, with no `Deposit` event
* `= 0` in `migrateTreasury` (`:1836`), owner only on a paused bridge, which hands the whole book
  to a successor (CUST-2)

Nothing else writes it — in particular not the AAVE routing functions, which move tokens between
the bridge and the pool without touching the book at all.

## Where the money actually is

Custody is split across two places, and the split is legitimate:

* liquid USDC on the contract — `usdc.balanceOf(bridge)`
* the AAVE position — `aUsdcBalance()` (`:1387`), booked at `suppliedPrincipal()` (`:123`)

`totalAssets()` (`:1400`) is their sum. So the solvency statement is an **inequality, not an
equality**:

```
usdc.balanceOf(bridge) + aUsdcBalance()  ≥  treasuryBalance
```

There is no invariant that liquid USDC equals `treasuryBalance`, and there should not be — part of
the principal is deliberately lent out. Any text claiming the two are kept in sync is wrong.

Fuzz-tested as `testFuzz_totalAssetsCoversTreasury`
(`contracts/ethereum/test/AckiNackiBridgeAave.t.sol:282`).

## Deposit — the external call comes first, deliberately

Actual order in `deposit` (`:578-593`):

1. **Checks** — `amount != 0` (`InvalidAmount`), `amount <= MAX_DEPOSIT_AMOUNT` (`DepositTooLarge`),
   `amount <= MAX_TRANSFER_AMOUNT` (`TransferTooLarge`),
   `anAccount != 0` (`InvalidAnAccount`), and
   `treasuryBalance + amount <= MAX_BRIDGE_TVL` (`TvlCapReached`).
2. **Interaction** — `usdc.transferFrom(msg.sender, address(this), amount)`; a `false` return reverts
   `TransferFromFailed` (`:585-587`).
3. **Effects** — `depositCounter++`, then `treasuryBalance += amount` (`:588-590`).
4. **Event** — `Deposit(depositId, sender, amount, anAccount, timestamp)`.

So on this path the interaction precedes the effects. That is the correct order here — the bridge
books only what it has actually received — and it is safe for two independent reasons: the function
is `nonReentrant` (`:419-424`), and `usdc` is `immutable` (`:111`), bound at construction, so the
callee is a fixed known token rather than caller-supplied.

State it this way rather than as "checks-effects-interactions", which this path does not follow.

Two consequences worth naming:

* `MAX_DEPOSIT_AMOUNT` is the first width guard, `type(uint64).max`.
  `MAX_TRANSFER_AMOUNT` adds the lower 50 USDC ceiling on deposits and withdrawals;
  `MAX_BRIDGE_TVL` refuses a deposit that would take `treasuryBalance` past 10,000 USDC
  per L1 bridge. Aave supply, yield and donations do not reset that capacity.
  `fundTreasury` is not checked against it, so a migrated or owner-funded book can sit
  above the cap; deposits then stop until payouts bring it back under. The amount remains a parameter;
  deposits are not payable and have no minimum beyond `amount != 0`.
* The booked figure is the **requested** `amount`, not an observed balance delta. Exact for USDC;
  it would understate custody for a fee-on-transfer token, which is why the token is fixed at
  construction.

## Payout — effects before interactions

`withdrawByProof` runs the other way round, and the source says so in its own comments
(`:1192`, `:1196`):

1. Verify the Circuit-4 proof; reject with `WithdrawalProofRejected` (`:1185`).
2. **Solvency check before touching AAVE**: `pub.amount > treasuryBalance` reverts
   `WithdrawTreasuryShortfall` (`:1188-1190`).
3. **Effects** — mark the nullifier used, `treasuryBalance -= pub.amount` (`:1193-1194`).
4. **Interactions** — pull the shortfall from AAVE only if liquid USDC is short (`:1199-1205`), then
   `usdc.transfer(recipient, amount)` (`:1208`).

A revert anywhere in step 4 rolls back steps 2–3 atomically, so a failed AAVE pull cannot leave the
book decremented.

The two paths differ on purpose: a deposit must confirm receipt before crediting, a payout must
debit before paying.

## Pause is two switches, not one

`AckiNackiBridge.pause()` stops the Ethereum user entry points (`deposit`,
`verifyBlock`, `applyBkSetUpdate`, `withdrawByProof`). It does not call
`eccUSDCBridge.setPaused`, and `setPaused` does not call `pause()`. Circle can
also freeze the USDC contract on Ethereum; that is a third control, not wired
to either bridge.

The AN side keeps its own ECC ledger. A pause here leaves mint and burn on
Acki Nacki running; a pause there leaves Ethereum deposits and payouts
running. Stopping both directions of the bridge in an incident means flipping
**both** owner switches. There is no single stop switch (QC-AN-J5).

## USDC issuer risk is accepted

*Checked against `AckiNackiBridge.sol` at `3d519742`; the line references in this section are to
that commit, not to the one in the header.*

The bridge is built for Circle's standard USDC, fixed at construction (`usdc`, `:120`, `:693`).
Circle can pause the USDC contract or blacklist an address, and the bridge has no defence against
either. This is accepted as the issuer's risk and is not mitigated in the bridge (audit QC-A1-2).
What it means:

* While USDC is paused, or for a blacklisted depositor, `deposit` reverts inside `_pullExactUsdc`
  (`:787`, `:1551`) and nothing is booked.
* While USDC is paused, or for a blacklisted recipient, `withdrawByProof` reverts inside
  `_pushExactUsdc` (`:1525`), and the nullifier and `treasuryBalance` (`:1512`) roll back with it.
  The proof fixes the recipient, so the payout can only be retried once Circle lifts the block, and
  only while the proof's anchor is still in its window (Case 3e of
  [`live_withdrawByProof_runbook.md`](../crates/bridge-relayer-daemon/docs/live_withdrawByProof_runbook.md#case-3e--anchor-eviction-deadline----anchor-layer-runway-is-finite)).
  After that the amount stays in `treasuryBalance` for good.
* A blacklisted bridge address freezes all custody: the liquid USDC, and the AAVE position too,
  since `withdrawFromAave` can only send to the bridge itself (`:1632`). The contract has no path
  around it.

## AAVE routing never moves the book

The supply in `deposit`, the pull in `withdrawByProof`, `withdrawFromAave` and
`emergencyWithdrawAll` move tokens between the bridge and the pool and adjust `suppliedPrincipal`
only — `deposit` and `withdrawByProof` change the book by the deposit or the payout, never by what
they move to or from the pool; `writeOffUnbackedPrincipal` adjusts `suppliedPrincipal`
and moves nothing. `harvestYield` and `skimExcessUsdc` send surplus to `yieldRecipient` and are
each bounded by `treasuryBalance`: `skimExcessUsdc` takes at most `excessUsdc()`, the liquid USDC
above the principal that a pool loss does not need, and
`harvestYield` reverts if liquid USDC + aUSDC would drop below it. That bound, not the
`suppliedPrincipal` book, is what keeps them from reaching user principal. Operating detail:
[`aave-yield.md`](aave-yield.md).

## Labelled rules, restated

Quote these, not their earlier versions.

| | Rule |
|---|---|
| **DEP-1** | Every successful `deposit` emits `Deposit(depositId, sender, amount, anAccount, timestamp)` with a unique monotonic `depositId`; `anAccount == 0` reverts `InvalidAnAccount`. |
| **DEP-2** | `amount == 0` reverts `InvalidAmount`; `amount > type(uint64).max` reverts `DepositTooLarge`; otherwise an amount above 50 USDC reverts `TransferTooLarge`, and principal above 10,000 USDC reverts `TvlCapReached`, before a token call. The function is not `payable`. |
| **DEP-3** | A successful `deposit` makes exactly two state mutations: `depositCounter++` and `treasuryBalance += amount`. Apart from `deposit`, only `fundTreasury` (owner or `predecessor`, no `Deposit` event) increases `treasuryBalance`; `withdrawByProof` decreases it, and `migrateTreasury` zeroes it while handing it to a successor bridge. |
| **DEP-4** | `deposit` is `nonReentrant` and makes exactly one external call, `usdc.transferFrom`, to the `immutable` token bound at construction. It happens **before** the state mutations. |
| **CUST-1** | `treasuryBalance` is book value, not a balance. The solvency invariant is `totalAssets() ≥ treasuryBalance`, never equality. |
| **CUST-2** | Owner functions can move custody between the bridge and AAVE, and can send only the surplus above `treasuryBalance` to `yieldRecipient`. The exception is `migrateTreasury`: on a paused bridge it sends the whole `treasuryBalance` to a contract whose `predecessor()` returns this bridge, through that contract's `fundTreasury`. The contract cannot tell a real successor bridge from any other contract that gives that answer, so this is an owner power over the whole book, not a guarantee about where it goes. `fundTreasury` (owner or `predecessor`), the other owner function that writes `treasuryBalance`, only raises it by USDC the caller supplies. |
| **CEI-1** | `withdrawByProof` mutates state before every external call. `deposit` does not, and does not need to — see above. A blanket "all external interactions follow CEI" claim is false for this contract. |

## What earlier text got wrong

Kept short so anyone holding an older copy can reconcile it: the deposit cap was stated as
`100 ether` and the credited value as `msg.value`, both from the ETH-deposit design; `deposit` was
described as making no external calls at all; the deposit mutation was called the only one, omitting
the counter; and `treasuryBalance` was said to have no admin decrement path "without a balanced AAVE
supply/withdraw", which implies AAVE routing can move the book — it cannot.
