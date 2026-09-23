#!/bin/bash

# Trusted Setup helper for the Deposit Prover (Halo2 KZG, BN254).
#
# Deposit proofs are keyed on the Hermez / Polygon Perpetual Powers of Tau.
# The Acki Nacki `ZKHALO2VERIFYWITHVK` opcode verifies them against the Hermez
# [s]·G2 it embeds (`tvm-sdk`, `tvm_vm/src/executor/zk_halo2_utils.rs`,
# `KZG_S_G2_BYTES` = 928fafb3d0cc…b3be595c6900), and
# `load_kzg_params_from_trusted_setup` in `src/prover.rs` reads only
# `data/kzg_params_{k}.srs`.
#
# An SRS from any other ceremony produces proofs the opcode rejects. That
# includes the Acki Nacki chain ceremony (s_g2 = c6028acf4420…), which only the
# legacy Dark DEX `ZKHALO2VERIFY` opcode still uses. So this script checks the
# [s]·G2 of the file it keeps and refuses anything that is not Hermez.

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
DATA_DIR="data"
SRS_FILE="kzg_params_18.srs"          # Hermez k=18, what prover.rs loads
SRS_URL="https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-18"
MIN_SIZE=30000000  # At least 30 MB

# Hermez [s]·G2, first and last 6 bytes. In the halo2 raw SRS format [s]·G2 is
# always the final 128 bytes of the file.
HERMEZ_SG2_HEAD="928fafb3d0cc"
HERMEZ_SG2_TAIL="b3be595c6900"

# Succeeds only when the file's [s]·G2 is the Hermez one.
check_ceremony() {
    local head tail
    head=$(tail -c 128 "$1" | head -c 6 | od -An -tx1 | tr -d ' \n')
    tail=$(tail -c 6 "$1" | od -An -tx1 | tr -d ' \n')
    if [ "$head" = "$HERMEZ_SG2_HEAD" ] && [ "$tail" = "$HERMEZ_SG2_TAIL" ]; then
        echo -e "${GREEN}✅ [s]·G2 is the Hermez ceremony (${head}…${tail}).${NC}"
        return 0
    fi
    echo -e "${RED}❌ $1 is not the Hermez ceremony: [s]·G2 = ${head}…${tail},${NC}"
    echo -e "${RED}   expected ${HERMEZ_SG2_HEAD}…${HERMEZ_SG2_TAIL}. Proofs keyed on it are rejected on chain.${NC}"
    return 1
}

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Deposit Prover — Trusted Setup helper (KZG / BN254)      ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Create data directory if it doesn't exist
mkdir -p "$DATA_DIR"

# Check if file already exists
if [ -f "$DATA_DIR/$SRS_FILE" ]; then
    ACTUAL_SIZE=$(stat -f%z "$DATA_DIR/$SRS_FILE" 2>/dev/null || stat -c%s "$DATA_DIR/$SRS_FILE" 2>/dev/null)
    if [ "$ACTUAL_SIZE" -gt "$MIN_SIZE" ]; then
        echo -e "Found $DATA_DIR/$SRS_FILE (${ACTUAL_SIZE} bytes, ~$(($ACTUAL_SIZE / 1048576)) MB)."
        if check_ceremony "$DATA_DIR/$SRS_FILE"; then
            exit 0
        fi
        echo -e "   Delete it and re-run: ${YELLOW}rm $DATA_DIR/$SRS_FILE${NC}"
        exit 1
    else
        echo -e "${RED}❌ Existing file is too small ($ACTUAL_SIZE bytes) — removing and re-downloading...${NC}"
        rm "$DATA_DIR/$SRS_FILE"
    fi
fi

# Download the file
echo -e "${BLUE}Downloading Hermez/Polygon Powers of Tau (k=18, ~33 MB)...${NC}"
echo -e "${BLUE}Source: $SRS_URL${NC}"
echo ""

# Fail on an HTTP error rather than saving the error page as the SRS.
if command -v wget &> /dev/null; then
    DOWNLOAD=(wget --show-progress -O "$DATA_DIR/$SRS_FILE" "$SRS_URL")
elif command -v curl &> /dev/null; then
    DOWNLOAD=(curl -fL --progress-bar -o "$DATA_DIR/$SRS_FILE" "$SRS_URL")
else
    echo -e "${RED}❌ Error: Neither wget nor curl is installed!${NC}"
    exit 1
fi
if ! "${DOWNLOAD[@]}"; then
    rm -f "$DATA_DIR/$SRS_FILE"
    echo -e "${RED}❌ Download failed: $SRS_URL${NC}"
    exit 1
fi

echo ""

# Verify file size
ACTUAL_SIZE=$(stat -f%z "$DATA_DIR/$SRS_FILE" 2>/dev/null || stat -c%s "$DATA_DIR/$SRS_FILE" 2>/dev/null)
if [ "$ACTUAL_SIZE" -lt "$MIN_SIZE" ]; then
    echo -e "${RED}❌ Size verification failed (got $ACTUAL_SIZE bytes, expected ≥ $MIN_SIZE).${NC}"
    echo -e "${YELLOW}The download may be corrupted. Please try again.${NC}"
    rm "$DATA_DIR/$SRS_FILE"
    exit 1
fi

if ! check_ceremony "$DATA_DIR/$SRS_FILE"; then
    rm "$DATA_DIR/$SRS_FILE"
    exit 1
fi

echo -e "${GREEN}✅ Hermez SRS downloaded (${ACTUAL_SIZE} bytes, ~$(($ACTUAL_SIZE / 1048576)) MB).${NC}"
