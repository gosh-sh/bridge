# AAVE yield — operator runbook

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `a69ba36`, 2026-08-19.
Line references are to that file.*

Idle USDC is lent to the AAVE V3 USDC market. This page is what an operator needs: where the money
is, which command collects what, and the two ways to get it wrong. The contract-level detail is in
[`EVM-contracts-spec.md`](EVM-contracts-spec.md) §10.

## Which market a deploy wires

`DeployRealBridge` (`contracts/ethereum/script/DeployRealBridge.s.sol`) fills `aavePool` and `aUSDC`
only on the chain ids below, and only when `USE_AAVE=true`. The flag defaults to false. Both
addresses are constructor immutables. A bridge deployed with either address zero cannot be switched
onto a market later: `setAaveEnabled(true)` reverts `InvalidAaveAddress`. `DeployShellnetE2EBridge`,
which is what the shellnet runbooks run, always passes zeros and does not take `USE_AAVE`.

| Chain | Chain id | USDC the pool lists | Pool | aToken |
|---|---:|---|---|---|
| Ethereum | 1 | Circle `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` | `0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c` |
| Ethereum Sepolia | 11155111 | Aave's faucet token `0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8`. This is not Circle's Sepolia USDC `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`. | `0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951` | `0x16dA4541aD1807f4443d92D26044C1147406EB80` |
| Arbitrum Sepolia | 421614 | Circle `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` | `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff` | `0x460b97BD498E1157530AEb3086301d5225b91216` |

Arbitrum Sepolia is the testnet market whose underlying is Circle's USDC. Read on 2026-10-28: the
aToken's `UNDERLYING_ASSET_ADDRESS()` was that USDC, its `POOL()` was that pool, and the reserve was
active, not frozen and not paused, with a supply cap of 10_500_000. Testnet markets move. Before a
deploy, call those two views again and confirm `getReserveData(usdc)` still returns this aToken.
`AckiNackiBridgeArbSepoliaAaveForkTest` does that check and then calls `supplyToAave`,
`harvestYield` (the reserve already has a borrower), `withdrawFromAave`, `emergencyWithdrawAll`
and a `withdrawByProof` that tops the payout up from the pool:

```bash
cd contracts/ethereum
FOUNDRY_PROFILE=fork FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
  forge test --match-contract AckiNackiBridgeArbSepoliaAaveForkTest -vv
```

`USE_AXIOM_ORACLE` deploys `AxiomBlockHeaderOracle` only on chain ids 1 and 11155111. On 421614
leave it unset: the script deploys `MockBlockHeaderOracle`. Setting it `true` reverts
`Axiom oracle not supported on this chain` and does not fall back to the mock. The bridge stores
the oracle address and does not call it on the deposit path.

A shellnet withdrawal of this bridge also needs `WITHDRAW_ALT_TOKEN_ID=3` (`USDC_ECC_ID` on Acki
Nacki). `DeployRealBridge` defaults that value to 0. It is immutable, so a bridge deployed without
it rejects every shellnet withdrawal with `UnsupportedTokenId(3)` and has to be deployed again.
The shellnet e2e deploy sets 3; this script does not.

