# Implementation Status - Deposit Event Proof Circuit

## Overview

This document tracks the implementation status of the deposit event proof circuit for the Acki Nacki Bridge.

## Architecture Decision

We've implemented a **two-circuit architecture** due to incompatible halo2 library versions:

| Component | Purpose | Halo2 Version | Status |
|-----------|---------|---------------|--------|
| **Deposit Prover** | Prove Ethereum event emission | axiom-crypto v0.4.1 | ✅ Scaffolded |
| **Withdrawal Circuit** | Prove withdrawal on Acki Nacki | scroll-tech develop | ✅ Existing |

## Deposit Prover Status

### ✅ Completed

1. **Project Structure**
   - Created standalone `deposit-prover/` package
   - Separate Cargo workspace (excluded from main workspace)
   - Proper dependency isolation

2. **CLI Interface**
   - `prove` command: Generate deposit event proof
   - `setup` command: Generate proving/verifying keys
   - `verify` command: Verify a proof
   - Full argument parsing with clap

3. **Type Definitions**
   - `DepositEventData`: Event data from Ethereum
   - `ReceiptProof`: MPT proof data
   - `DepositProofInput`: Circuit inputs
   - `DepositProofOutput`: Proof + public inputs

4. **Ethereum Client**
   - Connect to Ethereum RPC
   - Fetch transaction receipts
   - Parse Deposit events from logs
   - Extract event data (depositHash, sender, amount, timestamp)

5. **Circuit Structure**
   - Circuit trait implementation
   - Public/private input definitions
   - Detailed TODO comments for implementation

6. **Build System**
   - Successfully compiles with axiom-eth dependencies
   - Release build working
   - No compilation errors

7. **Documentation**
   - `deposit-prover/README.md`: Usage guide
   - `docs/DEPOSIT_CIRCUIT_ARCHITECTURE.md`: Technical spec
   - Inline code documentation

### 🚧 In Progress

1. **MPT Proof Generation**
   - Need to fetch all receipts in block
   - Build receipt trie from scratch
   - Extract proof path for specific transaction
   - Status: Placeholder implementation with detailed TODOs

2. **RLP Encoding/Decoding**
   - Encode transaction receipts
   - Encode block headers
   - Decode event logs
   - Status: Not started

3. **Circuit Implementation**
   - Integrate axiom-eth MPT verification
   - Integrate axiom-eth RLP decoder
   - Integrate axiom-eth keccak chip
   - Integrate zkevm-hashes Poseidon
   - Status: Structure defined, logic not implemented

4. **Proof Generation**
   - Generate proving key
   - Generate verifying key
   - Create proof using snark-verifier-sdk
   - Serialize proof to JSON
   - Status: Not started

### ❌ Not Started

1. **Key Generation**
   - Implement `setup` command
   - Generate and save proving key
   - Generate and save verifying key

2. **Proof Verification**
   - Implement `verify` command
   - Load verifying key
   - Verify proof validity

3. **Integration Testing**
   - Deploy test contract on Sepolia
   - Make test deposit
   - Generate proof end-to-end
   - Verify proof on-chain

4. **Solidity Verifier**
   - Generate Solidity verifier contract
   - Deploy verifier to Sepolia
   - Test on-chain verification

## Technical Challenges

### Resolved ✅

1. **Dependency Conflicts**
   - Problem: axiom-eth requires axiom-crypto's halo2-lib, incompatible with scroll-tech
   - Solution: Created separate workspace for deposit-prover

2. **Ethers Compilation Error**
   - Problem: `TypedTransaction::DepositTransaction` pattern not covered
   - Solution: Used specific ethers-rs git revision

3. **Circuit Trait Implementation**
   - Problem: Missing `Params` associated type
   - Solution: Added `params()` method to Circuit impl

### Pending ⏸️

1. **MPT Proof Generation**
   - Challenge: No direct RPC method for receipt proofs
   - Options:
     - A) Build trie manually (most decentralized)
     - B) Use third-party service (Axiom, Herodotus)
     - C) Run custom Ethereum node with proof API
   - Recommendation: Implement option A

2. **RLP Encoding**
   - Challenge: Need to RLP encode receipts and block headers
   - Options:
     - A) Use existing Rust RLP library (rlp crate)
     - B) Use axiom-eth's RLP components
   - Recommendation: Use rlp crate for encoding, axiom-eth for in-circuit decoding

3. **Circuit Complexity**
   - Challenge: Large circuit (MPT + RLP + Poseidon + keccak)
   - May require circuit optimization
   - May need to split into multiple circuits

## Next Steps

### Priority 1: MPT Proof Generation

1. Add `rlp` crate dependency
2. Implement receipt RLP encoding
3. Implement block header RLP encoding
4. Fetch all receipts in block
5. Build receipt trie using `cita_trie` or similar
6. Extract proof path for specific transaction

### Priority 2: Circuit Implementation

1. Study axiom-eth examples
2. Implement MPT verification using axiom-eth
3. Implement RLP decoding using axiom-eth
4. Implement keccak using axiom-eth
5. Implement Poseidon using zkevm-hashes
6. Wire up all components

### Priority 3: Proof Generation

1. Implement key generation (setup command)
2. Implement proof generation using snark-verifier-sdk
3. Implement proof verification
4. Test end-to-end

### Priority 4: Integration

1. Generate Solidity verifier
2. Deploy to Sepolia
3. Update withdrawal contract to accept deposit proofs
4. Test full deposit → proof → withdrawal flow

## Timeline Estimate

| Task | Estimated Time | Dependencies |
|------|----------------|--------------|
| MPT Proof Generation | 2-3 days | None |
| RLP Encoding | 1 day | None |
| Circuit Implementation | 5-7 days | MPT, RLP |
| Proof Generation | 2-3 days | Circuit |
| Integration Testing | 2-3 days | Proof Gen |
| **Total** | **12-17 days** | |

## Resources

### Documentation
- [axiom-eth GitHub](https://github.com/axiom-crypto/axiom-eth)
- [Ethereum MPT Spec](https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/)
- [RLP Encoding](https://ethereum.org/en/developers/docs/data-structures-and-encoding/rlp/)
- [halo2-lib Docs](https://github.com/axiom-crypto/halo2-lib)

### Example Code
- [axiom-eth examples](https://github.com/axiom-crypto/axiom-eth/tree/main/axiom-query/examples)
- [zkBridge light client](https://github.com/TusimaNetwork/zkBridge-lightClient) (TypeScript/Circom reference)

### Libraries
- `rlp`: RLP encoding/decoding
- `cita_trie`: Merkle-Patricia Trie implementation
- `axiom-eth`: Ethereum state proof primitives
- `zkevm-hashes`: Poseidon hash for halo2

## Success Criteria

The deposit prover is considered complete when:

1. ✅ Compiles without errors
2. ⏸️ Can fetch Deposit events from Ethereum
3. ⏸️ Can generate MPT proofs for receipts
4. ⏸️ Can generate ZK proofs of event emission
5. ⏸️ Proofs can be verified on-chain
6. ⏸️ Full deposit → proof → withdrawal flow works on testnet

## Current Status: **Phase 1 Complete** 🎉

The scaffolding and architecture are complete. Ready to begin implementation of MPT proof generation and circuit logic.

