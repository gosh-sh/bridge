# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki. These are the contracts in
`exchange/` — `eccUSDCBridge` at version 1.8.3, `DepositVoucher` at 1.4.0 and
`EthBeaconLightClient` at 1.4.8:

- `eccUSDCBridge` — premined into the zerostate. Mints USDC on a proven
  Ethereum deposit (`finalizeDeposit`, confirmed back by the voucher through
  `confirmDeposit`) once the deposit's block is in its anchor set, and burns
  USDC on `initiateWithdrawal`. The owner can stop both of those with
  `setPaused(true)`, which makes them throw `ERR_PAUSED` (231) until it is
  lifted; nothing else the bridge does is affected. That switch is independent
  of `AckiNackiBridge.pause()` on Ethereum and of a Circle freeze of USDC —
  see *Two pause domains* below.
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
until `disableOwnerAnchors()`, which needs `setLightClientCode` (so
`_lightClient` is non-zero) and a stored L1 id from `deployLightClient`
(`ERR_LIGHT_CLIENT_UNSET` (226) until both); it does not check that the
light-client account is Active. The call turns the owner writer off only
for the stored L1 id (`ERR_NOT_LIGHT_CLIENT_CHAIN` (227) on any other id);
the bridge does not read the light client's own `getConfig().l1ChainId`.
The light client adds and ages out hashes with
`acceptBlockHashFromLightClient` and `forgetBlockHashFromLightClient`. L2
source chains on the deposit allowlist keep the owner writer. `getAnchorConfig()`
returns the light client and whether the owner may still write that L1;
`isOwnerAnchorsEnabled(chainId)` is the per-chain view;
`getLightClientL1ChainId()` is the stored L1 id (0 until
`deployLightClient`). No function turns owner
anchors back on, but the owner keeps two ways around it: `updateCode` does
(`onCodeUpgrade` leaves every chain owner-writable), and `setLightClientCode`
still replaces the light client afterwards.

### External-message replay protection

Bridge 1.8.3 uses the hash-and-expiry mechanism from the DEX/multifactor
contracts. The compiler still verifies signatures. A successful public deposit
cannot advance a shared timestamp or block signed owner calls. Identical
processed external messages and deposits with an active reservation are rejected
with `ERR_MESSAGE_IS_EXIST` (400). Pending deposits reject before `accept`, proof
verification or another voucher deployment. There is no permanent settled-voucher
registry. After settlement, a fresh message attempts deployment at the same
voucher address; the one-shot constructor rejects it and its bounce releases
the temporary reservation.
A new header may resubmit the same proof; voucher identity still prevents a
second mint. Internal voucher callbacks do not consume external-message records.

Every external message, including getters, must expire strictly after the block
time and strictly before five minutes after it. `ERR_MESSAGE_EXPIRED` (402)
means expired; `ERR_MESSAGE_WITH_HUGE_EXPIREAT` (401) means the validity window
is too long. Keep sender clocks synchronized and the SDK expiration timeout
below 300,000 milliseconds, with room for clock skew. The deposit CLI clamps
its message lifetime to 1–240 seconds, preserving shorter SDK timeouts and
leaving margin below the contract ceiling. The deposit CLI keeps
ambiguous sends on the same BOC; a confirmed 400/402 allows a new header and
keeps voucher/event checks authoritative.

Install the rebuilt bridge through the existing `updateCode(newcode, userCell)`
entry point, not by replacing its code without running the migration: the
compiler's implicit `_timestamp` is removed from storage. The existing nine-field
migration tuple stays compatible. The internal V5 envelope carries the replay
journal and per-network accounting through subsequent upgrades. When upgrading
from timestamp protection, the old committed timestamp becomes a fixed lower
bound for signed messages (`ERR_MESSAGE_BEFORE_UPGRADE`, 403), so previously
executed owner commands, including the upgrade itself, cannot be replayed.
Public deposits cannot move that bound. New signed commands must use a `time`
greater than the upgrading message's timestamp; if that message was future-dated,
keep using greater timestamps until the local clock catches up.

`confirmDeposit` now requires an active matching reservation (235 otherwise).
Before upgrading from code that dispatched vouchers without reservations,
pause new finalizations and drain all of those callbacks. Upgrades from the
reservation-based versions carry pending deposits. The V5 reader accepts older
migration envelopes and discards their old permanent settled-voucher registry.
For `recoverDeposit`, verify that the original callback conclusively failed;
never recover a callback that is pending or missing from the index.

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

