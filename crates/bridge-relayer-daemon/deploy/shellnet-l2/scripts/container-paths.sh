#!/usr/bin/env bash

# Map the host-oriented runtime environment produced after deployment onto the
# immutable paths inside the production container. Keep all persistent and
# heavyweight paths below /data; the Compose file supplies the bind mounts.
remap_compose_runtime_paths() {
  BRIDGE_REPO_DIR=/opt/gosh-relayer
  RELAYER_BINARY=/opt/gosh-relayer/bin/relayer
  RELAYER_STATE_PATH=/data/L2_config/relayer-state.json
  BRIDGE_CONFIG_DIR=/data/L2_config
  BRIDGE_STATE_DIR=/data/L2_config/state
  BRIDGE_PARAMS_DIR=/data/params
  PARAMS_SHA256SUMS=/data/params/SHA256SUMS
  BRIDGE_PK_CACHE_DIR=/data/pk-cache
  BRIDGE_DUMP_SUBMISSIONS_DIR=/data/submissions
  BRIDGE_BK_SET_CONFIG=/opt/gosh-relayer/config/bk_set.shellnet.json
  BRIDGE_AGGREGATOR_DIR=/opt/gosh-relayer/aggregator
  BRIDGE_VERIFIERS_DIR=/opt/gosh-relayer/verifiers

  export BRIDGE_REPO_DIR RELAYER_BINARY RELAYER_STATE_PATH
  export BRIDGE_CONFIG_DIR BRIDGE_STATE_DIR BRIDGE_PARAMS_DIR
  export PARAMS_SHA256SUMS BRIDGE_PK_CACHE_DIR
  export BRIDGE_DUMP_SUBMISSIONS_DIR BRIDGE_BK_SET_CONFIG
  export BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR
}
