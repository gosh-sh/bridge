#!/bin/bash

# Download Trusted Setup for Deposit Prover
# This script downloads the trusted setup parameters from Perpetual Powers of Tau ceremony

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
TRUSTED_SETUP_DIR="trusted_setup"
PTAU_FILE="powersOfTau28_hez_final_18.ptau"
PTAU_URL="https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau"
EXPECTED_SIZE=302072984  # ~288 MB
EXPECTED_SHA256="e970efa7774da80101e0ac336d083ef3339855c98112539338d706b2b89ac694"

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Downloading Trusted Setup for Deposit Prover             ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Create trusted_setup directory if it doesn't exist
mkdir -p "$TRUSTED_SETUP_DIR"

# Check if file already exists
if [ -f "$TRUSTED_SETUP_DIR/$PTAU_FILE" ]; then
    echo -e "${YELLOW}⚠️  File already exists: $TRUSTED_SETUP_DIR/$PTAU_FILE${NC}"
    echo -e "${YELLOW}Verifying existing file...${NC}"
    
    # Verify size
    ACTUAL_SIZE=$(stat -f%z "$TRUSTED_SETUP_DIR/$PTAU_FILE" 2>/dev/null || stat -c%s "$TRUSTED_SETUP_DIR/$PTAU_FILE" 2>/dev/null)
    if [ "$ACTUAL_SIZE" != "$EXPECTED_SIZE" ]; then
        echo -e "${RED}❌ Size mismatch! Expected: $EXPECTED_SIZE, Got: $ACTUAL_SIZE${NC}"
        echo -e "${YELLOW}Removing corrupted file and re-downloading...${NC}"
        rm "$TRUSTED_SETUP_DIR/$PTAU_FILE"
    else
        # Verify checksum
        echo -e "${BLUE}Calculating SHA256 checksum...${NC}"
        ACTUAL_SHA256=$(sha256sum "$TRUSTED_SETUP_DIR/$PTAU_FILE" | awk '{print $1}')
        
        if [ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ]; then
            echo -e "${GREEN}✅ File already exists and is valid!${NC}"
            echo -e "${GREEN}SHA256: $ACTUAL_SHA256${NC}"
            echo ""
            echo -e "${GREEN}No download needed. You're all set!${NC}"
            exit 0
        else
            echo -e "${RED}❌ Checksum mismatch!${NC}"
            echo -e "${RED}Expected: $EXPECTED_SHA256${NC}"
            echo -e "${RED}Got:      $ACTUAL_SHA256${NC}"
            echo -e "${YELLOW}Removing corrupted file and re-downloading...${NC}"
            rm "$TRUSTED_SETUP_DIR/$PTAU_FILE"
        fi
    fi
fi

# Download the file
echo -e "${BLUE}Downloading trusted setup from Perpetual Powers of Tau...${NC}"
echo -e "${BLUE}Source: $PTAU_URL${NC}"
echo -e "${BLUE}Size: ~288 MB${NC}"
echo ""

# Check if wget or curl is available
if command -v wget &> /dev/null; then
    echo -e "${BLUE}Using wget to download...${NC}"
    wget --show-progress -O "$TRUSTED_SETUP_DIR/$PTAU_FILE" "$PTAU_URL"
elif command -v curl &> /dev/null; then
    echo -e "${BLUE}Using curl to download...${NC}"
    curl -L --progress-bar -o "$TRUSTED_SETUP_DIR/$PTAU_FILE" "$PTAU_URL"
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
ACTUAL_SIZE=$(stat -f%z "$TRUSTED_SETUP_DIR/$PTAU_FILE" 2>/dev/null || stat -c%s "$TRUSTED_SETUP_DIR/$PTAU_FILE" 2>/dev/null)

if [ "$ACTUAL_SIZE" != "$EXPECTED_SIZE" ]; then
    echo -e "${RED}❌ Size verification failed!${NC}"
    echo -e "${RED}Expected: $EXPECTED_SIZE bytes${NC}"
    echo -e "${RED}Got:      $ACTUAL_SIZE bytes${NC}"
    echo -e "${YELLOW}The download may be corrupted. Please try again.${NC}"
    exit 1
fi

echo -e "${GREEN}✅ Size verified: $ACTUAL_SIZE bytes (~288 MB)${NC}"
echo ""

# Verify SHA256 checksum
echo -e "${BLUE}Calculating SHA256 checksum (this may take a minute)...${NC}"
ACTUAL_SHA256=$(sha256sum "$TRUSTED_SETUP_DIR/$PTAU_FILE" | awk '{print $1}')

if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
    echo -e "${RED}❌ Checksum verification failed!${NC}"
    echo -e "${RED}Expected: $EXPECTED_SHA256${NC}"
    echo -e "${RED}Got:      $ACTUAL_SHA256${NC}"
    echo -e "${YELLOW}The download may be corrupted. Please try again.${NC}"
    exit 1
fi

echo -e "${GREEN}✅ Checksum verified: $ACTUAL_SHA256${NC}"
echo ""

# Success message
echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║  ✅ Trusted Setup Downloaded Successfully!                 ║${NC}"
echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${BLUE}File Details:${NC}"
echo -e "  Location: $TRUSTED_SETUP_DIR/$PTAU_FILE"
echo -e "  Size:     $ACTUAL_SIZE bytes (~288 MB)"
echo -e "  SHA256:   $ACTUAL_SHA256"
echo ""
echo -e "${BLUE}About this trusted setup:${NC}"
echo -e "  Source:       Perpetual Powers of Tau (Hermez/Polygon)"
echo -e "  Curve:        BN254 (alt_bn128)"
echo -e "  Max Degree:   2^18 = 262,144 constraints"
echo -e "  Participants: 100+ independent contributors"
echo -e "  Date:         November 22, 2023"
echo ""
echo -e "${YELLOW}⚠️  Security Note:${NC}"
echo -e "  This trusted setup is from a multi-party ceremony with 100+ participants."
echo -e "  It is considered secure as long as at least ONE participant destroyed"
echo -e "  their secret randomness (\"toxic waste\")."
echo ""
echo -e "${GREEN}Next Steps:${NC}"
echo -e "  1. Verify the download (optional):"
echo -e "     ${BLUE}cargo run --release --example verify_trusted_setup${NC}"
echo ""
echo -e "  2. Run tests to ensure everything works:"
echo -e "     ${BLUE}cargo test --lib${NC}"
echo ""
echo -e "  3. Generate proofs using the trusted setup:"
echo -e "     ${BLUE}cargo run --release --example generate_verifier${NC}"
echo ""
echo -e "${GREEN}You're all set! 🎉${NC}"

