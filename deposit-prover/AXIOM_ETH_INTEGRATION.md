# Axiom-eth Integration - Implementation Status

## Overview

This document describes the integration of axiom-eth library into the deposit-prover circuit. The circuit proves that a specific Deposit event was emitted on Ethereum by verifying the Merkle-Patricia Trie (MPT) proof of the receipt.

## Architecture

### Circuit Structure

The circuit follows the **EthCircuitInstructions** pattern from axiom-eth, which separates the circuit into two phases:

1. **Phase 0**: MPT verification and RLP decoding (no randomness needed)
2. **Phase 1**: Keccak verification and final constraints (uses challenge from phase 0)

### File: `src/circuit_v2.rs`

This is the new axiom-eth-based circuit implementation.

**Key Components:**

```rust
pub struct DepositEventCircuitV2 {
    pub inputs: DepositProofInput,
    pub params: EthReceiptChipParams,
}

impl EthCircuitInstructions<Fr> for DepositEventCircuitV2 {
    type FirstPhasePayload = Phase0Output;
    
    fn virtual_assign_phase0(...) -> Phase0Output {
        // 1. Load transaction index
        // 2. Convert receipt proof to MPTInput
        // 3. Verify MPT inclusion proof
        // 4. Return witnesses for phase1
    }
    
    fn virtual_assign_phase1(...) {
        // 1. Parse receipt to extract logs
        // 2. Find Deposit event log
        // 3. Verify event signature with Keccak
        // 4. Extract and verify event data
        // 5. Expose public outputs
    }
}
```

## Current Implementation Status

### ✅ Completed

1. **Circuit Structure** - `DepositEventCircuitV2` struct created
2. **Phase 0 Implementation** - MPT verification fully integrated ✅
   - Transaction index loading
   - Receipt proof conversion to MPTInput
   - MPT proof verification using `EthReceiptChip`
   - Log index loading for event selection
3. **Phase 1 Partial Implementation** - RLC verification and log extraction ✅
   - Receipt parsing in phase1
   - **Log extraction using `extract_receipt_log`** ✅
   - Log witness contains the raw RLP bytes of the specific log
4. **Helper Functions** - `ToMPTInput` trait for converting `ReceiptProof` to `MPTInput`
5. **Tests** - Basic circuit creation test passing
6. **Dependencies** - All axiom-eth dependencies configured correctly
   - `axiom-eth` from GitHub
   - `halo2-lib` v0.4.1-git (axiom-crypto ecosystem)
   - `ethers-core` =2.0.14 (matches axiom-eth)

### 🚧 In Progress / TODO

#### Phase 1 RLP Parsing ✅ COMPLETE

The phase1 implementation has successfully parsed the log structure:

1. **✅ Extract logs from receipt** - DONE
   ```rust
   let log_witness = chip.extract_receipt_log(
       ctx_gate,
       &phase0_output.receipt_witness,
       phase0_output.log_index,
   );
   ```

2. **✅ Parse log RLP structure** - DONE
   ```rust
   let rlp_chip = chip.rlp();
   let log_max_field_lens = [20, 150, 70]; // address, topics, data
   let log_array = rlp_chip.decompose_rlp_array_phase0(
       ctx_gate,
       log_witness.log_bytes.clone(),
       &log_max_field_lens,
       false, // fixed 3 fields
   );
   ```

3. **✅ Parse topics array** - DONE
   ```rust
   let topics_rlp = &log_array.field_witness[1].field_cells;
   let topic_max_lens = [32, 32, 32, 32]; // max 4 topics
   let topics_array = rlp_chip.decompose_rlp_array_phase0(
       ctx_gate,
       topics_rlp.clone(),
       &topic_max_lens,
       true, // variable length
   );
   ```

4. **✅ Extract event parameters** - DONE
   ```rust
   // Event signature (topics[0])
   let event_sig_bytes = &topics_array.field_witness[0].field_cells;

   // depositId (topics[1])
   let deposit_id_bytes = &topics_array.field_witness[1].field_cells;

   // sender (topics[2])
   let sender_bytes = &topics_array.field_witness[2].field_cells;

   // data contains [amount, timestamp]
   let data_bytes = &log_array.field_witness[2].field_cells;

   // address
   let address_bytes = &log_array.field_witness[0].field_cells;
   ```

#### Next Steps: Event Verification

5. **Verify event signature** - TODO
   ```rust
   // Compute keccak256("Deposit(uint256,address,uint256,uint256)")
   // and constrain it equals event_sig_bytes
   ```

6. **Verify contract address** - TODO
   ```rust
   // Constrain address_bytes equals expected bridge contract
   ```

