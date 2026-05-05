# Verifying an Ethereum Proof on the Acki Nacki Side

This document is the operational guide for **verifying that an Ethereum-side proof of a `Deposit` event is correct** before the Acki Nacki side credits the corresponding tokens to the user.

It is the mirror image of `docs/verifying_an_proof.md` (which covers AN → ETH proofs). The deposit flow is the **other** direction:

```
[ Ethereum ] AckiNackiBridge.deposit() → Deposit event
                  │
                  ▼
       deposit-prover (Halo2, K=20)
                  │
                  ▼
       gnark-wrapper (Groth16 on BN254)
                  │
                  ▼
[ Acki Nacki ] verifies → mints / credits user
                  │
                  ▼
       nullifier tracked to prevent replay
```

The doc serves three audiences:

- **Acki Nacki BK / verifier node**: you receive a proof from a relayer and must decide whether to credit the user.
- **Auditor**: you want to confirm a published proof would have been accepted.
- **Relayer (producer)**: you generate the proof and want to confirm it before submitting.

---

## 0. State of Implementation (Important)

The deposit-side verifier is **not yet deployed on Acki Nacki** as of this writing. The on-chain TVM Groth16 verifier is part of remaining work (`integration_plan.md` M7–M9). Until then, deposit verification on the AN side runs **off-chain** through the producer pipeline plus a BK consensus check.

This doc therefore covers:

- **What exists today**: off-chain verification paths (Halo2 + gnark + RPC cross-checks).
- **What is planned**: on-chain TVM Groth16 verification + AN-side nullifier mapping + canonical-Ethereum oracle.

The structure is identical to the AN→ETH doc; the differences are flagged with **[planned]** vs **[available today]**.

---

## 1. What Each Proof Asserts

A single deposit proof commits to **7 BN254 Fr public inputs** packed into a 288-byte payload:

| # | Field | Type | Source |
|---|---|---|---|
| 0 | `depositId` | uint256 | Indexed value from `Deposit(depositId, sender, amount, timestamp)` |
| 1 | `sender` | uint256 (uint160 cast) | `msg.sender` of `deposit()` |
| 2 | `amount` | uint256 | `msg.value` of `deposit()` |
| 3 | `contractAddress` | uint256 (uint160 cast) | The bridge contract address itself |
| 4 | `blockHashHigh` | uint128 | Upper 128 bits of `blockhash(blockNumber)` |
| 5 | `blockHashLow` | uint128 | Lower 128 bits of `blockhash(blockNumber)` |
| 6 | `promise_commit` | uint256 | Keccak coprocessor commitment (Poseidon) |

**Payload format** (288 bytes total):
- Bytes 0..256: 8 × uint256 Groth16 proof points (A, B, C in EIP-197 layout).
- Bytes 256..288: `promise_commit` as the 7th public input.

A valid proof certifies, given the witness, that:

1. The block whose keccak256 hash equals `blockHashHigh ‖ blockHashLow` was the producer of the receipt referenced.
2. A `Receipt` exists in that block's receipts trie at some transaction index (Merkle Patricia Trie inclusion).
3. Inside the receipt's logs, there is a `Log` entry whose:
   - `address == contractAddress` (the bridge);
   - `topics[0] == keccak256("Deposit(uint256,address,uint256,uint256)")`;
   - `topics[1] == bytes32(depositId)`, `topics[2] == bytes32(uint256(uint160(sender)))`;
   - `data` decodes to `(amount, timestamp)`.
4. All keccak256 calls inside the circuit are bound to `promise_commit` via the Poseidon coprocessor pattern (~500× constraint savings; see `docs/keccak_coprocessor_flowchart.mmd`).

**It does not certify**:
- That `blockHashHigh ‖ blockHashLow` is the *canonical* Ethereum block hash. That is the verifier's job (V1 below).
- That `depositId` has not already been credited. That is the AN-side nullifier's job (V6 below).

---

## 2. Pipeline Overview

