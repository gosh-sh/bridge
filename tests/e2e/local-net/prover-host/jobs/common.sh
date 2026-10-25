# Sourced by every job. The dispatcher (ci-entry job) mounts: /src (the commit, read-only),
# /bin-ci (its build), /params (SRS and proving keys), /cache, /out (files the runner
# reads back with `ci-entry get`), /work (this run's scratch) and /run/input (stdin).
set -euo pipefail
export PATH=/bin-ci:$PATH
OUT=/out WORK=/work
GQL=http://nginx0/graphql
AN_BRIDGE=0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
AN_BRIDGE_ABI=/src/contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json
ARB_RPC=${ARB_RPC:-https://sepolia-rollup.arbitrum.io/rpc}
SEPOLIA_RPC=${SEPOLIA_RPC:-https://ethereum-sepolia-rpc.publicnode.com}
ARB_USDC=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d
log() { printf '[%s] %s\n' "$(date -u +%T)" "$*"; }
load() { set -a; . "$OUT/$1"; set +a; }   # an env file an earlier job wrote
# KEY=VALUE lines, appended to an env file in /out that later jobs and the runner read.
emit() { local f=$1; shift; for kv in "$@"; do printf '%s\n' "$kv" >>"$OUT/$f"; done; }
