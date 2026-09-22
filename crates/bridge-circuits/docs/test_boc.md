# Decoding the test event BOC

`event.boc` is a captured `WithdrawalInitiated` event used as a Circuit 4 fixture.
`TokenBridge.abi.json` is the Solidity-side ABI of the emitting contract.

Decode locally with `tvm-cli` (build from
[tvmlabs/tvm-sdk](https://github.com/tvmlabs/tvm-sdk) — `cargo build -r --bin tvm-cli`):

```
./tvm-cli decode msg --abi TokenBridge.abi.json event.boc
```
