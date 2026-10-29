# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki. These are the contracts in
`exchange/` — `eccUSDCBridge` at version 1.14.0, `DepositVoucher` at 1.4.0,
`EthBeaconLightClient` at 1.6.1 and `ArbitrumBlockVerifier` at 1.0.0:

- `eccUSDCBridge` — premined into the zerostate. Mints USDC on a proven
  Ethereum deposit (`finalizeDeposit`, confirmed back by the voucher through
  `confirmDeposit`) once the deposit's block is in its anchor set, and burns
  USDC on `initiateWithdrawal`. A withdrawal without enough bridged or destination-chain inventory
  returns the attached ECC without a burn. ECC is fungible: owner-minted and TIP-3-minted ECC can still
  consume headroom that deposits created; the cap stops burned exceeding
  minted. An `updateCode` of a live bridge that already burned more than
  it minted (`getTotalBridged` burned above minted) returns every withdrawal
  until deposits bring minted up to burned; a fresh zerostate starts both
  counters at 0. The owner can stop `finalizeDeposit` and `initiateWithdrawal`
  with `setPaused(true)`, which makes them throw `ERR_PAUSED` (231) until
  it is lifted; `confirmDeposit`, owner mint (`mintAndSend`,
  `mintAndSendBridged`, `mintAndSendAccumulator`) and TIP-3 mint keep running.
  That switch is independent of `AckiNackiBridge.pause()` on Ethereum and
  of a Circle freeze of USDC — see *Two pause domains* below.
- `DepositVoucher` — deployed once per finalized deposit at an address derived
  from the deposit identity and from the voucher's own code; a second
  finalization of the same deposit collides with it and does not mint again.
  The bridge holds that code in `_depositVoucherCode`, and it is what makes the
  collision happen — part of the replay protection rather than an
  implementation detail. It can be changed, see *Upgrading the bridge* below.
- `EthBeaconLightClient`, with the `EthKeccak` library — deployed by the bridge
  (`deployLightClient`) from the code the zerostate installs
  (`setLightClientCode`). It verifies Ethereum sync-committee step proofs and
  pushes the execution block hashes it admits into the bridge's anchor set. Its
  bridge address is a constant, and its constructor accepts only the bridge as
  sender.
- `ArbitrumBlockVerifier`, with the `EthTrie` library — one per Arbitrum chain,
  deployed by the bridge (`deployL2Verifier`) from the code the zerostate
  installs (`setL2VerifierCode`). It proves an Arbitrum block from the state
  assertion the chain posts on its parent chain, whose block the light client
  has already admitted, and pushes the block's hash into the anchor set. See
  *Arbitrum blocks* below.

`finalizeDeposit` is permissionless — the proof is the authorization. It
rejects a deposit whose proven `(chainId, contractAddress)` pair is not a
trusted L1 bridge (`setTrustedL1Bridge`; the zerostate seeds one, see below),
verifies the proof against the embedded `VK_BLOB` with `ZKHALO2VERIFYWITHVK`,
and then requires the proven block hash in the anchor set: the proof places the
event in a block, but cannot show that the block is canonical. Three writers
fill the anchor set: the owner, the light clients and the Arbitrum block
verifiers. The owner admits or retracts hashes with `setAcceptedBlockHash`
until the flip. `disableOwnerAnchorsOn(chainId)` turns the owner writer off
for one chain and needs a light client or an Arbitrum block verifier deployed
for it (`ERR_LIGHT_CLIENT_UNSET` (226) otherwise, `ERR_NOT_LIGHT_CLIENT_CHAIN` (227)
for chain id 0). A chain with an Arbitrum block verifier closes only after its
parent chain: until the owner writer is off there, `disableOwnerAnchorsOn`
throws `ERR_PARENT_OWNER_ANCHORS_ENABLED` (239), because a parent-chain block
the owner accepts lets the verifier admit any block of the Arbitrum chain. The
no-argument `disableOwnerAnchors()` does the same for the chain whose client
was deployed last. Neither checks that the light-client
account is Active. Each client writes only for its own chain:
`acceptBlockHashFromLightClient` and `forgetBlockHashFromLightClient` compare
the sender against the client of the chain named in the call. An L2 source
chain on the deposit allowlist keeps the owner writer until it has a verifier. `getLightClient(chainId)` is
the client of a chain (`addr_none` if none), `isOwnerAnchorsEnabled(chainId)` the
per-chain writer state, and `getAnchorConfig()` the pair for the chain
deployed last — with clients on several chains, read the per-chain getters.
The flip disables `setAcceptedBlockHash` for the selected chain; it does not
revoke the owner's configuration or upgrade authority. There is no direct
re-enable function in the current code. A bridge `updateCode` resets the
owner-anchor flags and recorded clients, so operators must restore configuration
and repeat the intended flips after upgrading. `setLightClientCode` alone keeps
recorded clients unchanged, but calling `deployLightClient` afterwards for the
same chain can replace its writer even while the owner-anchor flag is off;
`setL2VerifierCode` followed by `deployL2Verifier` does the same for an
Arbitrum chain's verifier. The light client's owner also retains `reAnchorCommittee` and `updateCode`.
An owner who retains `updateCode` can install arbitrary replacement code,
including code that removes these checks. The current implementation cannot
constrain owner-selected replacement code, even if a migration preserves the
flags. Both owner keys therefore remain trusted after a flip.