## Deposit amounts

Ethereum `AckiNackiBridge.deposit` and Acki Nacki `finalizeDeposit` share one
per-deposit ceiling: `type(uint64).max` USDC base units (6 decimals). The EVM
side refuses anything larger with `DepositTooLarge` (`MAX_DEPOSIT_AMOUNT`);
the AN side refuses a proven `fr[2]` that does not fit `uint64` with exit 214
(`ERR_OVERFLOW`) before it mints. There is no extra product minimum: `amount
> 0` is the floor on both sides (`InvalidAmount` on Ethereum, `ERR_ZERO_AMOUNT`
(204) on Acki Nacki). Older notes that named a 100 USDC cap are describing a
limit that is not in the contracts.

## Rebuilding

    make -C contracts/an/exchange SOLD_0_80=/path/to/sold-0.80.0 SOLD_0_81=/path/to/sold-0.81.0

The `.tvc` metadata of the tracked artefacts records `sold` 0.80.0 for
`eccUSDCBridge.tvc` and 0.81.0 for `DepositVoucher.tvc` and
`EthBeaconLightClient.tvc`, which is why the compiled folders carry those names.
Any other compiler changes the code hash, so the `Makefile` builds each contract
with its own compiler, `SOLD_0_80` or `SOLD_0_81`, and stops when a variable is
unset or the binary reports a different version. `SOLD_0_80` is the release
tarball `sold_gosh_0.80.0_linux_x86_64.tar.gz` from
`gosh-sh/TVM-Solidity-Compiler`, tag `gosh_0.80.0`; it reproduces the tracked
`eccUSDCBridge.tvc` byte for byte.

The pin on 0.80.0 for `eccUSDCBridge` is not only about the code hash. That
version emits the legacy `ext_out_msg_info$11` event format, while 0.81.0 writes
one extra bit after `created_at` in the message info and produces a
byte-identical `abi.json`. Building the bridge with another compiler therefore
changes the wire form of `WithdrawalInitiated` — the message the withdrawal
proof is built over, whose cell layout is pinned in `bridge-event-witness` and
in the proving circuit — with nothing in the ABI to warn about it.

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

### Installing the volume caps on a live bridge

New transfers are limited to 50 USDC, with 10,000 USDC TVL per supported L1
network. AN counts outstanding and pending reservations per `chainId`, across
all source contracts/accounts on that network. A withdrawal frees only its
`dstChainId` capacity. N networks can hold N × 10,000 USDC in total.
`getDepositLimits(chainId, l1Bridge)` reports `maxTransfer`, `maxTvl`,
`outstanding`, `reserved` and `legacyDepositCount`, in micro-USDC except for
that final deposit count.

Before enabling the caps, stop new AN burns, finalize outstanding deposits and
pay historical burns on their original L1 route. Then call `pause()` on every
old L1 bridge and verify `paused() == true` before recording its finalized
`depositCounter`. Keep old routes paused: new deposits above the frozen cutoff
must not be accepted. Stopping a UI or relayer alone is insufficient. After upgrading AN with the existing voucher code (empty
`userCell`), restore its trust entries and canonical anchors, and call
`setLegacyDepositCount(chainId, l1Bridge, depositCount)` for those sources.
This allows proven, already-paid deposits whose `depositId < depositCount` to
finish even above 50 USDC or the TVL cap. Never use an estimated or future
counter: the cutoff authorizes an exception for exactly those historical
IDs. The source must already be trusted; after initial configuration the cutoff
can only be lowered, including to zero. It is carried through later code
upgrades. A fresh zerostate needs no legacy cutoff.

An upgrade from aggregate-only accounting stays paused: it cannot infer
historical network balances, and `getDepositLimits` throws 239. Reconstruct each
network's outstanding amount from finalized deposits and burns by source and
destination chain, in micro-USDC. While paused, call
`initializeChainAccounting(chainIds, outstanding)` once. Arrays must have equal
nonzero length and distinct nonzero chain IDs. The sum must cover the carried
aggregate balance; per-network floors can expose a larger sum than the old
aggregate floor. Supply verified balances, not arbitrary empty capacity.
Restore trust, anchors and light-client/owner-anchor policy before resuming.
Fresh zerostates start at zero; later upgrades retain network balances.

