#!/usr/bin/env bash
# Offline TVM bench for `EthKeccak`: compiles `KeccakCheck.sol` against a chosen
# copy of the library and executes it in tvm-cli's local VM (`debug run --tvc`).
# No network, no keys. Prints exit code and cumulative gas per call.
#
#   SOLD=/path/to/sold TVM_CLI=/path/to/tvm-cli ./run.sh            # library from ../../contracts/an
#   ... ./run.sh --lib reference/EthKeccak_1.4.0_as_deployed.sol    # the copy that shipped on shellnet
#
# Exit codes: 0 digest matches, 201 digest differs, 50 / 4 the library threw
# (sold defects), -14 the debugger's gas credit (~16.7M) ran out before the
# call finished - i.e. the call needs more than one AN transaction (10M) anyway.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
LIB=${LIB:-$HERE/../../contracts/an/EthKeccak.sol}
FIX=${FIX:-$HERE/fixtures/sepolia_11683168_headers.json}
while [ $# -gt 0 ]; do
  case $1 in
    --lib) LIB=$2; shift 2 ;;
    --fixture) FIX=$2; shift 2 ;;
    -h|--help) sed -n 2,12p "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
SOLD=${SOLD:-sold}
TVM_CLI=${TVM_CLI:-tvm-cli}
command -v "$SOLD" >/dev/null || { echo "sold not found (SOLD=$SOLD)" >&2; exit 2; }
command -v "$TVM_CLI" >/dev/null || { echo "tvm-cli not found (TVM_CLI=$TVM_CLI)" >&2; exit 2; }
[ -f "$LIB" ] || { echo "library not found: $LIB" >&2; exit 2; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$LIB" "$WORK/EthKeccak.sol"
cp "$HERE/KeccakCheck.sol" "$WORK/"
( cd "$WORK" && "$SOLD" KeccakCheck.sol >/dev/null 2>"$WORK/sold.err" ) || { cat "$WORK/sold.err" >&2; exit 1; }
read -r RLP HASH PARENT < <(python3 "$HERE/fetch_headers.py" --fields "$FIX")

echo "library : $LIB"
echo "sold    : $("$SOLD" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
echo "tvm-cli : $( { "$TVM_CLI" --version; "$TVM_CLI" version; } 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
echo "fixture : $FIX ($(( ${#RLP} / 2 ))-byte header, keccak 0x${HASH:0:8}...)"
printf '%-13s %-6s %s\n' "call" "exit" "gas (cumulative in the trace)"

run() { # <method> <params-json>
  local m=$1 p=$2 log="$WORK/$1.log" code gas
  "$TVM_CLI" -j debug run --tvc --addr "$WORK/KeccakCheck.tvc" --abi "$WORK/KeccakCheck.abi.json" -m "$m" -o "$log" "$p" >/dev/null 2>&1 || true
  # exit code from the debugger summary; gas = cumulative gas on the last executed
  # instruction (column 2 of the trace), which also covers the out-of-credit case
  code=$(awk '/exit code -?[0-9]+/{for(i=1;i<=NF;i++) if($i=="code") c=$(i+1)} END{print c}' "$log")
  gas=$(awk '/^[0-9]+ [0-9]+ /{g=$2} END{print g}' "$log")
  note=""; [ "${code:-}" = "-14" ] && note="  (debugger gas credit exhausted)"
  printf '%-13s %-6s %s%s\n' "$m" "${code:-?}" "${gas:-?}" "$note"
}
run checkEmpty  '{}'
run checkAbc    '{}'
run checkParent "{\"header\":\"$RLP\",\"expected\":\"0x$PARENT\"}"
run checkHash   "{\"data\":\"$RLP\",\"expected\":\"0x$HASH\"}"
