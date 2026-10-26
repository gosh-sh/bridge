# Local acki-nacki network e2e (BRI-133)

verified-against: `c7054529ae3bf33839dd0c45119540f4f1196c7a` (PR base) plus
[PR #128](https://github.com/gosh-sh/bridge/pull/128). After merge, replace this
stamp with the merge commit.
covers: `.woodpecker/local-net-e2e.yaml`, `tests/e2e/local-net/`,
`crates/bridge-prover-libraries/python/helper/msig.py`,
`crates/bridge-prover-libraries/python/helper/common.py`,
`crates/bridge-prover-libraries/python/test_deploy_and_withdraw_only.py`.

`.woodpecker/local-net-e2e.yaml` brings up a complete acki-nacki network on one
dedicated host, bakes **this PR's** `contracts/an/*` into its zerostate, and runs
`crates/bridge-prover-libraries/python/test_deploy_and_withdraw_only.py` against it
(multisig deploy → `mintAndSendBridged` → `initiateWithdrawal` → `WithdrawalInitiated`),
then `test_arbitrum_block_proof.py` (`deployL2Verifier` → `proveBlock` on the bench's
synthetic vector, refused before its parent block is accepted and admitted after).
It follows the dexdo e2e pattern (gosh-sh/dexdo `.woodpecker/e2e.yml`) and reuses
acki-nacki's `tests/dexdo/up.sh` + `ansible/dexdo-e2e-{prepare,up,down}.yml` as they are.
The network follows a **branch** of acki-nacki (`dev`), not a commit: each run resolves
the branch's latest build on the host and uses that one build for the checkout, the
zerostate helpers and the nodes. The host is the one dexdo's e2e uses, shared through
the same lease; the host, its user and the inventory repository are Woodpecker secrets,
not files here.

Trigger: a pull request carrying the label `local-network`, or a manual run. Woodpecker
evaluates the label on the pull-request events it receives (open, new commits); set the
label before opening the PR, or push once after labelling, and the pipeline runs with
the next push either way.

Trust boundary: the label decides when a network is built, not who may build one. The
host's ssh key is pinned, so what the runner sources and copies from the host (`.acki/env`,
the zerostate helpers) comes from that host and no other. The
pipeline YAML and the host scripts come from the pull request's own tree, so whoever can
put code on a branch of this repository, or whose fork pipeline a maintainer approves, can
run code with the `pull_request` secrets. The repository's Woodpecker setting is
`require_approval: forks`: a fork's pipeline waits for that approval. The secrets give ssh
as the CI user on one shared, disposable e2e VPS and a fine-grained GitHub token that only
reads two repositories; the same pattern and setting serve the dexdo e2e this follows.

## Flow

