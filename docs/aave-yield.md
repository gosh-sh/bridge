# AAVE yield — operator runbook

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `a69ba36`, 2026-08-19.
Line references are to that file.*

Idle USDC is lent to the AAVE V3 USDC market. This page is what an operator needs: where the money
is, which command collects what, and the two ways to get it wrong. The contract-level detail is in
[`EVM-contracts-spec.md`](EVM-contracts-spec.md) §10.

## Where the money is

| | What it means | Read it with |
|---|---|---|
| **Owed to users** | A book number. Up on deposit, down on a proven payout; also up on `fundTreasury` and to zero on `migrateTreasury` when a bridge is replaced (below). | `treasuryBalance()` (`:98`) |
| **Liquid** | USDC on the contract, spendable now. | `usdc.balanceOf(bridge)` |
| **In AAVE** | `suppliedPrincipal()` is what we put in; `aUsdcBalance()` is what it is worth now. | both (`:123`, `:1387`) |

Two derived numbers decide everything below:

* `accruedYield()` = `aUsdcBalance() − suppliedPrincipal()` — yield **still inside AAVE** (`:1393`).
* `excessUsdc()` = `usdc.balanceOf(bridge) − treasuryBalance()` — surplus **already on the contract**
  (`:1307`).

Solvent iff `totalAssets() ≥ treasuryBalance()`. `treasuryBalance` is book value and is never
reconciled against real assets, so a loss in AAVE does not shrink it — it surfaces later, on a payout.

## Which command collects what

**The pocket decides, not the history.**

| Yield is… | Command | Fails with |
|---|---|---|
| in AAVE | `harvestYield(amount)` (`:1287`) | `NoYield` if `amount` is 0 or above `accruedYield()`; pays at most liquid + aUSDC − `treasuryBalance()`; `HarvestWouldBreakSolvency` when that surplus is zero |
| on the contract | `skimExcessUsdc(amount)` (`:1315`) | `NoExcessUsdc` if `excessUsdc() == 0` |

Both send to `yieldRecipient`, never to the caller. `skimExcessUsdc` accepts `type(uint256).max` for
"all of it"; `harvestYield` does not — pass `accruedYield()`. The call pays at most the surplus of
liquid USDC + aUSDC over `treasuryBalance()`, so that amount is safe after AAVE rounding (a 1-wei
short mint) and after a write-off. It reverts `HarvestWouldBreakSolvency` only when that surplus is
zero. Neither can touch user principal — neither function appears in the accounting of what we
supplied to AAVE, and that absence is the guarantee.

**The two unwind paths leave yield in different places.** This is the part that gets misread:

| You called | Yield ends up | Collect with |
|---|---|---|
| `withdrawFromAave(max)` (`:1258`) | stays in AAVE — it pulls only the booked principal | `harvestYield` |
| `emergencyWithdrawAll()` (`:1270`) | on the contract — it pulls principal **and** interest, and zeroes the book | `skimExcessUsdc` |

### After an emergency unwind — three cases, not one

"We ran `emergencyWithdrawAll`" does not by itself decide the answer. Read the two numbers.

| State | Collector |
|---|---|
| `accruedYield() == 0`, `excessUsdc() > 0` — the usual state right after the unwind: the position is at zero and the interest came back with the principal | `skimExcessUsdc`. `harvestYield` reverts `NoYield`, correctly — there is nothing in AAVE to see |
| `accruedYield() > 0` — yield is in AAVE again | `harvestYield`, exactly as normal |
| both non-zero | both, `harvestYield` first |

Yield can be back in AAVE after an emergency in three ways: the module was re-enabled and re-supplied
(the normal one); someone sent aUSDC to the bridge directly, which reads as pure yield because the
book says we supplied nothing; or rounding dust survived the `withdraw(max)`. In all three
`harvestYield` works — note it has **no** `aaveEnabled` guard (`:1287-1289`), so a disabled module
does not block collection. `withdrawFromAave`, by contrast, reverts `InvalidAmount` while
`min(suppliedPrincipal(), aUsdcBalance())` is zero, and for any explicit amount above it.

This is why the rule above is stated over pockets rather than over history: it answers all three
cases without anyone having to remember what was called last.

## Two ways to get it wrong

**1. Re-supplying before you skim.** `supplyToAave` subtracts only the liquid reserve, not the
surplus (`:1358-1363`), so uncollected yield goes back into the pool and is booked as *principal*.
It is then invisible to both collectors. Not lost — it keeps earning — but recovering it needs a
`withdrawFromAave(max)` first. Correct order after an emergency:

```
skimExcessUsdc(max)  →  setAaveEnabled(true)  →  supplyToAave(max)
```

To undo it later, use `withdrawFromAave(max)`, not `emergencyWithdrawAll` — the latter also disables
the module and you would have to re-enable it.

**If `emergencyWithdrawAll` reverts `EmergencyLeftoverAToken`.** AAVE paid some USDC but left
aUSDC on the bridge (reserve cap / pause). The whole call reverts, so `aaveEnabled` and
`suppliedPrincipal` are unchanged. Do not harvest from that leftover: zeroing the book first
is what would have made leftover shares look like yield, which is why the revert exists.

