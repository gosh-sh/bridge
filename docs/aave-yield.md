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
`AckiNackiBridgeAaveArbSepoliaForkTest` does that check and then calls `supplyToAave`,
`harvestYield` (the reserve already has a borrower), `withdrawFromAave`, `emergencyWithdrawAll`
and a `withdrawByProof` that tops the payout up from the pool:

```bash
cd contracts/ethereum
FOUNDRY_PROFILE=fork FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
  forge test --match-contract AckiNackiBridgeAaveArbSepoliaForkTest -vv
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
`0x3FB082a905a895151224F6d93bD074E816338638` (`README.md`), has no pool, so this
deploy is a second contract. Call it the old bridge; the AAVE deploy is the new bridge.

### Why the old bridge has to be empty first

A withdrawal proof carries no EVM bridge address
(`WithdrawalPublicInputs` in `IBridgeWithdrawalVerifier.sol`). `withdrawByProof`
checks `dappFr` and `accFr` (the same Acki Nacki bridge on both deploys),
`dstChainId`, that contract's own `_nullifiers`, and that contract's own anchor
window. `verifyBlock` has no caller restriction. One Acki Nacki burn with
`dstChainId` 421614 can be paid by every unpaused bridge that has an anchor
covering it and USDC in `treasuryBalance`. The two treasuries are separate, and
nothing but `withdrawByProof` takes USDC out of one: there is no sweep, and
`emergencyWithdrawAll` only pulls the AAVE position back into the same contract.
`setTrustedL1Bridge` on `eccUSDCBridge` adds or removes one address, and only
`finalizeDeposit` reads that list.

So the contracts give two ways to get a cutover wrong:

* Both bridges unpaused, both holding USDC: one burn is paid twice.
* The old bridge paused while `treasuryBalance()` is still some amount X: those X USDC
  stay locked in it, the eccUSDC they back is still burnable towards 421614, and the new
  bridge pays those burns out of its own depositors' USDC. It ends X short, and its last
  withdrawals revert `WithdrawTreasuryShortfall`.

Neither contract can retire a bridge that still owes USDC. Cut over only when the old
bridge owes nothing:

1. Deploy the new bridge, with the inputs the shellnet-l2 kit needs (below) if the kit
   is to run it. Then call `pause()` on it from the deploying account (the owner) at
   once. Check `paused()` is `true`. While it is paused, `deposit`,
   `verifyBlock`, `applyBkSetUpdate` and `withdrawByProof` revert, so it pays no burn.
   Do not trust it on Acki Nacki and do not start a relayer for it yet.
2. Stop sending users to the old bridge: rebuild the web form without
   `BRIDGE_CONTRACT_ADDRESS`, which makes it refuse deposits
   (`frontend/src/config.rs`). Keep the old bridge unpaused and trusted, and keep its
   deposit relayer, its `eth-lc-relayer arbitrum-anchors` and its Acki Nacki to Ethereum
   relayer running. Deposits already sent to it still have to be finalized on Acki
   Nacki, and burns against them still have to be paid by it.
3. Wait until all three hold: `parked_deposit_ids` and `outstanding_deposits` in the
   old bridge's deposit relayer `state.json` are empty, `treasuryBalance()` on the old
   bridge reads 0, and `getDepositLimits(421614)` on `eccUSDCBridge` returns
   `outstanding` 0. A burn is refused above `outstanding`, so 0 means no eccUSDC can
   still be redeemed on 421614. If holders never burn, the numbers never reach 0. The
   cutover then cannot be done safely: leave the old bridge live and the new one paused.
4. `pause()` the old bridge and check all three again: a deposit sent to the contract
   directly can land between steps 3 and 4. If any fails, `unpause()` it and go back to
   step 3. Otherwise stop the old bridge's relayers and call
   `setTrustedL1Bridge(421614, <old bridge>, false)` on `eccUSDCBridge`. Leave the old
   bridge paused.
5. `setTrustedL1Bridge(421614, <new bridge>, true)`. Without it, `finalizeDeposit`
   reverts `ERR_UNSUPPORTED_SRC_CHAIN`. Reconfigure the consumers below, `unpause()` the
   new bridge, then start them and rebuild the web form with its address.

The consumers:

* Deposit relayer. `scripts/ursus/deposit-relayer.service` passes
  `--from-block ${FROM_BLOCK}` and `--start-deposit-id ${START_DEPOSIT_ID}`, and
  does not pass `--force-state`. `--from-block` wins over `BRIDGE_DEPLOY_BLOCK`
  whenever it is not 0 (`resolve_from_block`), and the current `FROM_BLOCK` is
  the old deploy block `323405848`, so setting only `BRIDGE_DEPLOY_BLOCK` is
  ignored. In `deposit-relayer.env` set `BRIDGE_ADDRESS` to the new bridge,
  `FROM_BLOCK` and `BRIDGE_DEPLOY_BLOCK` to its deploy block, and
  `START_DEPOSIT_ID=0`. A raised `START_DEPOSIT_ID` never delivers deposits
  below that id.
  A `state.json` stamped with the old bridge refuses to start with
  `state.json deployment mismatch`. `--force-state` replaces that file with a
  fresh one: progress, the scan cursor, parked ids, outstanding ids and
  `last_finalized_deposit_id` are dropped (`RelayerState::ensure_deployment`).
  After step 3 it holds nothing the old bridge still needs. Keep a copy, add the
  flag for one start, then take it back out.
* `eth-lc-relayer arbitrum-anchors --bridge-address <new bridge>`. It anchors
  only Arbitrum blocks that contain `Deposit` logs from that address. An
  `ARBITRUM_ANCHOR_STATE` file recorded for the old bridge (the default path is
  `./arbitrum-anchors-state.json`) refuses to start and tells the operator to
  move it away. Move that file away and pass `--from-block` of the new deploy
  block. `BRIDGE_DEPLOY_BLOCK` applies only when `--from-block` is 0.
* The web form. `BRIDGE_CONTRACT_ADDRESS` is compiled in
  (`frontend/src/config.rs`). Rebuild the form with the new address.
* `ackinacki-bridge`: `BRIDGE_ADDRESS` and `BRIDGE_DEPLOY_BLOCK`.
* The shellnet-l2 kit runs `relayer daemon-live`
  (`crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/container-entrypoint.sh`),
  and its `scripts/preflight.sh` refuses a bridge that does not match its env. The kit
  runs anchor level 2 only, so deploy with the output of
  `compute_bridge_anchors --level 2` (with `--at-head` or `--seed-seqno`):
  `BUNDLE_STRIDE`, `GENESIS_BK_SET_COMMITMENT`, `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`, `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, and
  `GENESIS_SEED_SEQNO` exported as `GENESIS_LAST_SEEN_BLOCK_SEQNO`, as
  `deploy_bridge_bundle.sh` does. Set `PRIVATE_KEY` to the key of the kit's
  `RELAYER_ADDRESS`: the deploying account becomes the owner, and preflight requires the
  owner to be `RELAYER_ADDRESS`. Then set in `runtime.env`:
  * `BRIDGE_ADDRESS` and `BRIDGE_DEPLOY_BLOCK`.
  * `RPC_URL`, and `RPC_FAILOVER_URLS` if set: Arbitrum Sepolia endpoints, and
    `EXPECTED_EVM_CHAIN_ID=421614`. Preflight checks the chain id of every endpoint
    that answers.
  * `EXPECTED_USDC_ADDRESS=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d`. The example
    holds Ethereum Sepolia USDC.
  * `BRIDGE_BOOTSTRAP_SEQNO`: `GENESIS_SEED_SEQNO`. `EXPECTED_GENESIS_PREV_ANCHOR`:
    `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`. `EXPECTED_BK_SET_COMMITMENT`:
    `GENESIS_BK_SET_COMMITMENT`.
  * `EXPECTED_WITHDRAW_DAPP_FR` and `EXPECTED_WITHDRAW_ACC_FR`: the `WITHDRAW_DAPP_FR`
    and `WITHDRAW_ACC_FR` of the deploy.
  * `EXPECTED_WITHDRAW_ALT_DST_CHAIN_ID`, `EXPECTED_WITHDRAW_ALT_DST_HOST_CHAIN_ID` and
    `EXPECTED_WITHDRAW_ALT_TOKEN_ID`: the `WITHDRAW_ALT_*` values of the deploy. The
    example's 1, 11155111 and 3 are the Ethereum Sepolia bridge's. The chain id alias
    applies only where `block.chainid` is the host chain id, and a burn towards this
    bridge names 421614 itself, so a deploy that sets only `WITHDRAW_ALT_TOKEN_ID=3`
    needs 0, 0 and 3.

  Move the `*.json` files in `BRIDGE_STATE_DIR` aside, as `deploy_bridge_bundle.sh`
  does. While `prover_state.json` is there, preflight skips its cold-start check of the
  new bridge's cursor and `expectedPrevAnchor(2)`. `relayer daemon-bridge` takes
  `--bridge-address`, has no deploy-block setting, and does not submit
  `applyBkSetUpdate`.

## Where the money is

| | What it means | Read it with |
|---|---|---|
| **Owed to users** | A book number. Up on deposit, down on a proven payout — nothing else. | `treasuryBalance()` (`:98`) |
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

The owner cannot reduce `treasuryBalance` and cannot replace a verifier — those bindings are
immutable. The owner can pause the bridge (`pause()` / `unpause()`): `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield controls above stay
available, so funds can be moved out of AAVE during a pause (`EVM-contracts-spec.md`, §10.1).
