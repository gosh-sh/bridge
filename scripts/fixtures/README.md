# Bridge replay-protection upgrade fixture

`bridge_timestamp_rp_1_8_2.tvc.b64` is the base64 encoding of the tracked
`contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.tvc` at `1cc0f34`.
It runs the previous compiler-managed timestamp replay protection unchanged.
The TVM regression initializes that artifact, executes an owner command,
upgrades through its real `updateCode`, and checks that the new storage layout
works while old signed commands and the original upgrade message are rejected.
The artifact is used only locally and is never deployed by the scripts.
