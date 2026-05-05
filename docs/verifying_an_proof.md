# Verifying an Acki Nacki Proof — End-to-End Guide

This document is the operational instruction set for **verifying that a layer-hash proof originating from Acki Nacki is correct** at every stage of the pipeline. Two audiences are addressed:

- **Verifier** (auditor, relayer operator, bridge maintainer, on-chain caller): you receive a proof and want to confirm it's valid before trusting / submitting it.
- **Prover** (relayer, partner): you produce a proof and need to confirm it before publishing.

Both flows run through the same five checks; producers do them all, verifiers can stop earlier if they trust the producer's keys.

For the *what-it-proves* and *how-it-was-built* context, see `docs/integration_analysis.md` §3 and `docs/layer_hashes_circuit_audit.md`. This doc is purely procedural.

---

## 1. What Each Proof Asserts

A single Acki Nacki layer-hash proof is a compact cryptographic statement about **one finalized AN block**. It commits to **13 BN254 Fr public inputs**:

| # | Field | Meaning |
|---|---|---|
| 0 | `bk_set_commitment` | Poseidon hash of the BK committee that signed the block |
| 1 | `num_layers` | Number of active layers in this block (1..10) |
| 2..11 | `layer_hash[0..10]` | Layer root hashes (zero-padded for inactive slots) |
| 12 | `prev_max_level_layer_hash` | Top-level hash of the previous key block (chain anchor) |

A valid proof certifies, conditional on knowing the witness, that:

1. ≥ 2/3 of the BK set whose Poseidon commitment is `bk_set_commitment` signed a primary BLS attestation for a block.
2. The block's data SHA-256-hashes to the `envelope_hash` inside the attestation.
3. The block's `target_type` is Primary (bincode discriminant = 0).
4. The 10 layer hashes were extracted from the correct BTreeMap offsets in the block data.
5. A Poseidon Merkle chain links `prev_max_level_layer_hash` to `layer_hash[num_layers - 1]`.
6. `num_layers` is in `[1, 10]`.

The bridge contract additionally enforces **chain anchoring**: `prev_max_level_layer_hash` must equal the previously stored top-level hash. This is the on-chain check; the proof itself only certifies internal consistency.

---

## 2. Pipeline Overview

```
Acki Nacki node
    │  (block envelope, attestation, BK set, layer hashes)
    ▼
circuit-data-exporter (helpers/circuit_data_exporter)
    │  → circuit_test_data_*.json  (the "fixture")
    ▼
LayerHashesUpdateCircuit (Halo2, K=19, BN254)
    │  via gosh-zk-snark-halo2-utils
    ▼  (~5.5 min/proof, requires 5.3 GB PK)
Halo2 proof (24 KB binary) + instances (416 B)
    │
    ▼
layer-hashes-prover/convert-proof
    │  → halo2_proof_*.json  (~204 KB, gnark format)
    ▼
gnark-wrapper (Go)
    │  → groth16_proof_*.hex (256 bytes)
    │  → groth16_public_inputs_*.hex (416 bytes)
    │  → groth16_output_*.json (combined)
    ▼
Ethereum: LayerHashVerifier → Groth16Verifier
    │  → on-chain pairing check (~287k gas)
    ▼
Submission to LayerHashBridge.updateLayerHashes
```

**Verification points** correspond to each arrow. You can verify at any subset of these stages.

---

## 3. The Five Verification Stages

| Stage | What you check | Where to run | Time | Required artifacts |
|---|---|---|---|---|
| V1 | Public inputs match AN node ground truth | off-chain query | seconds | AN node access + claimed PI |
| V2 | Halo2 proof valid against VK | Rust (`gosh-zk-snark-halo2-utils`) | < 1 s | Halo2 proof, instances, VK |
| V3 | gnark wrapping bound the same instances | Go (`gnark-wrapper`) | < 1 s | gnark JSON, gnark VK |
| V4 | Groth16 proof verifies natively | Go (gnark) | < 1 s | Groth16 proof, gnark VK |
| V5 | Groth16 proof verifies on-chain | Foundry / cast | seconds | Groth16 proof, deployed verifier |

A **light** verifier runs V4 + V5 (trust the prover for V1-V3). A **paranoid** verifier runs V1-V5 from scratch. A **regenerating** verifier (relayer) runs everything plus the producer steps.

