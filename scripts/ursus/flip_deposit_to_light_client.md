# Flip ETH→AN canonicality onto the beacon light client

`finalizeDeposit` reads `USDCBridge._acceptedBlockHash`. The light client
**pushes** proven checkpoint hashes into that map
(`acceptBlockHashFromLightClient`); the owner writes the rest with
`setAcceptedBlockHash`. The flip turns the owner path off.

**Do not flip yet.** `disableOwnerAnchors` turns the owner writer off only for
the light client's L1; L2s on the allowlist keep `setAcceptedBlockHash`. The
light client writes checkpoints and the ancestry spans between them
(`docs/eth-light-client.md` §3.4). A block no span covers — accepted
checkpoints are often two or more epochs apart — fails
`finalizeDeposit` with `ERR_UNKNOWN_BLOCK` (224). The daemon therefore does
not flip unless started with `--flip-owner`. See §3.5.

The **relayer** issues the owner calls. Relayer keys (`AN_KEYS_PATH`) must be
the owner pubkey of both contracts. tvm-sdk#284 co-deploys with this contract.

1. The bridge is `eccUSDCBridge` from the zerostate. The zerostate installs the
   light-client code (`setLightClientCode`), and the bridge derives the
   light-client address from it (`getAnchorConfig().lightClient`).
2. With the owner key, call `deployLightClient(pubkey, l1ChainId, 0, 0)` on the
   bridge. The light client's constructor accepts only the bridge as sender, so
   a copy deployed with `sold` or `tvm-cli deploy` — including the standalone
   `contracts/an/EthBeaconLightClient.sol` — is not the one the bridge listens
   to, and the flip refuses it. `getAnchorConfig().lightClient` returns
   `0:<account>`; set `AN_LIGHT_CLIENT` to `<account>::<account>`.
3. Pin the Gloas fork slot. The constructor does not set it, and
   `submitUpdate` / `submitRotate` revert 256 until it is set:
   `eth-lc-relayer set-gloas-fork-slot --slot 11296768` on Sepolia,
   `--slot 18446744073709551615` on mainnet while Gloas is unscheduled.
   The daemon does not call this itself.
4. Set `AN_USDC_BRIDGE` + `AN_USDC_ABI_PATH` (slim ABI at
   `crates/eth-light-client-relayer/abi/USDCBridge.abi.json`). `--flip-owner`
   refuses to start without them.
5. Run `eth-lc-relayer daemon --flip-owner` (`--features live-submit`, no
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
$ETH_RPC_URL --checkpoint-hash 0x<newer> --older-anchor-hash 0x<older>` fills
the blocks between two proven anchors (`docs/eth-light-client.md` §3.4). A
deposit in a block no span covers — one whose bracketing anchors are further
apart than a span — needs the owner's `setAcceptedBlockHash`, which the flip
turns off for that L1. L2 deposits keep the owner writer.

Period jumps: the daemon rotates by default. After `disableOwnerRotation()`, a
lag past one sync-committee period (~27 h) is `reAnchorCommittee`, not
`setCommitteeCommitment`.
