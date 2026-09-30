# Flip ETH→AN canonicality onto the beacon light client

`finalizeDeposit` reads `USDCBridge._acceptedBlockHash`. The light client
**pushes** proven checkpoint hashes into that map
(`acceptBlockHashFromLightClient`); the owner writes the rest with
`setAcceptedBlockHash`. The flip turns the owner path off.

**Do not flip yet.** Today the light client admits one block per epoch — the
checkpoint of its own chain — and no L2. After the flip every other deposit
block has no writer: deposits from the L2s on the allowlist and from the other
31 blocks of an epoch fail `finalizeDeposit` with `ERR_UNKNOWN_BLOCK` (224).
The daemon therefore does not flip unless started with `--flip-owner`. See
`docs/eth-light-client.md` §3.4 and §3.5.

The **relayer** issues the owner calls. Relayer keys (`AN_KEYS_PATH`) must be
the owner pubkey of both contracts. tvm-sdk#284 co-deploys with this contract.

1. The bridge is `eccUSDCBridge` from the zerostate. The zerostate installs the
   light-client code (`setLightClientCode`), and the bridge derives the
   light-client address from it (`getAnchorConfig().lightClient`).
2. With the owner key, call `deployLightClient(pubkey, l1ChainId, 0, 0)` on the
   bridge. The light client's constructor accepts only the bridge as sender, so
   a copy deployed with `sold` or `tvm-cli deploy` — including the standalone
   `contracts/an/EthBeaconLightClient.sol` — is not the one the bridge listens
   to, and the flip refuses it. Set `AN_LIGHT_CLIENT` to
   `getAnchorConfig().lightClient` in `dapp_id::account_id` form.
3. Set `AN_USDC_BRIDGE` + `AN_USDC_ABI_PATH` (slim ABI at
   `crates/eth-light-client-relayer/abi/USDCBridge.abi.json`). `--flip-owner`
   refuses to start without them.
4. Run `eth-lc-relayer daemon --flip-owner` (`--features live-submit`, no
   `--dry-run` / `--mock-prove` / `--no-rotate`). After the first accepted
   `submitUpdate` it calls, in order:

```text
USDCBridge.getAnchorConfig()                     # lightClient must match AN_LIGHT_CLIENT
USDCBridge.disableOwnerAnchors()                 # skipped when ownerAnchorsEnabled is already false
EthBeaconLightClient.disableOwnerRotation()
```

If `getAnchorConfig().lightClient` does not match `AN_LIGHT_CLIENT`, or the
getter fails, the flip sends nothing and the daemon retries after the next
accepted update. The state file records `owner_flip_done` so a restart does not
resend. One-shot without waiting for a tick:

```bash
eth-lc-relayer flip-owner \
  --an-usdc-bridge "$AN_USDC_BRIDGE" \
  --an-usdc-abi-path crates/eth-light-client-relayer/abi/USDCBridge.abi.json \
  --an-light-client "$AN_LIGHT_CLIENT" …
```

Non-checkpoint deposits: `eth-lc-relayer submit-ancestry --eth-rpc-url
$ETH_RPC_URL --checkpoint-hash 0x…` walks the epoch back from a proven
checkpoint, but it does not fit the per-transaction gas limit today
(`docs/eth-light-client.md` §3.4). Until it does, a deposit in any of the other
31 blocks needs the owner's `setAcceptedBlockHash`, which the flip turns off.

Period jumps: the daemon rotates by default. After `disableOwnerRotation()`, a
lag past one sync-committee period (~27 h) is `reAnchorCommittee`, not
`setCommitteeCommitment`.
