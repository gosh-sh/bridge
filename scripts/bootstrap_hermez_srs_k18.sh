#!/usr/bin/env bash
# Bootstrap Hermez KZG SRS k=18 for deposit-prover (when S3 pre-converted blob 403s).
#
# Uses Polygon zkEVM Google Cloud .ptau mirror + han0110/halo2-kzg-srs converter.
# Output: deposit-prover/data/kzg_params_18.srs
#
# Wall-clock: ~5 min download (1.2 GB, cached) + ~15 min convert @ k=18.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${REPO_ROOT}/deposit-prover/data/kzg_params_18.srs"
PTAU_URL="https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_20.ptau"
PTAU_PATH="${HOME}/.cache/halo2-kzg-srs/powersOfTau28_hez_final_20.ptau"
PTAU_EXPECTED_SIZE=1208042648
TOOL_DIR="${HOME}/.cache/halo2-kzg-srs/halo2-kzg-srs"

mkdir -p "$(dirname "$OUT")" "$(dirname "$PTAU_PATH")"

if [[ -f "$OUT" ]] && [[ "$(stat -f '%z' "$OUT" 2>/dev/null || stat -c '%s' "$OUT")" -gt 30000000 ]]; then
  echo "[OK] $OUT already present"
  exit 0
fi

if [[ ! -f "$PTAU_PATH" ]] || [[ "$(stat -f '%z' "$PTAU_PATH" 2>/dev/null || stat -c '%s' "$PTAU_PATH")" != "$PTAU_EXPECTED_SIZE" ]]; then
  echo "[1/3] Downloading Hermez .ptau (~1.2 GB) from Google Cloud..."
  curl -L --fail "$PTAU_URL" -o "$PTAU_PATH"
fi

if [[ ! -x "$TOOL_DIR/target/release/convert-from-snarkjs" ]]; then
  echo "[2/3] Building halo2-kzg-srs converter..."
  if [[ ! -d "$TOOL_DIR" ]]; then
    git clone --depth 1 https://github.com/han0110/halo2-kzg-srs.git "$TOOL_DIR"
  fi
  rm -f "$TOOL_DIR/rust-toolchain"
  (cd "$TOOL_DIR" && cargo build --release --quiet)
fi

echo "[3/3] Converting to halo2 raw SRS k=18..."
WORK_DIR="$(mktemp -d)"
trap "rm -rf '$WORK_DIR'" EXIT
"$TOOL_DIR/target/release/convert-from-snarkjs" "$PTAU_PATH" "$WORK_DIR/hermez-raw-" 18
mv "$WORK_DIR/hermez-raw-18" "$OUT"
echo "[OK] Wrote $OUT ($(du -h "$OUT" | awk '{print $1}'))"
