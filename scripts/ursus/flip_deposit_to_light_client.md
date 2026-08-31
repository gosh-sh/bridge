# Flip ETH→AN canonicality onto the beacon light client

Part of **PR #36**. `finalizeDeposit` already reads
`USDCBridge._acceptedBlockHash`. The light client **pushes** proven checkpoint
hashes into that map (`acceptBlockHashFromLightClient`). These owner calls
grant that writer and drop the attester/owner path.

tvm-sdk#284 (KZG accumulator decider, `accumulator_limbs=12`) **co-deploys
with this contract** — every AN node in the rollout must already be on that
opcode. Do this after:

1. `EthBeaconLightClient` is deployed (`EthBeaconLightClient_rotate_decider.patch` on `acki-nacki`).
2. `USDCBridge.setLightClient` points at that address.
3. `eth-lc-relayer daemon` (built `--features live-submit`, **no** `--mock-prove` / `--dry-run` / `--no-rotate`) has landed at least one real `submitUpdate` (`getHead` moved).

```text
USDCBridge.setLightClient(lightClient)
# confirm: getLightClient() != 0
# confirm: a checkpoint hash from getHead is isAcceptedBlockHash(l1ChainId, hash)

USDCBridge.disableOwnerAnchors()                 # one-way
EthBeaconLightClient.disableOwnerRotation()      # one-way; submitRotate is then the only committee writer
```

Both disables are the intended production trust reduction for this deploy, not a
later milestone.

Non-checkpoint deposits: after `submitUpdate`, run
`eth-lc-relayer submit-ancestry --eth-rpc-url $ETH_RPC_URL --checkpoint-hash 0x…`
(`EthBeaconLightClient.submitAncestry`). That keccak-binds the execution
parent-hash chain (≤ 31 parents) and writes those hashes into
`_acceptedBlockHash`. Then `deposit-relayer` can `finalizeDeposit` for a receipt
in any of those 32 blocks.

Period jumps: the daemon rotates by default. `--no-rotate` is the laptop/shadow
opt-out. After `disableOwnerRotation()`, a lag past one sync-committee period
(~27 h) is `reAnchorCommittee`, not `setCommitteeCommitment`.
