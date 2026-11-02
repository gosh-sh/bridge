# eth-light-client-relayer

Polls Ethereum beacon `finality_update` **and** `light_client/updates` (current
512-committee), proves a light-client **step** with a real committee
(`COMMITTEE_JSON_PATH` → `export_step_vk_blob`), and calls
`EthBeaconLightClient.submitUpdate` on Acki Nacki.

`finalizeDeposit` flip (owner): `scripts/ursus/flip_deposit_to_light_client.md`.
Ancestry (31/32 on-chain) fills the span between two proven checkpoints,
headers oldest first. The daemon fetches the span back to its previous
checkpoint when `ETH_RPC_URL` is set and checks it locally; sending
`submitAncestry` is `--submit-ancestry` (default off). One-shot:
`eth-lc-relayer submit-ancestry --checkpoint-hash <newer> --older-anchor-hash
<older>`. `ancestry-one` only reports whether a block sits on a checkpoint's
parent chain, which is necessary for a span to carry it, not sufficient.
Committee rotate: the daemon only auto-`submitRotate`s the committed
fixture period. A live period jump returns `RotateRequired` — prove
`rotate_tree_n8` on n14 and run `eth-lc-relayer submit-rotate --state`.
`--no-rotate` opts out of even that. tvm-sdk#284 co-deploys with this
contract.

## How it runs

```text
beacon REST ──▶ eth-lc-relayer daemon ──▶ export_step_vk_blob (prebuilt, --step-prover-bin)
   finality_update                         FINALITY_UPDATE_PATH + COMMITTEE_JSON_PATH
   updates?start_period=P-1                   real 512 keys (not OsRng)
                                              proof + public_inputs
                                                    │
                                                    ▼
                              EthBeaconLightClient.submitUpdate(proof, publicInputs)
```

CLI (`cargo run --bin eth-lc-relayer -- …`; `--features live-submit` for AN):

```bash
# 1. See the beacon head (no prove, no AN)
eth-lc-relayer beacon-watch --beacon-url https://lodestar-mainnet.chainsafe.io

# 2. Shadow loop (tests / laptop): mock prove + mock AN, no rotate prove
eth-lc-relayer daemon --beacon-url https://lodestar-mainnet.chainsafe.io \
  --mock-prove --dry-run --no-rotate --state ./eth-lc-relayer-state.json

# 3. Live systemd: scripts/ursus/eth-light-client-relayer.service
#    build: cargo build --release --features live-submit
#    ExecStart has no --dry-run / --mock-prove

# 4. Real step prove on n14
eth-lc-relayer prove-one --beacon-url … \
  --step-prover-bin ../../eth-light-client-prover/target/release/examples/export_step_vk_blob \
  --srs-path $HOME/srs/hermez-raw-19 \
  --out-dir ./bundle

# 4a. Keep the step proving key between proves: generate it once per prover
#     build (STEP_KEY_DIR=<dir> STEP_KEYGEN_ONLY=1 export_step_vk_blob), then
#     set STEP_KEY_DIR in the relayer's environment, which the prover inherits.
#     Generate it again after every prover upgrade: the prover refuses a key
#     another build generated.

# 5. Pin the Gloas fork slot (after deploy, and again on mainnet once Gloas
#    is scheduled; Sepolia 11296768, mainnet max until then)
eth-lc-relayer set-gloas-fork-slot --slot 11296768 …

# 6. Submit step / rotate
eth-lc-relayer submit-one --bundle-dir ./bundle …
eth-lc-relayer submit-rotate --bundle-dir ./rotate_tree --state ./eth-lc-relayer-state.json …

# 7. Is this deposit's execution hash on the checkpoint parent chain?
eth-lc-relayer ancestry-one --beacon-url … \
  --checkpoint-slot 12345678 --deposit-hash 0x…

# 8. Write the blocks between two proven anchors on-chain (needs
#    --submit-ancestry on the daemon, or this one-shot + live-submit)
eth-lc-relayer submit-ancestry --eth-rpc-url … --checkpoint-hash 0x… \
  --older-anchor-hash 0x… \
  --an-graphql-url … --an-keys-path … --an-lc-abi-path ./abi/EthBeaconLightClient.abi.json \
  --an-light-client 'dapp::account' --an-sender 'dapp::account'

# 9. One-shot owner flip (the daemon does it only with --flip-owner)
eth-lc-relayer flip-owner --an-usdc-bridge 'dapp::account' \
  --an-usdc-abi-path ./abi/USDCBridge.abi.json …

# 10. Bridge on an Arbitrum chain: anchor the blocks that hold deposits through
#     ArbitrumBlockVerifier.proveBlock (live-submit), next to deposit-relayer
#     daemon. Keep --anchor-state across restarts: the proofs in it cannot be
#     fetched again.
eth-lc-relayer arbitrum-anchors --parent-rpc-url … --arbitrum-rpc-url … \
  --bridge-address 0x… --from-block <bridge deploy block> \
  --an-graphql-url … --an-keys-path … --an-bridge-abi-path ./abi/eccUSDCBridge.abi.json \
  --an-verifier-abi-path ./abi/ArbitrumBlockVerifier.abi.json --an-token-bridge 'dapp::account'

# 11. One proveBlock vector from the live chains, for the bench (sends nothing)
eth-lc-relayer arbitrum-vector --parent-rpc-url … --arbitrum-rpc-url … --out ./vector.json
```

`--no-rotate` opts out of even the fixture auto-rotate. A live period
is never proved in-process: prove on n14 and `submit-rotate --state`
(tvm-sdk#284; the rotate tree is ~40 GB). The next daemon tick reads
`getCommitteeState` and continues.
`--flip-owner` opts in to the owner flip after the first accepted update. It is
**off** by default: after it, L1 deposits outside checkpoint blocks have no
anchor writer; L2s keep the owner path
(`scripts/ursus/flip_deposit_to_light_client.md`). It needs
`AN_USDC_BRIDGE` and `AN_USDC_ABI_PATH`. Relayer keys must be the owner pubkey.
Pin the Gloas slot with `set-gloas-fork-slot` before proving (`256` until it
is set). A later published date is the same call, including after the flip.
`gloas-slot-user-cell --slot <n>` prints an `updateCode` user cell that
carries the slot (`docs/eth-light-client.md` §4.5).

Standalone crate (own `Cargo.lock`). Tests:
`cd crates/eth-light-client-relayer && cargo test`.
