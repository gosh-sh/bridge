#!/usr/bin/env bash
# Arbitrum Sepolia e2e on the local acki-nacki network: a fresh AckiNackiBridge on Arbitrum
# Sepolia every run, a deposit from it to the local network and a withdrawal back. Runs on
# the Woodpecker runner after run-test (.woodpecker/local-net-e2e.yaml, step arbitrum-e2e).
#
#   local-network host            runner                 prover host (restricted CI key)
#   nginx0:80, nodeN:8600  <-- ssh -L -- this --- ssh -R -->  127.0.0.1:28080, :2860N
#                                                         ci-entry: proxies nginx0, nodeN on the
#                                                         run's docker network; jobs/*.sh in
#                                                         containers: build, genesis, deploy,
#                                                         deposit, relayer, anchors,
#                                                         deposit-relayer, withdraw
# Owner calls on the zerostate bridge run on the local-network host (an-on-host.sh): setTrustedL1Bridge,
# the multisig, and setAcceptedBlockHash for the Sepolia blocks that hold the assertions
# arbitrum-anchors waits on (standing in for the Sepolia light client).
#
# Required env: E2E_HOST, E2E_CI_USER, SSH_OPTS (the CI host's ssh, key already in ~/.ssh),
# PROVER_HOST, PROVER_HOST_PORT, PROVER_HOST_USER (three secrets, so that each is masked in
# the log on its own) and PROVER_HOST_SSH_KEY (newline-joined with @),
# ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY, ACKI_DIR,
# WORK_ROOT, CI_COMMIT_SHA, CI_PIPELINE_NUMBER, and the acki-nacki config the runner generated
# (ACKI_CONFIG, default $CI_WORKSPACE/.acki-nacki/config).
set -euo pipefail
: "${E2E_HOST:?}" "${E2E_CI_USER:?}" "${PROVER_HOST_SSH_KEY:?PROVER_HOST_SSH_KEY is required}"
: "${PROVER_HOST:?}" "${PROVER_HOST_PORT:?}" "${PROVER_HOST_USER:?}"
: "${ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY:?ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY is required}"
SHA=${CI_COMMIT_SHA:?}; RUN=bridge-${CI_PIPELINE_NUMBER:?}
HERE=$(cd "$(dirname "$0")" && pwd)
ACKI_CONFIG=${ACKI_CONFIG:-$CI_WORKSPACE/.acki-nacki/config}
DEADLINE=$(( $(date +%s) + ${ARBITRUM_E2E_SECS:-9000} ))
KEY=$ARBITRUM_SEPOLIA_E2E_PRIVATE_KEY
log() { printf '[%s] %s\n' "$(date -u +%T)" "$*"; }

install -m 600 /dev/null ~/.ssh/prover_ci && printf '%s' "$PROVER_HOST_SSH_KEY" | tr '@' '\n' >~/.ssh/prover_ci
PH_ADDR=$PROVER_HOST_USER@$PROVER_HOST
PH_OPTS=(-o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=30
  -o LogLevel=ERROR -i ~/.ssh/prover_ci -p "$PROVER_HOST_PORT")
ph() { ssh "${PH_OPTS[@]}" "$PH_ADDR" "$*"; }
# shellcheck disable=SC2086 # SSH_OPTS is a list of options
host() { ssh $SSH_OPTS "$E2E_CI_USER@$E2E_HOST" "$@"; }
an() { host "BRIDGE_SHA='$SHA' ACKI_DIR='$ACKI_DIR' WORK_ROOT=\$HOME/'$WORK_ROOT' bash -s -- $*" <"$HERE/an-on-host.sh"; }
renew() { host "bash -s -- renew $RUN" <"$HERE/host-lease.sh" >/dev/null; }
get() { ph get "$RUN" "$1"; }
val() { sed -n "s/^$2=//p" <<<"$1" | tail -1; }
job() { local name=$1; shift; log "job $name $*"; ph job "$RUN" "$SHA" "$name" "$@"; }
await() { # <job> <secs>: the job's exit code, its log tail on failure
  local rc; rc=$(ph wait "$RUN" "$1" "$2" | tail -1) || true
  if [ "$rc" != 0 ]; then ph logs "$RUN" "$1" 80 || true; log "job $1 exited $rc"; return 1; fi
}

TUNNEL_PIDS=()
cleanup() {
  local rc=$?
  set +e
  for j in relayer anchors deposit-relayer withdraw; do
    echo "----- $j (last 60 lines)"; ph logs "$RUN" "$j" 60 2>/dev/null
  done
  ph down "$RUN"
  kill "${TUNNEL_PIDS[@]}" 2>/dev/null
  exit $rc
}
trap cleanup EXIT

log "prover host: $(ph ping | cut -d' ' -f1,3)"
log "build $SHA on the prover host (cached per commit)"
ph build "$SHA"

