# Docker Compose: shellnet → Sepolia L2 relayer

Production-oriented Compose wrapper for one `relayer daemon-live` instance at
`BRIDGE_ANCHOR_LEVEL=2`. It packages target-built binaries into an immutable
runtime image and keeps large proving data in bind mounts.

This directory contains no live credentials or deployment addresses. Filled
`.env`, deployment env and runtime env files must remain outside Git. Contract
deployment and anchor derivation are covered by
[`../../docs/live_relayer_bridge_verifyBlock_runbook.md`](../../docs/live_relayer_bridge_verifyBlock_runbook.md).

## Runtime model

- `relayer` is the only long-running service.
- `preflight` is an opt-in, read-only one-shot service. It hashes all proving
  artifacts and checks the EOA, GraphQL endpoint, bridge configuration,
  verifier bytecode, nonce and balance. It never sends a transaction.
- The container runs as a non-root UID/GID, with a read-only root filesystem,
  all capabilities dropped and `no-new-privileges` enabled.
- Params, outer proving-key cache, state, submission dumps and scratch space
  are bind-mounted below `RELAYER_RUNTIME_ROOT`.
- `restart: unless-stopped` restores the daemon after an unexpected exit or a
  Docker/host restart while preserving an intentional operator stop. Every
  container start reruns artifact and on-chain preflight before the daemon can
  send a transaction. Alert on a growing restart count: a persistent logical
  rejection still requires stopping the service and reconciling nonce,
  receipt, on-chain cursor and both state JSON files.

Only one daemon may use a given bridge/EOA/state tuple.

## Prerequisites

Build on the target OS; copying binaries from a newer glibc host can make the
runtime image unstartable.

From the repository root:

```bash
cd crates/bridge-prover-libraries
cargo build --release --locked -p bridge-relayer-daemon --bin relayer

cd ../bridge-evm-aggregator
cargo build --release --locked --bin aggregate-proof
```

The image also requires:

- Foundry `cast`;
- official Linux `solc 0.8.19+commit.7dd6d404` (the Docker build verifies its
  published SHA-256);
- the four generated `contracts/ethereum/verifiers/*AggregatorVerifier.bin`
  files;
- Hermez SRS files for **K=17,19,20,21,22** and the primary/fallback/layer
  inner PK/VK/config files. K=22 is required by the layer outer aggregator;
  omitting it can fall back to an incompatible locally generated SRS.

`aggregate-proof` invokes `solc` at runtime, so merely having Solidity
bytecode in the image is not enough.

## Persistent layout

Create the service account and directories using the UID/GID selected in
`.env` (defaults below use `998:998`):

```text
$RELAYER_RUNTIME_ROOT/
├── params/                 # immutable SRS + inner PK/VK/config + SHA256SUMS
│   └── pk_cache/           # writable primary/layer outer PK cache (~17 GiB)
├── L2_config/
│   ├── relayer-state.json
│   └── state/prover_state.json
├── submissions/            # exact verifyBlock calldata dumps
└── tmp/
```

Keep at least 80 GiB free before a cold first cycle. The live n14 acceptance
used about 17 GiB of persistent outer cache and peaked near 23 GiB RAM;
individual Halo2 stages used many CPU cores, while attestation and layer proof
stages remained sequential.

After provisioning the SRS and inner keys, seal the static parameter set and
create the writable outer-cache directory:

```bash
sudo scripts/finalize-params.sh "$RELAYER_RUNTIME_ROOT/params" gosh-relayer
```

Record the emitted `PARAMS_MANIFEST_SHA256` in the runtime env.

## Configure

```bash
cp compose.env.example .env
sudo install -d -m 0750 -o root -g gosh-relayer /etc/gosh-bridge-relayer
sudo install -m 0600 -o root -g root \
  deploy.env.example /etc/gosh-bridge-relayer/deploy-shellnet-l2.env
sudo install -m 0640 -o root -g gosh-relayer \
  runtime.env.example /etc/gosh-bridge-relayer/shellnet-l2.env
```

Fill every `CHANGEME` value. `deploy-shellnet-l2.env` is consumed only by the
one-shot contract deployment workflow; `shellnet-l2.env` is mounted into the
long-running relayer. In particular, pin the full source commit, binary hashes,
params-manifest hash, fresh bridge address/anchor and a dedicated Sepolia EOA.
Never reuse the tracked test burner or repository-generated `shellnet.common`
as a production secret source.

The deployment seed must be W²-aligned (`seqno % 16384 == 0`). Use a stable,
authenticated Sepolia RPC for both preflight and runtime.

## Build, verify and start

```bash
docker compose build --pull=false relayer
# ABI smoke: catches binaries built against a newer glibc than Ubuntu 22.04.
docker run --rm --network none --read-only --user "${RELAYER_UID:-998}:${RELAYER_GID:-998}" \
  --entrypoint /opt/gosh-relayer/bin/relayer \
  "$(docker compose images -q relayer)" --help >/dev/null
docker compose run --rm preflight
docker compose up -d relayer
```

Every real container start repeats artifact and on-chain preflight before
`daemon-live` is executed. The initial outer PKs can only be generated from a
real inner proof, so the first live boundary may take longer and write roughly
17 GiB to `params/pk_cache`.

After the first confirmed `verifyBlock`, require local/on-chain cursor
equality, then perform one controlled stop/start. Startup must select
`WarmResume`, retain `anchor_level=2`, and must not repeat the transaction.

## Operate

```bash
sudo scripts/status.sh
sudo docker compose ps
sudo docker compose logs -f --tail 200 relayer
sudo docker stats --no-stream "$(sudo docker compose ps -q relayer)"
sudo docker compose run --rm preflight
```

`status.sh` combines Docker inspection, Foundry `cast`, shellnet GraphQL and
compact `jq` views of both state files. Before the next 16,384-block boundary,
`NotYetAvailable` / `block source still has no data` is expected. Alert on
`hard aborting`, `BridgeReverted`, startup drift, SRS/VK drift, pending nonce,
or unequal local/on-chain cursors after a receipt.

After a planned host reboot, the service returns automatically unless an
operator stopped it explicitly. Confirm recovery with `docker compose ps`,
logs and `status.sh`; the normal container entrypoint has already rerun the
same read-only preflight. If that preflight fails, inspect the restart logs and
run `docker compose run --rm preflight` after correcting the cause.
