# env.sh — source this before running acki-nacki mt tests by hand on macOS.
#
# Usage:
#   cd bridge/multithreading
#   source bins_macOS/env.sh
#   cd $ACKI_NACKI_DIR
#   python3 tests/mt/cli.py test-multithread-cross-thread ...
#
# run_multipath.sh does this automatically — this file is only for
# manual invocations of cli.py or other acki-nacki test scripts.
#
# Resolves paths relative to this file, so it works no matter where
# the bridge repo is checked out.

# Absolute path to the directory this file lives in.
_ENV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"

export CLI_NAME="$_ENV_DIR/tvm-cli"
export TVM_CLI="$_ENV_DIR/tvm-cli"
export SOLD="$_ENV_DIR/sold"
export TVM_DEBUGGER="$_ENV_DIR/tvm-debugger"
export ZEROSTATE_HELPER="$_ENV_DIR/zerostate-helper"
export NODE_HELPER="$_ENV_DIR/node-helper"

# Cross-thread test harness knobs (these were set alongside the tool
# paths on 2026-09-28 when the recipe was validated):
#   DISABLE_MV=true disables the message-view background service —
#     without it MV fights the harness for aerospike connections
#     under sustained cross-thread load.
#   MESSAGE_ARCHIVE_OTEL_RUN_ID tags OTel spans so the run is
#     identifiable in the archive.
export DISABLE_MV=true
export MESSAGE_ARCHIVE_OTEL_RUN_ID="${MESSAGE_ARCHIVE_OTEL_RUN_ID:-mt-local-macOS}"

echo "bridge/multithreading/bins_macOS: exported CLI_NAME/TVM_CLI/SOLD/TVM_DEBUGGER/ZEROSTATE_HELPER/NODE_HELPER + DISABLE_MV/MESSAGE_ARCHIVE_OTEL_RUN_ID"
unset _ENV_DIR
