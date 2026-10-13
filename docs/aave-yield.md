# AAVE yield — operator runbook

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `9b10946`, 2026-10-13.
Line references are to that file.*

The contract can lend idle USDC to the AAVE V3 USDC market. No live deploy does:
`aavePool` is immutable and is `address(0)` on every Sepolia bridge this tree
pins. This page is what an operator needs: where the money is, which command
collects what, and the two ways to get it wrong. The contract-level detail is in
[`EVM-contracts-spec.md`](EVM-contracts-spec.md) §10.

## Not wired on any live deploy

`deposit()` never talks to AAVE; idle USDC sits on the contract until the owner
calls `supplyToAave`. None of the Sepolia bridges this tree names have a pool:
the live pinned deploy `0xa1baf3f7…2cb36` (`crates/ackinacki-bridge/config/bridge_config.shellnet`),
the stopped `0x0F4F8b7E…2fc7` and `0xCdFd6Cef…4fdb`, and the rolled-back
NODE-4011 `0x8545129b…8644`. On each, `aavePool` is `address(0)`, `aaveEnabled`
is false, `suppliedPrincipal` is 0. `aavePool` is immutable, and
`setAaveEnabled(true)` reverts `InvalidAaveAddress` when the pool is zero, so
attaching AAVE means a new deploy.

