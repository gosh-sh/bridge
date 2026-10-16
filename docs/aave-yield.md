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

`DeployShellnetE2EBridge` never wires a pool at all. Its USDC is Circle's Sepolia token
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

> **Tooling this runbook needs is not in the tree yet.** `USE_AAVE` is read by
> `DeployRealBridge` (`_readWireInputs`), so the deploy itself works. But
> `deploy_bridge_bundle.sh` does not pass `USE_AAVE` through, `preflight.sh` does
> not read `EXPECTED_AAVE_POOL`, and `DeployRealBridge` does not print the
> reserve's cap. Until those land, run `DeployRealBridge` directly and check the
> cap with the `cast` calls below. See
> [gosh-sh/bridge#97](https://github.com/gosh-sh/bridge/pull/97) for that script
> support.

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
teardown (step 8).

The nullifier map does not carry over. A burn `OLD_BRIDGE` already paid
can be proven again on the stand once one of its anchors covers that
block — including higher-layer windows, which span history from before
the stand's genesis. That is why the stand must not hold user deposits:
those deposits become the treasury a replay would take. Keep it to what
the operator puts in for the test (step 7) and do not point
`deposit-relayer` or the user CLI at the stand.

### What is different from the shellnet stand

| | Shellnet stand (`DeployShellnetE2EBridge`) | AAVE stand (`DeployRealBridge`, `USE_AAVE=true`) |
|---|---|---|
| Deposit token | Circle USDC `0x1c7D4B19…7238`, [faucet.circle.com](https://faucet.circle.com) | AAVE test USDC `0x94a9D9AC…E4C8`, the AAVE faucet `0xC959483D…f42D` |
| AAVE pool / aToken | none | `0x6Ae43d32…8951` / `0x16dA4541…EB80` |
| Withdraw aliases | default to 1 / 11155111 / 3 | default to 0 — set them by hand |
| Acki Nacki side | its address is trusted | trusted only for the operator finalize window (step 7), then untrusted |

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
supply with room to spare; until then, there is no Sepolia stand that can earn yield
(see [Planned: a test integration on Arbitrum
Sepolia](#planned-a-test-integration-on-arbitrum-sepolia)).

To read all three numbers before deploying:

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

   Then stop its AN→ETH relayer (step 4 reuses the instance with fresh state).

   **Leave Acki Nacki paused.** The stand does not exist yet. Its genesis is
   taken at deploy time from the current head (`compute_bridge_anchors --at-head`). A burn made after a reopen, if a
   window boundary passes before that deploy, sits below the L1/L2 seed.
   Higher-layer windows can still cover older history once the stand has
   been proving — that is the replay path above, not a way to pay a user
   whose burn landed in that gap. `OLD_BRIDGE` is already paused and
   cannot pay it.

### 3. Deploy

Deploy after step 2, while Acki Nacki is still paused: the stand must not hold
anchors or a treasury while the live bridge can still pay, and no new burn
must land before the stand exists.

Once `deploy_bridge_bundle.sh` passes `USE_AAVE` through, that will be the usual
way: it derives the genesis values from the shellnet head, runs `DeployRealBridge`
with the shellnet withdraw aliases, and writes `L2_config/env`. It does not do that
today, so deploy by hand. with genesis values you already have — the verifier `.bin` files are read
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

**Pause the stand immediately.** `deposit()` is public and the AAVE
faucet is permissionless. A deposit that lands before the pause can be
finalized later, the moment the stand is trusted and Acki Nacki is
open, and that ECC[3] withdraws as Circle USDC from `OLD_BRIDGE` after
teardown.

```bash
cast send $BRIDGE 'pause()' --rpc-url $RPC --private-key $OWNER_PK
```

Do not trust the stand. A finalize would mint ECC[3] from faucet USDC,
and teardown would make that ECC[3] redeemable as Circle USDC on
`OLD_BRIDGE`. Step 7 deposits only so `supplyToAave` has a treasury.

### 4. Point the AN→ETH relayer at the stand

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
`EXPECTED_USDC_ADDRESS=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8`, then run
`preflight` **only**. Do not start the service. `preflight` does not check the
bridge's `aavePool()` today — see the note at the top of this section — so verify
it by hand with `cast call $BRIDGE 'aavePool()(address)'`. Move the whole
`L2_config` aside rather than one file: `daemon-live` reads a leftover
`relayer-state.json`.
`daemon-live` does, and it will drift if that file is still from
`OLD_BRIDGE` — that is why the whole `L2_config` is moved aside above,
not only `state/`. `verifyBlock` is `whenNotPaused`: leave the stand
paused and the relayer stopped. After step 7 (stand still untrusted,
Acki Nacki paused) you may unpause the stand for `verifyBlock` only;
pause it again afterwards. Do not trust it for that.

### 5. Finish leftover deposits, then pause Acki Nacki again

`finalizeDeposit` was refusing leftover `OLD_BRIDGE` deposits while
Acki Nacki was paused. Open it only for that work, then pause it
again so no one can burn. The stand is paused and **not trusted**:
nothing deposited into it can mint ECC[3] in this window.

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

### 6. Stop `deposit-relayer` and untrust `OLD_BRIDGE`

Keep `deposit-relayer` on `OLD_BRIDGE` through step 5. The daemon
follows one `--bridge-address`; if it is moved first, those deposits
stay on the paused bridge and the owner has no call that moves user
principal off it.

Then **stop** `deposit-relayer`. Do not point it, or the user CLI, at
the stand.

Then remove `OLD_BRIDGE` from the trusted set (new deposits already
fail there — it is paused):

```bash
$AN callx --abi $ABI --addr $AN_BRIDGE --keys <owner keys of the AN bridge> \
  -m setTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<OLD_BRIDGE>","allowed":false}'
```

### 7. Fund the stand. Do not finalize.

Unpause, deposit the operator's test USDC, pause again. **Do not trust
the stand and do not finalize.** A finalize would mint ECC[3] from
faucet USDC; teardown would then make that ECC[3] redeemable as Circle
USDC on `OLD_BRIDGE`. `supplyToAave` / `harvestYield` are `onlyOwner`
and not `whenNotPaused` — they run on the paused stand and do not mint.

`deposit()` is public during the unpause window. Count deposits before
and after; if `depositCounter()` moved by more than one, leave the
stand untrusted forever.

```bash
BEFORE=$(cast call $BRIDGE 'depositCounter()(uint256)' --rpc-url $RPC)
cast send $BRIDGE 'unpause()' --rpc-url $RPC --private-key $OWNER_PK
FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D   # permissionless, 10 000 USDC per call
cast send $FAUCET 'mint(address,address,uint256)' $USDC $WALLET 10000000000 \
  --rpc-url $RPC --private-key $WALLET_PK
cast send $USDC 'approve(address,uint256)' $BRIDGE 10000000000 --rpc-url $RPC --private-key $WALLET_PK
cast send $BRIDGE 'deposit(uint256,int8,bytes32)' 10000000000 0 <AN account> \
  --rpc-url $RPC --private-key $WALLET_PK
cast send $BRIDGE 'pause()' --rpc-url $RPC --private-key $OWNER_PK
AFTER=$(cast call $BRIDGE 'depositCounter()(uint256)' --rpc-url $RPC)
# AFTER must be BEFORE+1. A larger jump is a third-party deposit; do not trust.
```

Never trust the stand. Never point `deposit-relayer` or the user CLI
at it. Then:

```bash
cast send $BRIDGE 'supplyToAave(uint256)' $(cast max-uint) --rpc-url $RPC --private-key $OWNER_PK
```

`supplyToAave(max)` keeps `liquidReserveBps` (10 %) of `treasuryBalance` on the
contract and supplies the rest. If it reverts with `51`, see step 1.
Collect yield with `harvestYield(accruedYield())` (below), after pointing
`yieldRecipient` where it should go.

### 8. Tear the stand down

The stand is already paused and untrusted. If you unpaused it for
`verifyBlock`, pause it again. If anyone finalized a stand deposit,
**stop** — that ECC[3] is unbacked Circle-USDC debt on `OLD_BRIDGE`.
Keep Acki Nacki paused and do not restore payouts.

Otherwise restore every piece the earlier steps took apart:

1. `unpause()` on `OLD_BRIDGE` — `withdrawByProof` is `whenNotPaused`.
2. `setPaused(false)` on Acki Nacki — holders cannot `initiateWithdrawal` while it is paused.
3. `setTrustedL1Bridge(11155111, OLD_BRIDGE, true)` — step 6 removed it; new deposits otherwise finalize nowhere.
4. Move the stand's `L2_config` aside, then restore `L2_config.$OLD_BRIDGE` (step 4).
5. Revert `/etc/gosh-bridge-relayer/shellnet-l2.env` to the `OLD_BRIDGE` values (step 4 overwrote `BRIDGE_ADDRESS`, `BRIDGE_DEPLOY_BLOCK`, `BRIDGE_BOOTSTRAP_SEQNO`, `EXPECTED_*`, `EXPECTED_USDC_ADDRESS`).
6. Restart `deposit-relayer` on `OLD_BRIDGE` (step 6 stopped it).
7. Start the AN→ETH relayer on the restored `L2_config`.

A burn the stand already paid is then payable again on `OLD_BRIDGE` —
the nullifier map is per contract. Do not unpause `OLD_BRIDGE` while
the stand is still unpaused and funded.

## Planned: a test integration on Arbitrum Sepolia

The Sepolia problem above is that two different tokens both call themselves USDC: the
one Circle's faucet dispenses (`0x1c7D4B19…7238`) and the one the AAVE V3 pool lists
(`0x94a9D9AC…E4C8`). A bridge wired to the first cannot supply to the pool, and a
bridge wired to the second cannot be funded from a public faucet, so no single Sepolia
deploy gives both a fundable burner and a live yield market.

**Arbitrum Sepolia does not have that split.** The AAVE V3 market there (chain id
`421614`, Pool `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff`) lists Circle's own
Arbitrum Sepolia USDC, `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d`, as its reserve
underlying — the same address Circle's testnet faucet dispenses. One token, fundable
from a public faucet and suppliable to a real pool, which is what a yield stand needs.
Addresses from the [AAVE address book](https://github.com/bgd-labs/aave-address-book);
re-read them before a deploy, as testnet markets are re-listed from time to time.

A test integration on Arbitrum Sepolia is therefore **planned**, and nothing in this
repository targets it yet. What it needs beyond a redeploy:

- `421614` added to `crates/deposit-chain-ids/src/lib.rs` — the single source of truth
  for the chains the deposit side accepts. The prover and the relayer both read it, and
  a chain absent from it is refused before any proof is produced.
- a deposit `VkBlob` covering the new chain, and the AN side's
  `setTrustedL1Bridge(421614, <bridge>)` so `finalizeDeposit` admits it.
- the usual stand discipline from [Launching a Sepolia stand with
  AAVE](#launching-a-sepolia-stand-with-aave): one Acki Nacki bridge cannot serve two
  L1 bridges at once, so the live one is paused first.

Until that lands, no deploy on any network earns yield, and the owner-only
`supplyToAave` / `harvestYield` surface stays untested against a real pool.

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
`min(suppliedPrincipal(), aUsdcBalance())` is zero, and for any explicit amount above it.

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

`setLiquidReserveBps` — the share of user principal kept liquid. Default `1_000` = 10 % (`:683`),
capped at 50 % (`MAX_LIQUID_RESERVE_BPS`, `:61`). Ours, not an AAVE convention. `setAaveEnabled` —
gate on new supplies; `emergencyWithdrawAll` turns it off, and only an explicit call turns it back
on.

The owner cannot reduce `treasuryBalance` and cannot replace a verifier — those bindings are
immutable. The owner can pause the bridge (`pause()` / `unpause()`, `:1614`, `:1621`): `deposit`,
`verifyBlock`, `applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield
controls above stay available, so funds can be moved out of AAVE during a pause
(`EVM-contracts-spec.md`, §10.1).