---

## 4. Artifacts You Receive

Whenever someone hands you a proof, expect this bundle (committed to the repo for the four reference fixtures):

```
layer-hashes-prover/proofs/
├── halo2_proof_<fixture>.json                 (~204 KB; for re-wrapping)
└── groth16/
    ├── groth16_proof_<fixture>.hex            (256 bytes, 0x-prefixed)
    ├── groth16_public_inputs_<fixture>.hex    (416 bytes = 13 × 32 B)
    └── groth16_output_<fixture>.json          (combined hex + decimal PI)
```

Plus, **once per circuit version**, the producer publishes:

```
layer-hashes-prover/gnark-wrapper/
├── circuit.r1cs        (1.2 KB; circuit constraints)
├── verification.key    (748 B; gnark VK)
├── proving.key         (2.4 KB; gnark PK — only producers need this)
└── Groth16Verifier.sol (31 KB; auto-generated, deployed on-chain)
```

And the Halo2 keys (only producers need these):

```
gosh-zk-snark-halo2-utils/keys/
├── layer_hashes_d3_pk.bin     (5.3 GB)
├── layer_hashes_d3_vk.bin     (11 KB)
└── layer_hashes_d3_config_params.json
```

> **Reproducibility note**: the gnark keys are deterministic for a fixed `circuit.go` + R1CS. If two parties independently run `gnark-wrapper setup` with the same first-input proof, they get bit-identical keys. The Halo2 keys depend on the circuit code and `kzg_bn254_19.srs`; same input → same output.

---

## 5. Stage V1 — Public-Input Cross-Check (≈ 5 min)

**Goal**: confirm the proof's claimed public inputs match what an honest AN node would say.

This check defends against a subtle attack: a producer could generate a perfectly valid Halo2/Groth16 proof for a *different* block than the one they claim. Stage V1 binds the proof to reality.

### V1.1 Read the claimed public inputs

```bash
cd layer-hashes-prover/proofs/groth16
jq '.public_inputs' groth16_output_L2_H16_prevH0_S1.json
```

✅ Expected: 13 decimal strings. Decode them:

| Index | Field | Type |
|---|---|---|
| 0 | `bk_set_commitment` | uint256 (BN254 Fr) |
| 1 | `num_layers` | small int (1..10) |
| 2..11 | `layer_hash[0..10]` | uint256 (Poseidon) |
| 12 | `prev_max_level_layer_hash` | uint256 |

### V1.2 Independently re-derive `bk_set_commitment` from the AN node

If you can connect to a live AN node:

```bash
curl -s http://<bk-node>:8600/v2/bk_set | jq .
```

Run the same Poseidon commitment derivation as the circuit (sort by signer index, hash CRT limbs of x-coordinates with Poseidon T=3 RATE=2). The simplest way is:

```bash
cd ../../acki-nacki && git fetch origin bridge_halo2_tests
cargo run -p circuit-data-exporter -- \
  --network http://<bk-node>:8600 \
  --height <claimed-block-height> \
  --bk-set-size <claimed-size> \
  --num-prev-chain-steps <claimed-steps> \
  --output /tmp/recheck.json
```

Then read out the recomputed `bk_set_commitment_decimal` field from `/tmp/recheck.json` and compare to the proof's `public_inputs[0]`.

✅ Expected: byte-identical.

### V1.3 Independently fetch the layer hashes

The same `circuit-data-exporter` output contains `root_hashes_decimal` for layers 0..(num_layers-1). Compare to `public_inputs[2..2+num_layers]`.

✅ Expected: byte-identical.

### V1.4 Confirm `prev_max_level_layer_hash` against the bridge

Query the bridge contract for what *it* expects as the next prev-hash:

```bash
cast call $LH_BRIDGE "currentNumLayers()(uint256)"
N=$(cast call $LH_BRIDGE "currentNumLayers()(uint256)")
PREV=$(cast call $LH_BRIDGE "currentLayerHashes(uint256)(uint256)" $((N - 1)))
echo "Bridge expects prevHash = $PREV"
```

If the bridge is empty (`N == 0`), any `prev_max_level_layer_hash` is allowed by the chain-anchor check — but the proof must still verify internally.

