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
- the four generated `contracts/ethereum/verifiers/*AggregatorVerifier.bin` files **and** the
  `*AggregatorVerifier.sol` sources beside them;
- Hermez SRS files for **K=17,19,20,21,22** and the primary/fallback/layer
  inner PK/VK/config files. K=22 is required by the layer outer aggregator;
  omitting it can fall back to an incompatible locally generated SRS.

`aggregate-proof` self-checks every proof by regenerating the verifier's Solidity source and
comparing it with the committed `.sol`; it compiles nothing, so the image carries no `solc`. The
`.bin` files are still needed: `preflight.sh` compares each with the runtime code deployed on chain.
Both halves of every pair are listed in the image's `IMAGE-SHA256SUMS`.

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

## Upgrade a running instance

Preflight pins the source commit and the SHA-256 of both binaries, so the order
below is load-bearing: sources, then a target-OS build, then the env values,
then the image. Nothing here touches `params/`, `pk_cache/` or either state
file, so the daemon resumes warm.

```bash
# 1. sources — preflight refuses a different commit or a dirty tree
git fetch origin && git checkout <new-commit>

# 2. build on the target OS; binaries from a newer glibc host will not start
cd crates/bridge-prover-libraries
cargo build --release --locked -p bridge-relayer-daemon --bin relayer
cd ../bridge-evm-aggregator
cargo build --release --locked --bin aggregate-proof

# 3. the hashes preflight will demand
sha256sum ../bridge-prover-libraries/target/release/relayer target/release/aggregate-proof
```

Update, in the env files kept outside Git: `EXPECTED_BRIDGE_COMMIT` (in `.env`
and in the runtime env — preflight reads both), `EXPECTED_RELAYER_SHA256`,
`EXPECTED_AGGREGATE_PROOF_SHA256`, and `RELAYER_IMAGE` for the new tag. Then
rebuild and restart with the same verification the first install uses:

```bash
sudo docker compose build --pull=false relayer
sudo docker run --rm --network none --read-only \
  --user "${RELAYER_UID:-998}:${RELAYER_GID:-998}" \
  --entrypoint /opt/gosh-relayer/bin/relayer \
  "$(sudo docker compose images -q relayer)" --help >/dev/null
sudo docker compose run --rm preflight
sudo docker compose up -d relayer
```

Preflight refuses an `aggregate-proof` whose `--help` does not mention
`--allow-source-drift` — that binary predates the verifier-source self-check and
would look for `solc`, which the image no longer carries — and it refuses a
verifier lane whose `.sol` is missing next to its `.bin`.

**Watch the first `verifyBlock` after any upgrade that moves the aggregator,
`snark-verifier` or a verifier pair.** That cycle is where the 1A/1B/2 sources
are compared for real; the withdrawal verifier is only exercised when an event
arrives.

```bash
sudo docker compose logs -f --tail 200 relayer \
  | grep -E 'VK match|aggregator VK drift|verifyBlock'
```

`aggregator VK drift` stops the daemon before it submits anything — no funds
move, and the message names the first differing line and both possible causes.
Regenerate that verifier's pair from the checkout (regeneration compiles, so it
needs `solc 0.8.19` on the host, not in the image), commit both files, and
restart the upgrade from step 1:

```bash
cd crates/bridge-evm-aggregator
cargo run --release --locked --bin export-inner-aggregator -- \
  --inner-snark <inner.snark> --name <VerifierName> \
  --out-dir ../../contracts/ethereum/verifiers
SOLC=solc ../../scripts/check_verifier_sources.sh ../../contracts/ethereum/verifiers
```

To roll back, point `RELAYER_IMAGE` at the previous tag and `docker compose up
-d relayer`. An older image carries its own verifier copies and its own `solc`,
so a verifiers directory that has gained `.sol` files does not disturb it;
restore the matching `EXPECTED_*` values in the same step.

## Operate

```bash
sudo scripts/status.sh
sudo docker compose ps
sudo docker compose logs -f --tail 200 relayer
sudo docker stats --no-stream "$(sudo docker compose ps -q relayer)"
sudo docker compose run --rm preflight
```

`status.sh` combines Docker inspection, Foundry `cast`, shellnet GraphQL,
the `relayer_gql_*` counters and compact `jq` views of both state files.
Before the next 16,384-block boundary, `NotYetAvailable` / `block source still
has no data` is expected. Alert on `hard aborting`, `BridgeReverted`, startup
drift, SRS/VK drift, pending nonce, or unequal local/on-chain cursors after a
receipt.

## GraphQL failover and metrics

`BRIDGE_GQL_ENDPOINT` in the runtime env is the primary Acki Nacki GraphQL
endpoint; `BRIDGE_GQL_FAILOVER_ENDPOINTS` is an optional comma-separated list
of further endpoints (for example direct Block Manager URLs). Every request
starts at the primary, retries it three times one second apart, then moves
down the list and cycles until one attempt succeeds. There is no stickiness
and no give-up: a request that never succeeds blocks the daemon tick, and the
metrics below are how that is noticed. Any failure counts — transport error,
timeout, non-2xx status, GraphQL `errors`, or a `null` block. Tuning knobs and
their defaults are listed in `runtime.env.example`. `preflight.sh` (also run
by the container entrypoint on every start) smoke-tests the primary and every
failover endpoint; it fails the start only when none of them answers, so an
outage of one Block Manager cannot keep the relayer from restarting.

The `relayer` service exposes Prometheus text metrics at
`http://${RELAYER_METRICS_LISTEN}/metrics` (`.env`, default
`127.0.0.1:9464`; set the host's monitoring-network address to let vmagent or
Prometheus scrape it). Metric names:

| Metric | Labels | Meaning |
| --- | --- | --- |
| `relayer_gql_requests_total` | `endpoint`, `op` | GraphQL request attempts |
| `relayer_gql_errors_total` | `endpoint`, `op`, `kind` | Failed attempts; `kind` is `transport`, `timeout`, `http_status`, `decode`, `graphql_error` or `null_data` |
| `relayer_gql_failovers_total` | `from`, `to` | Switches to the next endpoint |
| `relayer_gql_full_rounds_total` | `op` | A request went through every endpoint without success |
| `relayer_gql_request_duration_seconds` | `endpoint`, `op`, `outcome` | Attempt latency histogram |

Alert example for an unhealthy source: `sum(rate(relayer_gql_errors_total[1m])) * 60 > 10`.

After a planned host reboot, the service returns automatically unless an
operator stopped it explicitly. Confirm recovery with `docker compose ps`,
logs and `status.sh`; the normal container entrypoint has already rerun the
same read-only preflight. If that preflight fails, inspect the restart logs and
run `docker compose run --rm preflight` after correcting the cause.
