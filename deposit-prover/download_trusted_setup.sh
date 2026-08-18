#!/bin/bash

# Trusted Setup helper for the Deposit Prover (Halo2 KZG, BN254).
#
# ┌──────────────────────────────────────────────────────────────────────────┐
# │  THERE ARE TWO DIFFERENT KZG CEREMONIES IN PLAY — DO NOT CONFUSE THEM.    │
# ├──────────────────────────────────────────────────────────────────────────┤
# │                                                                          │
# │  1. CHAIN ceremony  →  params/kzg_bn254_{k}.srs   (OPCODE-ALIGNED)        │
# │     This is the ONLY SRS whose [s]·G2 matches the points the Acki Nacki   │
# │     `ZKHALO2VERIFYWITHVK` opcode rebuilds its verifier from. Deposit      │
# │     proofs / VkBlobs MUST be keyed on this ceremony or the AN opcode      │
# │     REJECTS them. `deposit-prover/src/prover.rs` loads it FIRST.          │
# │     It is the AN node's own ceremony (kzg_bn254_19.srs) — obtained from   │
# │     the partner / node, then downsized to k=18 for the deposit circuit    │
# │     via `cargo run --release --example downsize_srs`. There is no public  │
# │     download URL; this script CANNOT fetch it.                            │
# │                                                                          │
# │  2. HERMEZ ceremony →  data/kzg_params_{k}.srs    (TEST / FALLBACK ONLY)  │
# │     The standard Hermez/Polygon Powers of Tau (snarkjs/iden3/zkEVM).      │
# │     This script downloads it. It is fine for local circuit testing and    │
# │     as a degree fallback (e.g. k=20), but proofs keyed on it are          │
# │     REJECTED by the AN opcode (different tau / [s]·G2). It is NOT the      │
# │     production deposit setup.                                             │
# │                                                                          │
# └──────────────────────────────────────────────────────────────────────────┘
#
# Verified empirically (2026-06-26): Hermez k=20 (SHA-256 80394564…) ends in
# s_g2 = 928fafb3…, whereas the opcode's embedded KZG_S_G2_BYTES = c6028acf…
# (== the chain `params/kzg_bn254_*.srs` files). Same g1/g2 generators, DIFFERENT
# tau. See `examples/downsize_srs.rs` and `docs/archive/deposit_vk_witness_independence.md`.

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
DATA_DIR="data"                       # Hermez (test/fallback) SRS lives here
PARAMS_DIR="params"                   # Chain (opcode-aligned) SRS lives here
SRS_FILE="kzg_params_18.srs"          # Hermez k=18 output
SRS_URL="https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-18"
MIN_SIZE=30000000  # At least 30 MB
CHAIN_SRS="$PARAMS_DIR/kzg_bn254_18.srs"  # opcode-aligned k=18 deposit SRS

# The opcode's embedded [s]·G2 (chain ceremony) — first/last 6 bytes.
# Source of truth: tvm-sdk/tvm_vm/src/executor/zk_halo2_utils.rs::KZG_S_G2_BYTES.
CHAIN_SG2_HEAD="c6028acf4420"
CHAIN_SG2_TAIL="7397d6664515"

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Deposit Prover — Trusted Setup helper (KZG / BN254)      ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

# ---------------------------------------------------------------------------
# Step 0: report the opcode-aligned CHAIN SRS status (this is what matters for
# producing AN-acceptable deposit proofs). Self-check its [s]·G2 if present.
# ---------------------------------------------------------------------------
echo -e "${BLUE}── Opcode-aligned CHAIN SRS (${CHAIN_SRS}) ──${NC}"
if [ -f "$CHAIN_SRS" ]; then
    # In the halo2 raw SRS format, [s]·G2 is always the final 128 bytes;
    # compare its first/last 6 bytes against the opcode's embedded constant.
    SG2_HEAD=$(tail -c 128 "$CHAIN_SRS" | head -c 6 | od -An -tx1 | tr -d ' \n')
    SG2_TAIL=$(tail -c 6 "$CHAIN_SRS" | od -An -tx1 | tr -d ' \n')
    if [ "$SG2_HEAD" = "$CHAIN_SG2_HEAD" ] && [ "$SG2_TAIL" = "$CHAIN_SG2_TAIL" ]; then
        echo -e "${GREEN}✅ Present and OPCODE-ALIGNED (s_g2 = ${SG2_HEAD}…${SG2_TAIL}).${NC}"
        echo -e "${GREEN}   Deposit proofs keyed on this SRS will be accepted by ZKHALO2VERIFYWITHVK.${NC}"
    else
        echo -e "${RED}❌ Present but s_g2 = ${SG2_HEAD}…${SG2_TAIL} does NOT match the opcode${NC}"
        echo -e "${RED}   (expected ${CHAIN_SG2_HEAD}…${CHAIN_SG2_TAIL}). This SRS is the WRONG ceremony —${NC}"
        echo -e "${RED}   regenerate it from the chain kzg_bn254_19.srs via downsize_srs.${NC}"
    fi