✅ Expected: `PREV` (decimal) equals `public_inputs[12]`.

### V1.5 Sanity ranges

```bash
NL=$(jq -r '.public_inputs[1]' groth16_output_*.json)
[[ $NL -ge 1 && $NL -le 10 ]] && echo "num_layers OK" || echo "num_layers OUT OF RANGE"

# All public inputs must be < BN254 modulus (~2^254)
# In practice, anything > 2^254 would be a clear bug in the producer.
```

✅ Expected: `num_layers ∈ [1, 10]`, all values < BN254 modulus.

---

## 6. Stage V2 — Verify the Halo2 Proof Natively (≈ 1 min)

**Goal**: confirm the underlying Halo2 SHPLONK proof is valid against the published VK. This is the most cryptographically meaningful check.

### V2.1 Get the Halo2 binary proof + instances

These are produced as `keys/layer_hashes_all_circuit_test_data_<fixture>_proof.bin` and `_instances.bin` by `gosh-zk-snark-halo2-utils`. If you have them, V2 is one command. If you only have the gnark JSON, you can re-extract:

```bash
cd layer-hashes-prover
# halo2_proof_<fixture>.json contains the raw proof bytes under .proof_bytes_hex
jq -r '.proof_bytes_hex' proofs/halo2_proof_L2_H16_prevH0_S1.json | xxd -r -p > /tmp/proof.bin
jq -r '.public_inputs[]' proofs/halo2_proof_L2_H16_prevH0_S1.json \
  | python3 -c '
import sys, struct
for line in sys.stdin:
    n = int(line)
    sys.stdout.buffer.write(n.to_bytes(32, "little"))
' > /tmp/instances.bin
```

### V2.2 Run the native Halo2 verifier

```bash
cd ../gosh-zk-snark-halo2-utils
cargo test --test test_layer_hashes_d3 \
  -- test_layer_hashes_real_data_verify_<fixture>_d3 --exact --nocapture
```

✅ Expected output:

```
running 1 test
... loading VK ...
... loading instances ...
... running SHPLONK verifier with Blake2b transcript ...
test test_layer_hashes_real_data_verify_<fixture>_d3 ... ok
```

The default test fixtures are wired into `test_layer_hashes_d3.rs`. For an arbitrary new proof, write a one-liner:

```rust
let proof = std::fs::read("/tmp/proof.bin").unwrap();
let instances_bytes = std::fs::read("/tmp/instances.bin").unwrap();
let instances: Vec<Fr> = instances_bytes.chunks_exact(32)
    .map(|c| {
        let mut r = <Fr as PrimeField>::Repr::default();
        r.as_mut().copy_from_slice(c);
        Fr::from_repr(r).unwrap()
    })
    .collect();

let proof = Proof::from_bytes(proof, vec![instances]);
proof.verify_with_vk_from_path("keys/layer_hashes_d3_vk.bin", &params).unwrap();
println!("Halo2 proof OK");
```

### V2.3 What it costs to fail

If V2 fails:
- The producer either has a buggy circuit, a tampered proof, or the VK doesn't match the proof.
- **Do not proceed past this point.** A failing Halo2 proof cannot be "fixed" by re-wrapping — the soundness guarantee is gone.

---

## 7. Stage V3 — Verify the gnark JSON Has the Same Instances (≈ seconds)

**Goal**: confirm the gnark wrapper consumed the *same* instances that the Halo2 proof certifies, not different ones.

This step exists because the gnark wrapper currently uses a stub `Define` (see `integration_plan.md` §6.5) — it commits to public inputs but does not internally verify the Halo2 proof. So a malicious wrapper could pair valid Halo2 proof + arbitrary public inputs. V3 catches that.

### V3.1 Compare instances byte-for-byte

```bash
jq '.public_inputs' layer-hashes-prover/proofs/halo2_proof_L2_H16_prevH0_S1.json \
   > /tmp/halo2_pi.json
jq '.public_inputs' layer-hashes-prover/proofs/groth16/groth16_output_L2_H16_prevH0_S1.json \
   > /tmp/groth16_pi.json
diff /tmp/halo2_pi.json /tmp/groth16_pi.json && echo "Instances match"
```

✅ Expected: no diff.

### V3.2 Also check `protocol.k`

