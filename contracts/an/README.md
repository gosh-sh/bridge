# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki. These are the contracts in
`exchange/`, all at version 1.4.0:

- `eccUSDCBridge` — premined into the zerostate. Mints USDC on a proven
  Ethereum deposit (`finalizeDeposit`, confirmed back by the voucher through
  `confirmDeposit`) once the deposit's block is in its anchor set, and burns
  USDC on `initiateWithdrawal`.
- `DepositVoucher` — deployed once per finalized deposit at an address derived
  from the deposit identity; a second finalization of the same deposit collides
  with it and does not mint again.
- `EthBeaconLightClient`, with the `EthKeccak` library — deployed by the bridge
  (`deployLightClient`) from the code the zerostate installs
  (`setLightClientCode`). It verifies Ethereum sync-committee step proofs and
  pushes the execution block hashes it admits into the bridge's anchor set. Its
  bridge address is a constant, and its constructor accepts only the bridge as
  sender.

| Path | Contents |
|------|----------|
| `exchange/` | Sources and a `Makefile` that builds them in place |
| `token/interface/ISubscriber.sol` | Copy of the token subscriber interface from acki-nacki; `eccUSDCBridge.sol` imports it as `../token/interface/ISubscriber.sol` |
| `0.82.0_compiled/exchange/` | `eccUSDCBridge.tvc`, `DepositVoucher.tvc`, `EthBeaconLightClient.tvc` and the matching `.abi.json` |
| `EthBeaconLightClient.sol`, `EthKeccak.sol` | A separate, standalone variant of the light client with a settable bridge address (`_usdcBridge`). It is not part of the zerostate and not built by the `Makefile` |

The compiled artefacts are what goes into a zerostate. acki-nacki does not keep
its own copy: its `contracts/scripts/bridge_contracts.py` pins one commit of
this repository and places the `exchange/` sources and these artefacts at
`contracts/exchange/` and `contracts/0.82.0_compiled/exchange/` before the
zerostate is generated. A change here reaches a network only after that pin is
moved.

## Rebuilding

    make -C contracts/an/exchange SOLD=/path/to/sold-0.82.0

All three contracts are built with `sold` 0.82.0, which is why the compiled
folder carries that name. Any other compiler changes the code hash, so the
`Makefile` stops when `SOLD` is unset or the binary reports a different version.

0.82.0 is not optional here: the sources use cross-dapp messages
(`internalMsg` / `externalMsg` / `crossDappMsg` on every public function, the
`dest_dapp_id` option on the deposit payout) and the per-emit event version
(`emit …{version: 1}`), and neither 0.80.0 nor 0.81.0 parses them.

The bridge keeps the voucher's code in its data, and the zerostate installs the
light client's code into the bridge, so replace the rebuilt artefacts together.

Before committing new artefacts:

    scripts/check_voucher_abi_consistency.py
    scripts/embed_deposit_vk_blob.py --check contracts/an/exchange/eccUSDCBridge.sol
