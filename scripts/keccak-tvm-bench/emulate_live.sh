#!/usr/bin/env bash
# Emulate `submitAncestry` against the LIVE light-client account state (local
# run in tvm-cli, nothing is sent, no keys): the exit code the daemon would get.
#   TVM_CLI=... ABI=EthBeaconLightClient.abi.json ADDR=<dapp_id>::<account_id> ./emulate_live.sh fixtures/sepolia_11683168_headers.json
# Run from a directory holding tvm-cli.conf.json for the network (e.g. the
# relayer's config dir), or pass --url through TVM_CLI_ARGS.
set -euo pipefail
TVM_CLI=${TVM_CLI:-tvm-cli}; ABI=${ABI:?ABI=path to EthBeaconLightClient.abi.json}; ADDR=${ADDR:?ADDR=dapp_id::account_id}
HEADERS=$(python3 -c "import json,sys; print(json.dumps(json.load(open(sys.argv[1])), separators=(',',':')))" "${1:?headers.json}")
"$TVM_CLI" -j ${TVM_CLI_ARGS:-} runx --abi "$ABI" --addr "$ADDR" -m submitAncestry --headerRlps "$HEADERS" 2>&1 | grep -E '"exit_code"|"description"|"value0"|Error"' || true
