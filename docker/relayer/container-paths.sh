#!/usr/bin/env bash

# The paths every bridge-relayer container uses, fixed by the image and the
# compose file (bridge-audit#132): the state, work and log directories under
# /data, the read-only params with the writable pk_cache inside, the binaries
# and verifiers under /opt/bridge.
# The operator's env file sets no path; this overrides whatever it says.
remap_compose_runtime_paths() {
  BRIDGE_REPO_DIR=/opt/bridge
  RELAYER_BINARY=/opt/bridge/bin/relayer
  BRIDGE_STATE_DIR=/data/state
  BRIDGE_WORK_DIR=/data/work
  BRIDGE_LOG_DIR=/data/logs
  BRIDGE_PARAMS_DIR=/data/params
  PARAMS_SHA256SUMS=/data/params/SHA256SUMS
  BRIDGE_PK_CACHE_DIR=/data/params/pk_cache
  BRIDGE_DUMP_SUBMISSIONS_DIR=/data/logs/submissions
  BRIDGE_AGGREGATE_PROOF_BIN=/opt/bridge/bin/aggregate-proof
  BRIDGE_VERIFIERS_DIR=/opt/bridge/verifiers

  export BRIDGE_REPO_DIR RELAYER_BINARY
  export BRIDGE_STATE_DIR BRIDGE_WORK_DIR BRIDGE_LOG_DIR BRIDGE_PARAMS_DIR
  export PARAMS_SHA256SUMS BRIDGE_PK_CACHE_DIR
  export BRIDGE_DUMP_SUBMISSIONS_DIR
  export BRIDGE_AGGREGATE_PROOF_BIN BRIDGE_VERIFIERS_DIR
  # Settings of the old kit's env file that the daemon would refuse (the
  # state is a directory now, bridge-audit#132): a reused file must not
  # stop the container on every start.
  unset RELAYER_STATE_PATH BRIDGE_CONFIG_DIR
}
