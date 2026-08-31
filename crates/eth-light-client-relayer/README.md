# eth-light-client-relayer

Polls Ethereum beacon `finality_update` **and** `light_client/updates` (current
512-committee), proves a light-client **step** with a real committee
(`COMMITTEE_JSON_PATH` → `export_step_vk_blob`), and calls
`EthBeaconLightClient.submitUpdate` on Acki Nacki.

`finalizeDeposit` flip (owner): `scripts/ursus/flip_deposit_to_light_client.md`.
Epoch ancestry (31/32 on-chain): `eth-lc-relayer submit-ancestry` →
`EthBeaconLightClient.submitAncestry`. Read-only check: `ancestry-one`.
Committee rotate: on by default (`submitRotate` on a period jump; `--no-rotate`
opts out). tvm-sdk#284 co-deploys with this contract.

## How it runs

```text
beacon REST ──▶ eth-lc-relayer daemon ──▶ cargo run --example export_step_vk_blob
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
  --prover-dir ../../eth-light-client-prover \
  --srs-path $HOME/srs/hermez-raw-19 \
  --out-dir ./bundle

# 5. Submit step / rotate
eth-lc-relayer submit-one --bundle-dir ./bundle …
eth-lc-relayer submit-rotate --bundle-dir ./rotate_tree …

# 6. Is this deposit's execution hash on the checkpoint parent chain?
eth-lc-relayer ancestry-one --beacon-url … \
  --checkpoint-slot 12345678 --deposit-hash 0x…

# 7. Write the 31 parent hashes on-chain (needs ETH_RPC_URL + live-submit)
eth-lc-relayer submit-ancestry --eth-rpc-url … --checkpoint-hash 0x… \
  --an-graphql-url … --an-keys-path … --an-lc-abi-path ./abi/EthBeaconLightClient.abi.json \
  --an-light-client 'dapp::account' --an-sender 'dapp::account'
```

`--no-rotate` opts out of auto `submitRotate`. Rotate is **on** by default;
tvm-sdk#284 co-deploys with this contract (n14 for the ~40 GB prove).

Standalone crate (own `Cargo.lock`). Tests:
`cd crates/eth-light-client-relayer && cargo test`.