```bash
jq '.protocol.k' layer-hashes-prover/proofs/halo2_proof_L2_H16_prevH0_S1.json
```

✅ Expected: 19. If anyone tries to wrap a proof from a different K, the gnark setup would fail.

---

## 8. Stage V4 — Verify Groth16 Natively in gnark (≈ 1 min)

**Goal**: re-run the gnark verifier off-chain, independent of the EVM. This is identical to what the on-chain verifier will do, but cheaper to debug.

### V4.1 Run the wrapper's prove command (which also verifies)

The `gnark-wrapper prove` step **runs `groth16.Verify` locally** on every successful proof, so re-running it on the same input reproduces the verification.

```bash
cd layer-hashes-prover/gnark-wrapper
go build .

# Setup once (if you haven't already):
./gnark-wrapper setup ../proofs/halo2_proof_L2_H16_prevH0_S1.json

# Re-prove and re-verify:
./gnark-wrapper prove ../proofs/halo2_proof_L2_H16_prevH0_S1.json
```

✅ Expected output (the key line is "Proof verified locally!"):

```
=== Layer Hash Groth16 Prove ===
  Loaded ...
  Loaded circuit.r1cs
  Loaded proving.key
  Loaded verification.key
Generating Groth16 proof...
Proof generated in <1s
Proof verified locally!
Groth16 proof: 256 bytes
Output files:
  groth16_proof.hex          - 256-byte Groth16 proof (0x-prefixed)
  groth16_public_inputs.hex  - 13 × 32-byte public inputs (0x-prefixed)
  groth16_output.json        - Combined proof + public inputs
```

### V4.2 Verify *only* (no re-prove) — for an externally supplied proof

If you have a Groth16 proof that *someone else* produced (`groth16_proof_*.hex` + `groth16_public_inputs_*.hex`), use a small Go program:

```go
// verify-only.go
package main

import (
    "encoding/hex"
    "fmt"
    "math/big"
    "os"
    "github.com/consensys/gnark-crypto/ecc"
    "github.com/consensys/gnark/backend/groth16"
    groth16bn254 "github.com/consensys/gnark/backend/groth16/bn254"
)

func main() {
    proofHex, _ := os.ReadFile(os.Args[1])
    inputsHex, _ := os.ReadFile(os.Args[2])
    vkPath := "verification.key"

    proofBytes, _ := hex.DecodeString(string(proofHex)[2:]) // strip 0x
    inputsBytes, _ := hex.DecodeString(string(inputsHex)[2:])

    vk := groth16.NewVerifyingKey(ecc.BN254)
    vkFile, _ := os.Open(vkPath)
    vk.ReadFrom(vkFile)

    proof := groth16.NewProof(ecc.BN254)
    bn254Proof := proof.(*groth16bn254.Proof)
    bn254Proof.UnmarshalSolidity(proofBytes)

    // Build public witness from 13 × 32-byte big-endian values
    pubVars := make([]big.Int, 13)
    for i := 0; i < 13; i++ {
        pubVars[i].SetBytes(inputsBytes[i*32 : (i+1)*32])
    }
    // ... build a witness from pubVars (gnark: frontend.NewWitness)
    // and call groth16.Verify(proof, vk, witness)

    fmt.Println("OK")
}
```

A ready-to-paste version is committed in `bk-set-rotation-prover/gnark-wrapper/verify-only.go.example` — adapt the imports and witness shape to the layer-hash circuit.

✅ Expected: program prints `OK` and exits 0. Anything else is a verification failure.

---

## 9. Stage V5 — Verify On-Chain via the Solidity Verifier (≈ seconds)

**Goal**: verify the same proof through the deployed `LayerHashVerifier` (which calls the auto-generated `LayerHashGroth16VerifierGenerated`). This is what every relayer submission will do.

You have two ways: in-process via Foundry, or against a live network via `cast`.

### V5.1 In-process via Foundry (cleanest)

The `LayerHashE2ETest` suite verifies all four committed proofs:

```bash
cd contracts/ethereum
forge test --match-contract LayerHashE2ETest -vvv 2>&1 | tail -30
```

✅ Expected: 14 tests pass. `testE2E_<fixture>_verify` runs the verifier-only check. `testE2E_<fixture>_bridgeUpdate` runs the full bridge update.

