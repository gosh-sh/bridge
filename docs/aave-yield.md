# AAVE yield — operator runbook

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol`, 2026-11-04. The design and its
acceptance criteria are in [`aave-integration-spec.md`](aave-integration-spec.md).*

User principal above a liquid reserve is lent to the AAVE V3 USDC market. This page is what an
operator needs: how the bridge moves money in and out of the pool, where the money is, which command
collects what, and the ways to get it wrong. The contract-level detail is in
[`EVM-contracts-spec.md`](EVM-contracts-spec.md) §10.

## Which market a deploy wires

The bridge is deployed on Arbitrum Sepolia (421614) only, and always on its AAVE V3 USDC market.
The constructor reverts `InvalidAaveAddress` when either AAVE address is zero, on every chain.
`DeployRealBridge` (`contracts/ethereum/script/DeployRealBridge.s.sol`) reverts on any other chain
id; it and `DeployShellnetE2EBridge` take the three addresses from
`contracts/ethereum/script/ArbitrumSepoliaAave.sol`, with no override. On local anvil (31337)
`DeployShellnetE2EBridge`, `DeployTestBridge` and `GenDepositSetup` deploy a mock market instead.

| Chain | Chain id | USDC the pool lists | Pool | aToken |
|---|---:|---|---|---|
| Arbitrum Sepolia | 421614 | Circle `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` | `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff` | `0x460b97BD498E1157530AEb3086301d5225b91216` |

Read on 2026-10-28: the aToken's `UNDERLYING_ASSET_ADDRESS()` was that USDC, its `POOL()` was that
pool, and the reserve was active, not frozen and not paused, with a supply cap of 10_500_000.
Testnet markets move. Before a deploy, call those two views again and confirm
`getReserveData(usdc)` still returns this aToken. `AckiNackiBridgeArbSepoliaAaveForkTest` does
that check and then runs the supply in `deposit`, the refill in `withdrawByProof`, a paused, a
frozen and a capped reserve, a reserve with its liquidity borrowed out, `harvestYield`,
`withdrawFromAave`, `emergencyWithdrawAll` and both deploy scripts against the live market.
`.woodpecker/solidity.yaml` runs it on every pull request:

```bash
cd contracts/ethereum
FOUNDRY_PROFILE=fork FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
  forge test --match-contract AckiNackiBridgeArbSepoliaAaveForkTest -vv
