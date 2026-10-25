#!/usr/bin/env bash
# Offline TVM bench for the Arbitrum block proof. Runs the tracked
# ArbitrumBlockVerifier.tvc under the code salt the bridge gives it, through
# constructor, initialize and proveBlock, and the two proofs inside proveBlock
# on live chain data. No network, no keys.
#
#   SOLD=/path/to/sold TVM_DEBUGGER=/path/to/tvm-debugger ./run.sh
#
# SOLD has to know the KECCAK256 instruction: sold 0.81.0 from
# gosh-sh/TVM-Solidity-Compiler branch feature/keccak256-builtin. TVM_DEBUGGER
# has to execute it: tvm_debugger from tvmlabs/tvm-sdk fd76a8a9 or later (3.0.4
# fails the instruction with exit 6). Exits non-zero when any check fails.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
AN=$(cd "$HERE/../../contracts/an" && pwd)
VERIFIER=${VERIFIER:-$AN/0.81.0_compiled/exchange/ArbitrumBlockVerifier}
SYNTH=${SYNTH:-$HERE/fixtures/synthetic_421614.json}
LIVE=${LIVE:-$HERE/fixtures/live_steps.json}
SOLD=${SOLD:-sold}
DBG=${TVM_DEBUGGER:-tvm-debugger}
BRIDGE=0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
case ${1:-} in -h|--help) sed -n 2,12p "$0"; exit 0 ;; esac
command -v "$SOLD" >/dev/null || { echo "sold not found (SOLD=$SOLD)" >&2; exit 2; }
command -v "$DBG" >/dev/null || { echo "tvm-debugger not found (TVM_DEBUGGER=$DBG)" >&2; exit 2; }

WORK=$(mktemp -d)
trap '[ -n "${KEEP:-}" ] && echo "kept $WORK" || rm -rf "$WORK"' EXIT
"$SOLD" --tvm-version gosh --base-path "$AN" "$HERE/ProofBench.sol" -o "$WORK" >/dev/null 2>"$WORK/sold.err" \
  || { cat "$WORK/sold.err" >&2; exit 1; }
cp "$WORK/ProofBench.tvc" "$WORK/bench.tvc"
cp "$VERIFIER.tvc" "$WORK/verifier.tvc"

echo "verifier : $VERIFIER.tvc"
echo "sold     : $("$SOLD" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
echo "debugger : $("$DBG" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
printf '%-34s %-6s %-9s %s\n' "call" "exit" "gas" "check"

FAILED=0
# run <label> <tvc> <abi> <method> <params-json> <want-exit> [debugger options...]
# Prints exit code and gas; the JSON answer is left in $WORK/last.json.
run() {
  local label=$1 tvc=$2 abi=$3 m=$4 p=$5 want=$6
  shift 6
  printf '%s' "$p" >"$WORK/params.json"
  "$DBG" run -i "$tvc" -a "$abi" -m "$m" -p "$(cat "$WORK/params.json")" -j "$@" 2>"$WORK/dbg.err" \
    | tail -1 >"$WORK/last.json" || true
  local code gas
  code=$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["exit_code"])' "$WORK/last.json" 2>/dev/null || echo "?")
  gas=$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["gas_used"])' "$WORK/last.json" 2>/dev/null || echo "?")
  local verdict="ok"
  [ "$code" = "$want" ] || { verdict="FAIL: want exit $want"; FAILED=1; }
  printf '%-34s %-6s %-9s %s\n' "$label" "$code" "$gas" "$verdict"
}
# expect <label> <fixture> <python expression over r = the last answer, v = the fixture>
expect() {
  if python3 -I -c 'import json,sys;r=json.load(open(sys.argv[1]));v=json.load(open(sys.argv[2]))
sys.exit(0 if eval(sys.argv[3]) else 1)' "$WORK/last.json" "$2" "$3"; then
    printf '%-34s %-6s %-9s %s\n' "  $1" "" "" "ok"
  else
    printf '%-34s %-6s %-9s %s\n' "  $1" "" "" "FAIL"; FAILED=1
  fi
}
field() { python3 -I -c 'import json,sys;v=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."): v=v[k]
print(json.dumps(v) if not isinstance(v,(str,int)) else v)' "$1" "$2"; }

BENCH=("$WORK/bench.tvc" "$WORK/ProofBench.abi.json")
VABI=$VERIFIER.abi.json
run "bench constructor" "${BENCH[@]}" constructor '{}' 0

# --- the verifier as the bridge deploys it -----------------------------------
L2=$(field "$SYNTH" l2ChainId)
CODE=$("$DBG" state-decode --state-init "$WORK/verifier.tvc" | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["code"])')
run "salt the verifier code ($L2)" "${BENCH[@]}" salted "{\"code\":\"$CODE\",\"l2ChainId\":$L2}" 0
SALTED=$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["response"]["code_"])' "$WORK/last.json")
"$DBG" run -i "$WORK/verifier.tvc" --replace-code "$SALTED" >/dev/null
cp "$WORK/verifier.tvc" "$WORK/verifier.tvc.salted"

FROM_BRIDGE=(--internal --message-source "$BRIDGE" --message-value 10000000000)
run "constructor from a stranger" "$WORK/verifier.tvc" "$VABI" constructor '{}' 310 \
  --internal --message-source "0:$(printf '2%.0s' {1..64})" --message-value 10000000000