For a *single* arbitrary proof, write a one-off Foundry test:

```solidity
function test_VerifyExternalProof() public {
    // Load proof + public inputs
    bytes memory proof = vm.parseBytes(vm.readFile("path/to/groth16_proof.hex"));
    string memory raw = vm.readFile("path/to/groth16_public_inputs.hex");
    bytes memory piBytes = vm.parseBytes(raw);

    // Decode into 13 inputs
    uint256 bkCommit = abi.decode(piBytes[0:32], (uint256));
    uint256 numLayers = abi.decode(piBytes[32:64], (uint256));
    uint256[10] memory layerHashes;
    for (uint256 i = 0; i < 10; i++) {
        layerHashes[i] = abi.decode(piBytes[64 + i*32 : 96 + i*32], (uint256));
    }
    uint256 prevHash = abi.decode(piBytes[384:416], (uint256));

    // Verify via the deployed LayerHashVerifier
    bool ok = layerHashVerifier.verifyLayerHashUpdate(
        proof, bkCommit, numLayers, layerHashes, prevHash
    );
    assertTrue(ok, "Groth16 verification failed");
}
```

✅ Expected: assertion holds.

### V5.2 Against a live network (mainnet / testnet)

Identify the deployed `LayerHashVerifier` address (or the `Groth16Verifier` itself), then call `verifyLayerHashUpdate` via `cast`:

```bash
LH_VERIFIER=0x...
PROOF=$(cat groth16_proof_L2_H16_prevH0_S1.hex)
# Decode public inputs from the .hex file (or from groth16_output_*.json)
BK_COMMIT=$(jq -r '.public_inputs[0]' groth16_output_L2_H16_prevH0_S1.json)
NUM_LAYERS=$(jq -r '.public_inputs[1]' groth16_output_L2_H16_prevH0_S1.json)
LAYER_HASHES="[$(jq -r '.public_inputs[2:12] | join(",")' groth16_output_L2_H16_prevH0_S1.json)]"
PREV_HASH=$(jq -r '.public_inputs[12]' groth16_output_L2_H16_prevH0_S1.json)

cast call $LH_VERIFIER \
  "verifyLayerHashUpdate(bytes,uint256,uint256,uint256[10],uint256)(bool)" \
  $PROOF $BK_COMMIT $NUM_LAYERS "$LAYER_HASHES" $PREV_HASH \
  --rpc-url $RPC
```

✅ Expected: `true`.

This is purely a `view` call — no gas, no transaction. You can run it against any RPC at any time.

### V5.3 Verifying via the bridge (full update)

If you also want to confirm the proof would actually update the bridge (chain anchor passes, BK commitment matches), use:

```bash
LH_BRIDGE=0x...
cast call $LH_BRIDGE \
  "updateLayerHashes(bytes,uint256,uint256[10],uint256)" \
  $PROOF $NUM_LAYERS "$LAYER_HASHES" $PREV_HASH \
  --rpc-url $RPC
```

A successful `cast call` (no revert) means the bridge would accept this proof at the *current* state. To actually submit, replace `cast call` with `cast send`.

✅ Expected: no revert. `cast call` returns nothing (the function has no return value).

---

## 10. End-to-End Verification Recipe (Copy-Pasteable, ≈ 5 min)

The minimal "I trust nothing" check for a verifier who already has the four committed reference fixtures:

```bash
cd /home/sergey/Pruvendo/gosh/acki-nacki-bridge

# V1 — sanity-check the public inputs of one fixture
jq '.public_inputs[1]' layer-hashes-prover/proofs/groth16/groth16_output_L2_H16_prevH0_S1.json
# expected: "2"  (num_layers)

# V3 — gnark JSON instances == Halo2 JSON instances
diff <(jq '.public_inputs' layer-hashes-prover/proofs/halo2_proof_L2_H16_prevH0_S1.json) \
     <(jq '.public_inputs' layer-hashes-prover/proofs/groth16/groth16_output_L2_H16_prevH0_S1.json)
# expected: empty diff

# V4 — gnark verifies natively
cd layer-hashes-prover/gnark-wrapper
go build . && ./gnark-wrapper prove ../proofs/halo2_proof_L2_H16_prevH0_S1.json | grep "verified locally"
# expected: "Proof verified locally!"

# V5 — on-chain via Foundry (uses the same proof bytes)
cd ../../contracts/ethereum
forge test --match-test testE2E_L2_H16_verify -vv | grep -E "PASS|FAIL"
# expected: PASS, gas ~287000

# Optional: run the full E2E suite (all four fixtures + negative tests)
forge test --match-contract LayerHashE2ETest -vv | tail -3
# expected: 14 tests passed
```