| Path | Contents |
|------|----------|
| `exchange/` | Sources and a `Makefile` that builds them in place |
| `token/interface/ISubscriber.sol` | Copy of the token subscriber interface from acki-nacki; `eccUSDCBridge.sol` imports it as `../token/interface/ISubscriber.sol` |
| `0.80.0_compiled/exchange/` | `eccUSDCBridge.tvc`, `eccUSDCBridge.abi.json` |
| `0.81.0_compiled/exchange/` | `DepositVoucher.tvc`, `DepositVoucher.abi.json`, `EthBeaconLightClient.tvc`, `EthBeaconLightClient.abi.json`, `ArbitrumBlockVerifier.tvc`, `ArbitrumBlockVerifier.abi.json` |
| `zerostate/` | `BridgeZerostateData.sol` with its artefacts, and `test_zerostate_init.py`, which tests `zerostate_init.py` one level up. The contract is never deployed: it builds the data cell the premined bridge is upgraded with |
| `EthBeaconLightClient.sol`, `EthKeccak.sol` | A separate, standalone variant of the light client with a settable bridge address (`_usdcBridge`). It is not part of the zerostate and not built by the `Makefile` |
| `place.json` | Manifest of every file's destination in acki-nacki, or that it stays here; `scripts/check_place_manifest.py` checks it against the tracked files |

The compiled artefacts are what goes into a zerostate. acki-nacki does not keep
its own copy: `contracts/an/place.json` lists every file that goes into an
Acki Nacki tree together with its destination there, and names what stays only
here — the standalone light-client variant (`EthBeaconLightClient.sol`,
`EthKeccak.sol`), the two `Makefile`s, the zerostate encoder's source
`BridgeZerostateData.sol` (only its compiled artefacts travel), the module's
own test, this README, and this repository's copy of `ISubscriber.sol`, which
acki-nacki keeps its own original of. acki-nacki's
`contracts/scripts/bridge_contracts.py` pins one commit of this repository and
places every file the manifest lists at the destination it names, before the
zerostate is generated, so a new contract or artefact only needs adding to
`place.json` — no edit on the acki-nacki side beyond moving the pin, and no
file list there to keep in step. `scripts/check_place_manifest.py` fails if a
tracked file under `contracts/an/` is left out of both lists, and if a
destination is not one of the few directories acki-nacki gives us —
`contracts/exchange/`, `contracts/zerostate/`, the two `*_compiled/exchange/`
folders and `contracts/scripts/bridge_*.py`, never
`contracts/scripts/bridge_contracts.py`, which is acki-nacki's own module and
the one that does the placing. A change here reaches a network only after that
pin is moved.

## Arbitrum blocks

A deposit made on an Arbitrum chain is finalized like any other: its block hash
has to be in the anchor set for that chain. `ArbitrumBlockVerifier` puts it
there without the owner.