```
Ethereum mainnet
    │  block produced, AckiNackiBridge.deposit() emits event
    ▼
Relayer fetches: receipt + block header + MPT proof
    │  (deposit-prover/examples/fetch_deposit_data.rs against Ethereum RPC)
    ▼
deposit-prover (Rust, axiom-eth halo2 fork, K=20)
    │  ~1-2 min + 5+ GB PK
    ▼
Halo2 proof + 7 instances + protocol metadata
    │  (deposit-prover/src/groth16_wrapper exports JSON)
    ▼
gnark-wrapper (Go, BN254)
    │  → groth16_proof_bytes.hex (256 bytes)
    │  → 7 public inputs
    │  → 32-byte promise_commit (the 7th input separately)
    ▼  Combined: 288-byte payload
[ Acki Nacki side — currently off-chain ]
    │
    ├── V1: Block hash is canonical Ethereum?  →  oracle / RPC quorum
    ├── V2: Public inputs match real event?     →  RPC cross-check
    ├── V3: Halo2 proof valid?                  →  Rust verifier
    ├── V4: Groth16 wrapping consistent?        →  diff of instances
    ├── V5: Groth16 proof valid?                →  gnark verify
    ├── V6: depositId not already used?          →  AN nullifier mapping [planned]
    └── V7: TVM Groth16 verifier on AN          →  [planned, M9]
```

---

## 3. The Seven Verification Stages

| Stage | What you check | Where to run | Time | Status |
|---|---|---|---|---|
| **V1** | `blockHash` is canonical on Ethereum | Ethereum RPC + Axiom (or AN-side oracle) | seconds | **available today** off-chain |
| **V2** | The 6 user-supplied public inputs match the actual Ethereum event | Ethereum RPC | seconds | **available today** |
| **V3** | Halo2 proof valid against published VK | Rust (deposit-prover) | < 1 s | **available today** |
| **V4** | gnark JSON wraps the same instances | `diff` | seconds | **available today** |
| **V5** | Groth16 proof verifies natively | Go (gnark) | < 1 s | **available today** |
| **V6** | `depositId` not previously credited | AN-side nullifier mapping | seconds | **planned** |
| **V7** | TVM contract verifies the proof on-chain | TVM Groth16 verifier | seconds | **planned (M9)** |

A relayer / BK node should run V1–V5 today (and V6 once implemented), conditioning the credit on every check passing.

---

## 4. Artifacts You Receive

When a relayer hands a deposit proof to the AN side, the bundle is:

```
deposit_proof_<txHash>.json:
{
  "deposit_id": "42",
  "sender":     "0xabc...",
  "amount":     "1000000000000000000",     // wei
  "contract":   "0xdef...",                 // mainnet bridge address
  "block_number": 18234567,
  "block_hash": "0x123...",                 // claimed block hash
  "groth16_proof_hex": "0x...",             // 288 bytes (proof + promise_commit)
  "promise_commit":  "0x...",               // also embedded in the last 32 bytes
  "k": 20,                                   // Halo2 circuit degree
  "circuit_version": "v2"                    // matches deposit-prover circuit_v2.rs
}
```

In addition, **once per circuit version**, the project publishes:

- `deposit-prover/gnark-wrapper/verification.key` — gnark VK (size ~700 B).
- `deposit-prover/gnark-wrapper/Groth16Verifier.sol` — auto-generated EVM verifier (used on Ethereum side; same logic must be re-implemented in TVM for V7).
- `deposit-prover/gnark-wrapper/circuit.r1cs` — wrapping circuit constraints.
- `deposit-prover/configs/<config>.json` — Halo2 protocol params (K, columns, etc.).

The producer additionally needs the proving keys (Halo2 PK ~5+ GB, gnark PK ~few KB), which are *not* required by verifiers.

---

## 5. Stage V1 — Block Hash is Canonical Ethereum (≈ 5 min)

**Goal**: Confirm that `blockHash = blockHashHigh ‖ blockHashLow` is the actual canonical block hash for `block_number` on Ethereum mainnet (not a fork, not a spoof, not a stale RPC).

This is the most important verification on the AN side because the rest of the proof is *cryptographically tied* to this hash. If the block hash is wrong, the entire chain of inferences (block → receipts → log → deposit) is meaningless.

### V1.1 Reassemble the block hash from the proof's public inputs

```bash
# Extract from the proof bundle
HIGH=$(jq -r '.public_inputs[4]' deposit_proof_<txHash>.json)
LOW=$(jq -r '.public_inputs[5]' deposit_proof_<txHash>.json)

# Reconstruct: hash = (high << 128) | low
python3 -c "
high = $HIGH
low  = $LOW
print(hex(high << 128 | low))
"
```