Previously dispatched voucher callbacks can complete after the initial
upgrade. Later upgrades preserve reservations, stored payout data, completed
voucher markers and legacy cutoffs in the existing migration tuple's tagged
ninth field. Pending deposits therefore do not block a repair upgrade; use an
empty `userCell`. Replacing voucher code while reservations remain fails with
236. Re-seed trust and anchors after the upgrade as before.

A deployed voucher whose callback failed cannot resend its constructor.
Pause the bridge and inspect the original voucher deployment and callback
transactions before the owner calls `recoverDeposit(voucher)`. Recovery uses
the proof-bound amount and recipient stored at finalization, releases the
reservation and marks the payout complete so a delayed callback cannot mint
again, even after an upgrade. Never recover an already-paid old voucher whose
constructor replay aborted with 51: its bounce releases the new temporary
reservation. This is an explicit owner repair, not an automatic retry.
Recovery requires a pause (238), and a missing reservation fails with 235.
Legacy settlements may exceed TVL; new deposits then wait for capacity.

Over-cap withdrawals return their attached ECC before reporting code 233,
including for incoming messages with `bounce: false`. The refund is committed
so the exception cannot roll it back. The transfer carries `math.min(msg.value, 0.1 vmshell)` plus the
full attached ECC, with forwarding fees paid separately and no native reserve.
The SHELL amount cannot exceed the incoming value or 0.1 vmshell. With the
CLI's 1 vmshell incoming value, the allowance remains 0.1 vmshell.

**`ensureBalance()` is free.** It replenishes SHELL with
`gosh.mintshellq(MIN_BALANCE)` before the refund. The bridge is a system contract
in DApp 0. On mainnet, DApp 0 has a deployed DApp config with an unlimited
configuration. **Non-system contracts cannot exist in DApp 0.** User withdrawal
messages therefore arrive from another DApp ID. The SHELL attached to the
return message is burned when it crosses back to that DApp; the user receives
the refunded ECC.

The offline TVM harness checks the outgoing message and does not model the
cross-DApp delivery or SHELL burn.

These AN migration exceptions do not move custody or spent-nullifier state
between Ethereum contracts. Proofs do not bind an L1 contract address, and
nullifier sets are local. Never enable two funded payout contracts for the same
AN identity/destination chain. Keep old contracts paused before enabling a new
route, and never unpause them while it is active. AN may retain old source trust
solely for already-paid deposit proofs below the frozen cutoff.

Pausing does not move old principal or stop a fresh contract accepting proofs
already paid by a predecessor. A separately reviewed custody/nullifier migration
is required before a funded replacement is activated. Do not promise that
old balances or replay protection migrate merely by redeploying the bridge.
An old bridge without an effective pause cannot participate in this cutover.

Run the TVM limit regressions offline after a contract change:

    SOLD_0_80=/path/to/sold-0.80.0 TVM_DEBUGGER=/path/to/tvm-debugger python3 scripts/test_an_bridge_limits.py

The harness substitutes proof verification and seeds storage in a temporary
copy; it runs reservation, callback, bounce, cutoff and upgrade logic in TVM
with the production voucher code. It does not test Halo2 proof validity or
network message delivery.
Use a debugger that supports internal calls emitting events; older builds
mistake `DepositFinalized` for the function response and abort before saving
the updated state.

## The zerostate module

`zerostate_init.py` brings a premined `eccUSDCBridge` to the state a fresh chain
needs: it builds the data cell with `BridgeZerostateData`, upgrades the premine
stub to the bridge's code, seeds the trusted L1 bridge, installs the
light-client code, and writes the account into the zerostate. acki-nacki places
it next to the contracts and calls `initialize(ctx)` while generating a
zerostate, so a new setup call or a new storage field is a change in this
repository alone.

`BRIDGE_ZS_L1_CHAIN_ID` and `BRIDGE_ZS_L1_BRIDGE` override the trusted L1 bridge
a generated zerostate starts with, defaulting to `11155111` and
`0xCdFd6Cef70F68d0849310cD970F8ef8F8E4b4fdb` (Sepolia).

    python3 contracts/an/zerostate/test_zerostate_init.py
    scripts/check_zerostate_data_encoder.py
