# Flip ETH→AN canonicality onto the beacon light client

Part of **PR #36**. `finalizeDeposit` already reads
`USDCBridge._acceptedBlockHash`. The light client **pushes** proven checkpoint
hashes into that map (`acceptBlockHashFromLightClient`).

The **relayer** issues the one-way owner calls. Relayer keys (`AN_KEYS_PATH`)
must be the contract owner pubkey. tvm-sdk#284 co-deploys with this contract.

1. Deploy `EthBeaconLightClient`, compiled from `contracts/an/EthBeaconLightClient.sol`
   with `sold --tvm-version gosh`. This unit wants the standalone variant — the
   settable `_usdcBridge` — which is what that file is; shellnet runs the
   constant-sink variant from `acki-nacki` `contracts/exchange`.
2. Apply `USDCBridge_disable_owner_allows_light_client.patch` so
   `disableOwnerAnchors` accepts a configured light client (not only an attester
   quorum).
3. Set `AN_USDC_BRIDGE` + `AN_USDC_ABI_PATH` (slim ABI at
   `crates/eth-light-client-relayer/abi/USDCBridge.abi.json`).
4. Run `eth-lc-relayer daemon` (`--features live-submit`, no `--dry-run` /
   `--mock-prove` / `--no-rotate` / `--no-flip-owner`). After the first accepted
   `submitUpdate` it calls, in order:

```text
USDCBridge.getAnchorConfig()                     # lightClient must match AN_LIGHT_CLIENT
USDCBridge.disableOwnerAnchors()                 # one-way; ERR_OWNER_ANCHORS_DISABLED is 225
EthBeaconLightClient.disableOwnerRotation()      # one-way
```

There is no `setLightClient` on `eccUSDCBridge` 1.5.0 — the address is derived
from `setLightClientCode` / `deployLightClient`. If `getAnchorConfig().lightClient`
does not match `AN_LIGHT_CLIENT`, the flip refuses instead of calling a missing
function.

State file records `owner_flip_done` so a restart does not resend. One-shot
without waiting for a tick:

```bash
eth-lc-relayer flip-owner \
  --an-usdc-bridge "$AN_USDC_BRIDGE" \
  --an-usdc-abi-path crates/eth-light-client-relayer/abi/USDCBridge.abi.json \
  --an-light-client "$AN_LIGHT_CLIENT" …
```

`--no-flip-owner` is the shadow/laptop opt-out.

Non-checkpoint deposits: after `submitUpdate`, run
`eth-lc-relayer submit-ancestry --eth-rpc-url $ETH_RPC_URL --checkpoint-hash 0x…`.
Then `deposit-relayer` can `finalizeDeposit` for a receipt in any of those 32
blocks.

Period jumps: the daemon rotates by default. After `disableOwnerRotation()`, a
lag past one sync-committee period (~27 h) is `reAnchorCommittee`, not
`setCommitteeCommitment`.
