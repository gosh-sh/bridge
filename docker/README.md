# Docker: the bridge services from images

> Written with the change that introduced it (bridge-audit#132, #135); the paths and
> variables below are the ones `docker/Dockerfile`, `docker/compose.yaml` and
> `docker/relayer/*.sh` use. Operations of each daemon are in its crate's runbook.

Four images, one `Dockerfile`, one compose file. The images are built from the sources of
one commit; the compose file runs the four services and the one-shot commands around them;
every value a host has to know is in two files the operator fills in. Nothing in the image
knows a host path, an address or a key.

## Images

| Image | Target | Holds | Runs |
|---|---|---|---|
| `bridge-relayer` | `bridge-relayer` | `relayer`, `aggregate-proof`, `compute_bridge_anchors`, `seed_prover_bk_set`, `cast`, the verifier bytecode and sources, the preflight scripts | `relayer daemon-live`; one-shot `preflight`, `fold-bk-set` |
| `bridge-deposit-relayer` | `bridge-deposit-relayer` | `deposit-relayer` | `deposit-relayer daemon` |
| `bridge-eth-lc-relayer` | `bridge-eth-lc-relayer` | `eth-lc-relayer`, `export_step_vk_blob`, `export_shard_snark`, `export_step_snark`, `rotate_tree_n8` | `eth-lc-relayer daemon`, `eth-lc-relayer arbitrum-anchors`; one-shot `lc-keygen` |
| `bridge-cli` | `bridge-cli` | `ackinacki-bridge`, `aggregate-proof`, `cast`, `forge`, the verifiers | `docker compose run --rm cli …` |
| (files) | `tools` | every binary above as plain files | `docker build --target tools --output type=local,dest=out .` |

Every image runs as the unprivileged user `bridge` (UID/GID build args, default 998), on
`debian:bookworm-slim`, the same glibc as the `rust:1-bookworm` builder. The commit the
sources came from is the OCI label `org.opencontainers.image.revision` and the file
`/opt/bridge/SOURCE_COMMIT`; `/opt/bridge/IMAGE-SHA256SUMS` pins every binary the image
carries, and the relayer's preflight checks it on every start.

CI (`.woodpecker/images.yaml`) builds all four on every pull request and pushes them to
`docker.gosh.sh/<image>` on a push to `main` (`sha-<12>`, `latest`) and on a version tag.
Locally:

```bash
# the names the compose file runs: ${BRIDGE_REGISTRY}/<image>:${BRIDGE_IMAGE_TAG}
for t in bridge-relayer bridge-deposit-relayer bridge-eth-lc-relayer bridge-cli; do
  docker build -f docker/Dockerfile --target $t -t docker.gosh.sh/$t:dev .
done
# then BRIDGE_IMAGE_TAG=dev and BRIDGE_PULL_POLICY=never in docker/.env
```

The first build compiles every workspace (tens of minutes); BuildKit cache mounts keep the
cargo registry and one target directory per workspace, so a rebuild after a small change is
short. `CLI_FEATURES` passes cargo features to `ackinacki-bridge` (the local-network e2e
builds it with `ackinacki-bridge/dev-unfixed-bridge`).

## Layout on the host

Two roots (bridge-audit#132): what is small and not recoverable, and what is large and can
be computed again. On a stage host they are the mirrored system volume and the large data
volume.

```text
$BRIDGE_OPT/                         # e.g. /opt/bridge-shellnet, root:bridge
  env/daemon-live.env                # 0640: the withdraw relayer's settings and EOA key
  env/deposit-relayer.env            # 0640
  env/eth-lc-relayer.env             # 0640
  env/arbitrum-anchors.env           # 0640
  env/cli.env                        # optional, for `compose run cli`
  keys/an-owner.keys.json            # bridge:bridge 0600: the light client's owner (eth-lc-relayer)
  keys/deposit-sender.keys.json      # bridge:bridge 0600: finalizeDeposit and proveBlock signer
  keys/bk_set.json                   # the BK set the bridge was deployed with
  daemon-live/state/                 # relayer-state.json, prover_state.json, prover_bk_set.json
  deposit-relayer/state/             # deposit-relayer-state.json, .lock
  eth-lc-relayer/state/              # eth-lc-relayer-state.json, .lock
  arbitrum-anchors/state/            # arbitrum-anchors-state.json, .lock
  cli/state/                         # the CLI's deposit and withdrawal records
$BRIDGE_DATA/                        # e.g. /mnt/data/bridge-shellnet, bridge:bridge
  daemon-live/work/  daemon-live/logs/
  deposit-relayer/logs/
  eth-lc-relayer/work/  eth-lc-relayer/logs/
  arbitrum-anchors/work/  arbitrum-anchors/logs/
  cli/work/  cli/logs/
  params/relayer/                    # SRS k=17..22, inner keys, SHA256SUMS (finalize-params.sh); pk_cache/ rw
  params/lc-prover/                  # kzg_bn254_19.srs, kzg_bn254_20.srs, kzg_bn254_21.srs;
                                     #   step-key/ from `compose run lc-keygen`;
                                     #   rotate-keys/ written by the daemon at its first rotate (~40 GB)
```

Each daemon logs to stdout and to one file per day in its `logs/` directory, keeping the newest
14; the prover transcripts it writes beside them are not pruned, so that directory needs a
retention rule of its own (logrotate, a cron `find -mtime`).

Inside every container the same four paths: `/data/state`, `/data/work`, `/data/logs` and
`/data/params` (read-only for the relayer except `pk_cache`, writable for the light-client
prover, which generates keys; the deposit relayer has none). The withdraw relayer's env file is mounted at `/run/secrets/runtime-env` and
sourced by its entrypoint; the other services get theirs as container environment
(`env_file`). Keys are mounted read-only at `/run/keys`, the BK set at
`/run/config/bk_set.json`.

## Settings

| File | Template | Holds |
|---|---|---|
| `docker/.env` (next to the compose file) | `.env.example` | `BRIDGE_NET`, registry and image tag, `BRIDGE_OPT`, `BRIDGE_DATA`, `BRIDGE_UID`/`GID`, `METRICS_LISTEN_ADDR` and the two metrics ports |
| `$BRIDGE_OPT/env/daemon-live.env` | `env/daemon-live.env.example` | `EXPECTED_BRIDGE_COMMIT`, RPC and GraphQL endpoints, bridge address and deploy block, EOA and **private key**, every `EXPECTED_*` pin, `PARAMS_MANIFEST_SHA256`, BK set sha256 and commitment |
| `$BRIDGE_OPT/env/deposit-relayer.env` | `env/deposit-relayer.env.example` | RPC, bridge address and deploy block, `AN_GRAPHQL_URL`, `AN_TOKEN_BRIDGE`, `AN_SENDER` |
| `$BRIDGE_OPT/env/eth-lc-relayer.env` | `env/eth-lc-relayer.env.example` | `BEACON_URL`, `ETH_RPC_URL`, `SUBMIT_ANCESTRY`, `AN_GRAPHQL_URL`, `AN_LIGHT_CLIENT`, `AN_SENDER`, `AN_USDC_BRIDGE` |
| `$BRIDGE_OPT/env/arbitrum-anchors.env` | `env/arbitrum-anchors.env.example` | `PARENT_RPC_URL`, `ARBITRUM_RPC_URL`, bridge address and deploy block, `AN_GRAPHQL_URL`, `AN_TOKEN_BRIDGE` |
| `$BRIDGE_OPT/env/cli.env` (optional) | none | endpoints and addresses for `compose run cli`; never a private key, which goes in as `run -e …` for the one run or as a file under `keys/` |

`METRICS_LISTEN_ADDR` is required and is the host's address on the monitoring network: a
`ports:` entry without an address publishes the port on every interface, past the host
firewall. `/metrics` is served by `daemon-live` (9464) and `deposit-relayer` (9467); the
compose healthchecks read them.

Flags without an environment variable (`eth-lc-relayer daemon --owner-hop --rotate-timeout-secs …`)
are the service's `command:` in the compose file; override it in a `compose.override.yaml`
next to it.

## Install

1. Create the user and the layout above (`BRIDGE_UID`/`GID` in `.env` must be the user's).
2. Params. Withdraw relayer: the SRS and inner keys into `params/relayer/`, then
   `docker/relayer/finalize-params.sh $BRIDGE_DATA/params/relayer bridge` seals them and
   writes `SHA256SUMS`; its sha256 goes into `daemon-live.env` as `PARAMS_MANIFEST_SHA256`.
   The deposit relayer needs no params. Light client: `kzg_bn254_19.srs` into
   `params/lc-prover/`, then the step key once per image:
   `docker compose --profile ops run --rm lc-keygen` (minutes, a few GB). The rotate needs the
   Hermez `kzg_bn254_21.srs` there too: the release asset of that name, checked against the
   release's `SHA256SUMS`. Its shard stage reads `kzg_bn254_20.srs`, cut from that file once
   with the image's `downsize_srs`:

   ```bash
   docker compose --profile ops run --rm --entrypoint /opt/bridge/bin/downsize_srs lc-keygen \
     --input /data/params/kzg_bn254_21.srs --output /data/params/kzg_bn254_20.srs --k 20
   ```

   The daemon refuses to start with the rotate on while either file is missing.

   The rotate keys are not an install step: the daemon writes them to `rotate-keys/` during
   its first rotate after an image upgrade, and writes them again when the image changes.
3. Fill `.env` and the four env files from the templates; the keys and `bk_set.json` into
   `keys/`.
4. `docker compose --profile ops run --rm preflight`: the withdraw relayer's checks, no
   transaction, no daemon.
5. `docker compose up -d`.

The old Docker Compose kit under `crates/bridge-relayer-daemon/deploy/shellnet-l2` is this
directory now: its scripts are `docker/relayer/`, its runtime env template is
`env/daemon-live.env.example`, its compose service `relayer` is `daemon-live`. A host that
ran the kit moves `L2_config/relayer-state.json` and `L2_config/state/*` into
`$BRIDGE_OPT/daemon-live/state/`, `params/` into `$BRIDGE_DATA/params/relayer/` and the BK
set file into `keys/bk_set.json`; `EXPECTED_BRIDGE_COMMIT` becomes the commit of the image,
since the BK set is no longer baked into it; `RELAYER_STATE_PATH` and `BRIDGE_CONFIG_DIR` in a
reused env file are ignored. A host that ran the systemd units moves each state file into its
service's `state/` directory **under the name the daemon expects** (`deposit-relayer-state.json`,
`eth-lc-relayer-state.json`, `arbitrum-anchors-state.json`): a daemon that finds other JSON files
there but not its own refuses to start rather than begin from an empty state, and an old
`--state` flag or `*_STATE` variable fails with the hint. `eth-lc-relayer set-committee` and
`submit-rotate` write the daemon's state file and take its lock: stop `eth-lc-relayer` first,
start it again after.

## Upgrade

Set `BRIDGE_IMAGE_TAG` to the new tag, `docker compose pull`, run `preflight`, then
`docker compose up -d` service by service. The state directories carry over; the light
client's step key is per image (`lc-keygen` again when the prover changed). To roll back, set
the previous tag and `up -d` again.

## Status

`docker/relayer/status.sh docker` (the directory with `compose.yaml` and `.env`) prints the
withdraw relayer's container, chain cursor, EOA, Acki Nacki head, state files and metrics.