# Tunnels: the host's nginx0 and nodes, through this runner, to the prover host's loopback.
fwd=(-L 127.0.0.1:18080:127.0.0.1:80); rev=(-R 127.0.0.1:28080:127.0.0.1:18080)
for n in 0 1 2 3 4; do fwd+=(-L "127.0.0.1:1860$n:node$n:8600"); rev+=(-R "127.0.0.1:2860$n:127.0.0.1:1860$n"); done
# shellcheck disable=SC2086
ssh $SSH_OPTS -N -o ExitOnForwardFailure=yes "${fwd[@]}" "$E2E_CI_USER@$E2E_HOST" & TUNNEL_PIDS+=($!)
ssh "${PH_OPTS[@]}" -N -o ExitOnForwardFailure=yes "${rev[@]}" "$PH_ADDR" & TUNNEL_PIDS+=($!)
sleep 5
for p in "${TUNNEL_PIDS[@]}"; do kill -0 "$p" || { log "FATAL: a tunnel did not come up"; exit 1; }; done
ph net-up "$RUN" "$SHA"

# The network's BK set, from the keys the zerostate generator wrote.
python3 - "$ACKI_CONFIG" >/tmp/bk_set.json <<'PY'
import glob, json, os, re, sys
keys = {}
for path in glob.glob(os.path.join(sys.argv[1], "block_keeper*_bls.keys.json")):
    idx = re.search(r"block_keeper(\d+)_bls", path).group(1)
    keys[idx] = json.load(open(path))[0]["public"]
assert keys, "no block_keeperN_bls.keys.json in " + sys.argv[1]
print(json.dumps(dict(sorted(keys.items(), key=lambda kv: int(kv[0])))))
PY
log "BK set: $(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))))' /tmp/bk_set.json) signers"

job genesis <"/tmp/bk_set.json"; await genesis 1900
GENESIS=$(get genesis.env); log "genesis: seed $(val "$GENESIS" GENESIS_SEED_SEQNO), stride $(val "$GENESIS" BUNDLE_STRIDE)"
renew

job deploy <<<"$KEY"; await deploy 1200
EVM=$(get evm.env); BRIDGE=$(val "$EVM" BRIDGE_ADDRESS)
log "AckiNackiBridge on Arbitrum Sepolia: $BRIDGE (block $(val "$EVM" BRIDGE_DEPLOY_BLOCK))"
log "https://sepolia.arbiscan.io/address/$BRIDGE"

log "AN: trust the bridge, deploy the multisig"
an trust "$BRIDGE" >/dev/null
MS=$(an msig); MSIG=$(val "$MS" MSIG); MSIG_ACCOUNT=$(val "$MS" MSIG_ACCOUNT)
MSIG_KEYS_JSON=$(host "cat '$(val "$MS" MSIG_KEYS)'" | tr -d '\n')
OWNER_KEYS_JSON=$(tr -d '\n' <"$ACKI_CONFIG/USDCBridge.keys.json")
log "multisig $MSIG: $(an ecc "$MSIG")"
renew

job deposit AMOUNT=2000000 "AN_ACCOUNT=$MSIG_ACCOUNT" <<<"$KEY"; await deposit 600
DEP=$(get deposit.env); DEPOSITOR=$(val "$DEP" DEPOSITOR)
log "deposit 2 USDC: https://sepolia.arbiscan.io/tx/$(val "$DEP" DEPOSIT_TX) (block $(val "$DEP" DEPOSIT_BLOCK))"

job relayer <<<"$KEY"
job anchors <<<"$OWNER_KEYS_JSON"
job deposit-relayer <<<"$OWNER_KEYS_JSON"
job withdraw "FROM=$MSIG" "TO=$DEPOSITOR" AMOUNT=1 <<<"$KEY
$MSIG_KEYS_JSON"

# Until both legs are done: owner-anchor the Sepolia blocks arbitrum-anchors waits on,
# watch the deposit get finalized and the withdrawal get paid.
anchored=" " deposit_done= withdraw_done= last=0
while :; do
  now=$(date +%s)
  [ "$now" -lt "$DEADLINE" ] || { log "FAIL: not done in time (deposit=${deposit_done:-no} withdraw=${withdraw_done:-no})"; exit 1; }
  if [ $(( now - last )) -ge 300 ]; then renew; last=$now; log "waiting: deposit=${deposit_done:-no} withdraw=${withdraw_done:-no}"; fi
  for h in $(get anchors-state.json 2>/dev/null | python3 -c 'import json,sys
for r in json.load(sys.stdin).get("ready", []): print(r["parent_hash"])' 2>/dev/null || true); do
    case "$anchored" in *" $h "*) continue ;; esac
    log "an assertion waits on Sepolia block $h: owner-anchoring it"
    an anchor 11155111 "$h" >/dev/null && anchored+="$h "
  done
  if [ -z "$deposit_done" ] && get deposit-relayer-state.json 2>/dev/null | python3 -c 'import json,sys
s = json.load(sys.stdin); sys.exit(0 if (s.get("last_processed_deposit_id") is not None and s["last_processed_deposit_id"] >= 0) else 1)' 2>/dev/null; then
    deposit_done=1; log "PASS deposit: finalized on the local network; multisig $(an ecc "$MSIG")"
  fi
  if [ -z "$withdraw_done" ]; then
    rc=$(ph wait "$RUN" withdraw 1 | tail -1) || true
    case "$rc" in
      0) withdraw_done=1; log "PASS withdrawal: $(get withdraw.json | tail -1)" ;;
      timeout) ;;
      *) ph logs "$RUN" withdraw 120; log "FAIL: withdraw exited $rc"; exit 1 ;;
    esac
  fi
  [ -n "$deposit_done" ] && [ -n "$withdraw_done" ] && break
  sleep 30
done
log "PASS: deposit from and withdrawal to Arbitrum Sepolia through $BRIDGE"