✅ Expected: a `0x...64-hex-chars` value.

Compare to `block_hash` in the bundle — they must be identical (the bundle's `block_hash` is just a convenience field).

### V1.2 Cross-check against multiple Ethereum RPCs

The single most important defence against fork attacks is **RPC quorum**: query several independent Ethereum endpoints and confirm they all return the same block hash.

```bash
BLOCK_NUMBER=$(jq -r '.block_number' deposit_proof_<txHash>.json)

for RPC in \
  "https://eth.llamarpc.com" \
  "https://rpc.ankr.com/eth" \
  "https://eth-mainnet.g.alchemy.com/v2/$ALCHEMY_KEY" \
  "https://mainnet.infura.io/v3/$INFURA_KEY"; do
  HASH=$(cast block $BLOCK_NUMBER --rpc-url "$RPC" --json | jq -r .hash)
  echo "$RPC  →  $HASH"
done
```

✅ Expected: all four URLs return the same hash, equal to `block_hash` in the bundle.

If any disagree, **stop and investigate**. Most likely causes:
- RPC out of date / serving a stale fork tip.
- One of the providers is malicious or compromised.
- An actual chain reorganisation occurred (rare for blocks > 12 confirmations old).

For production AN BK nodes, this RPC quorum should be **automated** and configured with at least 3 independent providers, requiring all to agree.

### V1.3 Cross-check via Axiom V2 (cryptographic oracle)

For maximum certainty, query Ethereum mainnet's `AxiomV2Core` directly:

```bash
AXIOM_V2_CORE=0x69963768F8407dE501029680dE46945F838Fc98B
ETH_RPC=https://eth.llamarpc.com

# Note: Axiom requires a witness for blocks > 256 old. For recent blocks,
# it has a recent-block path. For the deposit flow specifically, recent (<= 256)
# is the relevant case (relayers should submit promptly).

cast call $AXIOM_V2_CORE \
  "isRecentBlockHashValid(uint32,bytes32)(bool)" \
  $BLOCK_NUMBER $BLOCK_HASH \
  --rpc-url $ETH_RPC
```

✅ Expected: `true`. This means a SNARK-backed oracle agrees this is the canonical hash.

For older blocks (>256 confirmations), Axiom requires a Merkle witness; see `docs/bridge_verification.md` §7 for that flow. Most deposit relayers operate in the recent-block window to avoid this complication.

### V1.4 [Planned] AN-side oracle

In production, the AN side should run its own canonical-Ethereum oracle, fed by:

- A trusted set of Ethereum full nodes operated by AN BKs.
- Quorum = strict majority (e.g., 3-of-5).
- BFT-finalised result published into TVM storage.

Until that exists, V1 is the BK operator's manual responsibility (V1.2 + V1.3).

### V1.5 What it costs to fail V1

If V1 fails:
- The relayer is either lying about which chain the deposit happened on, or one of your RPCs is compromised.
- **Reject the proof.** A valid Halo2/Groth16 proof committed to a *fork* hash gives no real claim on assets.

---

## 6. Stage V2 — Public Inputs Match the Real Event (≈ 5 min)

**Goal**: Confirm the proof's public inputs (depositId, sender, amount, contractAddress) correspond to a real `Deposit` event in the canonical block from V1.

This is also a defence against the gnark wrapper stub `Define` issue (`integration_plan.md` §6.5): a malicious wrapper could pair a valid Halo2 proof with arbitrary public inputs. V2 binds the inputs to ground truth.

### V2.1 Extract claimed inputs from the proof bundle

```bash
DEPOSIT_ID=$(jq -r '.deposit_id'      deposit_proof_<txHash>.json)
SENDER=$(jq      -r '.sender'         deposit_proof_<txHash>.json)
AMOUNT=$(jq      -r '.amount'         deposit_proof_<txHash>.json)
CONTRACT=$(jq    -r '.contract'       deposit_proof_<txHash>.json)
BLOCK_NUMBER=$(jq -r '.block_number'  deposit_proof_<txHash>.json)
TX_HASH=$(jq     -r '.tx_hash'        deposit_proof_<txHash>.json)
```

### V2.2 Re-fetch the receipt from a trusted Ethereum RPC

```bash
ETH_RPC=https://eth.llamarpc.com

cast tx $TX_HASH --rpc-url $ETH_RPC --json > /tmp/tx.json
cast receipt $TX_HASH --rpc-url $ETH_RPC --json > /tmp/receipt.json

# Confirm the receipt is from the right block
jq -r '.blockNumber' /tmp/receipt.json   # expect: 0x... matching BLOCK_NUMBER hex
jq -r '.blockHash'   /tmp/receipt.json   # expect: same as the canonical hash from V1
```

✅ Expected: both identifiers match.

### V2.3 Decode the Deposit event from the receipt's logs

The `Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp)` event has signature:

```
keccak256("Deposit(uint256,address,uint256,uint256)")
  = 0x...   (run: `cast keccak "Deposit(uint256,address,uint256,uint256)"`)
```

Find the matching log:

```bash
DEPOSIT_TOPIC=$(cast keccak "Deposit(uint256,address,uint256,uint256)")
LOG=$(jq --arg sig "$DEPOSIT_TOPIC" --arg ctr "$CONTRACT" \
  '.logs[] | select(.topics[0] == $sig and .address | ascii_downcase == ($ctr | ascii_downcase))' \
  /tmp/receipt.json)
echo "$LOG"
```

Decode the indexed args and data:

```bash
LOG_DEPOSIT_ID=$(echo $LOG | jq -r '.topics[1]' | xargs cast --to-dec)
LOG_SENDER=$(echo $LOG     | jq -r '.topics[2]' | sed 's/0x0\{24\}/0x/' )
LOG_DATA=$(echo $LOG       | jq -r '.data')
LOG_AMOUNT=$(cast --to-dec ${LOG_DATA:0:66})       # first 32-byte word
# (timestamp is the second word; not part of the proof's public inputs)
```

### V2.4 Compare the four critical fields

```bash
echo "depositId       claimed=$DEPOSIT_ID   on-chain=$LOG_DEPOSIT_ID"
echo "sender          claimed=$SENDER       on-chain=$LOG_SENDER"
echo "amount          claimed=$AMOUNT       on-chain=$LOG_AMOUNT"
echo "contractAddress claimed=$CONTRACT     on-chain=$(jq -r '.address' <<<"$LOG")"
```

✅ Expected: all four pairs match exactly. If any differ, the relayer is lying about which event the proof corresponds to — **reject**.

### V2.5 Confirm the contract is the official AckiNackiBridge

```bash
EXPECTED_BRIDGE=0x...   # the canonical mainnet deployment, published with the integration

[ "${CONTRACT,,}" == "${EXPECTED_BRIDGE,,}" ] && echo "Contract OK" || echo "WRONG BRIDGE"
```

✅ Expected: `Contract OK`. A valid proof for a *different* bridge contract is irrelevant — that other bridge has a different treasury.

---

## 7. Stage V3 — Verify the Halo2 Proof Natively (≈ 1 min)

**Goal**: Confirm the underlying Halo2 SHPLONK proof is sound. This is identical in nature to V2 of the AN→ETH doc but exercises a different circuit (`circuit_v2.rs`, the deposit prover).

### V3.1 Get the Halo2 binary proof

If the relayer provided only the gnark JSON, the binary proof is recoverable from there:

```bash
cd deposit-prover

# halo2_proof.json (in gnark-wrapper) contains the raw bytes
jq -r '.proof_bytes_hex // .proof' gnark-wrapper/halo2_proof.json | xxd -r -p > /tmp/proof.bin
jq -r '.public_inputs[]' gnark-wrapper/halo2_proof.json \
  | python3 -c '
import sys
for line in sys.stdin:
    n = int(line)
    sys.stdout.buffer.write(n.to_bytes(32, "little"))
' > /tmp/instances.bin
```

### V3.2 Run the deposit-prover's verifier

The `deposit-prover` crate exposes verification via its existing test paths:

```bash
cd deposit-prover
cargo run --example test_with_real_data --release -- \
  --proof /tmp/proof.bin \
  --instances /tmp/instances.bin \
  --vk data/deposit_vk.bin \
  --verify-only
```

✅ Expected: `Proof OK`.

If the deposit-prover doesn't currently expose a `--verify-only` flag, fall back to:

```bash
# Re-run the prover with the same input — re-verifies as a side-effect of the test harness
cargo test --release verify_proof_test -- --nocapture
```

(The exact command depends on which examples have been wired to verify-only mode. Several of `examples/*.rs` cover this — `analyze_proof_structure.rs`, `inspect_snark.rs`, `parse_proof_detailed.rs`. Pick whichever matches your needs.)

### V3.3 If V3 fails

- The producer's Halo2 proof is broken or the VK doesn't match.
- **Reject.** Do not credit the user.
- Investigate whether the producer is using a stale circuit version (check `circuit_version` field in the bundle vs. what's published).

---

## 8. Stage V4 — gnark JSON Has the Same Instances (≈ seconds)

**Goal**: Confirm the gnark wrapper bundled the *same* instances the Halo2 proof certifies. This catches the "valid Halo2 + arbitrary public inputs" attack against the wrapper stub.

### V4.1 Diff the instance lists

```bash
cd deposit-prover

# halo2_proof.json contains the original public_inputs from the Halo2 proof.
# The Groth16 output should have the same 7 inputs (or 6 + promise_commit appended).
jq '.public_inputs' gnark-wrapper/halo2_proof.json > /tmp/halo2_pi.json

# The Groth16 outputs from gnark-wrapper:
echo '[
  "'$(jq -r '.deposit_id'      deposit_proof_<txHash>.json)'",
  "'$(jq -r '.sender_uint160'  deposit_proof_<txHash>.json)'",
  "'$(jq -r '.amount'          deposit_proof_<txHash>.json)'",
  "'$(jq -r '.contract_uint160' deposit_proof_<txHash>.json)'",
  "'$(jq -r '.public_inputs[4]' deposit_proof_<txHash>.json)'",
  "'$(jq -r '.public_inputs[5]' deposit_proof_<txHash>.json)'",
  "'$(jq -r '.promise_commit'  deposit_proof_<txHash>.json)'"
]' > /tmp/groth16_pi.json

diff /tmp/halo2_pi.json /tmp/groth16_pi.json && echo "Instances match"
```

✅ Expected: empty diff.

### V4.2 Confirm the wrapping circuit version

```bash
jq '.protocol.k // .k' gnark-wrapper/halo2_proof.json   # → 20 (deposit prover K)
```

If `k != 20`, the wrapping is incompatible with the published VK — reject.

---

## 9. Stage V5 — Verify Groth16 Natively in gnark (≈ 1 min)

**Goal**: re-run the gnark verifier off-chain. This is the same logic that the on-chain TVM verifier (V7) will run once it's deployed.

### V5.1 Use the wrapper's prove pipeline

`gnark-wrapper prove` calls `groth16.Verify` locally before exporting. Re-running it on the same input is the cleanest way to verify:

```bash
cd deposit-prover/gnark-wrapper
go build .

# Setup once per circuit version (skip if keys already present):
./gnark-wrapper setup halo2_proof.json   # produces verification.key, etc.

# Re-prove and re-verify:
./gnark-wrapper prove halo2_proof.json
```

✅ Expected output:

```
Generating Groth16 proof...
Proof generated in <1s
Proof verified locally!
Groth16 proof: 256 bytes
```

The "Proof verified locally!" line is the gnark `Verify` call returning success.

### V5.2 Verify-only for an externally supplied proof

If you have a Groth16 proof from someone else (`groth16_proof_bytes.hex` + 7 public inputs), use a small Go program:

```go
// verify_deposit.go
package main

import (
    "encoding/hex"
    "fmt"
    "math/big"
    "os"

    "github.com/consensys/gnark-crypto/ecc"
    "github.com/consensys/gnark/backend/groth16"
    groth16bn254 "github.com/consensys/gnark/backend/groth16/bn254"
    "github.com/consensys/gnark/frontend"
)

func main() {
    proofHex, _ := os.ReadFile(os.Args[1])  // 0x-prefixed 256-byte proof
    inputsJson, _ := os.ReadFile(os.Args[2]) // JSON array of 7 decimal strings
    vkPath := "verification.key"

    proofBytes, _ := hex.DecodeString(string(proofHex)[2:])

    var inputs []string
    _ = json.Unmarshal(inputsJson, &inputs)
    if len(inputs) != 7 {
        log.Fatal("expected 7 public inputs")
    }

    vk := groth16.NewVerifyingKey(ecc.BN254)
    vkFile, _ := os.Open(vkPath)
    defer vkFile.Close()
    vk.ReadFrom(vkFile)

    proof := groth16.NewProof(ecc.BN254)
    p := proof.(*groth16bn254.Proof)
    p.UnmarshalSolidity(proofBytes)

    // Build public witness
    schema := /* the wrapping circuit's Schema with 7 public ins */
    witness, _ := frontend.NewWitness(/* assignment */, ecc.BN254.ScalarField())

    if err := groth16.Verify(proof, vk, witness); err != nil {
        log.Fatalf("Verify failed: %v", err)
    }
    fmt.Println("Groth16 OK")
}
```

A reference implementation for the layer-hash side is in `layer-hashes-prover/gnark-wrapper/main.go`'s `runProve`; the deposit equivalent lives in `deposit-prover/gnark-wrapper/main.go`. Adapt it to a verify-only command if needed.

✅ Expected: prints `Groth16 OK`.

### V5.3 If V5 fails

- The Groth16 proof is invalid.
- Check that `verification.key` is the right one for this circuit version.
- If the keys are correct, **reject the proof.**

---

## 10. Stage V6 — Nullifier Check (≈ instant) [planned]

**Goal**: Confirm the same `depositId` has not already been credited on the AN side. This is the AN equivalent of the `processedDeposits` mapping in `AckiNackiBridge.sol`.

### V6.1 Current state

The AN-side nullifier is **not yet implemented**. It needs to be deployed as a TVM contract that:

```rust
// Pseudocode for the AN-side nullifier contract
struct DepositNullifier {
    consumed: BTreeMap<u256, bool>,  // depositId → consumed?
    bridge_treasury: Address,
}

impl DepositNullifier {
    fn credit(&mut self, deposit_id: u256, recipient: Address, amount: u256, proof: ...) {
        require!(!self.consumed.get(deposit_id).unwrap_or(false), "ALREADY_CREDITED");
        require!(self.verify_groth16(proof, ...), "INVALID_PROOF");

        self.consumed.insert(deposit_id, true);
        // mint or transfer `amount` to `recipient`
    }
}
```

### V6.2 Verifier action today

Until V6 is on-chain, BK operators must keep their own off-chain nullifier set:

```bash
# A sqlite DB or JSON file the relayer / BK consults
nullifier_db="$HOME/.an-deposit-nullifiers.json"

DEPOSIT_ID=$(jq -r '.deposit_id' deposit_proof_<txHash>.json)

if jq -e ".consumed | index(\"$DEPOSIT_ID\")" "$nullifier_db" > /dev/null; then
  echo "ALREADY CREDITED — reject"
  exit 1
fi

# After successful credit:
jq ".consumed += [\"$DEPOSIT_ID\"]" "$nullifier_db" > "$nullifier_db.tmp" \
  && mv "$nullifier_db.tmp" "$nullifier_db"
```

This is a **stop-gap** until V6/V7 land on-chain. It is BK-operator-local and not Byzantine-fault-tolerant — multiple honest BKs must independently maintain their own DBs and reject duplicates.

---

## 11. Stage V7 — On-chain TVM Verification (≈ seconds) [planned]

**Goal**: A TVM contract verifies the Groth16 proof and atomically updates the nullifier mapping in a single transaction.

### V7.1 What needs to be built

| Component | Status | Notes |
|---|---|---|
| BN254 pairing in TVM | not implemented | TVM lacks a precompile; needs in-VM implementation or a new opcode |
| Groth16 verifier (TVM) | not implemented | Port of `Groth16Verifier.sol` to Solidity-on-TVM or TVM ABI |
| Nullifier mapping | not implemented | Ordered set / dictionary in TVM persistent storage |
| Canonical Ethereum oracle | not implemented | Multi-RPC quorum or BLS-signed bridge |

This is the M9 milestone. See `integration_plan.md` for design progress.

### V7.2 Once deployed

```
[user / relayer] →  call `creditDeposit(proof, recipient)` on TVM bridge
                       │
                       ▼
                 BN254 pairing check
                       │
                       ▼
                 nullifier check + mark
                       │
                       ▼
                 mint / transfer to recipient
```

The verifier on the AN side will return a single `bool` from the pairing check, exactly mirroring the EVM Groth16 verifier's behaviour.

---

## 12. End-to-End Verification Recipe (Copy-Pasteable, ≈ 5 min)

The minimal "I trust nothing" check for a BK operator who already has the proof bundle:

```bash
cd /home/sergey/Pruvendo/gosh/acki-nacki-bridge
BUNDLE=deposit_proof_<txHash>.json

# V1 — RPC quorum on the block hash
BN=$(jq -r '.block_number' $BUNDLE)
BH=$(jq -r '.block_hash'   $BUNDLE)
for RPC in https://eth.llamarpc.com https://rpc.ankr.com/eth; do
  H=$(cast block $BN --rpc-url $RPC --json | jq -r .hash)
  [ "$H" = "$BH" ] && echo "$RPC ✓" || { echo "$RPC ✗"; exit 1; }
done

# V2 — public inputs match the on-chain log
DID=$(jq -r '.deposit_id' $BUNDLE)
TX=$(jq -r '.tx_hash'     $BUNDLE)
LOG_DID=$(cast receipt $TX --rpc-url https://eth.llamarpc.com --json \
  | jq -r '.logs[] | select(.topics[0] == "'$(cast keccak "Deposit(uint256,address,uint256,uint256)")'") | .topics[1]' \
  | xargs cast --to-dec)
[ "$DID" = "$LOG_DID" ] && echo "depositId ✓" || { echo "depositId ✗"; exit 1; }

# V3 — Halo2 proof verifies natively (depends on having the Halo2 keys)
cd deposit-prover
cargo run --release --example test_with_real_data -- --bundle ../$BUNDLE 2>&1 | grep -E "OK|VERIFIED"

# V5 — Groth16 verifies via gnark
cd gnark-wrapper
./gnark-wrapper prove halo2_proof.json | grep "verified locally"

echo "ALL GREEN — safe to credit"
```

If every line succeeds, the proof is verified at all available stages.

---

## 13. Negative Tests — What Should Fail

A verifier should also exercise that *invalid* proofs are correctly rejected. The deposit-prover suite covers many:

```bash
cd contracts/ethereum   # the EVM side rejects the same conditions an AN verifier would

forge test --match-contract FuzzGroth16DepositVerifierTest -vv 2>&1 | tail -10
forge test --match-contract FuzzAckiNackiBridgeTest        -vv 2>&1 | tail -10

# Specific scenarios:
forge test --match-test testFuzz_RandomProofAndInputsReject -vv  # malformed proofs
forge test --match-test testFuzz_WrongProofLengthRejects    -vv  # wrong byte count
forge test --match-test testFuzz_WrongInputCountRejects     -vv  # wrong PI count
forge test --match-test testFuzz_DoubleSpendReverts         -vv  # nullifier
```

✅ Expected: every test passes (i.e., the verifier correctly *rejected* the invalid input).

For the off-chain BK pipeline, simulate each failure mode by:

- Modifying one byte of `groth16_proof_bytes.hex` → V5 should fail.
- Modifying one of the public inputs → V4 diff should fail.
- Modifying `block_hash` in the bundle → V1 RPC quorum should fail.
- Modifying `sender` or `amount` → V2 should fail.

---

## 14. What Each Failure Mode Tells You

| Failure | Likely cause | What to do |
|---|---|---|
| V1 RPC quorum disagrees | Stale RPC / fork / compromised provider | Wait for more confirmations; query more RPCs |
| V1 Axiom disagrees | Block was reorged | Wait until target block has > 64 confirmations |
| V2 mismatch on `depositId`/`amount`/`sender` | Relayer lying about which event the proof represents | Reject; possible malicious relayer |
| V2 mismatch on `contractAddress` | Proof is for a different bridge | Reject |
| V3 fails | Tampered Halo2 proof / wrong VK | Reject; check `circuit_version` |
| V4 mismatch | Wrapping bound different instances than the proof | Reject; producer is buggy or malicious |
| V5 fails | Groth16 proof invalid | Reject; verify `verification.key` is correct |
| V6 already consumed | Replay attempt | Reject; alert ops |
| V7 [planned] | Same as V5 + access control / insufficient gas | Once deployed: reject; collect logs |

---

## 15. Trust Model on the AN Side

After running V1–V7 (when V6/V7 land), the trust assumption reduces to:

1. **Ethereum L1 consensus** — the chain Ethereum says is canonical.
2. **The RPC quorum / Axiom oracle** that you query in V1 (defence: use multiple independent providers).
3. **BN254 hardness** (DLP, pairing soundness).
4. **KZG SRS ceremony** for `kzg_bn254_20.srs` (deposit-prover uses K=20).
5. **Halo2 / SHPLONK soundness** (same as for AN→ETH).
6. **gnark Groth16 implementation** + the auto-generated TVM verifier (when implemented).
7. **The `deposit-prover` circuit source** (Receipt RLP, MPT, log decoding, Keccak coprocessor).
8. **gnark wrapper Define stub** — same limitation as on the layer-hash side. Mitigated by V4 (instance binding) until full in-circuit Halo2 verification ships.

If an attacker compromises Ethereum L1 (51% reorg) or breaks BN254 / Poseidon / Blake2b, no procedural verification can catch it. Those are out of scope for any cross-chain bridge.

---

## 16. What Differs From the AN→ETH Side

| Aspect | AN → ETH (`verifying_an_proof.md`) | ETH → AN (this doc) |
|---|---|---|
| Source of truth | AN node (BLS attestations) | Ethereum chain (block hash) |
| Public inputs | 13 (BK commitment, layer hashes, anchor) | 7 (deposit fields, block hash split, promise) |
| Halo2 K | 19 | 20 |
| Halo2 stack | gosh-fork halo2-base | axiom-eth halo2 fork |
| Wrapped proof bytes | 256 (no extra) | 288 (256 + 32 promise_commit) |
| On-chain verifier (today) | EVM `LayerHashGroth16Verifier` | EVM `Groth16Verifier` (deposit) — but the **AN-side equivalent isn't deployed yet** |
| Chain anchor / nullifier | Layer-hash chain anchor in `LayerHashBridge` | `processedDeposits[depositId]` (Ethereum side); AN-side equivalent **planned (V6)** |
| Canonical-chain oracle | `AxiomBlockHeaderOracle` on Ethereum | RPC quorum on AN BK nodes (no on-chain Ethereum oracle on AN today) |

The two flows are conceptually symmetric, but the **implementation maturity differs**: AN→ETH is fully on-chain end-to-end; ETH→AN's on-chain AN-side verification is M9-pending.

---

## 17. Quick Reference

**Sanity-check a bundle**:

```bash
jq '. | {tx_hash, deposit_id, sender, amount, block_number,
         proof_bytes: (.groth16_proof_hex | length)}' deposit_proof_<txHash>.json
# expected: { ..., proof_bytes: 578 }   (288 bytes × 2 hex chars + "0x" = 578)
```

**Full off-chain verify pipeline (V1–V5)**:

```bash
./scripts/verify_deposit_bundle.sh deposit_proof_<txHash>.json
# (script to be written; consolidates V1.2, V2.4, V3.2, V4.1, V5.1)
```

**Re-fetch ground truth**:

```bash
cast tx     <txHash> --rpc-url <ETH_RPC> --json
cast receipt <txHash> --rpc-url <ETH_RPC> --json
cast block  <blockNumber> --rpc-url <ETH_RPC> --json
```

---

## 18. Cross-References

- `docs/verifying_an_proof.md` — the mirror image (AN → ETH proof verification).
- `docs/integration_analysis.md` §1.3 — what the deposit proof asserts at the architecture level.
- `docs/keccak_coprocessor_flowchart.mmd` — the keccak coprocessor pattern that produces `promise_commit`.
- `docs/integration_plan.md` M7–M9 — pending work for the AN-side on-chain verifier.
- `docs/bridge_verification.md` §4 — Ethereum-side properties (DEP-1 through DEP-6) that this AN-side flow mirrors.
- `deposit-prover/README.md` — local generation pipeline.
- `deposit-prover/examples/fetch_deposit_data.rs` — RPC fetcher used in V2.