7. **Convert bytes to field elements** - TODO
   ```rust
   // Convert 32-byte values to Fr field elements for public outputs
   ```

8. **Expose public outputs** - TODO
   ```rust
   // Make these values public inputs
   builder.assigned_instances.push(deposit_id);
   builder.assigned_instances.push(sender);
   builder.assigned_instances.push(amount);
   builder.assigned_instances.push(contract_address);
   ```

## Axiom-eth API Reference

### Key Types

**EthReceiptChip:**
- `parse_receipt_proof_phase0(ctx, input)` - Verifies MPT proof in phase 0
- `parse_receipt_proof_phase1(ctx, witness)` - Verifies RLC in phase 1
- `parse_log_field(ctx, log)` - Parses log structure

**EthReceiptWitness:**
- `value` - RlpArrayWitness containing receipt fields
- `logs` - RlpArrayWitness containing logs array
- `mpt_witness` - MPTProofWitness

**EthReceiptLogFieldWitness:**
- `address()` - Contract address that emitted the log
- `topics_bytes()` - Array of indexed event parameters
- `data_bytes()` - Non-indexed event parameters
- `num_topics()` - Number of topics

### Receipt Structure

Ethereum receipt RLP encoding:
```
[
    status,           // field 0
    cumulativeGasUsed, // field 1
    logsBloom,        // field 2
    logs              // field 3 - array of logs
]
```

### Log Structure

Ethereum log RLP encoding:
```
[
    address,   // field 0 - contract address
    topics,    // field 1 - array of indexed params
    data       // field 2 - non-indexed params
]
```

### Deposit Event Encoding

```solidity
event Deposit(
    uint256 indexed depositId,
    address indexed sender,
    uint256 amount,
    uint256 timestamp
);
```

Encoded as:
- `topics[0]` = keccak256("Deposit(uint256,address,uint256,uint256)")
- `topics[1]` = depositId (32 bytes)
- `topics[2]` = sender (32 bytes, left-padded address)
- `data` = abi.encode(amount, timestamp) (64 bytes total)

## Testing Strategy

### Unit Tests

1. **Circuit Creation** ✅
   - Test: `test_circuit_creation`
   - Status: Passing

2. **MPT Verification** (TODO)
   - Test with real Ethereum receipt proof
   - Verify MPT inclusion proof passes

3. **Event Extraction** (TODO)
   - Test log parsing
   - Verify event signature matching
   - Verify parameter extraction

4. **Public Outputs** (TODO)
   - Verify correct values are exposed
   - Verify constraints are satisfied

### Integration Tests

1. **End-to-End Proof** (TODO)
   - Deploy test contract to Sepolia
   - Make deposit transaction
   - Fetch receipt proof
   - Generate ZK proof
   - Verify proof

## Dependencies

### Axiom-crypto Ecosystem

```toml
axiom-eth = { git = "https://github.com/axiom-crypto/axiom-eth" }
halo2-base = { git = "https://github.com/axiom-crypto/halo2-lib.git", tag = "v0.4.1-git" }
halo2-ecc = { git = "https://github.com/axiom-crypto/halo2-lib.git", tag = "v0.4.1-git" }
snark-verifier = { git = "https://github.com/axiom-crypto/snark-verifier.git", tag = "v0.1.7-git" }
ethers-core = { version = "=2.0.14" }
```

### Why Separate Workspace?

The deposit-prover is a **standalone workspace** because:
- Axiom-eth requires axiom-crypto's halo2-lib v0.4.1
- Main workspace uses scroll-tech's halo2-lib (for Acki Nacki compatibility)
- These two versions are incompatible

## Next Steps

1. **Implement Phase 1 Event Extraction** (IMMEDIATE)
   - Parse logs from receipt
   - Extract Deposit event
   - Verify event signature
   - Extract parameters

2. **Add Public Outputs** (IMMEDIATE)
   - Expose depositId, sender, amount, contract_address

3. **Test with Real Data** (NEXT)
   - Deploy to Sepolia
   - Generate real receipt proofs
   - Test circuit with real data

4. **Implement Proof Generation** (NEXT)
   - Setup function (generate keys)
   - Prove function (generate proof)
   - Verify function (verify proof)

5. **Generate Solidity Verifier** (FINAL)
   - Use snark-verifier-sdk
   - Deploy verifier contract
   - Test end-to-end flow

## References

- Axiom-eth GitHub: https://github.com/axiom-crypto/axiom-eth (archived but functional)
- Axiom-eth docs: https://docs.axiom.xyz/
- Halo2 book: https://zcash.github.io/halo2/
- Ethereum Yellow Paper (MPT): https://ethereum.github.io/yellowpaper/paper.pdf

