# Acki Nacki contracts

The TVM side of the bridge, deployed on Acki Nacki:

- `eccUSDCBridge` — premined into the zerostate. Mints USDC on a proven
  Ethereum deposit (`finalizeDeposit`, confirmed back by the voucher through
  `confirmDeposit`) and burns it on `initiateWithdrawal`.
- `DepositVoucher` — deployed once per finalized deposit at an address derived
  from the deposit identity; a second finalization of the same deposit collides
  with it and does not mint again.

| Path | Contents |
|------|----------|
| `exchange/` | Sources and a `Makefile` that builds them in place |
| `token/interface/ISubscriber.sol` | Copy of the token subscriber interface from acki-nacki; `eccUSDCBridge.sol` imports it as `../token/interface/ISubscriber.sol` |
| `0.80.0_compiled/exchange/` | `eccUSDCBridge.tvc`, `eccUSDCBridge.abi.json` |
| `0.81.0_compiled/exchange/` | `DepositVoucher.tvc`, `DepositVoucher.abi.json` |

The compiled artefacts are what goes into a zerostate. acki-nacki does not keep
its own copy: its `contracts/scripts/bridge_contracts.py` pins one commit of
this repository and places these files at `contracts/exchange/`,
`contracts/0.80.0_compiled/exchange/` and `contracts/0.81.0_compiled/exchange/`
before the zerostate is generated. A change here reaches a network only after
that pin is moved.

## Rebuilding

    make -C contracts/an/exchange SOLD=/path/to/sold

The `.tvc` metadata of the tracked artefacts records `sold` 0.80.0 for
`eccUSDCBridge.tvc` and 0.81.0 for `DepositVoucher.tvc`, which is why the
compiled folders carry those names.

`sold` 0.81.0 reproduces `DepositVoucher.tvc` and both ABIs byte for byte;
`eccUSDCBridge.tvc` comes out different, so a rebuild changes the bridge's code
hash. `sold` 0.82.0 does not compile these sources. `eccUSDCBridge` embeds the
voucher's code in its data, so rebuild and replace the two artefacts as a pair.

Before committing new artefacts:

    scripts/check_voucher_abi_consistency.py
    scripts/embed_deposit_vk_blob.py --check contracts/an/exchange/eccUSDCBridge.sol
