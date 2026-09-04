# Shellnet shadow deployment of the Ethereum beacon light client

Operator kit for running `eth-lc-relayer` against **Sepolia** (beacon) and a
**private `EthBeaconLightClient`** on Acki Nacki shellnet, in shadow mode:

- the contract is ours (fresh deploy, our owner key), `usdcBridge` unset, so
  nothing it does reaches `USDCBridge` or `finalizeDeposit`;
- step proofs only (`--no-rotate`): shellnet nodes run tvm-sdk `v3.0.4.an`
  without the rotate decider (tvm-sdk#284), so `submitRotate` is unsound
  there; period hops are done with the owner key (`set-committee`);
- no one-way owner flip (`--no-flip-owner`).

What it proves: the whole pipeline beacon → Halo2 step proof → opcode
`ZKHALO2VERIFYWITHVK` on shellnet → light-client head advancing with Sepolia
finality, and that the finalized execution hashes it records match Sepolia.

Everything here is public. Host-specific facts (which box, key file
locations, addresses of the live instance) live in the GOSH infra runbook.

## Requirements

- Linux x86_64 host, ~40 GB RAM for a step proof, `cargo` with rustup
  (the repo pins `nightly-2026-08-03`), `python3`, `curl`.
- Hermez KZG SRS `kzg_bn254_19.srs` (see `install-srs.sh`; the public
  ceremony mirrors are gone, copy the slice from an existing relayer host).
- `tvm-cli` 3.0.6 (`linux-musl-amd64` build on Ubuntu 22.04, the glibc build
  needs 2.38) and `sold` >= gosh_0.81.0 (Linux release tarball).
- Outbound HTTPS to a Sepolia beacon REST node with light-client endpoints
  (`finality_update`, `updates`, `beacon/genesis`, `config/spec`) and to
  `shellnet.ackinacki.org`.
- Shellnet giver key (bridge repo `crates/an-bridge-prover/python/contracts/GiverV3.keys.json`)
  to fund the new contract.

## Layout

`ETH_LC_ROOT` (default `/mnt/data/gosh-eth-lc-relayer`):

| Path | Content |
|---|---|
| `src/bridge` | bridge checkout (branch with this kit) |
| `bin/` | `tvm-cli`, `sold`, `eth-lc-relayer` |
| `srs/kzg_bn254_19.srs` | Hermez SRS, mode 0444 |
| `contracts/` | compiled `EthBeaconLightClient.{tvc,abi.json}` |
| `config/` | `eth-lc-relayer.env`, owner keys, giver ABI/keys, `tvm-cli.conf.json` (mode 0700) |
| `state/` | daemon state file |
| `bundles/` | `prove-one` outputs, prover logs |
| `logs/` | build / prove logs |

## Procedure

```bash
export ETH_LC_ROOT=/mnt/data/gosh-eth-lc-relayer
K=$ETH_LC_ROOT/src/bridge/crates/eth-light-client-relayer/deploy/shellnet-shadow

$K/build.sh                          # prover example + relayer (live-submit) -> bin/eth-lc-relayer
$K/install-srs.sh /path/kzg_bn254_19.srs
$K/compile-contract.sh               # sold -> contracts/, checks VK_BLOB == fixture
cp $K/env.example $ETH_LC_ROOT/config/eth-lc-relayer.env && chmod 600 $ETH_LC_ROOT/config/eth-lc-relayer.env
```

Smoke-test the beacon side (prints slots, `signature_slot`, fork version and
`genesis_validators_root` the prover will be given):

```bash
$ETH_LC_ROOT/bin/eth-lc-relayer beacon-watch --beacon-url https://lodestar-sepolia.chainsafe.io
```

Deploy the contract with the committee unset and fill the env file with the
printed `AN_LIGHT_CLIENT` / `AN_SENDER` / key and ABI paths:

```bash
$K/deploy-contract.sh                # genaddr -> giver funding -> deployx -> getters
```

Bootstrap the committee (weak-subjectivity anchor). The first proof exposes
the commitment of the committee that signed the attested header; the owner
writes it on-chain and the state file records the period:

```bash
set -a; source $ETH_LC_ROOT/config/eth-lc-relayer.env; set +a
$ETH_LC_ROOT/bin/eth-lc-relayer prove-one --beacon-url $BEACON_URL \
    --prover-dir $LIGHT_CLIENT_PROVER_DIR --srs-path $STEP_SRS_PATH \
    --out-dir $ETH_LC_ROOT/bundles/bootstrap
$ETH_LC_ROOT/bin/eth-lc-relayer set-committee --bundle-dir $ETH_LC_ROOT/bundles/bootstrap \
    --state $ETH_LC_ROOT/state/eth-lc-relayer-state.json
$ETH_LC_ROOT/bin/eth-lc-relayer submit-one --bundle-dir $ETH_LC_ROOT/bundles/bootstrap
$K/status.sh                          # getHead must show the bundle's finalized slot
```

Run the daemon:

```bash
install -m 644 $K/eth-lc-relayer-shadow.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now eth-lc-relayer-shadow
journalctl -u eth-lc-relayer-shadow -f
```

Each Sepolia finalized epoch (~6.4 min) the daemon fetches the finality
update, proves it (keygen + prove, ~7 min at k=19 on 48 threads) and calls
`submitUpdate`. Expect `HeadUpdated` roughly every 7 to 13 minutes.

## Verifying against Sepolia

`status.sh` prints `getHead` (finalized slot, execution block hash) next to
the live `finality_update`. Independently, for any recorded hash:

```bash
curl -s https://ethereum-sepolia-rpc.publicnode.com -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_getBlockByHash","params":["0x<executionBlockHash>",false]}' \
  | python3 -c 'import sys,json; b=json.load(sys.stdin)["result"]; print(int(b["number"],16), b["hash"])'
```

The block must exist and be canonical (`eth_getBlockByNumber` for that number
returns the same hash).

## Period hop (every ~27 h) while rotate is off

The daemon logs `committee period advanced; rotate prove is off` and returns
`RotateRequired` on every tick until the committee is advanced. Prove one
update of the new period and hop with the owner key:

```bash
systemctl stop eth-lc-relayer-shadow
set -a; source $ETH_LC_ROOT/config/eth-lc-relayer.env; set +a
$ETH_LC_ROOT/bin/eth-lc-relayer prove-one --beacon-url $BEACON_URL \
    --prover-dir $LIGHT_CLIENT_PROVER_DIR --srs-path $STEP_SRS_PATH \
    --out-dir $ETH_LC_ROOT/bundles/hop-$(date -u +%Y%m%dT%H%M%SZ)
$ETH_LC_ROOT/bin/eth-lc-relayer set-committee --bundle-dir <that dir> \
    --state $ETH_LC_ROOT/state/eth-lc-relayer-state.json
systemctl start eth-lc-relayer-shadow
```

This is the trust seam `submitRotate` removes once tvm-sdk#284 is on the
nodes; in shadow it is acceptable and logged (`CommitteeRotated` is not
emitted, `getCommitteeState` shows the new period).

## What is not covered by shadow

- `USDCBridge` wiring (`setLightClient`, `acceptBlockHashFromLightClient`,
  `disableOwnerAnchors`): the shellnet `eccUSDCBridge` has no such surface yet.
- `submitRotate` and `disableOwnerRotation`.
- `submitAncestry` is available (`eth-lc-relayer submit-ancestry --eth-rpc-url ...`)
  but only exercises the contract's own map.

## Notes

- The prover's signing domain (`fork_version`, `genesis_validators_root`) is
  resolved by the relayer from the beacon node (`/eth/v1/beacon/genesis`,
  `/eth/v1/config/spec` at `signature_slot`) and passed to the prover as
  `BEACON_FORK_VERSION` / `BEACON_GENESIS_VALIDATORS_ROOT`. Both are
  witnesses, the VK does not depend on the network.
- The state file is pinned to the first network it sees
  (`genesis_validators_root`); pointing the daemon at another network fails
  instead of mixing heads.
- `prove-one` keeps `prover-stdout.log` / `prover-stderr.log` next to the
  bundle; a failed daemon prove carries the stderr tail in its error.