run "constructor from the bridge" "$WORK/verifier.tvc" "$VABI" constructor '{}' 0 "${FROM_BRIDGE[@]}"
field "$SYNTH" proveBlock >"$WORK/prove.json"
run "proveBlock before initialize" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/prove.json")" 314
INIT="{\"l2ChainId\":$L2,\"l1ChainId\":$(field "$SYNTH" l1ChainId),\"rollup\":\"$(field "$SYNTH" rollup)\"}"
run "initialize, wrong chain id" "$WORK/verifier.tvc" "$VABI" initialize \
  "{\"l2ChainId\":42161,\"l1ChainId\":1,\"rollup\":\"$(field "$SYNTH" rollup)\"}" 312 "${FROM_BRIDGE[@]}"
run "initialize" "$WORK/verifier.tvc" "$VABI" initialize "$INIT" 0 "${FROM_BRIDGE[@]}"
run "initialize again" "$WORK/verifier.tvc" "$VABI" initialize "$INIT" 311 "${FROM_BRIDGE[@]}"

run "proveBlock (synthetic, $L2)" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/prove.json")" 0
expect "one message, to the bridge" "$SYNTH" \
  'len(r["messages"])==1 and r["messages"][0]["destination"]=="'"$BRIDGE"'"'
# The body is the function id and three uint256 in one cell: its last 100 bytes.
expect "body: chain id, parent, N" "$SYNTH" '(lambda d: d[:4].hex()==__import__("hashlib").sha256(
  b"acceptBlockHashFromL2Verifier(uint256,uint256,uint256)()v2").digest()[:4].hex()
  and [int.from_bytes(d[i:i+32],"big") for i in (4,36,68)]==[v["l2ChainId"],
  int(v["expect"]["parentBlockHash"],16), int(v["expect"]["blockHash"],16)])(
  __import__("base64").b64decode(r["messages"][0]["body"])[-100:])'

# One flipped byte in each input the proof stands on.
tamper() { # <key> <index or -> <exit>
  python3 -I - "$WORK/prove.json" "$1" "$2" >"$WORK/bad.json" <<'EOF'
import json, sys
p = json.load(open(sys.argv[1])); k = sys.argv[2]; i = sys.argv[3]
def flip(h, at): b = bytearray.fromhex(h); b[at] ^= 1; return b.hex()
if i == "-": p[k] = flip(p[k], len(p[k]) // 4)
else: p[k][int(i)] = flip(p[k][int(i)], 40)
print(json.dumps(p))
EOF
  local label="  tampered $1"
  [ "$2" = "-" ] || label+="[$2]"
  run "$label" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/bad.json")" "$3"
}
tamper receiptProof 1 302
# A byte of the parent header's receiptsRoot: the first receipt node no longer
# matches. A byte elsewhere passes here with another parent hash, which the
# bridge refuses (224) as a block it has not accepted.
python3 -I - "$WORK/prove.json" >"$WORK/bad.json" <<'EOF'
import json, sys
p = json.load(open(sys.argv[1])); b = bytearray.fromhex(p["l1Header"])
at = 3 + 33 + 33 + 21 + 33 + 33 + 1          # list header, five fields, receiptsRoot's own header
b[at] ^= 1; p["l1Header"] = b.hex(); print(json.dumps(p))
EOF
run "  tampered l1Header.receiptsRoot" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/bad.json")" 302
tamper l2Header - 306      # M's header no longer hashes to the assertion's block hash
tamper accountProof 1 302
tamper storageProof 2 302
python3 -I -c 'import json,sys;p=json.load(open(sys.argv[1]));p["blockNumber"]+=1;print(json.dumps(p))' \
  "$WORK/prove.json" >"$WORK/bad.json"
run "  another block number" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/bad.json")" 302
# blockNumber only picks the slot. N + 393168 is past M ("M before N") and N
# is then outside the window: the same slot, and what it holds is still the
# hash of N, an ancestor of M. Nothing admits a hash for the later number.
python3 -I -c 'import json,sys;p=json.load(open(sys.argv[1]));p["blockNumber"]+=393168;print(json.dumps(p))' \
  "$WORK/prove.json" >"$WORK/bad.json"
run "  N + 393168 (same slot)" "$WORK/verifier.tvc" "$VABI" proveBlock "$(cat "$WORK/bad.json")" 0
expect "admits N's hash, not a later one" "$SYNTH" \
  'int.from_bytes(__import__("base64").b64decode(r["messages"][0]["body"])[-32:],"big")==int(v["expect"]["blockHash"],16)'

# A verifier initialized with another rollup finds no assertion of its own.
cp "$WORK/verifier.tvc.salted" "$WORK/other.tvc"
run "  other: constructor" "$WORK/other.tvc" "$VABI" constructor '{}' 0 "${FROM_BRIDGE[@]}"
run "  other: init, rollup 0x11..11" "$WORK/other.tvc" "$VABI" initialize \
  "{\"l2ChainId\":$L2,\"l1ChainId\":$(field "$SYNTH" l1ChainId),\"rollup\":\"0x$(printf '1%.0s' {1..40})\"}" 0 \
  "${FROM_BRIDGE[@]}"
run "  foreign rollup" "$WORK/other.tvc" "$VABI" proveBlock "$(cat "$WORK/prove.json")" 307

# --- the two proofs on live data ---------------------------------------------
run "step 1: assertion -> M (live)" "${BENCH[@]}" assertionBlock "$(field "$LIVE" assertionBlock.args)" 0
expect "M = $(field "$LIVE" assertionBlock.expect | cut -c1-18)..." "$LIVE" \
  'int(r["response"]["blockHashM"],16)==int(v["assertionBlock"]["expect"],16)'
run "step 2: M -> N (live)" "${BENCH[@]}" ancestorHash "$(field "$LIVE" ancestorHash.args)" 0
expect "N = $(field "$LIVE" ancestorHash.expect | cut -c1-18)..." "$LIVE" \
  'int(r["response"]["blockHashN"],16)==int(v["ancestorHash"]["expect"],16)'

exit $FAILED
