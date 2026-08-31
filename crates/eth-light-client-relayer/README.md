# eth-light-client-relayer

Polls Ethereum beacon `finality_update`, proves a light-client **step** (subprocess
into `eth-light-client-prover`), and calls `EthBeaconLightClient.submitUpdate` on
Acki Nacki.

This is the operator loop that PR #36 left as “M5 relayer — not started”.
It does **not** flip `USDCBridge.finalizeDeposit` onto the oracle (ancestry +
tvm-sdk#284 still required for a trustless gate-flip).

## How it runs

```text
beacon REST ──▶ eth-lc-relayer daemon ──▶ cargo run --example export_step_vk_blob
   finality_update                         FINALITY_UPDATE_PATH=…  (n14 + Hermez SRS)
                                              proof + public_inputs
                                                    │
                                                    ▼
                              EthBeaconLightClient.submitUpdate(proof, publicInputs)
```

CLI (`cargo run --bin eth-lc-relayer -- …` from this crate; add `--features live-submit` only for real AN `submitUpdate`):

```bash
# 1. See the beacon head (no prove, no AN)
eth-lc-relayer beacon-watch --beacon-url https://lodestar-mainnet.chainsafe.io

# 2. Shadow loop: real beacon, mock prove + mock AN (state.json only)
eth-lc-relayer daemon --beacon-url https://lodestar-mainnet.chainsafe.io \
  --mock-prove --dry-run --state ./eth-lc-relayer-state.json

# 3. Real step prove on n14 (writes bundle/)
eth-lc-relayer prove-one --beacon-url … \
  --prover-dir ../../eth-light-client-prover \
  --srs-path $HOME/srs/hermez-raw-19 \
  --out-dir ./bundle

# 4. Submit an existing bundle
eth-lc-relayer submit-one --bundle-dir ./bundle \
  --an-graphql-url https://shellnet.ackinacki.org/graphql \
  --an-keys-path … --an-lc-abi-path ./abi/EthBeaconLightClient.abi.json \
  --an-light-client 'dapp::account' --an-sender 'dapp::account'
```

`--enable-rotate` is **off** by default: a period jump logs `RotateRequired`
(prove `rotate_tree_n8` on n14, then submit). Auto-rotate is not emission-sound
until tvm-sdk#284 is on every node.

Standalone crate (own `Cargo.lock`), excluded from the root workspace like
`deposit-relayer-daemon`. Tests: `cd crates/eth-light-client-relayer && cargo test`.
Live CLI: `cargo run --features live-submit --bin eth-lc-relayer -- --help`.
