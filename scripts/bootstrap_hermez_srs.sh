#!/usr/bin/env bash
#
# Bootstrap the Hermez Perpetual Powers of Tau KZG SRS (BN254, K=20)
# into `crates/bridge-snark-utils/params/kzg_bn254_20.srs`.
#
# Pulls the public ceremony output `powersOfTau28_hez_final_20.ptau`
# from Polygon zkEVM's official Google Cloud mirror, then converts it
# to the halo2 canonical raw SRS format via `han0110/halo2-kzg-srs`.
#
# This is the **production-grade** SRS — the same multi-party
# (80+ contributions) BN254 ceremony used by snarkjs, iden3 and
# Polygon zkEVM. The conversion tool revalidates the file via
# `same_ratio` (`e(g[1], g2) == e(g[0], s_g2)`), so a successful run
# implies a well-formed BN254 KZG SRS.
#
# Outputs:
#   crates/bridge-snark-utils/params/kzg_bn254_20.srs
#     (~128 MB, SHA-256 80394564e2598883dbb5d7d61630287f34e29cdd806d7ef74f68acc6bffeb608)
#
# Wall-clock cost: ~5 min download + ~20 min conversion = ~25 min.
#
# Re-running is safe: skips download if the .ptau is already present
# and skips conversion if the canonical halo2 SRS already exists and
# matches the expected SHA-256.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PARAMS_DIR="${REPO_ROOT}/crates/bridge-snark-utils/params"
SRS_PATH="${PARAMS_DIR}/kzg_bn254_20.srs"
EXPECTED_SHA="80394564e2598883dbb5d7d61630287f34e29cdd806d7ef74f68acc6bffeb608"

PTAU_URL="https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_20.ptau"
PTAU_PATH="${HOME}/.cache/halo2-kzg-srs/powersOfTau28_hez_final_20.ptau"
PTAU_EXPECTED_SIZE=1208042648  # bytes

TOOL_DIR="${HOME}/.cache/halo2-kzg-srs/halo2-kzg-srs"

mkdir -p "$PARAMS_DIR" "$(dirname "$PTAU_PATH")"

# ---------------------------------------------------------------------
# Short-circuit: if the canonical halo2 SRS already exists with the
# right SHA-256, we have nothing to do.
# ---------------------------------------------------------------------
if [[ -f "$SRS_PATH" ]]; then
    actual_sha="$(sha256sum "$SRS_PATH" | awk '{print $1}')"
    if [[ "$actual_sha" == "$EXPECTED_SHA" ]]; then
        echo "[OK] $SRS_PATH already present with expected SHA-256 — nothing to do."
        exit 0
    fi
    echo "[WARN] $SRS_PATH exists but SHA-256 mismatch:"
    echo "       expected $EXPECTED_SHA"
    echo "       got      $actual_sha"
    echo "       Will overwrite."
fi

# ---------------------------------------------------------------------
# Step 1: download .ptau (idempotent)
# ---------------------------------------------------------------------
if [[ -f "$PTAU_PATH" ]] && [[ "$(stat -c '%s' "$PTAU_PATH" 2>/dev/null || stat -f '%z' "$PTAU_PATH")" == "$PTAU_EXPECTED_SIZE" ]]; then
    echo "[OK] $PTAU_PATH already cached."
else
    echo "[1/3] Downloading Hermez ceremony output (~1.2 GB)..."
    curl -L --progress-bar --fail "$PTAU_URL" -o "$PTAU_PATH"
fi

# ---------------------------------------------------------------------
# Step 2: build han0110/halo2-kzg-srs converter (idempotent)
# ---------------------------------------------------------------------
if [[ ! -x "$TOOL_DIR/target/release/convert-from-snarkjs" ]]; then
    echo "[2/3] Building halo2-kzg-srs converter..."
    if [[ ! -d "$TOOL_DIR" ]]; then
        git clone --depth 1 https://github.com/han0110/halo2-kzg-srs.git "$TOOL_DIR"
    fi
    # Drop the pinned `rust-toolchain` (1.63.0) and build with the
    # ambient stable toolchain.
    rm -f "$TOOL_DIR/rust-toolchain"
    ( cd "$TOOL_DIR" && cargo build --release --quiet )
fi

# ---------------------------------------------------------------------
# Step 3: convert .ptau -> halo2 canonical raw SRS (~20 min CPU)
# ---------------------------------------------------------------------
echo "[3/3] Converting .ptau to halo2 raw SRS K=20 (~20 min; runs same_ratio validation)..."
WORK_DIR="$(mktemp -d)"
trap "rm -rf '$WORK_DIR'" EXIT
"$TOOL_DIR/target/release/convert-from-snarkjs" "$PTAU_PATH" "$WORK_DIR/hermez-raw-" 20

# The tool emits files for k=1..20; we only need K=20.
mv "$WORK_DIR/hermez-raw-20" "$SRS_PATH"

# Final integrity check.
actual_sha="$(sha256sum "$SRS_PATH" | awk '{print $1}')"
if [[ "$actual_sha" != "$EXPECTED_SHA" ]]; then
    echo "[FAIL] SHA-256 mismatch on freshly converted SRS:"
    echo "       expected $EXPECTED_SHA"
    echo "       got      $actual_sha"
    exit 1
fi

echo ""
echo "[OK] Wrote $SRS_PATH"
echo "     SHA-256: $actual_sha"
echo ""
echo "Next steps:"
echo "  - Delete stale cached keys to force regeneration against this SRS:"
echo "      rm -f $PARAMS_DIR/{fallback,primary,layer_hashes}_{vk,pk}.bin"
echo "      rm -f $PARAMS_DIR/{fallback,primary,layer_hashes}_config_params.json"
echo "  - Run round-trip:"
echo "      cd $REPO_ROOT/crates/bridge-snark-utils"
echo "      cargo test --release --test halo2_tvm_bundle_round_trip -- --nocapture"
