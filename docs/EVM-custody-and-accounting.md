# USDC custody and accounting

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `a69ba36`, 2026-08-19.
Line references are to that file. Written by reading the source; earlier revisions of this material
were carried over from the ETH-deposit era and are superseded by it.*

What the bridge owes, what it actually holds, and the exact order of operations on each path. This
page exists because those three are easy to conflate, and the labelled rules below are what auditors
and operators quote at each other during an incident.

**Scope: the EVM side only** — `AckiNackiBridge.sol` and the AAVE position it holds. The
Acki Nacki side keeps its own accounting in `USDCBridge`, a TVM contract in another repository; only
its ABI is bundled here, so nothing on this page describes it. Where the two meet is the deposit
proof, covered in [`EVM-contracts-spec.md`](EVM-contracts-spec.md) §6.

## The book number is not a balance

`treasuryBalance` (`:98`) is **book value**: the sum of user principal the bridge owes. It is not a
token balance and is never reconciled against one.

It moves in exactly two places in the whole contract:

* `+= amount` in `deposit` (`:590`)
* `-= pub.amount` in `withdrawByProof` (`:1194`)

Nothing else writes it. No owner function does, under any condition — including the AAVE routing
functions, which move tokens between the bridge and the pool without touching the book at all.

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
   `anAccount != 0` (`InvalidAnAccount`).
2. **Interaction** — `usdc.transferFrom(msg.sender, address(this), amount)`; a `false` return reverts
   `TransferFromFailed` (`:585-587`).
3. **Effects** — `depositCounter++`, then `treasuryBalance += amount` (`:588-590`).
4. **Event** — `Deposit(depositId, sender, amount, anWorkchain, anAccount, timestamp)`.

So on this path the interaction precedes the effects. That is the correct order here — the bridge
books only what it has actually received — and it is safe for two independent reasons: the function
is `nonReentrant` (`:419-424`), and `usdc` is `immutable` (`:111`), bound at construction, so the
callee is a fixed known token rather than caller-supplied.

State it this way rather than as "checks-effects-interactions", which this path does not follow.

Two consequences worth naming:

* `MAX_DEPOSIT_AMOUNT` is `type(uint64).max` (`:61`) — sized so the amount fits the AN mint path,
  not as a TVL cap. The function is **not** `payable`; the amount is a parameter.
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

## AAVE routing never moves the book

`supplyToAave`, `withdrawFromAave` and `emergencyWithdrawAll` move tokens between the bridge and the
pool and adjust `suppliedPrincipal` only. `harvestYield` and `skimExcessUsdc` send surplus to
`yieldRecipient` and appear in neither the book nor the principal accounting — that absence is what
makes them incapable of reaching user principal. Operating detail: [`aave-yield.md`](aave-yield.md).

## Labelled rules, restated

Quote these, not their earlier versions.

| | Rule |
|---|---|
| **DEP-1** | Every successful `deposit` emits `Deposit(depositId, sender, amount, anWorkchain, anAccount, timestamp)` with a unique monotonic `depositId`; `anAccount == 0` reverts `InvalidAnAccount`. |
| **DEP-2** | `amount == 0` reverts `InvalidAmount`; `amount > type(uint64).max` reverts `DepositTooLarge`. The function is not `payable`. |
| **DEP-3** | A successful `deposit` makes exactly two state mutations: `depositCounter++` and `treasuryBalance += amount`. No other function in the contract increases `treasuryBalance`, and only `withdrawByProof` decreases it. |
| **DEP-4** | `deposit` is `nonReentrant` and makes exactly one external call, `usdc.transferFrom`, to the `immutable` token bound at construction. It happens **before** the state mutations. |
| **CUST-1** | `treasuryBalance` is book value, not a balance. The solvency invariant is `totalAssets() ≥ treasuryBalance`, never equality. |
| **CUST-2** | Owner functions can move custody between the bridge and AAVE, and can send only the surplus above `treasuryBalance` to `yieldRecipient`. None of them writes `treasuryBalance`. |
| **CEI-1** | `withdrawByProof` mutates state before every external call. `deposit` does not, and does not need to — see above. A blanket "all external interactions follow CEI" claim is false for this contract. |

## What earlier text got wrong

Kept short so anyone holding an older copy can reconcile it: the deposit cap was stated as
`100 ether` and the credited value as `msg.value`, both from the ETH-deposit design; `deposit` was
described as making no external calls at all; the deposit mutation was called the only one, omitting
the counter; and `treasuryBalance` was said to have no admin decrement path "without a balanced AAVE
supply/withdraw", which implies AAVE routing can move the book — it cannot.
