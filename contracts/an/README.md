# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki. These are the contracts in
`exchange/` — `eccUSDCBridge` at version 1.5.0, the other two at 1.4.0:

- `eccUSDCBridge` — premined into the zerostate. Mints USDC on a proven
  Ethereum deposit (`finalizeDeposit`, confirmed back by the voucher through
  `confirmDeposit`) once the deposit's block is in its anchor set, and burns
  USDC on `initiateWithdrawal`. The owner can stop both of those with
  `setPaused(true)`, which makes them throw `ERR_PAUSED` (231) until it is
  lifted; nothing else the bridge does is affected.
- `DepositVoucher` — deployed once per finalized deposit at an address derived
  from the deposit identity; a second finalization of the same deposit collides
  with it and does not mint again.
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
until `disableOwnerAnchors()`, which needs a light client first; the light
client adds and ages out hashes with `acceptBlockHashFromLightClient` and
`forgetBlockHashFromLightClient`. `getAnchorConfig()` returns the light client
and whether the owner may still write. No function turns owner anchors back on,
but the owner keeps two ways around it: `updateCode` does (`onCodeUpgrade` sets
the flag back to true), and `setLightClientCode` still replaces the light
client afterwards.

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

## Rebuilding

    make -C contracts/an/exchange SOLD_0_80=/path/to/sold-0.80.0 SOLD_0_81=/path/to/sold-0.81.0

The `.tvc` metadata of the tracked artefacts records `sold` 0.80.0 for
`eccUSDCBridge.tvc` and 0.81.0 for `DepositVoucher.tvc` and
`EthBeaconLightClient.tvc`, which is why the compiled folders carry those names.
Any other compiler changes the code hash, so the `Makefile` builds each contract
with its own compiler, `SOLD_0_80` or `SOLD_0_81`, and stops when a variable is
unset or the binary reports a different version.

`sold` 0.81.0 reproduces `DepositVoucher.tvc`, `EthBeaconLightClient.tvc` and
all three ABIs byte for byte, but not `eccUSDCBridge.tvc`. `sold` 0.82.0 does not
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
