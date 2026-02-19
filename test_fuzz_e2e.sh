#!/bin/bash
# Fuzz End-to-End Test Script for Acki Nacki Bridge
#
# This script tests the FULL proof generation pipeline with multiple deposits
# of varying amounts, using the Acki Nacki API (Halo2 circuit + Groth16 wrapper).
#
# For each iteration:
#   1. Deposit a random amount of ETH to the bridge
#   2. Fetch deposit event data & MPT proof from Ethereum (fetch_deposit_data)
#   3. Test circuit with MockProver (Acki Nacki Halo2 API)
#   4. Generate Halo2 SNARK proof (Acki Nacki Halo2 API)
#   5. Export Halo2 proof → gnark JSON
#   6. Generate Groth16 wrapper proof
#   7. Set block hash on oracle
#   8. Withdraw and verify on-chain
#
# Prerequisites:
#   - Successful positive E2E test (test_e2e.sh) with existing deployment
#   - Groth16 keys must exist (proving.key, verification.key, circuit.r1cs)
#   - KZG trusted setup parameters in deposit-prover/data/
#
# Usage:
#   NUM_ITERATIONS=3 ./test_fuzz_e2e.sh           # default: 3 iterations
#   NUM_ITERATIONS=5 MOCK_ONLY=true ./test_fuzz_e2e.sh   # MockProver only, no on-chain withdrawal

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACTS_DIR="$SCRIPT_DIR/contracts/ethereum"
DEPOSIT_PROVER_DIR="$SCRIPT_DIR/deposit-prover"
GNARK_DIR="$DEPOSIT_PROVER_DIR/gnark-wrapper"
DATA_DIR="$SCRIPT_DIR/e2e_test_data"

NUM_ITERATIONS="${NUM_ITERATIONS:-3}"
MOCK_ONLY="${MOCK_ONLY:-false}"

# Deposit amounts in wei — vary across iterations to test different values
# These are intentionally small to conserve testnet ETH
AMOUNTS=(
    "50000000000000"    # 0.00005 ETH
    "100000000000000"   # 0.0001  ETH
    "200000000000000"   # 0.0002  ETH
    "77777000000000"    # 0.000077777 ETH (odd amount)
    "1000000000000"     # 0.000001 ETH (tiny)
    "500000000000000"   # 0.0005 ETH
    "123456789012345"   # 0.000123... ETH (arbitrary)
    "999000000000000"   # 0.000999 ETH
)

# Load environment
source "$CONTRACTS_DIR/.env"

# ── Helpers ──────────────────────────────────────────────────

fail() { echo -e "${RED}FAIL: $1${NC}"; exit 1; }
info() { echo -e "${YELLOW}$1${NC}"; }
ok()   { echo -e "${GREEN}✓ $1${NC}"; }

# ── Validate prerequisites ───────────────────────────────────

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║  Acki Nacki Bridge — Fuzz E2E Tests (Acki Nacki API)      ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

[ -f "$DATA_DIR/deployment.json" ] || fail "No deployment found. Run test_e2e.sh first."

BRIDGE_ADDRESS=$(jq -r '.bridge_address' "$DATA_DIR/deployment.json")
ORACLE_ADDRESS=$(jq -r '.oracle_address' "$DATA_DIR/deployment.json")
VERIFIER_ADDRESS=$(jq -r '.verifier_address' "$DATA_DIR/deployment.json")
ORACLE_TYPE=$(jq -r '.oracle_type // "mock"' "$DATA_DIR/deployment.json")
SENDER_ADDRESS=$(cast wallet address "$PRIVATE_KEY")

echo "Configuration:"
echo "  Iterations: $NUM_ITERATIONS"
echo "  Mock-only:  $MOCK_ONLY"
echo "  Bridge:     $BRIDGE_ADDRESS"
echo "  Oracle:     $ORACLE_ADDRESS ($ORACLE_TYPE)"
echo "  Verifier:   $VERIFIER_ADDRESS"
echo "  Sender:     $SENDER_ADDRESS"
echo "  Balance:    $(cast balance "$SENDER_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" --ether) ETH"
echo ""