`DeployShellnetE2EBridge` never wires the real pool; with `DEMO_AAVE=true` it wires
a demo pool of our own ([A demo pool on Sepolia](#a-demo-pool-on-sepolia)). Its USDC is Circle's Sepolia token
(`0x1c7D4B19…7238`). The AAVE V3 pool on Sepolia (`0x6Ae43d32…8951`) does not list
that token; `getReserveData` returns a zero aToken. The pool's underlying USDC is
`0x94a9D9AC…E4C8` (aToken `0x16dA4541…EB80`). `DeployRealBridge` always uses that
underlying as `usdc` on Sepolia; `USE_AAVE=true` only fills the pool and aToken
slots. A stand that must earn yield is a `DeployRealBridge` redeploy
([Launching a Sepolia stand with AAVE](#launching-a-sepolia-stand-with-aave)) with
`USE_AAVE=true` **and** the stand's withdraw aliases
(`WITHDRAW_ALT_DST_CHAIN_ID=1`, `WITHDRAW_ALT_DST_HOST_CHAIN_ID=11155111`,
`WITHDRAW_ALT_TOKEN_ID=3`). Left at the script's zeros, `withdrawByProof` for
`dstChainId=1` / `tokenId=3` reverts and the shellnet-l2 `preflight.sh` stops on
the USDC and alt-chain checks — Circle USDC and those aliases are what
`runtime.env.example` expects. That redeploy also changes the deposit token: the
live shellnet bridges hold Circle USDC, not the pool's.

`DeployRealBridge` only knows AAVE addresses on Ethereum mainnet and Sepolia. It
reverts on any other chain. The deposit allowlist's six L2s have no pool wiring
in this tree. Mainnet is not a deposit-source chain.

`supplyToAave` and `harvestYield` are `onlyOwner`. There is no keeper role and no
script in this repository that calls them outside tests. A bot that should run
them needs the owner key (or a later role split, which the contract does not
have).

## Launching a Sepolia stand with AAVE

**This is a test stand. It cannot run next to the live bridge, and it
must not take user deposits.** Same withdraw identity means a burn the
old bridge already paid can be paid again on the stand. Everything below
was checked against Sepolia on 2026-10-13.

### Why two bridges cannot share the Acki Nacki bridge

The stand is deployed with the live bridge's withdraw identity (`WITHDRAW_ACC_FR`,
`WITHDRAW_DAPP_FR`) and its aliases 1 / 11155111 / 3, and it is trusted on the same
Acki Nacki account `…::1a1a…`. Nothing in a withdrawal names the L1 bridge it is
for:

- `withdrawByProof` checks the withdraw identity, the destination chain and the
  token (`:1371-1386`). `WithdrawalPublicInputs` has no field for the bridge
  address, and the spent-nullifier map is per contract (`:1405`, `:1430`). One
  `initiateWithdrawal` burn is therefore payable **on every unpaused bridge** that
  holds an anchor covering it and enough `treasuryBalance` — once on each.
- `eccUSDCBridge.finalizeDeposit` mints the same ECC[3] for a deposit into any
  trusted bridge, and `initiateWithdrawal` burns that same ECC[3]. With the AAVE
  stand trusted next to the live bridge, test USDC from the permissionless AAVE
  faucet becomes ECC[3], and that ECC[3] withdraws as Circle USDC from the live
  bridge's treasury.

So the live bridge is paused before the stand takes anchors, and stays
paused for the life of the stand. Circulating ECC[3] is frozen until
the owner calls `unpause()` on `OLD_BRIDGE` (`:1621`). There is no
other way back: the owner has no call that moves that principal off
the paused bridge, and the stand is not a payout path. `unpause()`
has its own hazard — nullifiers are per contract, so a burn the stand
already paid is payable again on `OLD_BRIDGE`. Do that only after
teardown (step 9).

The nullifier map does not carry over. A burn `OLD_BRIDGE` already paid
can be proven again on the stand once one of its anchors covers that
block — including higher-layer windows, which span history from before
the stand's genesis. That is why the stand must not hold user deposits:
those deposits become the treasury a replay would take. Keep it to what
the operator puts in for the test (step 8) and do not point
`deposit-relayer` or the user CLI at the stand.

### What is different from the shellnet stand

| | Shellnet stand (`DeployShellnetE2EBridge`) | AAVE stand (`DeployRealBridge`, `USE_AAVE=true`) |
|---|---|---|
| Deposit token | Circle USDC `0x1c7D4B19…7238`, [faucet.circle.com](https://faucet.circle.com) | AAVE test USDC `0x94a9D9AC…E4C8`, the AAVE faucet `0xC959483D…f42D` |
| AAVE pool / aToken | none | `0x6Ae43d32…8951` / `0x16dA4541…EB80` |
| Withdraw aliases | default to 1 / 11155111 / 3 | default to 0 — set them by hand |
| Acki Nacki side | its address is trusted | trusted so the operator's test deposit can finalize (step 4); not a user cutover |

A user's Circle USDC cannot be deposited into the AAVE stand, and the reverse.

### 1. Check the AAVE reserve first

The Sepolia USDC reserve is regularly over its supply cap. On 2026-10-13 the cap was
2 000 000 000 USDC and about 4.55 × 10⁹ USDC was supplied, so **every `supplyToAave`
reverts with AAVE error `51` (`SUPPLY_CAP_EXCEEDED`)**. The bridge itself is fine in
that state — deposits, `verifyBlock` and payouts do not touch the pool while
`suppliedPrincipal` is 0 — but it earns nothing until AAVE raises the cap or
withdrawals bring the supply under it.

The cap is the smaller problem. The same day about 5.0 × 10⁹ USDC was borrowed and
**only about 11 000 USDC was left in the pool**: utilization is at its ceiling, and
nobody can withdraw more than that until borrowers repay. Supply cannot fall back
under the cap by withdrawals either, for the same reason. If AAVE raised the cap, a
bridge could supply, but `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`
and the AAVE top-up inside `withdrawByProof` would all fail beyond the pool's free
USDC — so payouts larger than the USDC on the bridge would revert. Launch on this
pool only when supply is under the cap **and** the free USDC covers what you plan to
supply with room to spare; until then, use the
[demo pool](#a-demo-pool-on-sepolia).

`DeployRealBridge` prints the cap and the supply with a warning; to read all three
numbers without deploying:

```bash
RPC=<Sepolia RPC>
POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951
USDC=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8
CFG=$(cast call $POOL 'getConfiguration(address)(uint256)' $USDC --rpc-url $RPC | awk '{print $1}')
# bash arithmetic is 64-bit; the configuration word is 256-bit.
python3 -c "c=int('$CFG',0); print('cap (whole USDC):', (c >> 116) & ((1 << 36) - 1))"   # 0 means no cap
cast call 0x16dA4541aD1807f4443d92D26044C1147406EB80 'totalSupply()(uint256)' --rpc-url $RPC   # supplied, 6 decimals
cast call $USDC 'balanceOf(address)(uint256)' 0x16dA4541aD1807f4443d92D26044C1147406EB80 --rpc-url $RPC   # free to withdraw
```

`AckiNackiBridgeAaveForkTest` lifts the cap inside its fork when it is reached, so the
fork suite checks the bridge against the real pool either way; a green fork run says
nothing about whether the live reserve takes supplies today.

### 2. Retire the live bridge

`OLD_BRIDGE` is the live bridge, `AN_BRIDGE` and `ABI` the Acki Nacki bridge account
and its ABI:

```bash
# tvm-cli v3 addresses accounts as <dapp_id>::<account_id>; the shellnet bridge's dapp_id is zero.
AN_BRIDGE=0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
ABI=contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json
AN="tvm-cli -u https://shellnet.ackinacki.org"
```

1. **Stop new burns.** `setPaused(true)` on the Acki Nacki bridge makes
   `initiateWithdrawal` and `finalizeDeposit` refuse
   (`contracts/an/exchange/eccUSDCBridge.sol`):

   ```bash
   $AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
     -m setPaused '{"paused":true}'
   ```

2. **Let the live bridge pay what is already burned.** Users withdraw with
   `ackinacki-bridge withdraw`; that proof can take hours. Keep the AN→ETH
   relayer running until every `proof_event_*.json` it holds has been paid
   (`isNullifierUsed(nullifier)` is true on `OLD_BRIDGE`), no burn made
   before step 1 is still waiting for its covering anchor, **and** a
   proof-window has passed since step 1 so a user who burned just before
   the pause has time to finish proving. Pausing `OLD_BRIDGE` while that
   proof is still running strands them: `withdrawByProof` reverts
   `whenNotPaused`, and the stand is not a payout path for those burns.

3. **Pause the live bridge.** `pause()` makes `deposit`, `verifyBlock`,
   `applyBkSetUpdate` and `withdrawByProof` revert (`:1614`), so it pays nothing
   from here on:

   ```bash
   cast send $OLD_BRIDGE 'pause()' --rpc-url $RPC --private-key $OWNER_PK
   cast call $OLD_BRIDGE 'paused()(bool)' --rpc-url $RPC   # true
   ```

   Then stop its AN→ETH relayer (step 5 reuses the instance with fresh state).

   **Leave Acki Nacki paused.** The stand does not exist yet. Its genesis is
   taken at deploy time from the current head (`deploy_bridge_bundle.sh` /
   `compute_bridge_anchors --at-head`). A burn made after a reopen, if a
   window boundary passes before that deploy, sits below the L1/L2 seed.
   Higher-layer windows can still cover older history once the stand has
   been proving — that is the replay path above, not a way to pay a user
   whose burn landed in that gap. `OLD_BRIDGE` is already paused and
   cannot pay it.

### 3. Deploy

Deploy after step 2, while Acki Nacki is still paused: the stand must not hold
anchors or a treasury while the live bridge can still pay, and no new burn
must land before the stand exists.

The usual way is the shellnet bundle script with `USE_AAVE=true`. It derives the
genesis values from the shellnet head, runs `DeployRealBridge` with the shellnet
withdraw aliases, and writes `L2_config/env` with the `EXPECTED_USDC_ADDRESS` and
`EXPECTED_AAVE_POOL` lines the relayer's preflight needs:

```bash
cd crates/bridge-prover-libraries
LEVEL=2 USE_AAVE=true CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
  PRIVATE_KEY=0x... WITHDRAW_ACC_FR=<Fr> SEPOLIA_RPC_URL=<Sepolia RPC> \
  ./scripts/deploy_bridge_bundle.sh
```

By hand, with genesis values you already have — the verifier `.bin` files are read
from `contracts/ethereum/verifiers/`:

```bash
cd contracts/ethereum
export PRIVATE_KEY=0x...                      # becomes owner and yieldRecipient
export USE_AAVE=true
export WIRE_VERIFY_BLOCK=true
export GENESIS_BK_SET_COMMITMENT=<Fr>
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=<Fr>
export GENESIS_LAST_SEEN_BLOCK_SEQNO=<seqno>  # the bootstrap seqno the prover starts from
export WITHDRAW_ACC_FR=<Fr>
export WITHDRAW_DAPP_FR=<Fr or 0>
export WITHDRAW_ALT_DST_CHAIN_ID=1            # shellnet aliases; DeployRealBridge defaults them to 0
export WITHDRAW_ALT_DST_HOST_CHAIN_ID=11155111
export WITHDRAW_ALT_TOKEN_ID=3

forge script script/DeployRealBridge.s.sol:DeployRealBridge --rpc-url $RPC   # simulation
forge script script/DeployRealBridge.s.sol:DeployRealBridge --rpc-url $RPC --broadcast
```

The script stops before broadcasting if the pool no longer lists the USDC under the
hard-coded aToken. The whole deploy is about 31 M gas. Deploy from the relayer's EOA:
the shellnet-l2 `preflight.sh` requires `owner()` to be that EOA.

Check the result on chain; `aave_enabled` in `deployment_real.json` only repeats the
flag:

```bash
cast call $BRIDGE 'usdc()(address)'           --rpc-url $RPC   # 0x94a9D9AC…E4C8
cast call $BRIDGE 'aavePool()(address)'       --rpc-url $RPC   # 0x6Ae43d32…8951
cast call $BRIDGE 'aUSDC()(address)'          --rpc-url $RPC   # 0x16dA4541…EB80
cast call $BRIDGE 'aaveEnabled()(bool)'       --rpc-url $RPC   # true
cast call $BRIDGE 'bridgeWithdrawalAltTokenId()(uint256)' --rpc-url $RPC   # 3
cast call $BRIDGE 'yieldRecipient()(address)' --rpc-url $RPC
```

### 4. Trust the stand on Acki Nacki

`eccUSDCBridge.finalizeDeposit` rejects a deposit whose proven
`(chainId, contractAddress)` is not in its allowlist (`contracts/an/README.md`). The
owner of the Acki Nacki bridge account adds the stand with
`setTrustedL1Bridge(chainId, l1Bridge, allowed)`:

```bash
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<stand address>","allowed":true}'
$AN runx --abi $ABI --addr $AN_BRIDGE \
  -m isTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<stand address>"}'
```

Until this is done every deposit into the stand is proven but refused on Acki Nacki.
Withdrawals (AN→ETH) do not depend on it. Leave `OLD_BRIDGE` in the set until
step 7: deposits made into it before its pause still need `finalizeDeposit`.
Take the stand out of the set in step 9: `deposit()` is public, and the
CLI will `finalizeDeposit` against any trusted address.

### 5. Point the AN→ETH relayer at the stand

The stopped instance from step 2, **with fresh state**. Its state files belong
to the live bridge; move them aside rather than deleting them:

```bash
ROOT=/var/lib/gosh-bridge-relayer   # RELAYER_RUNTIME_ROOT
sudo mv $ROOT/L2_config $ROOT/L2_config.$OLD_BRIDGE
sudo install -d -o gosh-relayer -g gosh-relayer $ROOT/L2_config $ROOT/L2_config/state
```

In `/etc/gosh-bridge-relayer/shellnet-l2.env` set `BRIDGE_ADDRESS`,
`BRIDGE_DEPLOY_BLOCK`, `BRIDGE_BOOTSTRAP_SEQNO`, the `EXPECTED_*` genesis and
withdraw values you deployed with,
`EXPECTED_USDC_ADDRESS=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8` and
`EXPECTED_AAVE_POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951`, then run
`preflight` and start the service. `preflight` writes
`$BRIDGE_STATE_DIR/bridge_address` and stops if **that file** names another
bridge. It does not look at leftover `L2_config/relayer-state.json`.
`daemon-live` does, and it will drift if that file is still from
`OLD_BRIDGE` — that is why the whole `L2_config` is moved aside above,
not only `state/`. The stand can now `verifyBlock`; it still cannot see
burns that have not been made yet.

### 6. Finish leftover deposits, then pause Acki Nacki again

`finalizeDeposit` was refusing leftover `OLD_BRIDGE` deposits while
Acki Nacki was paused. Open it only for that work, then pause it
again so no one can burn:

```bash
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setPaused '{"paused":false}'
# deposit-relayer, still on OLD_BRIDGE, finalizes what was already deposited
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setPaused '{"paused":true}'
```

A burn made in that window has no payout: `OLD_BRIDGE` is paused
(`withdrawByProof` reverts `whenNotPaused`) and the stand is not a
user payout path. Do not announce the window. Leave Acki Nacki paused
when you are done. Circulating ECC[3] stays frozen until teardown.

### 7. Stop `deposit-relayer` and untrust `OLD_BRIDGE`

Keep `deposit-relayer` on `OLD_BRIDGE` through step 6. The daemon
follows one `--bridge-address`; if it is moved first, those deposits
stay on the paused bridge and the owner has no call that moves user
principal off it.

Then **stop** `deposit-relayer`. Do not point it, or the user
`ackinacki-bridge` CLI, at the stand. That instruction is not a
control: `deposit()` is public and the CLI will `finalizeDeposit`
against any trusted address. The control is `pause()` on the stand
and taking it out of the trusted set (step 9).

Then remove `OLD_BRIDGE` from the trusted set (new deposits already
fail there — it is paused):

```bash
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<OLD_BRIDGE>","allowed":false}'
```

### 8. Fund and supply

```bash
FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D   # permissionless, 10 000 USDC per call
cast send $FAUCET 'mint(address,address,uint256)' $USDC $WALLET 10000000000 \
  --rpc-url $RPC --private-key $WALLET_PK
cast send $USDC 'approve(address,uint256)' $BRIDGE 10000000000 --rpc-url $RPC --private-key $WALLET_PK
cast send $BRIDGE 'deposit(uint256,int8,bytes32)' 10000000000 0 <AN account> \
  --rpc-url $RPC --private-key $WALLET_PK

cast send $BRIDGE 'supplyToAave(uint256)' $(cast max-uint) --rpc-url $RPC --private-key $OWNER_PK
```

`supplyToAave(max)` keeps `liquidReserveBps` (10 %) of `treasuryBalance` on the
contract and supplies the rest. Run it again after new deposits; nothing supplies on
its own. If it reverts with `51`, see step 1.

When the USDC on the contract is below a withdrawal's amount, `withdrawByProof`
withdraws the difference from AAVE in the same transaction — as long as the pool has
that much free USDC (step 1). On a pool that is short of it, raise the reserve with
`setLiquidReserveBps(5000)` (the 50 % maximum) and supply in small amounts. Collect
yield with `harvestYield(accruedYield())` (below), after pointing `yieldRecipient`
where it should go.

The operator's own `finalizeDeposit` needs Acki Nacki open for that
message only: the same brief unpause / pause as step 6. Then go to
teardown.

### 9. Tear the stand down

`deposit()` on the stand is public. Leaving it trusted and unpaused is
how a third party mints ECC[3] from faucet USDC, or adds treasury a
replay would take. After the test:

```bash
cast send $BRIDGE 'pause()' --rpc-url $RPC --private-key $OWNER_PK
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<stand address>","allowed":false}'
```

Leave Acki Nacki paused. Circulating ECC[3] is still frozen.

To pay those holders again, `unpause()` on `OLD_BRIDGE`. That is the
only way back. A burn the stand already paid is then payable again on
`OLD_BRIDGE` — the nullifier map is per contract. Do not unpause
`OLD_BRIDGE` while the stand is still unpaused and funded.

## A demo pool on Sepolia

For a stand that has to show the yield flow while the real pool cannot take supplies
(above). `src/DemoAavePool.sol` speaks the part of the AAVE V3 Pool the bridge uses —
`supply`, `withdraw`, `getReserveData` — over any token, with a receipt token
`DemoAToken` that only the pool mints and burns and that cannot be transferred. It
holds every supplied token 1:1: no borrowing, no supply cap, no interest rate. **Yield
is only what the pool's owner pays in** with `accrueYield(holder, amount)`, which pulls
`amount` of real USDC from the owner and credits the same in receipt tokens to
`holder`. Every receipt token is therefore backed and redeemable, and
`harvestYield`, `withdrawFromAave`, `emergencyWithdrawAll` and the top-up in
`withdrawByProof` always find the USDC they ask for. It shows that the mechanics work,
not what a market would pay.

The demo stand keeps everything else of the shellnet stand — `DeployShellnetE2EBridge`,
Circle USDC, the withdraw aliases — so users fund from
[faucet.circle.com](https://faucet.circle.com) as before. Its deposit token is the live
bridge's, so the faucet problem above does not arise, but one burn would still be
payable on both bridges: the demo stand is the same kind of test stand.
Retire the live bridge first ([step 2](#2-retire-the-live-bridge)), then
deploy while Acki Nacki is still paused:

```bash
cd crates/bridge-prover-libraries
LEVEL=2 DEMO_AAVE=true CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS \
  PRIVATE_KEY=0x... WITHDRAW_ACC_FR=<Fr> SEPOLIA_RPC_URL=<Sepolia RPC> \
  ./scripts/deploy_bridge_bundle.sh
```

`DEMO_AAVE=true` (also read by `DeployShellnetE2EBridge` directly; refused on
mainnet) deploys a `DemoAavePool` over Circle USDC, owned by the `PRIVATE_KEY`
address, and wires the bridge to it and its `DemoAToken`. The script prints the pool
address and writes it into `L2_config/env` as `EXPECTED_AAVE_POOL`; set that and
`EXPECTED_USDC_ADDRESS=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` in the relayer's
runtime env. Steps 4–9 of the section above apply unchanged: trust the
stand, point the AN→ETH relayer with fresh state, briefly reopen Acki
Nacki so leftover `OLD_BRIDGE` deposits can finalize, pause it again,
stop `deposit-relayer`, then pause and untrust the stand. Do not move
the ETH→AN relayer or the user CLI onto the stand.

After deposits, supply, pay yield in, and collect it. `USDC` is Circle's token here,
not the AAVE test USDC of step 1:

```bash
USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238   # the demo pool's underlying
POOL=<DemoAavePool>
cast send $BRIDGE 'supplyToAave(uint256)' $(cast max-uint) --rpc-url $RPC --private-key $OWNER_PK
cast send $USDC 'approve(address,uint256)' $POOL 1000000 --rpc-url $RPC --private-key $POOL_OWNER_PK
cast send $POOL 'accrueYield(address,uint256)' $BRIDGE 1000000 --rpc-url $RPC --private-key $POOL_OWNER_PK
cast call $BRIDGE 'accruedYield()(uint256)' --rpc-url $RPC                                    # 1000000
cast send $BRIDGE 'harvestYield(uint256)' 1000000 --rpc-url $RPC --private-key $OWNER_PK
```

Deployed through the bundle script, the pool owner and the bridge owner are the same
key. `getReserveData` on the demo pool returns a zero configuration, which reads as no
supply cap.

## Where the money is

| | What it means | Read it with |
|---|---|---|
| **Owed to users** | A book number. Up on deposit, down on a proven payout — nothing else. | `treasuryBalance()` (`:104`) |
| **Liquid** | USDC on the contract, spendable now. | `usdc.balanceOf(bridge)` |
| **In AAVE** | `suppliedPrincipal()` is what we put in; `aUsdcBalance()` is what it is worth now. | both (`:129`, `:1709`) |

Two derived numbers decide everything below:

* `accruedYield()` = `aUsdcBalance() − suppliedPrincipal()` — yield **still inside AAVE** (`:1715`).
* `excessUsdc()` = `usdc.balanceOf(bridge) − treasuryBalance()` — surplus **already on the contract**
  (`:1591`).

Solvent iff `totalAssets() ≥ treasuryBalance()`. `treasuryBalance` is book value and is never
reconciled against real assets, so a loss in AAVE does not shrink it — it surfaces later, on a payout.

## Which command collects what

**The pocket decides, not the history.**

| Yield is… | Command | Fails with |
|---|---|---|
| in AAVE | `harvestYield(amount)` (`:1571`) | `NoYield` if `accruedYield() == 0`, `amount == 0` or `amount > accruedYield()` |
| on the contract | `skimExcessUsdc(amount)` (`:1599`) | `NoExcessUsdc` if `excessUsdc() == 0` |

Both send to `yieldRecipient`, never to the caller. `skimExcessUsdc` accepts `type(uint256).max` for
"all of it"; `harvestYield` does not — anything above `accruedYield()` reverts `NoYield`, so pass
the current `accruedYield()` to collect everything.
Neither can touch user principal — neither function appears in the accounting of what we supplied to
AAVE, and that absence is the guarantee.

**The two unwind paths leave yield in different places.** This is the part that gets misread:

| You called | Yield ends up | Collect with |
|---|---|---|
| `withdrawFromAave(max)` (`:1531`) | stays in AAVE — it pulls only the booked principal | `harvestYield` |
| `emergencyWithdrawAll()` (`:1548`) | on the contract — it pulls principal **and** interest, and zeroes the book | `skimExcessUsdc` |

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
`harvestYield` works — note it has **no** `aaveEnabled` guard (`:1571-1573`), so a disabled module
does not block collection. `withdrawFromAave`, by contrast, reverts `InvalidAmount` while
`suppliedPrincipal` is zero (`:1532-1533`).

This is why the rule above is stated over pockets rather than over history: it answers all three
cases without anyone having to remember what was called last.

## Two ways to get it wrong

**1. Re-supplying before you skim.** `supplyToAave` subtracts only the liquid reserve, not the
surplus (`_amountSupplyable`, `:1677-1682`), so uncollected yield goes back into the pool and is
booked as *principal*. It is then invisible to both collectors. Not lost — it keeps earning — but
recovering it needs a `withdrawFromAave(max)` first. Correct order after an emergency:

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
withdrawFromAave(amount)   # repeat until principal is 0 or AAVE stops paying
setAaveEnabled(false)
harvestYield(accruedYield())  # only if accruedYield() > 0 — yield still in AAVE; max reverts NoYield
skimExcessUsdc(max)        # liquid surplus, if any
```

`withdrawFromAave` pulls booked principal only, so leftover yield stays in AAVE and
`harvestYield` is the right collector for that pocket. `skimExcessUsdc` is for whatever
already sits on the contract.

**2. Reading a skim revert as "no yield".** If AAVE returned less than the principal — the depeg case
the emergency exists for — then `excessUsdc()` is zero and the skim reverts. That is the principal
guard doing its job, and it may be telling you there is a shortfall. Check `totalAssets()` against
`treasuryBalance()` before assuming it just means "nothing to collect".

## Check the recipient before the first collection

The constructor sets `owner = yieldRecipient = msg.sender` (`:680-681`). Ownership moves in two
steps, `transferOwnership` then `acceptOwnership` by the new owner (`:1649`); `acceptOwnership`
moves `yieldRecipient` along only while it still equals the outgoing owner (`:1664-1667`). A
recipient set explicitly with `setYieldRecipient` (`:1641`) stays where it is through any number of
ownership changes, so after a handover to a multisig, yield can still go to an address the new
owner does not control.

Separating the two roles is intentional and fine. The hazard is that they separate *silently*.

```bash
cast call $BRIDGE 'owner()(address)'          --rpc-url $RPC
cast call $BRIDGE 'yieldRecipient()(address)' --rpc-url $RPC   # is this who should be paid?
```

## Health check

```bash
cast call $BRIDGE 'treasuryBalance()(uint256)'   --rpc-url $RPC   # owed to users (book)
cast call $BRIDGE 'suppliedPrincipal()(uint256)' --rpc-url $RPC   # supplied to AAVE (book)
cast call $BRIDGE 'aUsdcBalance()(uint256)'      --rpc-url $RPC   # position value now
cast call $BRIDGE 'accruedYield()(uint256)'      --rpc-url $RPC   # harvestable, in AAVE
cast call $BRIDGE 'excessUsdc()(uint256)'        --rpc-url $RPC   # skimmable, on contract
cast call $BRIDGE 'totalAssets()(uint256)'       --rpc-url $RPC   # liquid + aUSDC
cast call $BRIDGE 'aaveEnabled()(bool)'          --rpc-url $RPC
```

Alert a keeper on any of these:

```
totalAssets() < treasuryBalance()                 # solvency breach
suppliedPrincipal() > aUsdcBalance()              # book ahead of AAVE; should never happen
aaveEnabled() == false AND suppliedPrincipal > 0  # stuck position, needs a manual unwind
excessUsdc() > 0 across a collection cycle        # uncollected surplus; the next supply absorbs it
```

## Other owner controls

`setLiquidReserveBps` — the share of user principal kept liquid. Default `1_000` = 10 % (`:683`),
capped at 50 % (`MAX_LIQUID_RESERVE_BPS`, `:61`). Ours, not an AAVE convention. `setAaveEnabled` —
gate on new supplies; `emergencyWithdrawAll` turns it off, and only an explicit call turns it back
on.

The owner cannot reduce `treasuryBalance` and cannot replace a verifier — those bindings are
immutable. The owner can pause the bridge (`pause()` / `unpause()`, `:1614`, `:1621`): `deposit`,
`verifyBlock`, `applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield
controls above stay available, so funds can be moved out of AAVE during a pause
(`EVM-contracts-spec.md`, §10.1).
