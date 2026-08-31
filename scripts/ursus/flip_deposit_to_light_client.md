# Flip ETH→AN canonicality onto the beacon light client

Part of **PR #36**. `finalizeDeposit` already reads
`USDCBridge._acceptedBlockHash`. The light client **pushes** proven checkpoint
hashes into that map (`acceptBlockHashFromLightClient`). These owner calls
grant that writer and drop the attester/owner path.

Do this only after:

1. `EthBeaconLightClient` is deployed (`EthBeaconLightClient_rotate_decider.patch` on `acki-nacki`).
2. `USDCBridge.setLightClient` points at that address.
3. `eth-lc-relayer daemon` (built `--features live-submit`, **no** `--mock-prove` / `--dry-run`) has landed at least one real `submitUpdate` (`getHead` moved).
4. tvm-sdk#284 (KZG accumulator decider, `accumulator_limbs=12`) is on **every** AN node before `EthBeaconLightClient.disableOwnerRotation()`.

```text
USDCBridge.setLightClient(lightClient)
# confirm: getLightClient() != 0
# confirm: a checkpoint hash from getHead is isAcceptedBlockHash(l1ChainId, hash)

USDCBridge.disableOwnerAnchors()   # one-way
```

Non-checkpoint deposits: after `submitUpdate`, run
`eth-lc-relayer submit-ancestry --eth-rpc-url $ETH_RPC_URL --checkpoint-hash 0x…`
(`EthBeaconLightClient.submitAncestry`). That keccak-binds the execution
parent-hash chain (≤ 31 parents) and writes those hashes into
`_acceptedBlockHash`. Then `deposit-relayer` can `finalizeDeposit` for a receipt
in any of those 32 blocks.

`--enable-rotate` / `submit-rotate` stay off until #284 is everywhere. Period
jumps without rotate: owner `setCommitteeCommitment` or `reAnchorCommittee`
after `disableOwnerRotation()`.
