#!/usr/bin/env bash
# Run crates/bridge-prover-libraries/python/test_deploy_and_withdraw_only.py ON
# the local-net e2e host, against the co-located single-compose acki-nacki
# network that tests/dexdo/up.sh (acki-nacki) brought up there.
#
# Piped over ssh by .woodpecker/local-net-e2e.yaml (`bash -s`), never executed
# from a checkout already sitting on the host: that checkout is shared state
# the previous pipeline left behind. The lease-protocol pieces are inlined
# rather than sourced from host-lease.sh for the same reason.
#
# Why on the host: the tvm client follows the block-producer redirect to
# nodeN:8600, which only resolves on the host's docker network (up.sh maps
# nodeN into /etc/hosts). nginx0 answers on 127.0.0.1:80.
#
# Required env (set by the ssh invocation):
#   PIPELINE_ID      lease owner id (bridge-<CI_PIPELINE_NUMBER>, or "manual")
#   BRIDGE_SHA       gosh-sh/bridge commit under test
#   ACKI_DIR         acki-nacki checkout the network runs from (holds
#                    config/USDCBridge.keys.json and the giver key)
# Optional:
#   BRIDGE_REPO      clone URL (default: public https)
#   WORK_ROOT        checkout + work dir (default: ~/bridge-e2e)
#   NETWORK_ENDPOINT http endpoint of nginx0 (default http://127.0.0.1:80)
#   TVM_CLI_VERSION  tvmlabs/tvm-sdk release the test runs with (default v3.0.4-rc.an);
#                    the host's own tvm-cli is used when `tvm-cli version` reports that
#                    version, otherwise the release is fetched into WORK_ROOT/bin
#   TVM_CLI_SHA256   sha256 of that release's x86_64 tarball (checked when fetched)
#   DST_CHAIN_ID     destination chain id the withdrawal targets (test default)
set -euo pipefail

