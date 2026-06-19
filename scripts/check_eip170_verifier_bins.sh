#!/usr/bin/env bash
# Fail if any verifier .bin under the given paths exceeds EIP-170 (24 576 bytes).
set -euo pipefail

MAX=24576
fail=0

check_bin() {
  local f="$1"
  local size
  size=$(wc -c <"$f" | tr -d ' ')
  if (( size > MAX )); then
    echo "EIP-170 FAIL: $f is ${size} bytes (limit ${MAX})" >&2
    fail=1
  else
    echo "EIP-170 OK:   $f (${size} bytes)"
  fi
}

if [[ $# -eq 0 ]]; then
  set -- \
    contracts/ethereum/verifiers \
    crates/bridge-evm-aggregator/target/spike
fi

for dir in "$@"; do
  [[ -d "$dir" ]] || continue
  while IFS= read -r -d '' f; do
    check_bin "$f"
  done < <(find "$dir" -name '*.bin' -print0 2>/dev/null || true)
done

if (( fail )); then
  exit 1
fi

echo "All checked .bin files are within EIP-170 (${MAX} bytes)."