```
withdrawFromAave(max)      # booked principal only; aUSDC will not reach 0 if interest accrued
setAaveEnabled(false)
harvestYield(accruedYield())  # leftover yield still in AAVE; do not repeat withdrawFromAave
skimExcessUsdc(max)        # liquid surplus, if any
```

`withdrawFromAave` is capped at `_backedPrincipal()` (`min(suppliedPrincipal, aUsdcBalance)`), so
repeating it cannot drain interest and ends in `InvalidAmount`. `harvestYield` is the collector for
that pocket. `skimExcessUsdc` is for whatever already sits on the contract.

**If AAVE lost money: `suppliedPrincipal() > aUsdcBalance()`.** A haircut, or the shortfall that
`emergencyWithdrawAll` keeps on the books when AAVE redeemed less than it was given. The position is
worth less than the book; users are owed the same. What still works and what does not:

* Payouts keep drawing on whatever aUSDC is there — `withdrawByProof` pulls the shortfall up to
  `aUsdcBalance()`, interest included. Once liquid USDC plus aUSDC no longer covers a payout it
  reverts `InsufficientTreasury`; that is a real shortfall, and only fresh USDC sent to the bridge
  closes it.
* `withdrawFromAave(max)` pulls `min(suppliedPrincipal, aUsdcBalance)`, not the whole book.
* `accruedYield()` stays zero until interest catches up with the book.

`writeOffUnbackedPrincipal()` cuts the book down to `aUsdcBalance()`. Call it once the loss is
accepted — for example after the gap has been refilled in liquid USDC — so `accruedYield()` again
reflects the interest. It cannot be used to take yield from users: `harvestYield` refuses any amount
that would leave liquid + aUSDC below `treasuryBalance()`, whatever the book says.

**2. Reading a skim revert as "no yield".** If AAVE returned less than the principal — the depeg case
the emergency exists for — then `excessUsdc()` is zero and the skim reverts. That is the principal
guard doing its job, and it may be telling you there is a shortfall. Check `totalAssets()` against
`treasuryBalance()` before assuming it just means "nothing to collect".

## Check the recipient before the first collection

The constructor sets `owner = yieldRecipient = msg.sender` (`:554-555`), and `transferOwnership`
moves **only** `owner` (`:1347`). The constructor takes no recipient argument, so on any deploy where
ownership was handed to a multisig afterwards, yield still goes to the deploying key until someone
calls `setYieldRecipient` (`:1341`).

Separating the two roles is intentional and fine. The hazard is that they separate *silently*.

```bash
cast call $BRIDGE 'owner()(address)'          --rpc-url $RPC
cast call $BRIDGE 'yieldRecipient()(address)' --rpc-url $RPC   # is this still the deployer?
```

## Health check

```bash
cast call $BRIDGE 'treasuryBalance()(uint256)'   --rpc-url $RPC   # owed to users (book)
cast call $BRIDGE 'suppliedPrincipal()(uint256)' --rpc-url $RPC   # supplied to AAVE (book)
cast call $BRIDGE 'aUsdcBalance()(uint256)'      --rpc-url $RPC   # position value now
cast call $BRIDGE 'accruedYield()(uint256)'      --rpc-url $RPC   # in AAVE above the book
cast call $BRIDGE 'excessUsdc()(uint256)'        --rpc-url $RPC   # skimmable, on contract
cast call $BRIDGE 'totalAssets()(uint256)'       --rpc-url $RPC   # liquid + aUSDC
cast call $BRIDGE 'aaveEnabled()(bool)'          --rpc-url $RPC
```

Alert a keeper on any of these:

```
totalAssets() < treasuryBalance()                 # solvency breach
suppliedPrincipal() > aUsdcBalance()              # AAVE lost value; see "If AAVE lost money"
aaveEnabled() == false AND suppliedPrincipal > 0  # stuck position, needs a manual unwind
excessUsdc() > 0 across a collection cycle        # uncollected surplus; the next supply absorbs it
```

## Other owner controls

`setLiquidReserveBps` — the share of user principal kept liquid. Default `1_000` = 10 % (`:557`),
capped at 50 % (`:67`). Ours, not an AAVE convention. `setAaveEnabled` — gate on new supplies;
`emergencyWithdrawAll` turns it off, and only an explicit call turns it back on.