PIPELINE_ID=${PIPELINE_ID:?PIPELINE_ID is required}
BRIDGE_SHA=${BRIDGE_SHA:?BRIDGE_SHA is required}
ACKI_DIR=${ACKI_DIR:?ACKI_DIR is required}
REPO=${BRIDGE_REPO:-https://github.com/gosh-sh/bridge.git}
WORK_ROOT=${WORK_ROOT:-$HOME/bridge-e2e}
ENDPOINT=${NETWORK_ENDPOINT:-http://127.0.0.1:80}
TVM_CLI_VERSION=${TVM_CLI_VERSION:-v3.0.4-rc.an}
TVM_CLI_SHA256=${TVM_CLI_SHA256:-d5c0427237a4ccf06e7ec352ecda31c51f2be6f44bb680a36b5d8375db355cd0}
case "$PIPELINE_ID" in *[[:space:]]*) echo "FATAL: PIPELINE_ID must not contain whitespace"; exit 64 ;; esac

# Same guard/lease paths as host-lease.sh -- both must agree on where the
# lease lives. Every failure path exits explicitly (see host-lease.sh).
GUARD=/var/lock/dexdo-e2e.lock
LEASE=/var/lock/dexdo-e2e.lease
lease_assert() {
  exec 9>"$GUARD"
  flock -w 30 9 || { echo "FATAL: guard busy (flock on $GUARD not acquired within 30s)"; exit 70; }
  if [ -f "$LEASE" ]; then read -r L_ID _ < "$LEASE"; else L_ID=""; fi
  [ "$L_ID" = "$PIPELINE_ID" ] || { echo "FATAL: lease is not ours ($L_ID != $PIPELINE_ID)"; exit 72; }
  echo "$PIPELINE_ID $(date +%s)" > "$LEASE"
  exec 9>&-
}

lease_assert
DIR=$WORK_ROOT/bridge
mkdir -p "$WORK_ROOT/bin"
echo "==> bridge checkout $REPO @ $BRIDGE_SHA -> $DIR"
[ -d "$DIR/.git" ] || git clone -q "$REPO" "$DIR"
git -C "$DIR" fetch -q --depth 1 origin "$BRIDGE_SHA"
git -C "$DIR" checkout -q -f FETCH_HEAD
[ "$(git -C "$DIR" rev-parse HEAD)" = "$BRIDGE_SHA" ] || { echo "FATAL: checkout HEAD != BRIDGE_SHA"; exit 66; }
# Nothing from an earlier run may reach this one: the test puts its own directory
# first on sys.path, so a stray file left by another commit would shadow imports.
git -C "$DIR" clean -ffdxq
rm -rf "$WORK_ROOT/work_dir" "$WORK_ROOT/lease-lost"; mkdir -p "$WORK_ROOT/work_dir"

# tvm-cli: the test must speak the pinned version. The host's own binary is used
# when it reports that version; otherwise the release is fetched (sha256-checked)
# into WORK_ROOT/bin, which goes first on PATH. `tvm-cli version` prints
# "tvm-cli 3.0.4" for v3.0.4-rc.an: the pre-release suffix is not part of it.
want=${TVM_CLI_VERSION#v}; want=${want%%-*}
have_host=$(tvm-cli version 2>/dev/null | awk 'NR==1 && $1=="tvm-cli"{print $2}' || true)
have_local=$("$WORK_ROOT/bin/tvm-cli" version 2>/dev/null | awk 'NR==1 && $1=="tvm-cli"{print $2}' || true)
if [ "$have_host" = "$want" ]; then
  rm -f "$WORK_ROOT/bin/tvm-cli"
elif [ "$have_local" != "$want" ]; then
  echo "==> fetching tvm-cli $TVM_CLI_VERSION (host has '${have_host:-none}')"
  tmp=$(mktemp)
  curl -fsSL -o "$tmp" "https://github.com/tvmlabs/tvm-sdk/releases/download/$TVM_CLI_VERSION/tvm-cli-${TVM_CLI_VERSION#v}-x86_64.tar.gz"
  echo "$TVM_CLI_SHA256  $tmp" | sha256sum -c - >/dev/null
  tar xz -f "$tmp" -C "$WORK_ROOT/bin" tvm-cli && rm -f "$tmp"
  chmod +x "$WORK_ROOT/bin/tvm-cli"
fi
export PATH="$WORK_ROOT/bin:$PATH"
echo "==> tvm-cli: $(command -v tvm-cli) ($(tvm-cli version 2>/dev/null | head -n 1))"
[ "$(tvm-cli version 2>/dev/null | awk 'NR==1{print $2}')" = "$want" ] || { echo "FATAL: tvm-cli on PATH is not $want"; exit 67; }
[ -f "$ACKI_DIR/config/USDCBridge.keys.json" ] || { echo "FATAL: $ACKI_DIR/config/USDCBridge.keys.json missing (network not generated here?)"; exit 65; }

lease_assert
PY_DIR=$DIR/crates/bridge-prover-libraries/python
export MODE=local
export NETWORK="$ENDPOINT"
export GRAPHQL_URL="${ENDPOINT%:80}/graphql"
export ACKI_NACKI_ROOT="$ACKI_DIR"
export PROVER_DIR="$DIR/crates/bridge-prover-libraries"
export WORK_DIR="$WORK_ROOT/work_dir"
[ -z "${DST_CHAIN_ID:-}" ] || export DST_CHAIN_ID
echo "==> MODE=$MODE NETWORK=$NETWORK GRAPHQL_URL=$GRAPHQL_URL ACKI_NACKI_ROOT=$ACKI_NACKI_ROOT WORK_DIR=$WORK_DIR"
# Renew the lease every 5 minutes while the test runs (it takes minutes, and
# no pipeline step asserts through this stretch). The heartbeat gets its own
# stdio: anything of it still holding the ssh session's pipe (its `sleep`) would
# keep the session open for up to 5 minutes after the test has finished. A lost
# lease is recorded in a flag file and turns the run red after the test.
( while sleep 300; do ( lease_assert ) || { echo "lease lost at $(date -u +%FT%TZ)" > "$WORK_ROOT/lease-lost"; exit 72; }; done ) >/dev/null 2>&1 </dev/null &
HB=$!
trap 'pkill -P "$HB" 2>/dev/null; kill "$HB" 2>/dev/null; true' EXIT
# stdin is this script, still being read by ssh: the test and its tvm-cli children
# must not inherit it.
set +e
( cd "$PY_DIR" && python3 test_deploy_and_withdraw_only.py </dev/null )
rc=$?
set -e
echo "==> test exit code: $rc"
if [ -f "$WORK_ROOT/lease-lost" ]; then echo "FATAL: $(cat "$WORK_ROOT/lease-lost"): the host lease was not ours any more while the test ran"; exit 72; fi
exit $rc