| step | where | what |
|---|---|---|
| `remember-ssh-key` | runner | the CI ssh key from the `LOCAL_NET_SSH_KEY` secret |
| `inventory-clone` | runner | the private infrastructure repository, for the ansible inventory of the host |
| `prepare-host` | host | the playbooks from the branch head; `dexdo-e2e-prepare.yml` (build-essential, docker group); `install-rust-python.yaml` is NOT needed |
| `resolve-acki` | host | `resolve-acki-on-host.sh`: pulls `ackinacki-{node,gql-server,block-manager,all-bin}:<acki_image_tag>` where the registry login lives, reads the commit each was built from (OCI label `org.opencontainers.image.revision`) and its digest, requires node and all-bin to be one build, copies `node-helper` + `zerostate-helper` out of that all-bin image; the runner keeps the result in `.acki/env` and `.acki/helpers/` |
| `acki-clone` | runner | shallow-fetches private gosh-sh/acki-nacki at the resolved commit |
| `gen-zerostate` | runner | `gen-zerostate.sh`: x86_64 tvm-cli/tvm-debugger/sold (sha256-checked); moves acki-nacki's `BRIDGE_COMMIT` pin (`contracts/scripts/bridge_contracts.py`) to `$CI_COMMIT_SHA` in the disposable checkout and places `contracts/an/*` from `BRIDGE_REPO=$CI_WORKSPACE` (the generator re-places from that pin at import, so the pin is what decides); `make compile_contracts`; `generate_zerostate.py` |
| `lease-acquire` | host | `host-lease.sh acquire bridge-<pipeline>`: one network per host at a time, dexdo's runs included; a live lease is waited out for up to 30 min |
| `net-up` | host | asserts the lease; tears down what a cancelled or timed-out run left (`dexdo-e2e-down.yml -e full_cleanup=true`, as `net-down`); `checkout-acki-on-host.sh` puts acki-nacki at the resolved commit into `acki_dir` and applies `up.sh.patch` (see below); then `dexdo-e2e-up.yml -e target=bridge_e2e -e acki_repo= -e acki_dir=… …` ships `config/` + `docker/.env` and runs `up.sh` with the three images by digest (start, wait for finalization, ext-message probe, and with the patch: node0 serving accounts) |
| `run-test` | host | `run-test-on-host.sh` over ssh: clean bridge checkout at `$CI_COMMIT_SHA`, pinned tvm-cli, a bounded wait for nginx0's node to serve the bridge account (belt and braces over the patched hand-over), then `MODE=local` against `http://127.0.0.1:80` with a lease heartbeat while the test runs |
| `dump-logs` | host | on failure: `up.sh` log, compose state, each node's container state and `docker compose logs` (both runs of a node that failed its first start), node0's log, block-manager and gql tails |
| `net-down` | host | after success and after failure: lease owner check, `dexdo-e2e-down.yml -e full_cleanup=true -e dexdo_deploy_dir=…` (our checkout and data removed, dexdo's deploy dir untouched), `~/.netrc` removed, lease released even when the playbook fails. Not after a cancel or a timeout: no step runs then and the lease ages out (90 min) |

The test runs **on the host**: the tvm client follows the block-producer redirect to
`nodeN:8600`, which resolves only there (`up.sh` writes node0..4 into `/etc/hosts`).
`nginx0` must sit on port 80 (tvm-sdk strips non-default ports).

### Why `up.sh` is patched

`up.sh.patch` changes two things in acki-nacki's `tests/dexdo/up.sh` until the change
lands upstream (`checkout-acki-on-host.sh` refuses to proceed when it no longer applies):

- A node whose first run dies at once (`--gossip-seeds node2:10000: Temporary failure in
  name resolution`: docker's DNS does not know its peers yet) is never restarted by docker,
  whose restart policy only covers a container that ran for 10 s. `up.sh` noticed
  (`nodeN not running after 120s`) and went on; the node was started only by the
  full-stack `up -d` minutes later and then had to catch up with a network that had moved
  on. The patched wait starts such a node again after 15 s.
- The hand-over waited for finalization and for one external message to land, both
  network-wide; the node behind `nginx0` (node0) answers account reads from its own state,
  so a late node0 left the test with no accounts for minutes (pipelines 6599, 6602, 6618,
  6623). The patched hand-over also waits for node0 to serve the zerostate giver account.

The other nodes' APIs are no way around this: they want the token `nginx0` adds.

## Arbitrum Sepolia: `arbitrum-e2e`

After `run-test`, the pytest suite in `tests/e2e/local-net/arbitrum/` deploys a fresh
`AckiNackiBridge` on Arbitrum Sepolia (421614) and sends 2 USDC from it to this network with
`ackinacki-bridge deposit`, then the same 2 USDC back with `ackinacki-bridge withdraw`. Each
test is one step and asserts what the step must have produced, from the chains or from the
log of the job that did it; `-x` stops at the first failure, which names the step, what it
waited for and the tail of the log.

| Tests | Step |
|---|---|
| `test_00`–`02` | Clean every earlier deployment (its logs stay); the deployer key signs for `ARBITRUM_SEPOLIA_E2E_ADDRESS`, which has gas and ≥ 2 USDC; the Sepolia sync committee does not change within the next 6 hours |
| `test_10`–`14` | The zerostate bridge is Active at 1.14.0; the commit is built on the prover host; GraphQL and every node are reachable through the tunnels; the light client's step proving key is generated once (`lc-keygen`) and its VK is `EthBeaconLightClient`'s `VK_BLOB`; `eth-lc-relayer prove-one` starts proving the latest Sepolia finality update with it |
| `test_20`–`23` | Genesis at the network's latest 16384-block boundary (L2 anchoring, as the live relayer runs; a fresh network gets there in ~90 minutes); the bridge, oracle and four verifiers have code and the bridge's configuration; `deployL2Verifier` records the verifier; `deployLightClient` deploys the Sepolia light client without a committee |
| `test_30`–`32` | `setTrustedL1Bridge(421614, bridge)`, and the bridge holds the genesis anchor; the proven committee is set once (`set-gloas-fork-slot`, `set-committee`); the owner's anchors are off for 11155111 (`flip-owner`) and 421614, and the light client's owner rotation is off |
| `test_40`–`42` | The relayer and burner keys (derived from the deployer's) get gas; `eth-lc-relayer daemon`, `daemon-live` and `arbitrum-anchors` log their start, and the light client accepts its first checkpoint; an empty multisig |
| `test_50`–`57` | `deposit`: sent, Deposit event, the light client accepts a span (`submitAncestry`), block anchored through the verifier, proof, `finalizeDeposit`, `confirmDeposit`, 2 USDC on the multisig |
| `test_60`–`65` | `withdraw`: burn, WithdrawalInitiated, `verifyBlock` covering the burn, Circuit 4 proof, `withdrawByProof` paid, 2 USDC back |

Nothing is accepted by hand. The one trusted input is the Sepolia light client's committee:
`lc-prove` proves the latest finality update from the beacon chain
(`https://lodestar-sepolia.chainsafe.io`, `BEACON_URL` in `prover-host/jobs/common.sh`), and
`lc-bootstrap` sets the committee that signed it, then switches the owner off. From there the
light client accepts one finalized checkpoint per epoch and, with `--submit-ancestry`, the
blocks between two consecutive ones; `arbitrum-anchors` proves the deposit's Arbitrum block
from the assertion in one of those Sepolia blocks. The light client follows a new sync
committee (every ~27 h) only by a rotate proof, which does not run live yet, so a run must
not cross one: `test_02` refuses to start a run that would.

Owner calls and Acki Nacki reads run on this host (`an-on-host.sh` →
`python/arbitrum_e2e_an.py`).

The provers, daemons and the CLI run on a separate **prover host**, not on this one: bundle
proving peaks at ~23 GB and reads ~40 GB of SRS and keys. The runner reaches it with a CI key
whose `authorized_keys` entry allows only the dispatcher (a copy of
`tests/e2e/local-net/prover-host/ci-entry` in the CI directory's `bin/`) and the tunnel ports
127.0.0.1:28080 and :28600–28604. The copy is installed by hand: a pull request cannot change
what runs, and PR code runs only inside the containers the dispatcher starts, with no docker
socket and capped memory and CPU (40 GB / 12 cores for the build and the deposit prover, 64 GB /
32 cores for the provers that have to keep pace: the light client's step prover, which has an
epoch for each proof, `daemon-live` and the withdrawal's Circuit 4; 8 GB / 4 cores for the rest). It builds each commit once in a `rust:1-bookworm` container
(`prover-host/build.sh`; `ackinacki-bridge` with `dev-unfixed-bridge`, since no release has
set `MIN_BRIDGE_VERSION` yet), and runs `prover-host/jobs/*.sh` on a docker network of its own
where `nginx0` and `node0..4` are proxies to the runner's tunnels.

A run is named `arbitrum-e2e-<UTC timestamp>`. Its work directory is `runs/<run>/` and its
logs `run-logs/<run>/<job>.log` in the CI directory, an external volume every job mounts as
`/logs`: the logs outlive the containers, and the next run's clean removes everything else.

| Secret | Holds |
|---|---|
| `PROVER_HOST`, `PROVER_HOST_PORT`, `PROVER_HOST_USER` | the prover host's ssh address, port and user: three secrets so the log masks each |
| `PROVER_HOST_SSH_KEY` | the private half of its CI key, newline-joined with `@` |
| `ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY` | the deployer and depositor on Arbitrum Sepolia; the relayer and burner keys are derived from it. Its address, `ARBITRUM_SEPOLIA_E2E_ADDRESS`, is public and set in the pipeline itself; `test_01` checks the key against it |

The deployer needs Arbitrum Sepolia ETH and USDC (https://faucet.circle.com, network
Arbitrum Sepolia): each run deploys a bridge (~0.0013 ETH), funds the two derived senders and
moves 2 USDC there and back.

## Knobs (one place each; the future ansible role takes them over)

| knob | where | value / meaning |
|---|---|---|
| host, user, port | ansible inventory `ansible/inventory/bridge/e2e/local-net.yml` in the private infrastructure repository (group `bridge_e2e`; cloned by `inventory-clone` from the `LOCAL_NET_INVENTORY_REPO` secret) and the Woodpecker secrets `LOCAL_NET_HOST` / `LOCAL_NET_CI_USER` for the plain ssh steps | the host is not named in this public tree; it is dexdo's e2e host, shared through the lease |
| `acki_dir`, `dexdo_deploy_dir` on the host | pipeline `variables` `acki_dir_on_host`, `dexdo_deploy_dir_on_host`, passed as `-e` extra vars | `/opt/acki-nacki-bridge`, `/opt/acki-nacki-bridge/.no-dexdo`. The acki-nacki playbooks define both as play vars, which outrank inventory vars; only `-e` wins. `full_cleanup=true` removes both paths |
| ssh private key | Woodpecker secret `LOCAL_NET_SSH_KEY` | newline-joined with `@` (dexdo convention); public key in the host's `authorized_keys` |
| host key | `ansible/inventory/bridge/e2e/known_hosts` in the private infrastructure repository (`ssh-keyscan -t ed25519` of the host name and its address) | every ssh/scp step installs it as `~/.ssh/known_hosts` and runs with `StrictHostKeyChecking=yes`; the ansible steps get it through the inventory's `ansible_ssh_common_args`. Refresh the file when the VPS is rebuilt; until then the pipeline stops at `prepare-host` |
| GitHub token | Woodpecker secret `ACKI_NACKI_GH_TOKEN` | read access to private gosh-sh/acki-nacki and to the inventory repository; used through a temporary `~/.netrc` on the runner and, by `dexdo-e2e-up.yml`, on the host |
| acki-nacki branch and build | pipeline `variables` `acki_branch` (`dev`), `acki_image_tag` (`br_dev`), `registry` | the branch must carry `tests/dexdo/up.sh`, `ansible/dexdo-e2e-*.yml` and `contracts/scripts/bridge_contracts.py`; the images must carry the `org.opencontainers.image.revision` label (acki-nacki's CI sets it). node and all-bin of the tag must be one build (`block-manager` is rebuilt on its own cadence and runs as found). The resolved commit and digests are printed by `resolve-acki` in every run. `nginx0` is not pinned: acki-nacki's compose names `docker.gosh.sh/ackinacki-nginx` without a tag and `up.sh` does not pull it |
| tvm-cli / tvm-debugger | `tvm_cli_version`, `tvm_cli_sha256`, `tvm_debugger_sha256` | tvmlabs/tvm-sdk release; acki-nacki's `contracts/compiler/*` are macOS arm64 builds and cannot run in CI. On the host the test uses the host's `tvm-cli` only when `tvm-cli version` reports the pinned version, otherwise the release is fetched and checked |
| sold | `sold_version` + `sold_sha256` | gosh-sh/TVM-Solidity-Compiler release, verified before use |
| trusted L1 bridge baked into the zerostate | `bridge_zs_l1_bridge` | any 20-byte address for a local network |
| lease guard/lease files | `host-lease.sh`, `run-test-on-host.sh` | `/var/lock/dexdo-e2e.{lock,lease}`, the same files and protocol as dexdo's `tests/e2e/host-lease.sh`, TTL 5400 s as there (both sides must agree on when a lease is dead); bridge ids are `bridge-<pipeline>` |
| test endpoint | `run-test-on-host.sh` `NETWORK_ENDPOINT` | `http://127.0.0.1:80`, GraphQL `http://127.0.0.1/graphql` |
| withdrawal destination chain | test default `DST_CHAIN_ID` (11155111) | also the `chainId` of `mintAndSendBridged` |
| readiness waits | `run-test-on-host.sh` `API_WAIT_SECS` (default 600); test env `BRIDGE_READY_TIMEOUT_SECS` (default 180), `ACCOUNT_STATUS_TIMEOUT_SECS` (default 60) | the runner waits for nginx0's node to serve the zerostate bridge account (node0 may have come up late and still be syncing; the other nodes' APIs are behind the token nginx0 adds, so they are no alternative), the test then re-checks it; a block must finalize before `deployx` shows up (10 s was too short on the five-node network) |

Host prerequisites (a VPS; 8 cores / 22 GB / 190 GB are plenty): Ubuntu 24.04, docker with
compose v2, a registry login for docker.gosh.sh (pulls by `resolve-acki` and by `up.sh`), a CI
user with passwordless sudo and the docker group, and a `tvm-cli` on `PATH`: `up.sh` runs its
readiness probe with it during `net-up`, before the test's own fetch could help (any working
version does for the probe; the test itself uses the pinned one).

## By hand

Use a throwaway acki-nacki worktree for the generation: `gen-zerostate.sh` rewrites the
`BRIDGE_COMMIT` pin, overwrites the tracked `contracts/compiler/*` binaries and writes
`config/`. Linux x86_64 only.

```sh
# on the host: which build of the branch (prints ACKI_SHA, the image digests and the helpers dir)
ssh USER@HOST "REGISTRY=docker.gosh.sh TAG=br_dev WORK_ROOT=\$HOME/bridge-e2e bash -s" < tests/e2e/local-net/resolve-acki-on-host.sh
# controller: check out acki-nacki at ACKI_SHA, scp the two helpers into HELPERS_DIR, then
HELPERS_DIR=… ACKI_DIR=/path/to/acki-nacki-worktree BRIDGE_REPO=/path/to/bridge BRIDGE_SHA=$(git rev-parse HEAD) \
BRIDGE_ZS_L1_BRIDGE=0x0000000000000000000000000000000000000001 DISABLE_BLACKLIST=true tests/e2e/local-net/gen-zerostate.sh
ssh USER@HOST "bash -s -- acquire manual" < tests/e2e/local-net/host-lease.sh
# on the host: acki-nacki at ACKI_SHA with up.sh.patch applied (what CI does; the playbook's own clone stays off)
scp tests/e2e/local-net/up.sh.patch USER@HOST:bridge-e2e/up.sh.patch
ssh USER@HOST "ACKI_REPO=https://github.com/gosh-sh/acki-nacki.git ACKI_SHA=<ACKI_SHA> ACKI_DIR=/opt/acki-nacki-bridge GITHUB_TOKEN=… PATCH=\$HOME/bridge-e2e/up.sh.patch bash -s" < tests/e2e/local-net/checkout-acki-on-host.sh
ansible-playbook -i /path/to/infrastructure/ansible/inventory/bridge/e2e/local-net.yml /path/to/acki-nacki/ansible/dexdo-e2e-up.yml \
  -e target=bridge_e2e -e acki_repo= -e acki_dir=/opt/acki-nacki-bridge \
  -e config_src=/path/to/acki-nacki-worktree/config -e env_src=/path/to/acki-nacki-worktree/docker/.env \
  -e dexdo_net_image_tag=br_dev -e dexdo_node_image=<NODE_IMG> -e dexdo_gql_image=<GQL_IMG> -e dexdo_bm_image=<BM_IMG> -e dex_test_notes_cnt=0
ssh USER@HOST "PIPELINE_ID=manual BRIDGE_SHA=<sha> ACKI_DIR=/opt/acki-nacki-bridge bash -s" < tests/e2e/local-net/run-test-on-host.sh
ansible-playbook -i /path/to/infrastructure/ansible/inventory/bridge/e2e/local-net.yml /path/to/acki-nacki/ansible/dexdo-e2e-down.yml \
  -e target=bridge_e2e -e acki_dir=/opt/acki-nacki-bridge -e dexdo_deploy_dir=/opt/acki-nacki-bridge/.no-dexdo -e full_cleanup=true
ssh USER@HOST "bash -s -- release manual" < tests/e2e/local-net/host-lease.sh
```