The owner cannot replace a verifier — those bindings are immutable — and can reduce
`treasuryBalance` only by handing all of it to a successor with `migrateTreasury`. The bridge checks
only that the target has code and returns it from `predecessor()`, which any contract can do, so
the owner key decides where that money goes (see
[Replacing a bridge on the same chain](#replacing-a-bridge-on-the-same-chain)). The owner can pause the bridge (`pause()` / `unpause()`): `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield controls above stay
available, so funds can be moved out of AAVE during a pause (`EVM-contracts-spec.md`, §10.1).

## Replacing a bridge on the same chain

A withdrawal proof names the one EVM bridge that may pay it. `eccUSDCBridge` on Acki Nacki writes
the bridge its owner set for the destination chain (`setEvmBridge(dstChainId, evmBridge)`, read back
with `getEvmBridge`) into every `WithdrawalInitiated` event; Circuit 4 publishes it as `dstBridge`;
`withdrawByProof` refuses any other address with `WrongDstBridge`. A new bridge on a chain that
already has one therefore cannot pay a burn bound to the old one, and the old one cannot pay a burn
bound to the new one, whether either is paused or not. Burns made before the switch are paid by the
old bridge, and only by it.

The old bridge's user principal (`treasuryBalance`) has to follow the eccUSDC it backs once the old
bridge has paid what it owes, and the new bridge cannot pay anyone until it has that principal. So
withdrawals to the chain close for the time the old bridge needs to pay the burns already made, and
reopen on the new bridge once the principal is there. Deposits stay open throughout. The order is:

1. **Deploy the new bridge and trust it.** `PREDECESSOR_BRIDGE` (optional, zero by default) names
   the old bridge only so that the old bridge's `migrateTreasury` will accept the new one in step 4;
   the constructor reverts `InvalidPredecessor` unless that address has code on this chain and was
   deployed for the same `WITHDRAW_DAPP_FR` and `WITHDRAW_ACC_FR`. It plays no part in replay
   protection. Leave the new bridge unpaused: a paused bridge reverts `verifyBlock`, so it would
   anchor nothing. Start its `verifyBlock` relayer, its withdrawal relayer and a
   `deposit-relayer daemon` with `--bridge-address <new>` and its own state directory, and call
   `setTrustedL1Bridge(<chainId>, <new>, true)` on `eccUSDCBridge`, so that deposits into the new
   bridge are finalized. Deposits into the old bridge still are too.
2. **Close withdrawals to the chain**: `setEvmBridge(<chainId>, 0)` on `eccUSDCBridge`. From that
   message on, `initiateWithdrawal` to the chain returns the attached eccUSDC with reason 239
   (`ERR_NO_EVM_BRIDGE`) and burns nothing, and `ackinacki-bridge withdraw` stops before the burn
   because `getEvmBridge` is unset. The burns made before this message are bound to the old bridge,
   and the set is now closed.
3. **Let the old bridge pay what it owes.** Keep it unpaused and keep its `verifyBlock` relayer,
   its `relayer daemon-withdraw` (or `daemon-bridge`) and its `deposit-relayer` running, until it
   has emitted `WithdrawalByProofExecuted` for every burn made before step 2. Every one of them is
   paid there or nowhere. A user whose `ackinacki-bridge withdraw` stopped after the burn resumes it
   with `--bridge-address <old>`; the resume does not repeat the check that refuses a new burn. A
   bridge deployed before `dstBridge` existed, such as the 421614 bridge
   `0x3FB082a905a895151224F6d93bD074E816338638`, verifies only proofs of the previous Circuit 4:
   prove those burns with the `ackinacki-bridge` release that bridge was deployed for. This step
   takes as long as anchoring and proving the last of those burns, and withdrawals to the chain stay
   closed for all of it.
4. **Retire the old bridge and move the principal.** `pause()` it and pull its AAVE position back
   (`withdrawFromAave` or `emergencyWithdrawAll`) so its whole `treasuryBalance` is liquid USDC.
   - An old bridge that has `migrateTreasury(address)`: its owner calls
     `migrateTreasury(<new bridge>)`. It needs the old bridge paused, its `treasuryBalance`
     liquid (`TreasuryNotLiquid` otherwise), and a target with code whose `predecessor()` is
     the old bridge (`NotSuccessor` otherwise). Check the target address yourself: that test
     catches a typo, not a contract written to pass it. The whole `treasuryBalance` moves and the
     old bridge's book drops to 0. USDC above the book and any aUSDC yield stay behind for
     `skimExcessUsdc` / `harvestYield`. The new bridge emits `TreasuryFunded`, the old one
     `TreasuryMigrated`.
   - An older bridge, such as `0x3FB082a905a895151224F6d93bD074E816338638`, has no way to
     release its principal. The new bridge's owner covers that amount instead: `approve` the new
     bridge, then `fundTreasury(<old treasuryBalance>)`. The USDC left in the old bridge stays
     there.

   `fundTreasury` credits `treasuryBalance` without a `Deposit` event, so nothing is minted on
   Acki Nacki. Only the owner and `predecessor` may call it. A deposit the old bridge took before
   its pause is in the `treasuryBalance` that moves, and Acki Nacki still finalizes it: the old
   bridge stays trusted until step 6.
5. **Reopen withdrawals on the new bridge**: `setEvmBridge(<chainId>, <new>)` on `eccUSDCBridge`.
   Point `ackinacki-bridge withdraw --bridge-address` at the new bridge.
6. **Stop trusting the old bridge** (`setTrustedL1Bridge(<chainId>, <old>, false)`) and stop its
   relayers, only after every deposit it took has been finalized: every deposit id below its
   `depositCounter`, which the pause froze. `eccUSDCBridge` refuses a deposit from an untrusted
   bridge (`ERR_UNSUPPORTED_SRC_CHAIN`), and step 4 moved that deposit's USDC to the new bridge, so
   a deposit refused here has no way back.