```

`DeployRealBridge` deploys `MockBlockHeaderOracle`. `USE_AXIOM_ORACLE=true` reverts
`Axiom oracle not supported on Arbitrum Sepolia` and does not fall back to the mock. The bridge stores
the oracle address and does not call it on the deposit path.

A shellnet withdrawal of this bridge names `USDC_TOKEN_ID` (`USDC_ECC_ID` on Acki Nacki, `3`), which
`DeployRealBridge` defaults to 3. It is immutable, so a bridge deployed under another id rejects every
shellnet USDC withdrawal with `UnsupportedTokenId(3)` and has to be deployed again.

The bridge already recorded for Arbitrum Sepolia,
`0x3FB082a905a895151224F6d93bD074E816338638` (`README.md`), has no pool, so the AAVE bridge
replaces it on the same chain. Follow
[Replacing a bridge on the same chain](#replacing-a-bridge-on-the-same-chain). Two steps differ
for `0x3FB0…8638`:

* It predates `dstBridge`, so in step 3 its burns are proven with the `ackinacki-bridge` release
  it was deployed for.
* It predates `migrateTreasury`, so in step 4 its USDC stays in it. The new bridge's owner
  `approve`s the new bridge for the old `treasuryBalance()` (1.999999 USDC on 2026-10-30) and
  calls `fundTreasury(<amount>)`. `PREDECESSOR_BRIDGE` is not needed for that; the constructor
  accepts `0x3FB0…8638` if it is set.

`AckiNackiBridgeArbSepoliaAaveForkTest` exercises the AAVE calls against the live market on a
fork, with a bridge it deploys there. The web form deposits on Ethereum Sepolia only
(`USDC_CONTRACT_ADDRESS` and `SEPOLIA_CHAIN_ID` in `frontend/src/config.rs`) and plays no part on
421614.

### Deploy inputs that cannot be changed later

The anchors, the withdraw fields and `predecessor` are fixed at deploy, so deploy the bridge the
way its relayer will need it. The owner is not: `transferOwnership` / `acceptOwnership` move it,
but preflight checks it, so deploy from the account below. The Docker Compose kit runs
`relayer daemon-live` (`docker/relayer/container-entrypoint.sh`), and
`docker/relayer/preflight.sh` refuses a bridge that does not match its `daemon-live.env`
(template `docker/env/daemon-live.env.example`). The kit runs
anchor level 2 only, so deploy with the output of `compute_bridge_anchors --level 2` (with
`--at-head` or `--seed-seqno`): `BUNDLE_STRIDE`, `GENESIS_BK_SET_COMMITMENT`,
`GENESIS_PREV_MAX_LEVEL_LAYER_HASH`, `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, and
`GENESIS_SEED_SEQNO` exported as `GENESIS_LAST_SEEN_BLOCK_SEQNO`, as
`tests/e2e/local-net/prover-host/jobs/deploy.sh` does. `compute_bridge_anchors` builds the BK set
at the seed from GraphQL, so its endpoint must be gql-server 1.5.0 or later serving
`blockchain.latestBkSet`, and the seed must be at or above L3\*
([relayer runbook §3.1](../crates/bridge-relayer-daemon/docs/live_relayer_bridge_runbook.md#31-pick-the-seed)).
Set `PRIVATE_KEY` to the key of the kit's `RELAYER_ADDRESS`:
the deploying account becomes the owner, and preflight requires the owner to be
`RELAYER_ADDRESS`. Its `daemon-live.env` then needs:

* `BRIDGE_ADDRESS` and `BRIDGE_DEPLOY_BLOCK`.
* `RPC_URL`, and `RPC_FAILOVER_URLS` if set: Arbitrum Sepolia endpoints, and
  `EXPECTED_EVM_CHAIN_ID=421614`. Preflight checks the chain id of every endpoint that
  answers.
* `EXPECTED_USDC_ADDRESS=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d`.
* `BRIDGE_BOOTSTRAP_SEQNO`: `GENESIS_SEED_SEQNO`. `EXPECTED_GENESIS_PREV_ANCHOR`:
  `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`. `EXPECTED_BK_SET_COMMITMENT`:
  `GENESIS_BK_SET_COMMITMENT`.
* `EXPECTED_WITHDRAW_DAPP_FR` and `EXPECTED_WITHDRAW_ACC_FR`: the `WITHDRAW_DAPP_FR` and
  `WITHDRAW_ACC_FR` of the deploy.
* `EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID` and `EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID`: the
  `WITHDRAW_ALT_DST_*` values of the deploy; `EXPECTED_USDC_TOKEN_ID`: its `USDC_TOKEN_ID`. The
  example's 1, 11155111 and 3 are the Ethereum Sepolia bridge's. The chain id alias applies only
  where `block.chainid` is the host chain id, and a burn towards this bridge names 421614 itself,
  so a deploy that sets neither alias needs 0, 0 and 3.

## How money moves in and out of the pool

No call supplies the pool on its own: `supplyToAave` is gone. Two numbers set the band:
`liquidReserveBps` (`r`, default `1_000` = 10 %, between 10 % and 50 %) and `supplyMultiplierBps`
(`M`, default `20_000` = 2×, above 1×), with `r × M` at most 100 %. Write `B` for
`treasuryBalance()`, `S` for `suppliedPrincipal()`, `U` for the bridge's USDC and `L` for the
liquid principal, `min(U, B − S)`. The target is `T = B × r` and the threshold `H = T × M`.

* **`deposit`** books the deposit and emits `Deposit`; then, while `aaveEnabled()`, if `L ≥ H` it
  supplies `L − T`, leaving `T` liquid, and emits `SuppliedToAave`. Below the threshold it makes no
  pool call. A pool that refuses — paused or frozen reserve, supply cap, a revert, nothing
  credited, `approve` false — does not fail the deposit: the supply is undone, the USDC stays on
  the bridge and `AaveSupplyRefused(amount, reason)` carries the pool's revert data. USDC above
  the principal (`U − (B − S)`) is never supplied.
* **`withdrawByProof`** pays from the USDC on hand while the liquid principal covers the payout,
  with no pool call. Otherwise it pulls what the USDC on hand cannot pay — that part is required,
  and if the pool cannot return it the payout reverts and its nullifier stays unused — and in the
  same `withdraw` tries to pull enough more to leave `T` of the new book liquid. When the pool
  cannot serve that extra part, the payout still goes through with only the required part pulled,
  and `AaveRefillFailed(amount, reason)` names the part not pulled and the pool's revert data.

`AaveSupplyRefused` and `AaveRefillFailed` are the events to alert on: either one means the pool
is refusing the bridge.

## Where the money is

| | What it means | Read it with |
|---|---|---|
| **Owed to users** | A book number. Up on deposit, down on a proven payout; also up on `fundTreasury` and to zero on `migrateTreasury` when a bridge is replaced (below). | `treasuryBalance()` |
| **Liquid** | USDC on the contract, spendable now. | `usdc.balanceOf(bridge)` |
| **In AAVE** | `suppliedPrincipal()` is what we put in; `aUsdcBalance()` is what it is worth now. | both |

Two derived numbers decide everything below:

* `accruedYield()` = `aUsdcBalance() − suppliedPrincipal()` — yield **still inside AAVE**.
* `excessUsdc()` = `min(U + S − B, U + A − B)` with `A` = `aUsdcBalance()`, at most `U` and never
  below zero — surplus **already on the contract**. USDC sent to the bridge directly is skimmable
  while principal sits in the pool, and never while it covers a loss in the pool.

Solvent iff `totalAssets() ≥ treasuryBalance()`. `treasuryBalance` is book value and is never
reconciled against real assets, so a loss in AAVE does not shrink it — it surfaces later, on a payout.

## Which command collects what

**The pocket decides, not the history.**

| Yield is… | Command | Fails with |
|---|---|---|
| in AAVE | `harvestYield(amount)` | `NoYield` if `amount` is 0 or above `accruedYield()`; pays at most liquid + aUSDC − `treasuryBalance()`; `HarvestWouldBreakSolvency` when that surplus is zero |
| on the contract | `skimExcessUsdc(amount)` | `NoExcessUsdc` if `excessUsdc() == 0` |

Both send to `yieldRecipient`, never to the caller. `skimExcessUsdc` accepts `type(uint256).max` for
"all of it"; `harvestYield` does not — pass `accruedYield()`. The call pays at most the surplus of
liquid USDC + aUSDC over `treasuryBalance()`, so that amount is safe after AAVE rounding (a 1-wei
short mint) and after a write-off. It reverts `HarvestWouldBreakSolvency` only when that surplus is
zero. Neither can touch user principal — neither function appears in the accounting of what we
supplied to AAVE, and that absence is the guarantee.

**The two unwind paths leave yield in different places.** This is the part that gets misread:

| You called | Yield ends up | Collect with |
|---|---|---|
| `withdrawFromAave(max)` | stays in AAVE — it pulls only the booked principal | `harvestYield` |
| `emergencyWithdrawAll()` | on the contract — it pulls principal **and** interest, and zeroes the book | `skimExcessUsdc` |

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
`harvestYield` works — note it has **no** `aaveEnabled` guard, so a disabled module
does not block collection. `withdrawFromAave`, by contrast, reverts `InvalidAmount` while
`min(suppliedPrincipal(), aUsdcBalance())` is zero, and for any explicit amount above it.

This is why the rule above is stated over pockets rather than over history: it answers all three
cases without anyone having to remember what was called last.

## Two ways to get it wrong

**1. Re-enabling before you skim.** After `emergencyWithdrawAll`, the next crossing deposit
supplies only principal — USDC above it is never supplied — so uncollected interest is not swept
into the pool. Still, collect it first, so the books are simple to read:

```
skimExcessUsdc(max)  →  setAaveEnabled(true)
```

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

* Payouts keep drawing on whatever aUSDC is there — `withdrawByProof` pulls up to
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

The constructor sets `owner = yieldRecipient = msg.sender`, and `transferOwnership`
moves **only** `owner`. The constructor takes no recipient argument, so on any deploy where
ownership was handed to a multisig afterwards, yield still goes to the deploying key until someone
calls `setYieldRecipient`.

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
excessUsdc() > 0 across a collection cycle        # uncollected surplus
AaveSupplyRefused or AaveRefillFailed emitted     # the pool refuses the bridge
```

## Other owner controls

`setLiquidReserveBps` — the share of the book kept liquid. Default `1_000` = 10 %, at least 10 %
(`ReserveBpsTooLow`), at most 50 % (`ReserveBpsTooHigh`). `setSupplyMultiplierBps` — how far the
liquid principal has to grow above that target before a deposit supplies it. Default `20_000` =
2×, above 1× (`MultiplierTooLow`). Either setter reverts `ReserveBandTooWide` when `r × M` would
exceed 100 %, and the next deposit uses the new values. Ours, not an AAVE convention.
`setAaveEnabled` — gate on new supplies; `emergencyWithdrawAll` turns it off, and only an explicit
call turns it back on.

The owner cannot replace a verifier — those bindings are immutable — and can reduce
`treasuryBalance` only by handing all of it to a successor with `migrateTreasury`. The bridge checks
only that the target has code and returns it from `predecessor()`, which any contract can do, so
the owner key decides where that money goes (see
[Replacing a bridge on the same chain](#replacing-a-bridge-on-the-same-chain)). The owner can pause the bridge (`pause()` / `unpause()`): `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield controls above stay
available, so funds can be moved out of AAVE during a pause (`EVM-contracts-spec.md`, §10.1).

## Replacing a bridge on the same chain

A withdrawal proof names the one EVM bridge that may pay it. `eccBridge` on Acki Nacki writes
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
   `setTrustedL1Bridge(<chainId>, <new>, true)` on `eccBridge`, so that deposits into the new
   bridge are finalized. Deposits into the old bridge still are too.
2. **Close withdrawals to the chain**: `setEvmBridge(<chainId>, 0)` on `eccBridge`. From that
   message on, `initiateWithdrawal` to the chain returns the attached eccUSDC with reason 239
   (`ERR_NO_EVM_BRIDGE`) and burns nothing, and `ackinacki-bridge withdraw` stops before the burn
   because `getEvmBridge` is unset. The burns made before this message are bound to the old bridge,
   and the set is now closed.
3. **Let the old bridge pay what it owes.** Keep it unpaused and keep its `verifyBlock` relayer,
   its `relayer daemon-withdraw` (or `daemon-bridge`) and its `deposit-relayer` running, until it
   has emitted `WithdrawalByProofExecuted` for every burn made before step 2. Every one of them is
   paid there or nowhere. A user whose `ackinacki-bridge withdraw` stopped after the burn resumes it
   with `--bridge-address <old> --allow-retry`; the resume does not repeat the check that refuses a new burn. A
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

   Then call `migrateToken(<tokenId>, <new bridge>)` on the old bridge for every other listed token
   with liabilities; there are none while USDC is the only one listed. It hands over the listing's liabilities and the tokens behind them, through the
   new bridge's `fundToken`. A token left behind strands later burns of it, with the ECC already
   burned (`contracts/an/README.md`, "Which EVM bridge pays a withdrawal").
5. **Reopen withdrawals on the new bridge**: `setEvmBridge(<chainId>, <new>)` on `eccBridge`.
   Point `ackinacki-bridge withdraw --bridge-address` at the new bridge.
6. **Stop trusting the old bridge** (`setTrustedL1Bridge(<chainId>, <old>, false)`) and stop its
   relayers, only after every deposit it took has been finalized: every deposit id below its
   `depositCounter`, which the pause froze. `eccBridge` refuses a deposit from an untrusted
   bridge (`ERR_UNSUPPORTED_SRC_CHAIN`), and step 4 moved that deposit's USDC to the new bridge, so
   a deposit refused here has no way back.
