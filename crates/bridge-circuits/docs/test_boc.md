# Decoding the test event BOC

`event.boc` is a captured `WithdrawalInitiated` event used as a Circuit 4 fixture.
`TokenBridge.abi.json` is the ABI of the emitting contract, `eccBridge`: a copy of
`contracts/an/0.80.1_compiled/exchange/eccBridge.abi.json` that
`scripts/check_bridge_abi_in_sync.sh` keeps identical to it.

Decode locally with `tvm-cli` (build from
[tvmlabs/tvm-sdk](https://github.com/tvmlabs/tvm-sdk) — `cargo build -r --bin tvm-cli`):

```
./tvm-cli decode msg --abi TokenBridge.abi.json event.boc
```
