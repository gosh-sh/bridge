#!/usr/bin/env bash
# Runs the Arbitrum Sepolia fork suite (AckiNackiBridgeArbSepoliaAaveForkTest) against FORK_URL.
# Run it from contracts/ethereum; .woodpecker/solidity.yaml does.
#
# The public Arbitrum Sepolia RPC now and then answers a single state read with an error
# ("failed to call backend endpoint", pipeline 8811). forge does not retry that error: the test
# that needed the read fails, with "database error" or with a revert the missing state caused.
# The suite is therefore run again, up to FORK_ATTEMPTS times, but only when the RPC failed
# during the attempt. A failure in an attempt whose RPC reads all succeeded fails at once, so a
# real regression is never retried away.
set -uo pipefail

: "${FORK_URL:?set FORK_URL to an Arbitrum Sepolia RPC}"
attempts=${FORK_ATTEMPTS:-3}
pause=${FORK_RETRY_PAUSE_S:-30}

# What forge logs when the RPC fails: a state read that got an error ("ERROR sharedbackend:
# Failed to send/recv"), or a fork that could not be created in setUp.
rpc_failed='Failed to send/recv|could not instantiate forked environment'

log=$(mktemp)
trap 'rm -f "$log"' EXIT

for attempt in $(seq 1 "$attempts"); do
  if FOUNDRY_PROFILE=fork forge test --color never --match-contract AckiNackiBridgeArbSepoliaAaveForkTest >"$log" 2>&1; then
    cat "$log"
    exit 0
  fi
  cat "$log"
  if ! grep -qiE "$rpc_failed" "$log"; then
    echo "fork suite: failed, and forge logged no failed RPC read; not run again" >&2
    exit 1
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    echo "fork suite: attempt $attempt of $attempts failed while the RPC was failing; next in $pause s" >&2
    sleep "$pause"
  fi
done
echo "fork suite: all $attempts attempts failed while the RPC was failing; the RPC at FORK_URL is not serving" >&2
exit 1
