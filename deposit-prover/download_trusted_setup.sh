#!/bin/bash

# Download Trusted Setup for Deposit Prover (Halo2 format)
# This script downloads pre-converted KZG parameters in Halo2 .srs format
# Source: https://github.com/han0110/halo2-kzg-srs

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
DATA_DIR="data"
SRS_FILE="kzg_params_18.srs"
SRS_URL="https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-18"
MIN_SIZE=30000000  # At least 30 MB

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Downloading Trusted Setup for Deposit Prover             ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${BLUE}Source:${NC}  Hermez/Polygon Powers of Tau ceremony"
echo -e "${BLUE}Format:${NC}  Pre-converted Halo2 .srs format (k=18)"
echo -e "${BLUE}Tool:${NC}    https://github.com/han0110/halo2-kzg-srs"
echo ""

# Create data directory if it doesn't exist
mkdir -p "$DATA_DIR"

# Check if file already exists
if [ -f "$DATA_DIR/$SRS_FILE" ]; then
    echo -e "${YELLOW}⚠️  File already exists: $DATA_DIR/$SRS_FILE${NC}"

    # Verify size
    ACTUAL_SIZE=$(stat -f%z "$DATA_DIR/$SRS_FILE" 2>/dev/null || stat -c%s "$DATA_DIR/$SRS_FILE" 2>/dev/null)
    if [ "$ACTUAL_SIZE" -gt "$MIN_SIZE" ]; then
        echo -e "${GREEN}✅ File already exists and appears valid!${NC}"
        echo -e "${GREEN}Size: $ACTUAL_SIZE bytes (~$(($ACTUAL_SIZE / 1048576)) MB)${NC}"
        echo ""
        echo -e "${GREEN}No download needed. You're all set!${NC}"
        echo ""
        echo -e "${BLUE}To re-download, delete the file first:${NC}"
        echo -e "  ${YELLOW}rm $DATA_DIR/$SRS_FILE${NC}"
        exit 0
    else
        echo -e "${RED}❌ Existing file is too small ($ACTUAL_SIZE bytes)${NC}"
        echo -e "${YELLOW}Removing corrupted file and re-downloading...${NC}"
        rm "$DATA_DIR/$SRS_FILE"
    fi
fi

# Download the file
echo -e "${BLUE}Downloading pre-converted trusted setup...${NC}"
echo -e "${BLUE}Source: $SRS_URL${NC}"
echo -e "${BLUE}Size: ~33 MB${NC}"
echo ""

# Check if wget or curl is available
if command -v wget &> /dev/null; then
    echo -e "${BLUE}Using wget to download...${NC}"
    wget --show-progress -O "$DATA_DIR/$SRS_FILE" "$SRS_URL"
elif command -v curl &> /dev/null; then
    echo -e "${BLUE}Using curl to download...${NC}"
    curl -L --progress-bar -o "$DATA_DIR/$SRS_FILE" "$SRS_URL"
else
    echo -e "${RED}❌ Error: Neither wget nor curl is installed!${NC}"
    echo -e "${YELLOW}Please install wget or curl and try again.${NC}"
    exit 1
fi

echo ""
echo -e "${GREEN}✅ Download complete!${NC}"
echo ""

# Verify file size
echo -e "${BLUE}Verifying file size...${NC}"
ACTUAL_SIZE=$(stat -f%z "$DATA_DIR/$SRS_FILE" 2>/dev/null || stat -c%s "$DATA_DIR/$SRS_FILE" 2>/dev/null)

if [ "$ACTUAL_SIZE" -lt "$MIN_SIZE" ]; then
    echo -e "${RED}❌ Size verification failed!${NC}"
    echo -e "${RED}Expected: At least $MIN_SIZE bytes${NC}"
    echo -e "${RED}Got:      $ACTUAL_SIZE bytes${NC}"
    echo -e "${YELLOW}The download may be corrupted. Please try again.${NC}"
    rm "$DATA_DIR/$SRS_FILE"
    exit 1
fi

echo -e "${GREEN}✅ Size verified: $ACTUAL_SIZE bytes (~$(($ACTUAL_SIZE / 1048576)) MB)${NC}"
echo ""

# Success message
echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║  ✅ Trusted Setup Downloaded Successfully!                 ║${NC}"
echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${BLUE}File Details:${NC}"
echo -e "  Location: $DATA_DIR/$SRS_FILE"
echo -e "  Size:     $ACTUAL_SIZE bytes (~$(($ACTUAL_SIZE / 1048576)) MB)"
echo -e "  Format:   Halo2 KZG parameters (raw format)"
echo ""
echo -e "${BLUE}About this trusted setup:${NC}"
echo -e "  Source:       Hermez/Polygon Powers of Tau ceremony"
echo -e "  Curve:        BN254 (alt_bn128)"
echo -e "  Max Degree:   2^18 = 262,144 constraints"
echo -e "  Participants: 100+ independent contributors"
echo -e "  Converted by: halo2-kzg-srs tool"
echo ""
echo -e "${YELLOW}⚠️  Security Note:${NC}"
echo -e "  This trusted setup is from a multi-party ceremony with 100+ participants."
echo -e "  It is considered secure as long as at least ONE participant destroyed"
echo -e "  their secret randomness (\"toxic waste\")."
echo ""
echo -e "${GREEN}Next Steps:${NC}"
echo -e "  1. Run tests to ensure everything works:"
echo -e "     ${BLUE}cargo test --lib${NC}"
echo ""
echo -e "  2. Generate proofs using the trusted setup:"
echo -e "     ${BLUE}cargo run --release --example generate_verifier${NC}"
echo ""
echo -e "  3. Run end-to-end tests:"
echo -e "     ${BLUE}cargo test --test e2e_test${NC}"
echo ""
echo -e "${GREEN}You're all set! 🎉${NC}"