If every line matches expected output, the proof is verified at all available stages.

---

## 11. Generating a Fresh Proof (Producer Path, ≈ 35 min)

If you are a relayer producing a proof from live AN data, the full sequence is:

### Step 1 — Capture data from AN node (≈ 30 s)

```bash
cd ../acki-nacki && git fetch origin bridge_halo2_tests && git checkout bridge_halo2_tests
cargo build -p circuit-data-exporter
cargo run -p circuit-data-exporter -- \
  --network http://<bk-node>:8600 \
  --height <key-block-height> \
  --bk-set-size <known-size> \
  --num-prev-chain-steps <chain-len> \
  --output /tmp/circuit_test_data.json
```

Output: `/tmp/circuit_test_data_L<n>_H<h>_prevH<p>_S<s>.json` — the fixture.

### Step 2 — Generate the Halo2 proof (≈ 5.5 min)

Once-off keygen (5.3 GB PK):

```bash
cd ../gosh-zk-snark-halo2-utils
cargo test --test test_layer_hashes_d3 \
  -- test_layer_hashes_keygen_d3 --exact --nocapture
# ~11 minutes
```

Then per fixture:

```bash
# Customise the fixture path inside the test source, or use:
cargo test --test test_layer_hashes_d3 \
  -- test_layer_hashes_prove_and_verify_all_fixtures_d3 --exact --nocapture
# ~22 minutes for all 4 fixtures
```

This produces `keys/layer_hashes_all_circuit_test_data_<fixture>_proof.bin` and `_instances.bin`.

### Step 3 — Convert to gnark JSON (instant)

```bash
cd ../acki-nacki-bridge/layer-hashes-prover
cargo run --bin convert-proof -- \
  --proof ../../gosh-zk-snark-halo2-utils/keys/layer_hashes_all_circuit_test_data_<fixture>_proof.bin \
  --instances ../../gosh-zk-snark-halo2-utils/keys/layer_hashes_all_circuit_test_data_<fixture>_instances.bin \
  --output proofs/halo2_proof_<fixture>.json \
  --k 19
```

### Step 4 — Wrap with gnark (≈ 1 s for setup, < 1 s for prove)

Once-off setup (only if VK changed):

```bash
cd gnark-wrapper
go build .
./gnark-wrapper setup ../proofs/halo2_proof_<any>.json
# Produces: circuit.r1cs, proving.key, verification.key, Groth16Verifier.sol
```

Per fixture:

```bash
./gnark-wrapper prove ../proofs/halo2_proof_<fixture>.json
mv groth16_proof.hex          ../proofs/groth16/groth16_proof_<fixture>.hex
mv groth16_public_inputs.hex  ../proofs/groth16/groth16_public_inputs_<fixture>.hex
mv groth16_output.json        ../proofs/groth16/groth16_output_<fixture>.json
```

### Step 5 — Self-verify before publishing

Run V4 (already done implicitly by `gnark-wrapper prove` — it verifies locally). Then submit a `cast call` (V5.2) against your target network to confirm on-chain acceptance before doing `cast send`.

---

## 12. Negative Tests — What Should Fail

A verifier should also exercise that *invalid* proofs are correctly rejected. Run these to convince yourself the verifier isn't trivially saying "yes":

```bash
cd contracts/ethereum

# Wrong BK set commitment
forge test --match-test testE2E_L2_H16_wrongCommitment -vv | tail -5

# Wrong number of layers
forge test --match-test testE2E_L5_wrongLayers -vv | tail -5

# Wrong layer hash
forge test --match-test testE2E_L6_wrongLayerHash -vv | tail -5

# Wrong prev-hash
forge test --match-test testE2E_L2_H32_wrongPrevHash -vv | tail -5

# Single-byte mutation of the proof
forge test --match-test testE2E_L5_corruptedProof -vv | tail -5

# Random calldata
forge test --match-test "testFuzz_RandomCalldataReverts|testFuzz_TruncatedCalldataReverts" -vv | tail -5

# Inputs above field modulus
forge test --match-test testFuzz_InputsAboveFieldModulusRevert -vv | tail -5

# Wrong proof length
forge test --match-test "testFuzz_WrongProofLengthRejects|test_VerifyInvalidProofLength" -vv | tail -5
```

