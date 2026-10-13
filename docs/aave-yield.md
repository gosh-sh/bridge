# AAVE yield — operator runbook

*Verified against `contracts/ethereum/src/AckiNackiBridge.sol` at commit `a69ba36`, 2026-08-19.
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

`DeployShellnetE2EBridge` never wires a pool. Its USDC is Circle's Sepolia token
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

A new bridge, not a change to the live one. Everything below was checked against
Sepolia on 2026-10-13.

### What is different from the shellnet stand

| | Shellnet stand (`DeployShellnetE2EBridge`) | AAVE stand (`DeployRealBridge`, `USE_AAVE=true`) |
|---|---|---|
| Deposit token | Circle USDC `0x1c7D4B19…7238`, [faucet.circle.com](https://faucet.circle.com) | AAVE test USDC `0x94a9D9AC…E4C8`, the AAVE faucet `0xC959483D…f42D` |
| AAVE pool / aToken | none | `0x6Ae43d32…8951` / `0x16dA4541…EB80` |
| Withdraw aliases | default to 1 / 11155111 / 3 | default to 0 — set them by hand |
| Acki Nacki side | its address is already trusted | `setTrustedL1Bridge` for the new address |

A user's Circle USDC cannot be deposited into the AAVE stand, and the reverse; the
two stands do not share a treasury.

### 1. Check the AAVE reserve first

The Sepolia USDC reserve is regularly over its supply cap. On 2026-10-13 the cap was
2 000 000 000 USDC and about 4.55 × 10⁹ USDC was supplied, so **every `supplyToAave`
reverts with AAVE error `51` (`SUPPLY_CAP_EXCEEDED`)**. The bridge itself is fine in
that state — deposits, `verifyBlock` and payouts do not touch the pool while
`suppliedPrincipal` is 0 — but it earns nothing until AAVE raises the cap or
withdrawals bring the supply under it. `DeployRealBridge` prints both numbers and a
warning; to read them without deploying:

```bash
RPC=<Sepolia RPC>
POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951
USDC=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8
CFG=$(cast call $POOL 'getConfiguration(address)(uint256)' $USDC --rpc-url $RPC | awk '{print $1}')
echo "cap (whole USDC): $(( (CFG >> 116) & ((1 << 36) - 1) ))"   # 0 means no cap
cast call 0x16dA4541aD1807f4443d92D26044C1147406EB80 'totalSupply()(uint256)' --rpc-url $RPC   # 6 decimals
```

`AckiNackiBridgeAaveForkTest` lifts the cap inside its fork when it is reached, so the
fork suite checks the bridge against the real pool either way; a green fork run says
nothing about whether the live reserve takes supplies today.

### 2. Deploy

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

### 3. Trust the new bridge on Acki Nacki

`eccUSDCBridge.finalizeDeposit` rejects a deposit whose proven
`(chainId, contractAddress)` is not in its allowlist (`contracts/an/README.md`). The
owner of the Acki Nacki bridge account adds the new address with
`setTrustedL1Bridge(chainId, l1Bridge, allowed)`:

```bash
# tvm-cli v3 addresses accounts as <dapp_id>::<account_id>; the shellnet bridge's dapp_id is zero.
AN_BRIDGE=0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
ABI=contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json
tvm-cli -u https://shellnet.ackinacki.org callx --abi $ABI --addr $AN_BRIDGE \
  --keys <owner keys of the AN bridge> -m setTrustedL1Bridge \
  '{"chainId":"11155111","l1Bridge":"<new bridge address>","allowed":true}'
tvm-cli -u https://shellnet.ackinacki.org runx --abi $ABI --addr $AN_BRIDGE \
  -m isTrustedL1Bridge '{"chainId":"11155111","l1Bridge":"<new bridge address>"}'
```

Until this is done every deposit into the new bridge is proven but refused on Acki
Nacki. Withdrawals (AN→ETH) do not depend on it.

### 4. Point the off-chain parts at the new bridge

- AN→ETH relayer (`crates/bridge-relayer-daemon/deploy/shellnet-l2/`): in the runtime
  env set `BRIDGE_ADDRESS`, `BRIDGE_DEPLOY_BLOCK`, the `EXPECTED_*` genesis and
  withdraw values you deployed with,
  `EXPECTED_USDC_ADDRESS=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8` and
  `EXPECTED_AAVE_POOL=0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951`, then run
  `preflight`.
- ETH→AN relayer (`deposit-relayer`): `--bridge-address` and `BRIDGE_DEPLOY_BLOCK`.
- `ackinacki-bridge` CLI: `BRIDGE_ADDRESS` and `BRIDGE_DEPLOY_BLOCK` in its config. It
  reads the deposit token from the bridge's `usdc()`, so nothing else changes.

### 5. Fund and supply

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

Payouts need no extra step: when the USDC on the contract is below a withdrawal's
amount, `withdrawByProof` withdraws the difference from AAVE in the same transaction.
Collect yield with `harvestYield` (below), after pointing `yieldRecipient` where it
should go.

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
| in AAVE | `harvestYield(amount)` (`:1287`) | `NoYield` if `accruedYield() == 0` |
| on the contract | `skimExcessUsdc(amount)` (`:1315`) | `NoExcessUsdc` if `excessUsdc() == 0` |

Both send to `yieldRecipient`, never to the caller. Both accept `type(uint256).max` for "all of it".
Neither can touch user principal — neither function appears in the accounting of what we supplied to
AAVE, and that absence is the guarantee.

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
`suppliedPrincipal` is zero (`:1259-1260`).

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
withdrawFromAave(amount)   # repeat until principal is 0 or AAVE stops paying
setAaveEnabled(false)
harvestYield(max)          # only if accruedYield() > 0 — yield still in AAVE
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

`setLiquidReserveBps` — the share of user principal kept liquid. Default `1_000` = 10 % (`:557`),
capped at 50 % (`:67`). Ours, not an AAVE convention. `setAaveEnabled` — gate on new supplies;
`emergencyWithdrawAll` turns it off, and only an explicit call turns it back on.

The owner cannot reduce `treasuryBalance` and cannot replace a verifier — those bindings are
immutable. The owner can pause the bridge (`pause()` / `unpause()`): `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert while it is paused, and the yield controls above stay
available, so funds can be moved out of AAVE during a pause (`EVM-contracts-spec.md`, §10.1).
