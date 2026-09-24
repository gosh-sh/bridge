#!/bin/sh
# Check that every committed verifier source compiles to its committed bytecode.
#
# The bridge deploys `<name>.bin`; `aggregate-proof` self-checks every proof
# against `<name>.sol`. Nothing on the withdrawal or relayer path compiles one
# into the other, so a regeneration that replaces only one of the pair passes
# both of those checks and still ships a self-check that no longer describes
# the deployed contract. This script is that missing link: it compiles each
# `.sol` exactly the way snark-verifier does (`solc --bin -`, last token of
# stdout) and compares the result with the `.bin` byte for byte. A verifier
# with only one half of the pair fails too, and so does bytecode over the
# EIP-170 limit.
#
#   SOLC=/path/to/solc-0.8.19 scripts/check_verifier_sources.sh [dir]
#
# `dir` defaults to contracts/ethereum/verifiers. Another compiler version
# emits different bytecode and would report every verifier as drifted, so the
# version is checked first.
set -eu

REQUIRED_SOLC_VERSION="0.8.19"
EIP170_MAX_BYTES=24576
SOLC=${SOLC:-solc}
DIR=${1:-contracts/ethereum/verifiers}

version=$("$SOLC" --version 2>/dev/null | sed -n 's/.*Version: \([0-9][0-9.]*\).*/\1/p') || version=""
if [ "$version" != "$REQUIRED_SOLC_VERSION" ]; then
  echo "need solc $REQUIRED_SOLC_VERSION, but '$SOLC' reports '${version:-nothing}'" >&2
  exit 2
fi

hex_of() { od -An -v -tx1 "$1" | tr -d ' \n'; }

fail=0
checked=0
for bin in "$DIR"/*AggregatorVerifier.bin; do
  [ -e "$bin" ] || continue
  name=$(basename "$bin" .bin)
  sol="$DIR/$name.sol"
  if [ ! -s "$sol" ]; then
    echo "FAIL  $name: $name.bin has no $name.sol beside it" >&2
    fail=1
    continue
  fi
  compiled=$("$SOLC" --bin - <"$sol" 2>/dev/null | tr -s ' \t' '\n\n' | awk 'NF {last = $0} END {print last}')
  if [ "$compiled" != "$(hex_of "$bin")" ]; then
    echo "FAIL  $name: $name.sol does not compile to $name.bin — regenerate the pair together (see $DIR/README.md)" >&2
    fail=1
    continue
  fi
  size=$(wc -c <"$bin" | tr -d ' ')
  if [ "$size" -gt "$EIP170_MAX_BYTES" ]; then
    echo "FAIL  $name: $name.bin is $size bytes, over the EIP-170 limit of $EIP170_MAX_BYTES" >&2
    fail=1
    continue
  fi
  echo "ok    $name: $name.sol compiles to $name.bin ($size B)"
  checked=$((checked + 1))
done

for sol in "$DIR"/*AggregatorVerifier.sol; do
  [ -e "$sol" ] || continue
  name=$(basename "$sol" .sol)
  if [ ! -e "$DIR/$name.bin" ]; then
    echo "FAIL  $name: $name.sol has no $name.bin beside it" >&2
    fail=1
  fi
done

if [ "$checked" -eq 0 ] && [ "$fail" -eq 0 ]; then
  echo "no *AggregatorVerifier.bin under $DIR" >&2
  exit 2
fi
exit "$fail"