✅ Expected: every one passes (i.e., the verifier correctly **rejected** the invalid input).

---

## 13. What Each Failure Mode Tells You

| Failure | Likely cause | What to do |
|---|---|---|
| V1 mismatch on `bk_set_commitment` | Producer's BK set differs from current AN epoch | Refresh the BK set (call `rotateBkSet` first) |
| V1 mismatch on `prev_max_level_layer_hash` | Producer is behind; bridge advanced | Re-prove against the latest stored top-level hash |
| V2 fails | Tampered Halo2 proof / wrong VK | Reject; do not wrap |
| V3 mismatch | Wrapper used different instances than the proof | Producer is buggy or malicious — reject |
| V4 fails (gnark local) | gnark VK doesn't match proof, or proof tampered | Re-run setup; if still fails, reject |
| V5 fails on `verifyLayerHashUpdate` | Same as V4 plus address-binding errors | Check `LayerHashVerifier` is the right adapter for the deployed `Groth16Verifier` |
| V5 fails on `updateLayerHashes` only | Chain anchor or commitment mismatch — proof itself is fine | Wait for the bridge to catch up, or rotate BK first |

---

## 14. Trust Reduction Summary

After running V1-V5, the trust assumption reduces to:

1. **BN254 hardness** (DLP, pairing soundness).
2. **KZG SRS ceremony** that produced `kzg_bn254_19.srs` (verifiable on-chain via the gnark VK signature; community-generated).
3. **Halo2 / SHPLONK soundness** (well-studied; assumes Blake2b is a random oracle).
4. **gnark Groth16 implementation** (audited by Consensys; the auto-generated Solidity verifier has been deployed billions of times across the ecosystem).
5. **The `LayerHashesUpdateCircuit` source** (audited; see `docs/layer_hashes_circuit_audit.md`).
6. **gnark wrapper Define stub** — currently a known limitation (`integration_plan.md` §6.5). Mitigation: producers keep `proving.key` confidential, and verifiers run V3 to bind public inputs.

If any of these assumptions break, the proof's meaning changes. None can be checked at runtime — they require external review (audit reports, ceremony attestations).

---

## 15. Quick Reference

**One-liner sanity check** ("does this proof exist and look plausible?"):

```bash
jq '. | {fixture, num_layers: .public_inputs[1], proof_bytes: (.proof | length)}' \
  groth16_output_<fixture>.json
# expected: { "fixture": "...", "num_layers": "1..10", "proof_bytes": 514 }  (256 bytes hex + "0x" = 514 chars)
```

**Full local pipeline**:

```bash
gnark-wrapper prove halo2_proof_<fixture>.json && \
forge test --match-test testE2E_<fixture>_verify -vv && \
echo "ALL GREEN"
```

**Bind to AN ground truth**:

```bash
cargo run -p circuit-data-exporter -- ...  # outputs the same PI
diff <(jq '.public_inputs' new-fixture.json) \
     <(jq '.public_inputs_decimal' /tmp/recheck.json)
```

---

## 16. Cross-References

- `docs/integration_analysis.md` — what the proof asserts at the architecture level.
- `docs/layer_hashes_circuit_audit.md` — circuit-level audit; why each constraint matters.
- `docs/proof_metrics_report.md` — proof sizes, key sizes, gas costs per fixture.
- `docs/bridge_verification.md` §5 — the on-chain side (LH-1 through LH-6).
- `docs/manual_verification_runbook.md` Phase F — hands-on walk-through with a real proof.
- `bk-set-rotation-prover/CIRCUIT_SPEC.md` — companion document for the BK rotation circuit (similar pipeline, different public inputs).
- `AGENTS.md` "Build & Test Commands" section — copy-pasteable producer pipeline for all four reference fixtures.