# Verify Groth16 keys exist
for f in "$GNARK_DIR/proving.key" "$GNARK_DIR/verification.key" "$GNARK_DIR/circuit.r1cs"; do
    [ -f "$f" ] || fail "Groth16 key not found: $f — run test_e2e.sh first"
done
ok "Groth16 keys present"

echo ""

# ── Fuzz loop ────────────────────────────────────────────────

PASSED=0
FAILED=0
TOTAL_START=$(date +%s)

for ((i = 1; i <= NUM_ITERATIONS; i++)); do
    # Pick amount (cycle through AMOUNTS array)
    IDX=$(( (i - 1) % ${#AMOUNTS[@]} ))
    AMOUNT="${AMOUNTS[$IDX]}"
    AMOUNT_ETH=$(python3 -c "print(f'{int(\"$AMOUNT\")/1e18:.10f}')")

    echo -e "${BLUE}╔══════════════════════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║  Iteration $i/$NUM_ITERATIONS — Deposit $AMOUNT_ETH ETH${NC}"
    echo -e "${BLUE}╚══════════════════════════════════════════════════════════╝${NC}"
    echo ""

    ITER_DIR="$DATA_DIR/fuzz_iter_$i"
    mkdir -p "$ITER_DIR"
    ITER_START=$(date +%s)

    # ── Step 1: Deposit ──────────────────────────────────────

    info "[1/8] Depositing $AMOUNT_ETH ETH to bridge..."
    cd "$CONTRACTS_DIR"

    DEPOSIT_TX=$(cast send "$BRIDGE_ADDRESS" "deposit()" \
        --value "$AMOUNT" \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --json)

    DEPOSIT_TX_HASH=$(echo "$DEPOSIT_TX" | jq -r '.transactionHash')
    [ -n "$DEPOSIT_TX_HASH" ] && [ "$DEPOSIT_TX_HASH" != "null" ] || fail "Deposit TX failed"

    sleep 5
    RECEIPT=$(cast receipt "$DEPOSIT_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
    DEPOSIT_STATUS=$(echo "$RECEIPT" | jq -r '.status')
    [ "$DEPOSIT_STATUS" = "0x1" ] || fail "Deposit reverted (status=$DEPOSIT_STATUS)"

    BLOCK_NUMBER=$(echo "$RECEIPT" | jq -r '.blockNumber')
    TX_INDEX=$(echo "$RECEIPT" | jq -r '.transactionIndex')
    DEPOSIT_ID=$(echo "$RECEIPT" | jq -r '.logs[0].topics[1]' | xargs printf "%d")

    ok "Deposit confirmed — ID=$DEPOSIT_ID  Block=$BLOCK_NUMBER  TX=$DEPOSIT_TX_HASH"

    # Save deposit info
    cat > "$ITER_DIR/deposit_info.json" <<EOF
{
  "tx_hash": "$DEPOSIT_TX_HASH",
  "block_number": "$BLOCK_NUMBER",
  "tx_index": "$TX_INDEX",
  "deposit_id": $DEPOSIT_ID,
  "amount": "$AMOUNT",
  "bridge_address": "$BRIDGE_ADDRESS"
}
EOF
    echo ""

    # ── Step 2: Fetch deposit data (Acki Nacki API) ──────────

    info "[2/8] Fetching deposit event data & MPT proof..."
    cd "$DEPOSIT_PROVER_DIR"

    cargo run --release --example fetch_deposit_data -- \
        --tx-hash "$DEPOSIT_TX_HASH" \
        --contract "$BRIDGE_ADDRESS" \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --output "$ITER_DIR/deposit_proof_input.json"

    [ -f "$ITER_DIR/deposit_proof_input.json" ] || fail "fetch_deposit_data failed"
    ok "Event data & MPT proof fetched"
    echo ""

    # ── Step 3: MockProver test (Acki Nacki Halo2 API) ───────

    info "[3/8] Testing circuit with MockProver (Acki Nacki Halo2 API)..."
    cargo run --release --example test_with_real_data -- \
        --input "$ITER_DIR/deposit_proof_input.json" \
        --mock-only \
        --max-data-byte-len 2048

    ok "MockProver PASSED — circuit satisfied with deposit amount=$AMOUNT_ETH ETH"
    echo ""

    if [ "$MOCK_ONLY" = "true" ]; then
        ok "Iteration $i PASSED (mock-only mode)"
        PASSED=$((PASSED + 1))
        ITER_END=$(date +%s)
        echo "  Duration: $((ITER_END - ITER_START))s"
        echo ""
        continue
    fi

    # ── Step 4: Generate Halo2 SNARK proof (Acki Nacki API) ──

    info "[4/8] Generating Halo2 SNARK proof..."

    # Delete stale snark to force regeneration
    rm -f "$DEPOSIT_PROVER_DIR/data/deposit_proof_${DEPOSIT_ID}.snark"

    cargo run --release --example test_with_real_data -- \
        --input "$ITER_DIR/deposit_proof_input.json" \
        --generate-proof \
        --output "$ITER_DIR/deposit_proof_output.json" \
        --max-data-byte-len 2048

    [ -f "$ITER_DIR/deposit_proof_output.json" ] || fail "Halo2 proof generation failed"
    ok "Halo2 SNARK proof generated"
    echo ""

    # ── Step 5: Export Halo2 proof → gnark JSON ──────────────

    info "[5/8] Exporting Halo2 proof for gnark..."
    SNARK_FILE="data/deposit_proof_${DEPOSIT_ID}.snark"
    [ -f "$SNARK_FILE" ] || fail "SNARK file not found: $SNARK_FILE"

    cargo run --release --example export_proof_for_gnark -- \
        --input "$SNARK_FILE" \
        --output "$GNARK_DIR/halo2_proof.json"

    ok "Halo2 proof exported for gnark"
    echo ""

    # ── Step 6: Generate Groth16 wrapper proof ───────────────

    info "[6/8] Generating Groth16 wrapper proof..."
    cd "$GNARK_DIR"
    go run . prove halo2_proof.json

    [ -f "groth16_proof_bytes.hex" ] || fail "Groth16 proof generation failed"
    PROOF_BYTES=$(cat groth16_proof_bytes.hex)
    # Save a copy for this iteration
    cp groth16_proof_bytes.hex "$ITER_DIR/groth16_proof_bytes.hex"
    ok "Groth16 proof generated (288 bytes)"
    echo ""

    # ── Step 7: Set block hash on oracle ─────────────────────

    info "[7/8] Setting block hash on oracle..."
    cd "$CONTRACTS_DIR"

    EXEC_BLOCK_HASH=$(python3 -c "
import json
with open('$ITER_DIR/deposit_proof_output.json') as f:
    d = json.load(f)
print('0x' + bytes(d['block_hash']).hex())
")
    BLOCK_NUMBER_DEC=$(python3 -c "print(int('$BLOCK_NUMBER', 16))")

    if [ "$ORACLE_TYPE" = "mock" ]; then
        cast send "$ORACLE_ADDRESS" \
            "setBlockHash(uint256,bytes32)" \
            "$BLOCK_NUMBER_DEC" "$EXEC_BLOCK_HASH" \
            --rpc-url "$SEPOLIA_RPC_URL" \
            --private-key "$PRIVATE_KEY" 2>&1
        ok "Block hash set on mock oracle"
    else
        CURRENT_BLOCK=$(cast block-number --rpc-url "$SEPOLIA_RPC_URL")
        BLOCK_AGE=$((CURRENT_BLOCK - BLOCK_NUMBER_DEC))
        [ "$BLOCK_AGE" -lt 256 ] || fail "Block too old for Axiom ($BLOCK_AGE >= 256)"
        ok "Block is recent ($BLOCK_AGE < 256) — Axiom will use blockhash()"
    fi
    echo ""

    # ── Step 8: Withdraw ─────────────────────────────────────

    info "[8/8] Submitting withdrawal..."

    WITHDRAW_TX=$(cast send "$BRIDGE_ADDRESS" \
        "withdraw(address,uint256,uint256,uint256,bytes)" \
        "$SENDER_ADDRESS" "$AMOUNT" "$DEPOSIT_ID" "$BLOCK_NUMBER" "$PROOF_BYTES" \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$PRIVATE_KEY" \
        --json)

    WITHDRAW_TX_HASH=$(echo "$WITHDRAW_TX" | jq -r '.transactionHash')
    [ -n "$WITHDRAW_TX_HASH" ] && [ "$WITHDRAW_TX_HASH" != "null" ] || fail "Withdrawal TX failed"

    sleep 8
    WITHDRAW_RECEIPT=$(cast receipt "$WITHDRAW_TX_HASH" --rpc-url "$SEPOLIA_RPC_URL" --json)
    WITHDRAW_STATUS=$(echo "$WITHDRAW_RECEIPT" | jq -r '.status')

    if [ "$WITHDRAW_STATUS" = "0x1" ]; then
        ok "Withdrawal succeeded — TX=$WITHDRAW_TX_HASH"
        PASSED=$((PASSED + 1))
    else
        echo -e "${RED}✗ Withdrawal FAILED for iteration $i (depositId=$DEPOSIT_ID amount=$AMOUNT_ETH)${NC}"
        FAILED=$((FAILED + 1))
    fi

    # Save result
    cat > "$ITER_DIR/result.json" <<EOF
{
  "iteration": $i,
  "deposit_id": $DEPOSIT_ID,
  "amount": "$AMOUNT",
  "amount_eth": "$AMOUNT_ETH",
  "deposit_tx": "$DEPOSIT_TX_HASH",
  "withdraw_tx": "$WITHDRAW_TX_HASH",
  "status": "$WITHDRAW_STATUS",
  "block_number": "$BLOCK_NUMBER"
}
EOF

    ITER_END=$(date +%s)
    echo "  Duration: $((ITER_END - ITER_START))s"
    echo ""
done

# ── Summary ──────────────────────────────────────────────────

TOTAL_END=$(date +%s)
TOTAL_TIME=$((TOTAL_END - TOTAL_START))

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}║  Fuzz E2E Tests: $PASSED/$NUM_ITERATIONS PASSED ✓                          ║${NC}"
else
    echo -e "${RED}║  Fuzz E2E Tests: $PASSED passed, $FAILED FAILED ✗                   ║${NC}"
fi
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""
echo "Total time: ${TOTAL_TIME}s (~$((TOTAL_TIME / 60))m $((TOTAL_TIME % 60))s)"
echo "Balance:    $(cast balance "$SENDER_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" --ether) ETH"
echo ""

echo "Results per iteration:"
for ((i = 1; i <= NUM_ITERATIONS; i++)); do
    ITER_DIR="$DATA_DIR/fuzz_iter_$i"
    if [ -f "$ITER_DIR/result.json" ]; then
        AMT=$(jq -r '.amount_eth' "$ITER_DIR/result.json")
        DID=$(jq -r '.deposit_id' "$ITER_DIR/result.json")
        ST=$(jq -r '.status' "$ITER_DIR/result.json")
        if [ "$ST" = "0x1" ]; then
            echo -e "  [$i] depositId=$DID  amount=${AMT} ETH  ${GREEN}PASS${NC}"
        else
            echo -e "  [$i] depositId=$DID  amount=${AMT} ETH  ${RED}FAIL${NC}"
        fi
    elif [ "$MOCK_ONLY" = "true" ]; then
        echo -e "  [$i] MockProver only  ${GREEN}PASS${NC}"
    fi
done
echo ""

[ $FAILED -eq 0 ] && exit 0 || exit 1