An Arbitrum chain posts state assertions to its rollup contract on the parent
chain, and each `AssertionCreated` names the hash of the last Arbitrum block it
covers, M. Arbitrum keeps the hashes of its last 393 168 blocks in the EIP-2935
history contract `0x0000F90827F1C53a10cb7A02335B175320002935`. So M's state
holds the hash of every block N up to 393 168 blocks behind M, about 27 hours at
0.25 s a block. `proveBlock(l1Header, txIndex, receiptProof, l2Header,
accountProof, blockNumber, storageProof)` checks each piece with `KECCAK256`:

1. The receipt of transaction `txIndex` in the parent-chain block `l1Header`
   holds an `AssertionCreated` from the verifier's rollup. Its data word 13
   (`afterState.globalState.bytes32Vals[0]`) is M.
2. `l2Header` hashes to M. Under M's state root, `accountProof` and
   `storageProof` (an `eth_getProof` answer at M) give history slot
   `blockNumber % 393168`, which holds N's hash.

The verifier then sends `acceptBlockHashFromL2Verifier(l2ChainId,
parentBlockHash, blockHash)` to the bridge. The bridge takes it only from the
verifier it recorded for that chain (`ERR_INVALID_SENDER` (207) otherwise), and
only if the parent-chain block is already in that chain's anchor set
(`ERR_UNKNOWN_BLOCK` (224) otherwise). The message bounces and the verifier
stores nothing, so after a 224 the same call succeeds once the light client
has admitted that block.

`proveBlock` is permissionless: the hashes are the authorization. Like the
light client, the verifier pays its own gas and mints the shell for it. One
call on Arbitrum One data costs about 0.55M gas against the 10M
per-transaction limit (`scripts/l2-block-proof-bench`). The verifier's
errors are 300–307 from `EthTrie` and 310–315 from the verifier, named at the
top of each source file.

| Chain | `l2ChainId` | Parent chain | Rollup (`SequencerInbox.rollup()`, 2026-10-25) | Assertions |
|---|---|---|---|---|
| Arbitrum Sepolia | 421614 | Sepolia, 11155111 | `0x042b2e6c5e99d4c521bd49beed5e99651d9b0cf4` | about every 30 min |
| Arbitrum One | 42161 | Ethereum, 1 | `0x4dceb440657f21083db8add07665f8ddbe1dcfc0` | about every hour |

**The trust assumption.** The verifier accepts an assertion as soon as it is
posted, not when the rollup confirms it. A hash it admits stays in the anchor
set: the verifier has nothing like the light client's
`forgetBlockHashFromLightClient`, and a successful challenge neither removes
the hash nor undoes a deposit already finalized. What stands behind an
assertion is what posting it costs, read from each rollup on 2026-10-26:

| Rollup | Who may post (`validatorWhitelistDisabled()`) | Stake (`baseStake()`) | Confirmed after (`confirmPeriodBlocks()`) |
|---|---|---|---|
| Arbitrum One | anyone | 3 600 WETH | 45 818 blocks, about 6.4 days |
| Arbitrum Sepolia | whitelisted validators only | 36 of a test token (`0xefb383126640fe4a760010c6e59c397d2b6c7141`) | 20 blocks, about 4 minutes |

On Arbitrum One a false assertion locks 3 600 ETH, none has ever been posted,
and the 10 000 USDC per-chain ceiling bounds what one could take. That holds
only while the ceiling stays below the stake and a challenged assertion loses
it. On Arbitrum Sepolia nothing of value backs an assertion, only the
whitelist: use its verifier on test networks only.

Read the rollup address from the chain's `SequencerInbox.rollup()` rather than
from a list: Arbitrum Sepolia's rollup moved when it switched to BoLD, and
Arbitrum's docs still name the old one. The verifier keeps the address it was
initialized with, so a move needs new verifier code: the same code under the
same salt is the same account, already initialized.

A deposit in block N can be proven once an assertion with M at or after N is
posted and the light client has admitted the parent-chain block that holds it
(Ethereum finality, about 13 minutes). It has to be proven before M runs more
than 393 168 blocks past N.

### Deploying a verifier

1. The zerostate installs the verifier code. On a bridge without it, send
   `setL2VerifierCode` with the code of
   `0.81.0_compiled/exchange/ArbitrumBlockVerifier.tvc`.