The bridge already recorded for Arbitrum Sepolia,
`0x3FB082a905a895151224F6d93bD074E816338638` (`README.md`), has no pool, so the AAVE bridge
replaces it on the same chain. Follow
[Replacing a bridge on the same chain](#replacing-a-bridge-on-the-same-chain):

1. `pause()` `0x3FB0…8638` and record its `treasuryBalance()` (1.999999 USDC on 2026-10-30).
2. Deploy with `USE_AAVE=true` and `PREDECESSOR_BRIDGE=0x3FB082a905a895151224F6d93bD074E816338638`.
   Deploy it already paused if anything could reach it before step 4: the deploying account calls
   `pause()` right after the deploy.
3. `0x3FB0…8638` predates `migrateTreasury`, so its USDC stays in it. From the new bridge's owner,
   `approve` the new bridge for the recorded amount and call `fundTreasury(<amount>)`.
4. `setTrustedL1Bridge(421614, <new bridge>, true)` and
   `setTrustedL1Bridge(421614, 0x3FB0…8638, false)` on `eccUSDCBridge`; reconfigure the relayers
   below and the `ackinacki-bridge` users; `unpause()` the new bridge.

`AckiNackiBridgeArbSepoliaAaveForkTest` exercises the AAVE calls against the live market on a
fork, with a bridge it deploys there. The web form deposits on Ethereum Sepolia only
(`USDC_CONTRACT_ADDRESS` and `SEPOLIA_CHAIN_ID` in `frontend/src/config.rs`) and plays no part on
421614.

### Deploy inputs that cannot be changed later

The anchors, the withdraw fields and `predecessor` are fixed at deploy, so deploy the bridge the
way its relayer will need it. The owner is not: `transferOwnership` / `acceptOwnership` move it,
but preflight checks it, so deploy from the account below. The shellnet-l2 kit runs `relayer daemon-live`
(`crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/container-entrypoint.sh`), and its
`scripts/preflight.sh` refuses a bridge that does not match its `runtime.env`. The kit runs
anchor level 2 only, so deploy with the output of `compute_bridge_anchors --level 2` (with
`--at-head` or `--seed-seqno`): `BUNDLE_STRIDE`, `GENESIS_BK_SET_COMMITMENT`,
`GENESIS_PREV_MAX_LEVEL_LAYER_HASH`, `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, and
`GENESIS_SEED_SEQNO` exported as `GENESIS_LAST_SEEN_BLOCK_SEQNO`, as
`deploy_bridge_bundle.sh` does. Set `PRIVATE_KEY` to the key of the kit's `RELAYER_ADDRESS`:
the deploying account becomes the owner, and preflight requires the owner to be
`RELAYER_ADDRESS`. The kit's `runtime.env` then needs:

* `BRIDGE_ADDRESS` and `BRIDGE_DEPLOY_BLOCK`.
* `RPC_URL`, and `RPC_FAILOVER_URLS` if set: Arbitrum Sepolia endpoints, and
  `EXPECTED_EVM_CHAIN_ID=421614`. Preflight checks the chain id of every endpoint that
  answers.
* `EXPECTED_USDC_ADDRESS=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d`. The example holds
  Ethereum Sepolia USDC.
* `BRIDGE_BOOTSTRAP_SEQNO`: `GENESIS_SEED_SEQNO`. `EXPECTED_GENESIS_PREV_ANCHOR`:
  `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`. `EXPECTED_BK_SET_COMMITMENT`:
  `GENESIS_BK_SET_COMMITMENT`.
* `EXPECTED_WITHDRAW_DAPP_FR` and `EXPECTED_WITHDRAW_ACC_FR`: the `WITHDRAW_DAPP_FR` and
  `WITHDRAW_ACC_FR` of the deploy.
* `EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID`, `EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID` and
  `EXPECTED_WITHDRAW_ALT_TOKEN_ID`: the `WITHDRAW_ALT_*` values of the deploy. The example's
  1, 11155111 and 3 are the Ethereum Sepolia bridge's. The chain id alias applies only where
  `block.chainid` is the host chain id, and a burn towards this bridge names 421614 itself, so
  a deploy that sets only `WITHDRAW_ALT_TOKEN_ID=3` needs 0, 0 and 3.

## Where the money is

| | What it means | Read it with |
|---|---|---|
| **Owed to users** | A book number. Up on deposit, down on a proven payout; `fundTreasury` and `migrateTreasury` move it only when a bridge is replaced. | `treasuryBalance()` (`:98`) |
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
`treasuryBalance` only by handing all of it to a successor bridge with `migrateTreasury` (see
[Replacing a bridge on the same chain](#replacing-a-bridge-on-the-same-chain)). The owner can pause the bridge (`pause()` / `unpause()`): `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield controls above stay
available, so funds can be moved out of AAVE during a pause (`EVM-contracts-spec.md`, §10.1).

## Replacing a bridge on the same chain

A withdrawal proof names the destination chain, not a bridge contract, and each bridge keeps its
own record of the withdrawals it paid. A new bridge on a chain that already has one therefore has
to be told about the old one: it is deployed with `predecessor` set to the old bridge's address
(`PREDECESSOR_BRIDGE` for `DeployRealBridge`, which requires the variable on every deploy — the
zero address for the first bridge on a chain). The new bridge then refuses, with
`NullifierAlreadyUsed`, every withdrawal the old bridge — or any bridge before it — already paid,
and its `isNullifierUsed` reports those as used. The old bridge does not consult the new one, so
once the new bridge is live the old one never pays again.

The old bridge's user principal (`treasuryBalance`) has to follow the eccUSDC it backs, or the new
bridge pays those withdrawals out of other depositors' money and eventually stops with
`WithdrawTreasuryShortfall`. So the order is:

1. **Pause the old bridge** (`pause()`) and leave it paused for good. `DeployRealBridge`
   refuses a predecessor that is not paused. The yield controls keep working while it is
   paused: pull its AAVE position back with `withdrawFromAave` or `emergencyWithdrawAll`, so
   its whole `treasuryBalance` is liquid USDC.
2. **Deploy the new bridge** with `PREDECESSOR_BRIDGE` set to the old one. The constructor
   reverts `InvalidPredecessor` unless that address has code on this chain, answers
   `isNullifierUsed`, and was deployed for the same `WITHDRAW_DAPP_FR` and `WITHDRAW_ACC_FR`.
   For a third bridge, name the second one: each bridge answers for its own predecessor, so
   one address covers the whole line.
3. **Move the principal.**
   - An old bridge that has `migrateTreasury(address)` (this release onwards): its owner calls
     `migrateTreasury(<new bridge>)`. It needs the old bridge paused, its `treasuryBalance`
     liquid (`TreasuryNotLiquid` otherwise), and a target whose `predecessor()` is the old
     bridge (`NotSuccessor` otherwise). The whole `treasuryBalance` moves; the old bridge's
     book drops to 0, so it cannot pay anything even if unpaused. USDC above the book and any
     aUSDC yield stay behind for `skimExcessUsdc` / `harvestYield`. The new bridge emits
     `TreasuryFunded`, the old one `TreasuryMigrated`.
   - An older bridge, such as the 421614 bridge `0x3FB082a905a895151224F6d93bD074E816338638`, has
     no way to release its principal. The new bridge's owner covers that amount instead:
     `approve` the new bridge, then `fundTreasury(<old treasuryBalance>)`. The USDC left in the
     old bridge stays there.

   `fundTreasury` credits `treasuryBalance` without a `Deposit` event, so nothing is minted on
   Acki Nacki. Only the owner and `predecessor` may call it, and it works while the bridge is
   paused.
4. **Move Acki Nacki and the off-chain services over**: trust the new bridge on `eccUSDCBridge`,
   stop trusting the old one, point relayers and `ackinacki-bridge` at the new address, then
   `unpause()` the new bridge if it was deployed paused.

Check the new bridge before unpausing anything that depends on it:

```bash
cast call $NEW 'predecessor()(address)' --rpc-url $RPC               # the old bridge
cast call $OLD 'paused()(bool)' --rpc-url $RPC                        # true
cast call $NEW 'isNullifierUsed(uint256)(bool)' $PAID --rpc-url $RPC  # true for a nullifier the old bridge paid
cast call $NEW 'treasuryBalance()(uint256)' --rpc-url $RPC              # the old bridge's book, after step 3
```
