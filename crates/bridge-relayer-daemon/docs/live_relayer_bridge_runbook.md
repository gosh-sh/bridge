# Live relayer runbook — shellnet → Sepolia (L2 anchoring)

How to deploy, run and recover `relayer daemon-live`, the AN→ETH relayer that
proves Acki Nacki shellnet state and posts it to an `AckiNackiBridge` on
Sepolia. Withdrawals (`withdrawByProof`, Circuit 4) are covered by the
withdraw runbook. For a long-running server use the
[Docker Compose kit](../deploy/shellnet-l2/README.md); the commands below are
the direct-CLI equivalent, for development and recovery.

Run every command from `crates/bridge-prover-libraries/` in **bash**: the env
files find each other through `BASH_SOURCE`, and under zsh
`source L2_config/env` silently loads nothing.

## Contents

- [1. How it works](#1-how-it-works)
- [2. Prerequisites](#2-prerequisites)
- [3. Case 1 — Fresh deploy and cold start](#3-case-1--fresh-deploy-and-cold-start)
- [4. Healthy operation](#4-healthy-operation)
- [5. Restart and recovery](#5-restart-and-recovery)
- [6. Reference](#6-reference)

---

## 1. How it works

**Bundles.** The daemon proves one shellnet key block per `W² = 16384`
seq_nos (every 128th key block) and submits it with `verifyBlock`: a Circuit
1A/1B attestation proof plus a Circuit 2 layer-hashes proof, each wrapped and
aggregated by SHPLONK. At shellnet's ~3 blocks/s a window is ~90 min, and a
bundle takes ~10 min (warm cache) to ~14 min (cold) to prove
([measurements](daemon_live_performance.md)), so the prover keeps pace with the
chain. Each `verifyBlock` appends the block's layer hashes to the contract's
per-layer history windows, which withdrawals later prove against.

**BK-set rotations.** There is no rotation circuit. For each rotation block the
daemon proves the block's attestation with Circuit 1A/1B against the
**outgoing** set and submits `applyBkSetUpdate` with the block's block-id tree
leaves L2 (old set commitment) and L3 (new set commitment) — leaf positions 2
and 3, unrelated to L2 anchoring — plus three SHA-256 siblings. The contract
folds them up the 16-leaf tree and requires the result to equal the attested
`blockId` ([EVM spec §7.3](../../../docs/EVM-contracts-spec.md#73-applybksetupdate--rotate-the-bk-set-commitment)).
A rotation takes ~3.5–5 min to prove. Shellnet rotates in bursts: on
2026-10-13, 5 rotations roughly every 60 min.

**Current and previous set.** The contract stores the current set
(`storedBkSetCommitment`) and the previous one (`storedPrevBkSetCommitment`): a
`verifyBlock` for a block at or below `N = storedLastBkSetUpdateSeqNo` must be
signed by the previous set. Rotations that land before the boundary of the next
unverified bundle are applied back-to-back. One rotation past that boundary is
applied too; the daemon picks up the next one only after the bundle is
verified, so it never runs into the contract's `PrevSlotStillOwedForBundle`
check. The prover keeps
proving bundles with the outgoing set until the next bundle it proves lies
past `N`.

**One prover.** Each tick handles rotations first, then bundles, one at a
time; a rotation never runs in parallel with a bundle.

**L2 only.** `BRIDGE_ANCHOR_LEVEL` must be `2`; the `relayer` binary's built-in
default is `1`. The repo also has `./L1_config`: layer-1 anchoring with
thinning `P = 8`, one bundle per 1024 seq_nos (~5.7 min of chain time). A
bundle takes 1.75–2.4× longer to prove than that, so the prover falls further
behind with every bundle, and every withdrawal waits longer than the one
before. Use it only for a one-off test from a fresh seed; L1 anchoring is
planned to be disallowed, and the Compose kit already refuses it.

## 2. Prerequisites

### Binaries

```bash
cargo build --release --locked -p bridge-relayer-daemon --bin relayer
cargo build --release --locked -p bridge-prover-daemon --example seed_prover_bk_set
( cd ../bridge-evm-aggregator && cargo build --release --locked --bin aggregate-proof )
```

`scripts/deploy_bridge_bundle.sh` builds and runs `compute_bridge_anchors`
itself.

### SRS

`daemon-live` needs `params/kzg_bn254_{17,19,20,21,22}.srs` and does not
download them.

```bash
mkdir -p ~/.cache/halo2-kzg-srs
curl -L --fail -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau \
  https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau
# K=22 reads ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_22.ptau (~4.8 GB),
# which has no known public mirror — copy it from a machine that has it.
cargo build --release --locked -p bridge-prover-lib --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs --k 17 --k 19 --k 20 --k 21 --k 22
```

K=17–20 are cut from the K=20 Hermez file, which the tool downloads and checks
itself; K=21 and K=22 come from their own ceremony files.

### Disk and wallet

- `params/pk_cache/` holds the outer proving keys (Primary ~7.6 GB,
  LayerHashes ~10.5 GB). The first fallback-path attestation generates the
  Fallback key (~3–5 min). Keep at least 20 GB free: a key write that runs
  out of space leaves a truncated `.pk` that breaks every later proof
  ([details](daemon_live_performance.md)).
- Wallet: a dedicated Sepolia EOA (`cast wallet new`; never reuse a key that
  holds real funds), funded from a faucet without a mainnet-balance gate —
  https://sepolia-faucet.pk910.de/ (proof of work) or
  https://cloud.google.com/application/web3/faucet/ethereum/sepolia.
  Put its key into the untracked `crates/bridge-prover-libraries/.env` as
  `RELAYER_PRIVATE_KEY=0x…`: `shellnet.common` takes it from there, carries
  no key of its own, and warns when none is set. Never share the key: the
  relayer replaces whatever transaction waits at its account's next nonce,
  so two daemons on one key replace each other's transactions.
- A deploy reserves ~0.6 ETH at the script's 2 gwei gas price (the actual
  cost is lower); each `verifyBlock` or `applyBkSetUpdate` costs
  ~0.001–0.003 ETH.

### Environment

`L2_config/env` is written by the deploy script and sources `shellnet.common`
(RPC and GraphQL endpoints, paths, and the keys in `.env`). Every launch also needs:

| Variable | Value | Why |
|---|---|---|
| `BRIDGE_CONFIG_DIR` | `./L2_config` | config and state root |
| `BRIDGE_STATE_DIR` | `./L2_config/state` | `daemon-live` otherwise uses `./state` |
| `BRIDGE_BK_SET_CONFIG` | `./bk_set.shellnet.live.json` | the BK set the bridge was deployed with ([§3.2](#32-build-the-bk-set-at-s)); `shellnet.common` points at the pre-rotation genesis file |

Optional: `BRIDGE_PK_CACHE_DIR=./params/pk_cache`,
`BRIDGE_DUMP_SUBMISSIONS_DIR=./submissions` (saves each `verifyBlock`
calldata for replay), `RELAYER_METRICS_ADDR`, `BRIDGE_GQL_FAILOVER_ENDPOINTS`,
`BRIDGE_RPC_REQUEST_TIMEOUT_SECS` (default 20: how long one request to one
Ethereum endpoint may take before the next endpoint is tried).

**Ethereum RPC.** `RPC_URL` is the primary endpoint and `RPC_FAILOVER_URLS`
(comma-separated, also `--rpc-failover-urls`) the fallbacks, tried in order.
`shellnet.common` builds both: Infura first, then Alchemy, the EF DevOps
endpoint, publicnode and Tenderly's public gateway. The Infura and Alchemy
URLs carry API keys, which come from the untracked
`crates/bridge-prover-libraries/.env` (`INFURA_API_KEY=…`,
`ALCHEMY_API_KEY=…`; `.gitignore` covers it). Never put a key into
`shellnet.common`: it is tracked and `.gitleaks.toml` does not scan it. An
endpoint whose key is not set is left out.

- At start the relayer asks every endpoint for its chain id. It refuses to
  start when two disagree or none answers; one that does not answer stays in
  the list. The log line `Ethereum RPC endpoints, in order` names them by host
  only, as every relayer log line does: keys never reach the log.
- A request goes to the first endpoint that is not cooling down. An endpoint
  that times out, answers HTTP 401/403 (a rejected key), 404, 408, 429 or 5xx,
  or refuses with a rate-limit, quota or key error is skipped for 60 s
  (`WARN … endpoint failing, trying the next one`); one that cannot serve a
  particular request (an `eth_getLogs` span over its cap, which Alchemy's
  free tier answers with HTTP 400; pruned history; a block it has not seen
  yet) passes that request on and keeps its place. A revert or `nonce too
  low` is the answer and is not retried elsewhere.
- A signed transaction goes to the first endpoint that accepts it, and copies
  go to the others. Before every transaction the relayer estimates its gas,
  then reads the account's nonce from the chain; a call whose estimate
  reverts sends nothing and changes no nonce. Every transaction takes the
  nonce the chain mines next. One already waiting unmined there is replaced
  (`WARN … replacing it at the same nonce with higher fees`): 25 % above
  what the relayer last paid at that nonce, or above the current estimate
  when it does not know — after a restart — and 25 % more on every later
  attempt, up to 4 times the current estimate; past that a node refuses the
  replacement and the waiting transaction stays. Neither a lost send nor a
  restart while a transaction is stuck stalls the bridge. The replaced
  transaction can be anyone's: the relayer's key must be its own, and
  nobody sends from it by hand while a daemon runs.
- `BRIDGE_GET_LOGS_PAUSE_MS=600` in `shellnet.common` paces the startup scan
  for Infura's free tier (500 credits a second, 255 per `eth_getLogs`).

To check a list of endpoints without starting the daemon (read-only: chain
id, then the bridge's full state with the startup scan):

```bash
cd crates/bridge-prover-libraries
set -a && source L2_config/env && set +a     # bash: the env files use BASH_SOURCE
cargo test -p bridge-relayer-daemon --lib live_endpoints -- --ignored --nocapture
```

Check before every launch:

```bash
for v in RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT \
         BRIDGE_AGGREGATE_PROOF_BIN BRIDGE_VERIFIERS_DIR BRIDGE_PARAMS_DIR \
         BRIDGE_CONFIG_DIR BRIDGE_STATE_DIR BRIDGE_BK_SET_CONFIG \
         BRIDGE_BOOTSTRAP_SEQNO BRIDGE_DEPLOY_BLOCK; do
  [ -n "${!v:-}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
[ "${BRIDGE_ANCHOR_LEVEL:-}" = 2 ] && echo "  ok  BRIDGE_ANCHOR_LEVEL=2" \
  || echo "  FAIL BRIDGE_ANCHOR_LEVEL must be 2"
```

---

## 3. Case 1 — Fresh deploy and cold start

Use this for a first deploy, and whenever the current bridge cannot continue
([§5.5](#55-when-to-redeploy)).

**The one thing to get right: the BK set at the seed.** The bridge starts at
a key block S near chain head, and the contract and the daemon must both
start with the BK set that signs block S. Shellnet does not publish "the
current set" — GraphQL lists only the changes (`bkSetUpdates`: one keeper
leaves, another joins). So the set is rebuilt: take the set from before
rotations began (`bk_set.shellnet.json`) and replay every change up to block
S. A list replayed only up to an earlier block misses the changes in between:
the contract gets an outdated set while its rotation counter already skips
those changes, and the daemon stops at the next rotation with
`prover out of sync`. The deploy and the daemon's cold start both compare the
list with the commitment block S itself records, and refuse a mismatch.

### 3.1 Pick the seed

```bash
GQL=https://shellnet.ackinacki.org/graphql
HEAD=$(curl -s "$GQL" -H 'Content-Type: application/json' \
  -d '{"query":"{ blockchain { blocks(last:1) { edges { node { seq_no } } } } }"}' \
  | jq -r '.data.blockchain.blocks.edges[-1].node.seq_no')
S=$(( HEAD / 16384 * 16384 ))      # newest W² boundary at or below head
echo "seed S=$S"
```

### 3.2 Build the BK set at S

```bash
BRIDGE_GQL_ENDPOINT=$GQL BRIDGE_BK_SET_CONFIG=bk_set.shellnet.json \
  ./target/release/examples/seed_prover_bk_set --cap-height "$S" \
  --out "/tmp/prover_bk_set_$S.json"
[ -f bk_set.shellnet.live.json ] && \
  mv bk_set.shellnet.live.json "bk_set.shellnet.live.pre_$(date +%s).json"
jq '.pubkeys_hex' "/tmp/prover_bk_set_$S.json" > bk_set.shellnet.live.json
```

It prints the signer indices and the set's Poseidon commitment. The replay
needs every change since the network's genesis to still be served by
GraphQL.

### 3.3 Deploy

```bash
set -a; source L2_config/env; set +a          # your key (.env) and endpoints
CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS LEVEL=2 \
PRIVATE_KEY="$RELAYER_PRIVATE_KEY" \
WITHDRAW_ACC_FR=<AN bridge account field element> \
BRIDGE_SEED_SEQNO="$S" BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.live.json \
  ./scripts/deploy_bridge_bundle.sh 2>&1 | tee "logs/deploy_L2_$(date -u +%Y%m%dT%H%M%SZ).log"
```

- `BRIDGE_SEED_SEQNO` pins the seed to S; without it the script picks the
  newest boundary at deploy time, which can be later than the S the set was
  built for. `BRIDGE_BK_SET_CONFIG` must be a `./` path relative to this
  directory.
- `WITHDRAW_ACC_FR` is fixed for the life of the bridge. Reuse the previous
  bridge's value: `cast call <old bridge> 'bridgeWithdrawalAccFr()(uint256)'`.
  A dummy value only suits a `verifyBlock`-only smoke test.
- The script runs `compute_bridge_anchors`, which derives the constructor
  arguments: `GENESIS_BK_SET_COMMITMENT` (the file's Poseidon commitment,
  checked against block S), `GENESIS_LAST_BK_SET_UPDATE_SEQNO` (the last
  rotation at or below S — the contract's rotation counter starts there),
  `GENESIS_PREV_MAX_LEVEL_LAYER_HASH` (block S's layer-2 hash) and the seed.
  It then deploys the bundle with forge (14 transactions), rewrites
  `L2_config/env` (new `BRIDGE_ADDRESS`, `BRIDGE_DEPLOY_BLOCK`,
  `BRIDGE_BOOTSTRAP_SEQNO=S`) and archives `L2_config/state/*.json`.
- **Not idempotent**: every run deploys new contracts. After a timeout or a
  partial broadcast, inspect
  `contracts/ethereum/broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json`
  and the account's `latest`/`pending` nonces before retrying.

### 3.4 Check the new contract

```bash
set -a; source L2_config/env; set +a
c() { cast call "$BRIDGE_ADDRESS" "$@" --rpc-url "$RPC_URL"; }
c 'storedLastSeenBlockSeqNo()(uint64)'          # = S
c 'storedLastBkSetUpdateSeqNo()(uint64)'        # = GENESIS_LAST_BK_SET_UPDATE_SEQNO
c 'storedBkSetCommitment()(uint256)'            # = GENESIS_BK_SET_COMMITMENT
c 'storedPrevBkSetCommitment()(uint256)'        # = 0
c 'expectedPrevAnchor(uint8)(uint256)' 2        # = GENESIS_PREV_MAX_LEVEL_LAYER_HASH
c 'bundleStride()(uint64)'                      # = 16384
```

The `GENESIS_*` values are printed at the top of the deploy log. Any mismatch:
do not start the daemon.

### 3.5 Start the daemon

```bash
export BRIDGE_CONFIG_DIR=./L2_config
set -a; source "$BRIDGE_CONFIG_DIR/env"; set +a
export BRIDGE_STATE_DIR="$BRIDGE_CONFIG_DIR/state" \
       BRIDGE_BK_SET_CONFIG=./bk_set.shellnet.live.json \
       BRIDGE_PK_CACHE_DIR=./params/pk_cache \
       BRIDGE_DUMP_SUBMISSIONS_DIR=./submissions
# run the environment check from §2, then make sure nothing from an
# earlier bridge is left behind (the deploy script already archived state/*.json):
[ -e "$BRIDGE_CONFIG_DIR/relayer-state.json" ] && \
  mv "$BRIDGE_CONFIG_DIR/relayer-state.json" "$BRIDGE_CONFIG_DIR/relayer-state.pre_$(date +%s).json"
mkdir -p "$BRIDGE_STATE_DIR" logs submissions
nohup ./target/release/relayer --state "$BRIDGE_CONFIG_DIR/relayer-state.json" daemon-live \
  > "logs/live_cold_L2_config_$(date -u +%Y%m%dT%H%M%SZ).log" 2>&1 &
```

Within ~10 s the log shows (abridged):

```
INFO bridge_prover_lib::bk_set_bootstrap: BK-set bootstrap: mode=file, loaded 5 signers from ./bk_set.shellnet.live.json
INFO relayer: startup: read on-chain state for routing chain_last_seen=<S> chain_bk_upd=<C> local_last_seen=0 local_bk_upd=0 local_initialized=false anchor_level=2 bundle_stride=16384
INFO relayer: startup: bridge bundleStride matches chain_stride=16384
INFO relayer: startup: Cold — contract at genesis, bootstrapping policy=Explicit(<S>)
INFO relayer: daemon-live starting gql=https://shellnet.ackinacki.org/graphql params=./params prover_state=./L2_config/state
INFO bridge_prover_lib::live_driver: live_driver: cold-start bk-update cursor resolved — last_bk_set_update_seq_no=<C> (seed_seqno=<S>)
INFO bridge_prover_lib::live_driver: live_driver: bootstrap seed applied — seq_no=<S>, height=<S>, layers=2
```

- `C` is `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, and `prover_state` must be
  `./L2_config/state`.
- A wrong BK-set file stops here with
  `BK-set commitment … does not match the chain's … at seed seq_no=…` —
  rebuild it ([§3.2](#32-build-the-bk-set-at-s)).
- `Resurrect` or `WarmResume` instead of `Cold` means old local state was
  not archived.
- A daemon at another anchor level than the bridge's stops with
  `bridge … has bundleStride=…, but this daemon runs at L… with stride …`;
  check `BRIDGE_ANCHOR_LEVEL`.
- A bridge deployed before `bundleStride()` existed stops it with
  `bridge …: the bridge cannot report bundleStride() (…)`: such a bridge
  still applies the old rotation rule, which the daemon does not follow.
  Redeploy ([§5.5](#55-when-to-redeploy)); if the RPC failed, start again.

**Timing.** The seed is trusted, not proven. Rotations after S are proven and
applied as they appear. The first `verifyBlock` comes once the chain passes
S + 16384 (~90 min after S), plus ~10–14 min of proving.

---

## 4. Healthy operation

A bundle, once per window:

```
INFO bridge_prover_lib::live_driver::bundle: === Processing key block at seq_no <B> ===
INFO bridge_prover_lib::live_driver::bundle: key block <B>: primary proof generated in <ms> ms
INFO bridge_prover_lib::live_driver::bundle: key block <B>: Circuit 2 proof generated in <ms> ms
INFO bridge_relayer_daemon::aggregated_source: aggregated_source: bundle wrap+aggregate (attestation + layer) total <ms> ms seq_no=<B>
INFO bridge_relayer_daemon::relayer: verified seq_no=<B> target=<T> fin_type=Primary num_layers=2 tx=Some(0x…)
```

A rotation, for each `bkSetUpdates` event:

```
INFO bridge_prover_lib::live_driver::bk_update: === bk-update drain: processing event at seq_no <N> ===
INFO bridge_prover_lib::live_driver::bk_update: bk-update <N>: primary proof generated in <ms> ms
INFO bridge_relayer_daemon::aggregated_source: aggregated_source: wrap+aggregate bk-update attestation (Poseidon) seq_no=<N> last_seen=<L>
INFO bridge_relayer_daemon::relayer: bk-set update applied seq_no=<N> tx=Some(0x…)
```

A rotation that has to wait for the next bundle — a second rotation past the
boundary of a bundle not yet verified — is not picked up at all until that
bundle is verified, and nothing is logged for it meanwhile. It is proven and
applied right after the next `verified`.

Between boundaries every tick finds no bundle yet, and every 16th such tick
logs `INFO … waiting for the next bundle`. `WARN … no new bundle for much
longer than a bundle interval; check the prover and the chain` comes only
after 256 such ticks at L2 (16 at L1), about four hours: by then the prover or
the chain is stuck.

An occasional `WARN … tick failed, will back off` naming the RPC or GraphQL
endpoint is a failed read; the next tick repeats it. A request to an Ethereum
endpoint that gets no answer moves to the next endpoint after 20 s
(`BRIDGE_RPC_REQUEST_TIMEOUT_SECS`); the tick fails only when every endpoint
has failed it. `WARN … endpoint failing, trying the next one` once a minute
means the primary is down and its fallbacks carry the load.

**Snapshot.**

```bash
c() { cast call "$BRIDGE_ADDRESS" "$@" --rpc-url "$RPC_URL"; }
c 'storedLastSeenBlockSeqNo()(uint64)'      # +16384 per bundle
c 'storedLastBkSetUpdateSeqNo()(uint64)'    # last applied rotation
c 'layerWindowLen(uint8)(uint16)' 2         # history kept, up to 128 entries per layer
jq '{seen: .last_observed_on_chain.last_seen_block_seq_no,
     bkN: .last_observed_on_chain.last_bk_set_update_seq_no,
     attempts_since_progress, polls_since_progress,
     bk_update_attempts_since_progress}' \
  "$BRIDGE_CONFIG_DIR/relayer-state.json"
pgrep -fl 'relayer .*daemon-live'
```

Healthy: `storedLastSeenBlockSeqNo` is at most one window behind chain head,
local `seen` and `bkN` equal the chain values, and
`bk_update_attempts_since_progress` returns to 0 after each rotation.

---

## 5. Restart and recovery

### 5.1 Clean restart

```bash
kill "$(pgrep -f 'relayer .*daemon-live')"     # SIGTERM, graceful exit
pkill -f aggregate-proof                      # a stray child, if any
```

Relaunch with the same exports as [§3.5](#35-start-the-daemon), keeping the
state files. Expect `startup: WarmResume — local state matches chain
byte-for-byte` and `seed_policy=Resume`. A proof that was in progress is lost
and redone (~10–14 min for a bundle, ~3.5–5 min per pending rotation).

### 5.2 Hard abort after failed submissions

`daemon: stuck — hard aborting` follows the third submission in a row the
contract refused: a revert the node simulated before sending
(`execution reverted`, see [§5.3](#53-hard-abort-after-an-on-chain-revert))
or a failed post-submit check (`post-submit drift: …`). RPC failures do not
count — a send the RPC refused (rate limit, connection, nonce) or a receipt
that never came is logged as `… not confirmed — RPC failure, not counted
toward the abort threshold` and retried, and the next tick re-reads the chain,
so a transaction that landed anyway is picked up. The retry resends the
aggregated proof it already has (`… reusing its aggregated calldata`) rather
than spending ~2 min aggregating it again; a refused proof is aggregated
anew. Ticks that only wait for the next bundle count in
`polls_since_progress`, not toward the abort.

```bash
ADDR=$(cast wallet address --private-key "$RELAYER_PRIVATE_KEY")
cast nonce "$ADDR" --rpc-url "$RPC_URL"
cast nonce "$ADDR" --rpc-url "$RPC_URL" --block pending   # equal: nothing in flight
```

Compare the snapshot ([§4](#4-healthy-operation)) with the chain: any
difference between local and on-chain cursors means something else changed
the contract — investigate before going on. Otherwise reset the counters and
restart ([§5.1](#51-clean-restart)):

```bash
jq '.last_attempt_seqno = .last_processed_seqno
  | .attempts_since_progress = 0
  | .polls_since_progress = 0
  | .bk_update_attempts_since_progress = 0' \
  "$BRIDGE_CONFIG_DIR/relayer-state.json" > /tmp/relayer-state.json \
  && mv /tmp/relayer-state.json "$BRIDGE_CONFIG_DIR/relayer-state.json"
```

If `not confirmed` warnings keep recurring, point `RPC_URL` at an
authenticated endpoint.

`chain drift` errors are not aborts: the daemon keeps retrying, and only an
operator can clear them. `chain still below our last confirmed write` means the
chain has stayed behind the relayer's last confirmed transaction for 10 minutes
— a reorg that dropped it, or another chain behind `RPC_URL`; a lag of a few
seconds is retried silently. `last_seen_block_seq_no_jumped`,
`bk_set_commitment_without_update` or `last_bk_set_update_seq_no_uncovered`
mean the contract moved in a way this relayer did not cause: another sender, or
another contract behind `BRIDGE_ADDRESS`.

### 5.3 Hard abort after an on-chain revert

The reason ends with `execution reverted, data: "0x<selector>…"`. Fix the
cause before resetting counters:

| Selector | Error | Lane | Cause |
|---|---|---|---|
| `0x87bf1c06` | `AttestationProofRejected()` | both | An attestation public input does not match. On `applyBkSetUpdate` the inputs are `(blockId, oldCommitmentL2, blockSeqNo, attestationLastSeen)`; the proof must be against the outgoing set. |
| `0xe57f07c1` | `PrevAnchorMismatch(supplied, stored)` | verifyBlock | The bundle's Circuit 2 instance 13 does not match `expectedPrevAnchor(numLayers)`. Either local prover state diverged from the contract ([rebuild per §5.4](#54-new-machine-contract-already-advanced): move the state aside and startup picks `Resurrect`) or the contract moved between the pre-check and the send. Populated windows keep their last hash, so `t` does not fall when later key blocks skip higher layers. |
| `0xc3438e78` | `BkSetCommitmentMismatch(supplied, stored)` | verifyBlock | The bundle was proven with the wrong set: a missed rotation, a premature switch to the incoming set, or a rotation applied by someone else. |
| `0x33af5682` | `BlockSeqNoNotMonotonic(supplied, stored)` | verifyBlock | The block is at or below `storedLastSeenBlockSeqNo` — usually a state reset against an advanced contract. |
| `0x703a55ba` | `StaleBkSetCommitment(supplied, stored)` | applyBkSetUpdate | The rotation's old set is not the contract's current set. The daemon pre-checks this and sends nothing (`off-chain old_commitment … != on-chain …`); on-chain it means someone else applied a rotation. |
| `0xc942350c` | `BkUpdateSeqNoNotMonotonic(supplied, stored)` | applyBkSetUpdate | The rotation is already applied. |
| `0x4613e279` | `PrevSlotStillOwedForBundle(lastBkSetUpdateSeqNo, nextOwedBoundary)` | applyBkSetUpdate | A rotation while the previous one is at or past the next owed bundle boundary. The daemon holds these back itself, so on-chain it means another sender. |
| `0x5e38623e` | `AttestationLastSeenNotBeforeSeqNo(lastSeen, blockSeqNo)` | applyBkSetUpdate | The rotation was proven only after a bundle at or past its seq_no had been verified. |
| `0x24f0a1fa` | `BkUpdateMerkleMismatch(computedRoot, blockId)` | applyBkSetUpdate | The leaves or siblings do not belong to the attested block, or a byte-order regression. |
| `0x56048090` | `BkUpdateDisabled()` | applyBkSetUpdate | The bridge was deployed without the attestation verifiers — redeploy. |

Decode the arguments with `cast decode-error --sig '<signature>' <data>`.
With `BRIDGE_DUMP_SUBMISSIONS_DIR` set, a failing `verifyBlock` can be
replayed from its dump:

```bash
D=$(ls -t submissions/verifyBlock_seq*.json | head -1)
cast call "$BRIDGE_ADDRESS" \
  "verifyBlock(uint8,bytes,bytes,uint256,uint256,uint64,uint8,uint256[10],uint256)" \
  "$(jq -r .fin_type "$D")" "$(jq -r .attestation_proof_hex "$D")" \
  "$(jq -r .layer_hashes_proof_hex "$D")" "$(jq -r .block_id_uint256 "$D")" \
  "$(jq -r .bk_set_commitment_uint256 "$D")" "$(jq -r .block_seq_no "$D")" \
  "$(jq -r .num_layers "$D")" "[$(jq -r '.layer_hashes_uint256 | join(",")' "$D")]" \
  "$(jq -r .prev_max_level_layer_hash_uint256 "$D")" --rpc-url "$RPC_URL"
```

`applyBkSetUpdate` is not dumped.

### 5.4 New machine, contract already advanced

With no local state and a contract past its seed, startup picks `Resurrect`:
it rebuilds the prover state from the contract and resumes. The BK-set file
must then hold the set the contract currently expects — build it as in
[§3.2](#32-build-the-bk-set-at-s) with
`--cap-height <storedLastBkSetUpdateSeqNo>`. Startup stops instead (`Stop`)
when local state is ahead of the chain, belongs to another contract, or has a
different anchor level; archive the state dir and investigate.

### 5.5 When to redeploy

- The bridge fell behind GraphQL retention: the log keeps repeating
  `required object /blockchain/blockByHeight is null` for blocks the next
  bundle needs, and the contract can no longer advance.
- Startup refuses to resume and the state cannot be reconciled.
- The daemon stopped with `Acki Nacki chain head … is below the bridge's last
  verified block`: the chain head stayed below the bridge for 5 minutes, over
  at least 5 polls in a row. Shellnet was restarted from a new zerostate (or
  `BRIDGE_GQL_ENDPOINT` points at another network), and the new chain cannot
  extend this bridge. This stop exits with status 78 (every other failure
  exits with 1), so a supervisor can tell it apart: the shellnet-l2
  container stays up with the daemon stopped instead of restarting into the
  same stop every few minutes.
  Before §3, rebuild `bk_set.shellnet.json` for the new network (see the
  [shellnet BK-set section of `TECHNICAL_README.md`](../../bridge-prover-libraries/TECHNICAL_README.md#shellnet-bk-set--manual-maintenance-of-bk_setshellnetjson)).

Redeploy with [§3](#3-case-1--fresh-deploy-and-cold-start) at a seed near
head. The old bridge stays frozen at its last cursor; update everything keyed
on `BRIDGE_ADDRESS`.

---

## 6. Reference

### Files

```
crates/bridge-prover-libraries/
├── shellnet.common              shared development env (RPC, GraphQL, paths; keys from .env)
├── bk_set.shellnet.json         BK set at the network's genesis (restart of 2026-10-14); input of §3.2
├── bk_set.shellnet.live.json    BK set at the current bridge's seed; BRIDGE_BK_SET_CONFIG
├── L2_config/
│   ├── env                      written by the deploy script
│   ├── relayer-state.json       relayer cursors, attempt counters, last on-chain snapshot
│   └── state/
│       ├── prover_state.json    prover snapshot — the resume point
│       └── prover_bk_set.json   prover's current BK set, rotated on every applied rotation
├── submissions/                 verifyBlock calldata dumps (BRIDGE_DUMP_SUBMISSIONS_DIR)
├── logs/
└── params/                      SRS, inner keys, pk_cache — never delete
```

Never delete the state of a running daemon; archive it (`mv state
state.pre_<ts>`). Once a daemon has started, `prover_bk_set.json` is what it
uses; `bk_set.shellnet.live.json` is read only on a cold start.

### BK rotation on shellnet

Rotation is controlled by `minBlockKeepers` on `BlockKeeperContractRoot`
(`0:7777…7777`). With 5 active keepers, `minBlockKeepers = 5` stops rotations
and `4` enables them; `setConfig` (owner key in
`config/BlockKeeperContractRoot.keys.json`) overwrites all six config fields
at once, so read the live values (`getConfig`) before calling it. On the
network started on 2026-10-14 rotations have been on since 2026-10-15 (on the
previous one, since the evening of 2026-10-08). Step by step, with rollback:
[`turn_on_bk_sets_on_shellnet.md`](../../../turn_on_bk_sets_on_shellnet.md).

### Server deployment and handoff

The [Compose kit](../deploy/shellnet-l2/README.md) wraps the same steps:
read-only preflight on every start, `restart: unless-stopped`, and the kit's
`scripts/status.sh` for a snapshot. Never run two daemons on the
same bridge, key or state. Record for every deployment: the bridge address
and deploy transactions, S, `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, the SHA-256 of
`bk_set.shellnet.live.json`, `WITHDRAW_ACC_FR`, and — at handoff — the current
`storedLastSeenBlockSeqNo`, `storedBkSetCommitment` and
`storedLastBkSetUpdateSeqNo`.