else
    echo -e "${YELLOW}⚠️  Not present.${NC} The AN opcode will REJECT any deposit proof unless this"
    echo -e "    file exists and is opcode-aligned. To obtain it:"
    echo -e "      1. Get the chain ceremony SRS ${BLUE}kzg_bn254_19.srs${NC} from the AN node/partner"
    echo -e "         (it is NOT downloadable here — same ceremony the opcode embeds)."
    echo -e "      2. Downsize it to the deposit circuit's k=18 (tau-preserving):"
    echo -e "         ${BLUE}cargo run --release --example downsize_srs -- --input kzg_bn254_19.srs --output $CHAIN_SRS --k 18${NC}"
fi
echo ""

# ---------------------------------------------------------------------------
# Step 1: Hermez (test/fallback) SRS download.
# ---------------------------------------------------------------------------
echo -e "${BLUE}── Hermez TEST/FALLBACK SRS (${DATA_DIR}/${SRS_FILE}) ──${NC}"
echo -e "${YELLOW}NOTE: this is NOT the production deposit setup.${NC} Proofs keyed on it are"
echo -e "      ${YELLOW}rejected by the AN ZKHALO2VERIFYWITHVK opcode.${NC} It is useful only for"
echo -e "      local circuit testing and as a non-opcode degree fallback."
echo ""

# Create data directory if it doesn't exist
mkdir -p "$DATA_DIR"

# Check if file already exists
if [ -f "$DATA_DIR/$SRS_FILE" ]; then
    ACTUAL_SIZE=$(stat -f%z "$DATA_DIR/$SRS_FILE" 2>/dev/null || stat -c%s "$DATA_DIR/$SRS_FILE" 2>/dev/null)
    if [ "$ACTUAL_SIZE" -gt "$MIN_SIZE" ]; then
        echo -e "${GREEN}✅ Hermez fallback SRS already present (${ACTUAL_SIZE} bytes, ~$(($ACTUAL_SIZE / 1048576)) MB).${NC}"
        echo -e "   To re-download, delete it first: ${YELLOW}rm $DATA_DIR/$SRS_FILE${NC}"
        exit 0
    else
        echo -e "${RED}❌ Existing file is too small ($ACTUAL_SIZE bytes) — removing and re-downloading...${NC}"
        rm "$DATA_DIR/$SRS_FILE"
    fi
fi

# Download the file
echo -e "${BLUE}Downloading Hermez/Polygon Powers of Tau (k=18, ~33 MB)...${NC}"
echo -e "${BLUE}Source: $SRS_URL${NC}"
echo ""

if command -v wget &> /dev/null; then
    wget --show-progress -O "$DATA_DIR/$SRS_FILE" "$SRS_URL"
elif command -v curl &> /dev/null; then
    curl -L --progress-bar -o "$DATA_DIR/$SRS_FILE" "$SRS_URL"
else
    echo -e "${RED}❌ Error: Neither wget nor curl is installed!${NC}"
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

echo -e "${GREEN}✅ Hermez fallback SRS downloaded (${ACTUAL_SIZE} bytes, ~$(($ACTUAL_SIZE / 1048576)) MB).${NC}"
echo ""
echo -e "${YELLOW}Reminder:${NC} for AN-acceptable deposit proofs you still need the opcode-aligned"
echo -e "          CHAIN SRS at ${BLUE}${CHAIN_SRS}${NC} (see Step 0 above)."
