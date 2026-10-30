# Sourced by every job. The dispatcher (ci-entry job) mounts: /src (the commit, read-only),
# /bin-ci (its build), /params (SRS and proving keys), /cache, /out (files the runner
# reads back with `ci-entry get`), /work (this run's scratch), /logs (the run's logs, which
# outlive it; this job's own output already goes to /logs/$JOB.log) and /run/input (stdin).
set -euo pipefail
# The service jobs run in the image built for the service (ci-entry job), whose
# binaries are on PATH already; /bin-ci, the same build as plain files, is what
# the other jobs and the rust container run.
export PATH=$PATH:/bin-ci
OUT=/out WORK=/work
GQL=http://nginx0/graphql
AN_BRIDGE=0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
AN_BRIDGE_ABI=/src/contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json
ARB_RPC=${ARB_RPC:-https://sepolia-rollup.arbitrum.io/rpc}
# The deposit's RPC also has to serve eth_getRawTransactionByHash: the prover reads every
# transaction of the deposit's block raw. The Arbitrum Foundation's endpoint answers -32601.
ARB_DEPOSIT_RPC=${ARB_DEPOSIT_RPC:-https://arbitrum-sepolia-rpc.publicnode.com}
SEPOLIA_RPC=${SEPOLIA_RPC:-https://ethereum-sepolia-rpc.publicnode.com}
ARB_USDC=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d
log() { printf '[%s] %s\n' "$(date -u +%T)" "$*"; }
load() { set -a; . "$OUT/$1"; set +a; }   # an env file an earlier job wrote
# KEY=VALUE lines, appended to an env file in /out that later jobs and the runner read.
emit() { local f=$1; shift; for kv in "$@"; do printf '%s\n' "$kv" >>"$OUT/$f"; done; }
# Keys derived from the deployer's, one per sender, so that no two processes share a nonce:
# daemon-live replaces whatever waits at its account's next nonce.
derive_key() { cast keccak "$(cast concat-hex "$1" "$(cast from-utf8 "$2")")"; }
# The Sepolia light client: its beacon source, ABI, and the Gloas fork slot it proves against.
BEACON_URL=${BEACON_URL:-https://lodestar-sepolia.chainsafe.io}
LC_ABI=/src/contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.abi.json
SEPOLIA_GLOAS_FORK_SLOT=11296768
# The step proving key lc-keygen generates once per run; the prover reads it from STEP_KEY_DIR.
LC_STEP_KEY_DIR=$WORK/lc-step-key