2. Deploy a light client for the parent chain first: its accepted blocks are
   what the verifier's assertions are checked against.
3. `deployL2Verifier(421614, 11155111, 0x042b2e6c5e99d4c521bd49beed5e99651d9b0cf4)`
   for Arbitrum Sepolia, or `deployL2Verifier(42161, 1,
   0x4dceb440657f21083db8add07665f8ddbe1dcfc0)` for Arbitrum One. The bridge
   deploys the verifier at the address of its code salted with `l2ChainId`,
   with 10 vmshell, and initializes it with 2. Without the code it throws
   `ERR_L2_VERIFIER_UNSET` (237). Re-running it with the code of the recorded
   verifier throws `ERR_L2_VERIFIER_DEPLOYED` (238): that code reaches the
   same account, which keeps the rollup and parent chain it was first given.
   A new rollup address or parent chain needs new verifier code (see above),
   which deploys at a new address and replaces the recorded verifier.
4. Check `getL2Verifier(l2ChainId)` on the bridge, which returns the address
   and the parent chain, and `getConfig()` on the verifier.
5. When the verifier is admitting blocks, turn the owner writer off for the
   parent chain (`disableOwnerAnchorsOn(parentChainId)`, or the light client
   relayer's flip), then `disableOwnerAnchorsOn(l2ChainId)` for the Arbitrum
   chain. In the other order the second call throws
   `ERR_PARENT_OWNER_ANCHORS_ENABLED` (239). On a closed Arbitrum chain,
   `deployL2Verifier` throws 239 too for a parent chain the owner still
   anchors.

The verifier code and the recorded verifiers are not carried through
`updateCode`. After a bridge upgrade, repeat steps 1 and 3 with the arguments
the verifier was first deployed with. The verifier account already exists at
the same address, so the deploy and `initialize` fail on it and bounce, and the
bridge records the address again, with the `parentChainId` of this call: the
upgraded bridge has no record to refuse it against, so check `getConfig()` on
the verifier against `getL2Verifier` (step 4).

`deposit-relayer arbitrum-anchors` calls `proveBlock`; see section 9 of
`crates/deposit-relayer-daemon/docs/operator_runbook.md`. It needs the state
at M, and M is already 30 minutes (Arbitrum Sepolia) or an hour (Arbitrum One)
old when its assertion lands, which is more state than an ordinary node keeps.
M is always the last block of a batch, and the batch is posted to the parent
chain's `SequencerInbox` within a minute of M. So the relayer rolls forward: on
each `SequencerBatchDelivered` it takes M's header and one `eth_getProof` for
the slots of the deposits still waiting, 40–60 s after M. The Arbitrum node has
to keep M's state that long: public endpoints keep about 128 blocks, half a
minute, and miss many batches; a free Alchemy key keeps it. When an assertion ends at that block, it calls
`proveBlock` with the parent-chain receipt.

## Transfer and network limits

Deposits and withdrawals are limited to **50 USDC** (50,000,000 base units).
Each L1 bridge limits `treasuryBalance` (deposited principal minus completed
payouts) to **10,000 USDC**, including principal supplied to Aave. Yield and
direct token donations do not increase that counter or its capacity.
`MAX_DEPOSIT_AMOUNT = type(uint64).max` remains the first deposit width guard;
the lower product ceiling is `MAX_TRANSFER_AMOUNT`.

AN keeps the same 10,000 USDC ceiling separately for every EIP-155 chain id.
`getDepositLimits(chainId)` returns `maxTransfer`, `maxTvl` and `outstanding` in
base units. `confirmDeposit` increases that chain's outstanding amount;
a successful `initiateWithdrawal` decreases the destination chain's amount.
There are no deposit reservations or permanent voucher registry. A repeated
proof deploys the same voucher, whose consumed constructor prevents another
confirmation. Trust only one funded L1 bridge per chain for this accounting.
For that pair,
`L1 treasuryBalance = AN outstanding + unconfirmed deposits + unpaid burns`.
Thus confirming a valid pending deposit cannot exceed the L1 cap; simultaneous
finalizations need no reservation. Pause deposits and drain pending transfers
before rotating the trusted L1 contract. That identity, and the conclusion drawn
from it, hold while `confirmDeposit` and `initiateWithdrawal` are the only
writers of the counter. `setOutstandingByChain` writes it directly with no tie
to L1, so an owner who writes a figure other than the measured L1 principal
breaks both — see *Restoring a chain's outstanding amount* below.

AN finalization rejects amounts above 50 USDC with 234 and exhausted capacity
with 235; the latter is retryable after withdrawals. Both checks precede proof
verification and are repeated on confirmation. A withdrawal that is oversized,
that would burn more than bridged minted less bridged burned, or that exceeds
its destination chain's outstanding amount returns the attached ECC without
burning it and emits `WithdrawalRefused` instead of `WithdrawalInitiated`. That
event carries the same fields plus `reason` — the error code the call would have
thrown had the refusal thrown: 234, 233 and 236 for the three cases in that
order. It calls `ensureBalance()` and sends back `min(msg.value, 0.1 vmshell)`;
it uses no `rawReserve`.
`ensureBalance()` is free. Dapp 0 contains only system contracts, and mainnet
has an unlimited dapp configuration deployed for dapp 0. Withdrawal messages
arrive from another dapp id, so SHELL sent back to that dapp is burned.

External messages use expiring message hashes instead of a shared increasing
timestamp. `expire` must be in the future and less than five minutes ahead of
block time (errors 402 and 401); an identical recorded message is rejected with
400. Changing `time` cannot block another sender or the owner. These hashes
expire; voucher identity remains the deposit double-mint protection. CLI and
relayer finalizations cap expiry at 240 seconds; uncertain CLI sends reuse the
same BOC while valid.

### Restoring a chain's outstanding amount

`setOutstandingByChain(chainId, amount)` writes one chain's outstanding amount
under the owner key. It exists because that counter can legitimately be empty
while the principal is still locked on L1. `onCodeUpgrade` does carry the field
— `updateCode` hands it over as the tenth of eleven, and
`test_upgrade_preserves_both_networks` covers it — but only between builds that
both have it. 1.12.0 had nine fields and no such field to hand over.

A 1.12.0 bridge therefore cannot reach this build in one `updateCode`:
`onCodeUpgrade` decodes the eleven-field tuple only, and a direct attempt
throws on the cell. It throws safely — `updateCode` commits before swapping the
code, so the account keeps 1.12.0 and its state — but it does not arrive.
Crossing that gap needs an intermediate build that decodes the old nine fields
and hands over eleven, leaving the two new ones empty; the deployment procedure
below is the supported alternative. A bridge that crossed it that way, or any
bridge whose counter was never credited, holds a zero outstanding amount while
its principal is still locked on L1, and refuses every withdrawal to that chain
with 236. Nothing else repairs that: `mintAndSendBridged` raises the counter but
mints as it goes, so repairing with it widens the gap between the copies on AN
and the principal actually held on L1.

What it does not do:

- It moves that counter alone. A withdrawal is still bounded by bridged minted
  less bridged burned. Where that inventory is zero too — a fresh deployment —
  the withdrawal is refused with 233 however high the counter is set. Moving a
  chain's users onto a freshly deployed bridge needs the inventory migrated as
  well, alongside custody and the consumed withdrawal nullifiers.
- It does not touch `_totalMinted` or the bridged counters, and `mintAndSend`
  still does not credit the bridged counters. Owner-minted USDC is not
  withdrawable inventory, by design: nothing is locked on L1 behind it.

**The figure to write is not L1's balance.** Rearranging the identity above,

```
outstanding = L1 treasuryBalance - unconfirmed deposits - unpaid burns
```

where unconfirmed deposits are those locked on L1 but not yet credited on AN,
and unpaid burns are withdrawals burned on AN but not yet paid out on L1. The
counter is shared between the two directions — it is the withdrawal allowance
and it is the deposit room, since `finalizeDeposit` and `confirmDeposit` both
require `outstanding + amount <= MAX_BRIDGE_TVL` — so a counter above that
figure buys withdrawal room by closing the door on deposits, and the damage is
not symmetrical:

- a new deposit fails `finalizeDeposit` with 235, which is retryable;
- a deposit already between finalization and confirmation aborts in
  `confirmDeposit` with 235 **after** its voucher's constructor has run, so
  re-proving it deploys the same voucher and cannot confirm again. That USDC is
  locked on L1 with nothing minted on AN until the owner lowers the counter;
- `mintAndSendBridged`, the owner's recovery path for exactly that case, is
  capped by the same check and throws 235 too.

Writing L1's balance while transfers are in flight is enough to cause this. With
10,000 USDC on L1 of which 50 is a deposit not yet confirmed on AN, the counter
belongs at 9,950; write 10,000 and that confirmation aborts with 235 and a spent
voucher. So pause deposits, let the in-flight transfers on both sides settle,
and only then measure and write — the same order this section's parent advises
before rotating a trusted L1 contract.

**If in doubt, write less rather than more.** An under-count only refuses
withdrawals with 236, which return the ECC untouched and which the owner
corrects by raising the counter. An over-count strands a depositor, and the
depositor cannot correct it. For the same reason, raising the counter to the
ceiling in order to release an inventory larger than the ceiling is not an
operating procedure.

The ceiling itself still means what it says. It bounds the amount standing at
one moment rather than the amount that may pass through over time, but that is
a property of the accounting, not a budget to spend: the figure to write is
always the measured L1 principal. And the function grants no authority the
owner did not already hold — `mintAndSend` has no ceiling at all, only a nonce
and a `uint64` width guard, so an owner key can already create arbitrary
ECC[3], and a call that writes a counter without minting is strictly weaker
than one that mints. The ceiling exists to bound what a bug, a forged proof or a
compromised L1 contract can extract, and it still does that: every path that is
not owner-signed stays capped at 50 USDC per transfer and at the destination
chain's outstanding amount.

### Deployment

Deploy the 1.13.0 bridge as a fresh pair with empty accounting. Its
upgrade tuple adds chain balances and external-message hashes; the previous
1.12.0 tuple cannot initialize it. `BridgeZerostateData` encodes the new tuple.
Normal upgrades within this format retain balances and unexpired replay hashes.

`mintAndSendBridged(recipient, value, nonce, chainId)` credits the aborted
deposit's source chain as well as the global bridged total. It enforces the
50 USDC transfer and 10,000 USDC chain limits. Match the deposit's recipient,
amount and EIP-155 chain id exactly, once; this owner call has no deposit-id
deduplication.

Before replacing a funded pair, stop new deposits, finish every pending deposit
and withdrawal, and verify the old books are empty. Pause every old L1 bridge
and check `paused() == true` before enabling its replacement. Never run two
funded payout contracts for the same AN identity and destination chain. Moving
custody and consumed withdrawal nullifiers is a separate migration: complete it
before funding a replacement that accepts the same AN history, or use a fresh
AN identity/history. That migration is not implemented. Amounts above
50 USDC must settle before switching to the capped contracts.

## Rebuilding

    make -C contracts/an/exchange SOLD_0_80=/path/to/sold-0.80.0 SOLD_0_81=/path/to/sold-0.81.0

The `.tvc` metadata of the tracked artefacts records `sold` 0.80.0 for
`eccUSDCBridge.tvc` and 0.81.0 for `DepositVoucher.tvc`,
`EthBeaconLightClient.tvc` and `ArbitrumBlockVerifier.tvc`, which is why the compiled folders carry those names.
Any other compiler changes the code hash, so the `Makefile` builds each contract
with its own compiler, `SOLD_0_80` or `SOLD_0_81`, and stops when a variable is
unset or the binary reports a different version.

`sold` 0.81.0 reproduces `DepositVoucher.tvc` and all three ABIs byte for byte,
but not `eccUSDCBridge.tvc`. `EthBeaconLightClient.tvc` and
`ArbitrumBlockVerifier.tvc` need more than the version: their sources call
`gosh.keccak256`, so `SOLD_0_81` has to be the `sold` 0.81.0 built from
[gosh-sh/TVM-Solidity-Compiler `feature/keccak256-builtin`](https://github.com/gosh-sh/TVM-Solidity-Compiler/tree/feature/keccak256-builtin),
whose tvm-sdk carries the `KECCAK256` instruction. A stock 0.81.0 emits the
mnemonic its assembler does not know. The bridge does not import the verifier,
only `ArbitrumBlockVerifierDeploy.sol`, a contract with the verifier's state
variables and the two calls the bridge makes: importing the verifier would
compile `EthTrie` into the bridge, which `sold` 0.80.0 cannot build. Keep the
two contracts' state variables identical: the bridge lays out the verifier's
initial data, and so its address, from the deploy surface. `sold` 0.82.0 does not
compile the bridge. The bridge keeps the voucher's code in its data, and the
zerostate installs the light client's code into the bridge, so replace the
rebuilt artefacts together.

Editing `BridgeZerostateData.sol` is a separate rebuild:

    make -C contracts/an/zerostate SOLD=/path/to/sold

and its rebuilt `.tvc` / `.abi.json` are committed too.

Before committing new artefacts:

    scripts/check_voucher_abi_consistency.py
    scripts/embed_deposit_vk_blob.py --check contracts/an/exchange/eccUSDCBridge.sol
    scripts/check_zerostate_data_encoder.py

## Two pause domains

Custody on Ethereum (USDC in `AckiNackiBridge`, some of it in AAVE) and the ECC
ledger on Acki Nacki are two books. Each side has its own stop switch, and
neither reaches the other:

- Ethereum: `AckiNackiBridge.pause()` / `unpause()`. While `paused` is true,
  `deposit`, `verifyBlock`, `applyBkSetUpdate` and `withdrawByProof` revert
  `BridgePaused`. Owner AAVE management stays available so funds can be moved
  out of the pool.
- Acki Nacki: `eccUSDCBridge.setPaused(true)`, read back with `isPaused()`.
  `finalizeDeposit` and `initiateWithdrawal` throw 231 until it is lifted.
- Circle can freeze the USDC contract itself. That is a third control, not
  wired to either bridge.

Pausing Ethereum does not pause Acki Nacki, and `setPaused` does not pause
Ethereum. An incident that must stop both directions of the bridge has to
flip **both** owner switches. There is no single stop switch (QC-AN-J5).

## Upgrading the bridge

The voucher's code can be changed. But replay protection rests on the voucher's
address, and that address is derived from its code: the moment the code
changes, every deposit finalized so far gets a new, unoccupied address. Anyone
can finalize them again — the deposit event is still in its Ethereum block, and
a proof of it can be built afresh at any time.

**So an upgrade that changes the voucher's code also makes past deposits stop
going through.** The restriction ships in the upgraded code itself: a block
cutoff, where deposits from blocks older than the upgrade are refused. What is
rejected is the deposit, not the shape of its proof.

**The current contracts carry no such restriction, and none is needed here:**
the voucher's code is not changed, it is installed by the zerostate, and
`updateCode` is called with an empty `userCell` — the bridge keeps the code it
has, voucher addresses stay put, and replay protection holds. The restriction
arrives with the upgrade that first changes the voucher's code; no separate fix
to the present version is required.

The voucher's code can also change by accident: rebuilding `DepositVoucher.tvc`
with a different compiler yields a different code hash.
`getDepositVoucherCodeHash()` reports what the bridge currently holds — compare
it against the tracked artefact before and after an upgrade.

## The zerostate module

`zerostate_init.py` brings a premined `eccUSDCBridge` to the state a fresh chain
needs: it builds the data cell with `BridgeZerostateData`, upgrades the premine
stub to the bridge's code, seeds the trusted L1 bridge, installs the
light-client and Arbitrum block verifier code, and writes the account into the
zerostate. acki-nacki places
it next to the contracts and calls `initialize(ctx)` while generating a
zerostate, so a new setup call or a new storage field is a change in this
repository alone.

Set `BRIDGE_ZS_L1_BRIDGE` to the fresh paired L1 bridge's nonzero `0x` address
before generating a zerostate. There is no default: missing or malformed values
stop generation before any account is changed. `BRIDGE_ZS_L1_CHAIN_ID` defaults
to Sepolia (`11155111`); set it to the deployment's EIP-155 chain id for another
network.

    python3 contracts/an/zerostate/test_zerostate_init.py
    scripts/check_zerostate_data_encoder.py
