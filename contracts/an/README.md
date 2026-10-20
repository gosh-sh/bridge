# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki. These are the contracts in
`exchange/` — `eccUSDCBridge` at version 1.13.0, `DepositVoucher` at 1.4.0 and
`EthBeaconLightClient` at 1.5.0:

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

`finalizeDeposit` is permissionless — the proof is the authorization. It
rejects a deposit whose proven `(chainId, contractAddress)` pair is not a
trusted L1 bridge (`setTrustedL1Bridge`; the zerostate seeds one, see below),
verifies the proof against the embedded `VK_BLOB` with `ZKHALO2VERIFYWITHVK`,
and then requires the proven block hash in the anchor set: the proof places the
event in a block, but cannot show that the block is canonical. Two writers fill
the anchor set. The owner admits or retracts hashes with `setAcceptedBlockHash`
until the flip. `disableOwnerAnchorsOn(chainId)` turns the owner writer off
for one chain and needs a light client deployed for it
(`ERR_LIGHT_CLIENT_UNSET` (226) otherwise, `ERR_NOT_LIGHT_CLIENT_CHAIN` (227)
for chain id 0); the no-argument `disableOwnerAnchors()` does the same for the
chain whose client was deployed last. Neither checks that the light-client
account is Active. Each client writes only for its own chain:
`acceptBlockHashFromLightClient` and `forgetBlockHashFromLightClient` compare
the sender against the client of the chain named in the call. L2 source chains
on the deposit allowlist keep the owner writer. `getLightClient(chainId)` is
the client of a chain (0 if none), `isOwnerAnchorsEnabled(chainId)` the
per-chain writer state, and `getAnchorConfig()` the pair for the chain
deployed last — with clients on several chains, read the per-chain getters.
The flip disables `setAcceptedBlockHash` for the selected chain; it does not
revoke the owner's configuration or upgrade authority. There is no direct
re-enable function in the current code. A bridge `updateCode` resets the
owner-anchor flags and recorded clients, so operators must restore configuration
and repeat the intended flips after upgrading. `setLightClientCode` alone keeps
recorded clients unchanged, but calling `deployLightClient` afterwards for the
same chain can replace its writer even while the owner-anchor flag is off.
The light client's owner also retains `reAnchorCommittee` and `updateCode`.
An owner who retains `updateCode` can install arbitrary replacement code,
including code that removes these checks. The current implementation cannot
constrain owner-selected replacement code, even if a migration preserves the
flags. Both owner keys therefore remain trusted after a flip.

| Path | Contents |
|------|----------|
| `exchange/` | Sources and a `Makefile` that builds them in place |
| `token/interface/ISubscriber.sol` | Copy of the token subscriber interface from acki-nacki; `eccUSDCBridge.sol` imports it as `../token/interface/ISubscriber.sol` |
| `0.80.0_compiled/exchange/` | `eccUSDCBridge.tvc`, `eccUSDCBridge.abi.json` |
| `0.81.0_compiled/exchange/` | `DepositVoucher.tvc`, `DepositVoucher.abi.json`, `EthBeaconLightClient.tvc`, `EthBeaconLightClient.abi.json` |
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
before rotating the trusted L1 contract.

AN finalization rejects amounts above 50 USDC with 234 and exhausted capacity
with 235; the latter is retryable after withdrawals. Both checks precede proof
verification and are repeated on confirmation. An oversized withdrawal or one
without enough inventory on its destination chain returns the attached ECC
without burning it or emitting `WithdrawalInitiated`. It calls `ensureBalance()`
and sends back `min(msg.value, 0.1 vmshell)`; it uses no `rawReserve`.
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

### Deployment

Deploy this release as a fresh pair with empty accounting. The AN 1.13.0
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
AN identity/history. This release does not implement that migration. Old
amounts above 50 USDC must settle before switching to these contracts.

## Rebuilding

    make -C contracts/an/exchange SOLD_0_80=/path/to/sold-0.80.0 SOLD_0_81=/path/to/sold-0.81.0

The `.tvc` metadata of the tracked artefacts records `sold` 0.80.0 for
`eccUSDCBridge.tvc` and 0.81.0 for `DepositVoucher.tvc` and
`EthBeaconLightClient.tvc`, which is why the compiled folders carry those names.
Any other compiler changes the code hash, so the `Makefile` builds each contract
with its own compiler, `SOLD_0_80` or `SOLD_0_81`, and stops when a variable is
unset or the binary reports a different version.

`sold` 0.81.0 reproduces `DepositVoucher.tvc` and all three ABIs byte for byte,
but not `eccUSDCBridge.tvc`. `EthBeaconLightClient.tvc` needs more than the
version: the source calls `gosh.keccak256`, so `SOLD_0_81` has to be a `sold`
built against a tvm-sdk that carries the `KECCAK256` instruction. A stock
0.81.0 emits the mnemonic its assembler does not know. `sold` 0.82.0 does not
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
light-client code, and writes the account into the zerostate. acki-nacki places
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
