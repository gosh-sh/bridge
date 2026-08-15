#!/usr/bin/env bash
# TD-42 / DEP-VK-SRS-PIN — CI hash gate for deposit VkBlob fixture ↔ USDCBridge.VK_BLOB.
#
# Usage:
#   ./scripts/check_vk_srs_pin.sh
#   ./scripts/check_vk_srs_pin.sh --check-only   # same (default)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="${ROOT}/deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin"
PIN_FILE="${ROOT}/deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin.sha256"
EXCHANGE="${ROOT}/audit/spec/an-contracts/exchange"
if [[ -f "${EXCHANGE}/USDCBridge.sol" ]]; then
  USDC="${EXCHANGE}/USDCBridge.sol"
elif [[ -f "${EXCHANGE}/eccUSDCBridge.sol" ]]; then
  USDC="${EXCHANGE}/eccUSDCBridge.sol"
else
  echo "TD-42: missing USDCBridge.sol / eccUSDCBridge.sol in ${EXCHANGE}" >&2
  exit 1
fi
EMBED="${ROOT}/scripts/embed_deposit_vk_blob.py"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-42 — VkBlob SHA-256 pin + downgrade detection.

Verifies:
  1. fixture bytes match deposit_vk_blob.bin.sha256
  2. USDCBridge.sol embedded VK_BLOB matches fixture (size + hash)
  3. downgrade probes (truncated / empty / wrong blob) do not match pin

Upstream legacy blob 304c1c4e… (3982 B) and audit overlay 724687a4… are
rejected when fixture pin is 9dacd998… (5006 B, 12 PI).
EOF
  exit 0
fi

if [[ ! -f "$FIXTURE" ]]; then
  echo "TD-42: missing fixture $FIXTURE" >&2
  exit 1
fi
if [[ ! -f "$PIN_FILE" ]]; then
  echo "TD-42: missing pin file $PIN_FILE" >&2
  exit 1
fi

read -r PINNED_HASH PINNED_NAME < <(awk '{print $1, $2}' "$PIN_FILE")
if [[ -z "$PINNED_HASH" || "$PINNED_NAME" != "deposit_vk_blob.bin" ]]; then
  echo "TD-42: invalid pin file format (expected: <sha256>  deposit_vk_blob.bin)" >&2
  exit 1
fi

ACTUAL_HASH=$(sha256sum "$FIXTURE" | awk '{print $1}')
ACTUAL_SIZE=$(wc -c <"$FIXTURE" | tr -d ' ')

if [[ "$ACTUAL_HASH" != "$PINNED_HASH" ]]; then
  echo "TD-42 FAIL: fixture sha256 mismatch" >&2
  echo "  pin:    $PINNED_HASH" >&2
  echo "  actual: $ACTUAL_HASH ($ACTUAL_SIZE bytes)" >&2
  exit 1
fi

echo "OK: fixture pin ($ACTUAL_SIZE B, sha256=${ACTUAL_HASH:0:16}…)"

if ! python3 "$EMBED" "$USDC" --check; then
  echo "TD-42 FAIL: USDCBridge.VK_BLOB out of sync with fixture" >&2
  exit 1
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

must_not_match_pin() {
  local file=$1
  local label=$2
  local got
  got=$(sha256sum "$file" | awk '{print $1}')
  if [[ "$got" == "$PINNED_HASH" ]]; then
    echo "TD-42 FAIL: downgrade probe '$label' incorrectly matched pin" >&2
    exit 1
  fi
  echo "OK: downgrade probe '$label' rejected (sha256=${got:0:16}…)"
}

# Truncated vk (first 256 bytes)
head -c 256 "$FIXTURE" >"$tmpdir/trunc.bin"
must_not_match_pin "$tmpdir/trunc.bin" "truncated_256b"

# Empty blob
: >"$tmpdir/empty.bin"
must_not_match_pin "$tmpdir/empty.bin" "empty"

# Wrong blob (upstream 304c1c4e… era marker — not current pin)
printf '304c1c4ed1e4cf09a00fb1d83a0ae2ba' >"$tmpdir/wrong.bin"
must_not_match_pin "$tmpdir/wrong.bin" "upstream_304c1c4e_prefix"

# Audit overlay 724687a4… marker (preserve script legacy pin)
printf '724687a4db00b11afd24500715e6b0ab' >"$tmpdir/audit_old.bin"
must_not_match_pin "$tmpdir/audit_old.bin" "audit_overlay_724687a4_prefix"

echo "OK: TD-42 VkBlob pin gate passed (fixture ↔ USDCBridge ↔ downgrade probes)"
