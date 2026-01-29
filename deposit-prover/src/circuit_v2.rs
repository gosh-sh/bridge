//! Deposit Event Proof Circuit - Axiom-eth Integration
//!
//! This circuit uses axiom-eth to prove that a Deposit event was emitted on Ethereum.
//! It follows the EthCircuitInstructions pattern from axiom-eth.

use crate::types::{DepositProofInput, ReceiptProof};
use axiom_eth::{
    keccak::KeccakChip,
    mpt::MPTChip,
    receipt::{EthReceiptChip, EthReceiptChipParams, EthReceiptInputAssigned, EthReceiptWitness},
    rlc::{
        chip::RlcChip,
        circuit::builder::{RlcCircuitBuilder, RlcContextPair},
        FIRST_PHASE,
    },
    rlp::RlpChip,
    utils::{
        assign_vec,
        build_utils::aggregation::CircuitMetadata,
        eth_circuit::EthCircuitInstructions,
    },
};
use ethers_core::{types::Chain, utils::keccak256};
use halo2_base::{
    gates::{GateChip, GateInstructions, RangeChip},
    halo2_proofs::halo2curves::bn256::Fr,
    AssignedValue, Context,
    QuantumCell::Constant,
};
use std::marker::PhantomData;

/// Circuit parameters
pub const MAX_DATA_BYTE_LEN: usize = 256; // Max event data length
pub const MAX_LOG_NUM: usize = 20; // Max number of logs in receipt
pub const TOPIC_NUM_BOUNDS: (usize, usize) = (0, 4); // Min/max topics per log
pub const RECEIPT_PF_MAX_DEPTH: usize = 10; // Max MPT proof depth

/// Expected event signature: keccak256("Deposit(uint256,address,uint256,uint256)")
/// This is computed off-circuit and used as a constant
pub fn get_deposit_event_signature() -> [u8; 32] {
    keccak256("Deposit(uint256,address,uint256,uint256)")
}

/// Deposit event circuit using axiom-eth
#[derive(Clone)]
pub struct DepositEventCircuitV2 {
    pub inputs: DepositProofInput,
    pub params: EthReceiptChipParams,
}

impl DepositEventCircuitV2 {
    /// Create a new circuit with the given inputs and configuration.
    ///
    /// # Arguments
    ///
    /// * `inputs` - The deposit proof input containing event data and receipt proof
    /// * `config` - Circuit configuration (from prover module)
    pub fn new(inputs: DepositProofInput, config: &crate::prover::CircuitConfig) -> Self {
        let params = EthReceiptChipParams {
            max_data_byte_len: config.max_data_byte_len,
            max_log_num: config.max_log_num,
            topic_num_bounds: config.topic_num_bounds,
            network: Some(Chain::Mainnet), // Default to mainnet
        };
        Self { inputs, params }
    }

    /// Create a new circuit with default parameters (for backward compatibility).
    pub fn new_with_defaults(inputs: DepositProofInput, network: Chain) -> Self {
        let params = EthReceiptChipParams {
            max_data_byte_len: MAX_DATA_BYTE_LEN,
            max_log_num: MAX_LOG_NUM,
            topic_num_bounds: TOPIC_NUM_BOUNDS,
            network: Some(network),
        };
        Self {
            inputs,
            params,
        }
    }
}

/// Output from Phase 0 (MPT verification + RLP decoding)
#[derive(Clone)]
pub struct Phase0Output {
    pub receipt_witness: EthReceiptWitness<Fr>,
    pub log_index: AssignedValue<Fr>,
}

impl EthCircuitInstructions<Fr> for DepositEventCircuitV2 {
    type FirstPhasePayload = Phase0Output;

    fn virtual_assign_phase0(
        &self,
        builder: &mut RlcCircuitBuilder<Fr>,
        mpt: &MPTChip<Fr>,
    ) -> Self::FirstPhasePayload {
        let ctx = builder.base.main(FIRST_PHASE);
        let chip = EthReceiptChip::new(mpt, self.params);

        println!("🔧 Phase 0: MPT Verification + RLP Decoding");

        // 1. Load transaction index
        let tx_idx = ctx.load_witness(Fr::from(self.inputs.event_data.transaction_index));
        println!("   ✓ Loaded tx_index: {}", self.inputs.event_data.transaction_index);

        // 2. Convert receipt proof to MPTInput and assign
        let mpt_input = self.inputs.receipt_proof.to_mpt_input(self.inputs.event_data.transaction_index);
        let proof = mpt_input.assign(ctx);

        // 3. Create receipt input
        let rc_input = EthReceiptInputAssigned { tx_idx, proof };

        // 4. Parse receipt proof (verifies MPT inclusion)
        let receipt_witness = chip.parse_receipt_proof_phase0(ctx, rc_input);
        println!("   ✓ Verified MPT inclusion proof");

        // 5. Load log index
        let log_index = ctx.load_witness(Fr::from(self.inputs.event_data.log_index as u64));
        println!("   ✓ Loaded log_index: {}", self.inputs.event_data.log_index);

        Phase0Output {
            receipt_witness,
            log_index,
        }
    }

    fn virtual_assign_phase1(
        &self,
        builder: &mut RlcCircuitBuilder<Fr>,
        mpt: &MPTChip<Fr>,
        phase0_output: Self::FirstPhasePayload,
    ) {
        let chip = EthReceiptChip::new(mpt, self.params);
        let (ctx_gate, ctx_rlc) = builder.rlc_ctx_pair();

        println!("🔧 Phase 1: Event Verification");

        // 1. Parse receipt in phase 1 (RLC verification)
        let _receipt_trace = chip.parse_receipt_proof_phase1(
            (ctx_gate, ctx_rlc),
            phase0_output.receipt_witness.clone(),
        );
        println!("   ✓ Verified receipt RLC");

        // 2. Extract the specific log at log_index
        let log_witness = chip.extract_receipt_log(
            ctx_gate,
            &phase0_output.receipt_witness,
            phase0_output.log_index,
        );
        println!("   ✓ Extracted log at index {}", self.inputs.event_data.log_index);
        println!("   Log length: {:?}", log_witness.log_len);
        println!("   Log bytes (first 16): {:?}", &log_witness.log_bytes[0..16.min(log_witness.log_bytes.len())]);

        // 3. Parse log RLP structure: [address, topics[], data]
        let rlp_chip = chip.rlp();

        // Log structure has 3 fields: address (20 bytes), topics (array of 32-byte hashes), data (variable)
        // Max lengths: address=20, topics=4*32+overhead=150, data=64+overhead=70
        let log_max_field_lens = [20, 150, 70];

        let log_array = rlp_chip.decompose_rlp_array_phase0(
            ctx_gate,
            log_witness.log_bytes.clone(),
            &log_max_field_lens,
            false, // fixed length (always 3 fields)
        );
        println!("   ✓ Parsed log RLP structure");

        // 4. Extract address (field 0)
        let address_bytes = &log_array.field_witness[0].field_cells;
        println!("   Address bytes: {} bytes", address_bytes.len());

        // 5. Parse topics array (field 1)
        // Topics is an RLP array of 32-byte hashes
        // For Deposit event: [event_sig, depositId, sender] = 3 topics
        let topics_rlp = &log_array.field_witness[1].field_cells;
        let topic_max_lens = [32, 32, 32, 32]; // max 4 topics, each 32 bytes

        let topics_array = rlp_chip.decompose_rlp_array_phase0(
            ctx_gate,
            topics_rlp.clone(),
            &topic_max_lens,
            true, // variable length (can have 0-4 topics)
        );
        println!("   ✓ Parsed topics array");

        // 6. Extract event signature (topics[0])
        let event_sig_bytes = &topics_array.field_witness[0].field_cells;
        println!("   Event signature: {} bytes", event_sig_bytes.len());

        // 7. Extract depositId (topics[1])
        let deposit_id_bytes = &topics_array.field_witness[1].field_cells;
        println!("   DepositId: {} bytes", deposit_id_bytes.len());

        // 8. Extract sender (topics[2])
        let sender_bytes = &topics_array.field_witness[2].field_cells;
        println!("   Sender: {} bytes", sender_bytes.len());

        // 9. Extract data field (field 2)
        // Data contains: [amount (32 bytes), timestamp (32 bytes)]
        let data_bytes = &log_array.field_witness[2].field_cells;
        println!("   Data: {} bytes", data_bytes.len());

        // 10. Verify event signature
        // Load expected event signature as constant
        let expected_sig = get_deposit_event_signature();
        let expected_sig_bytes: Vec<AssignedValue<Fr>> = expected_sig
            .iter()
            .map(|&byte| ctx_gate.load_constant(Fr::from(byte as u64)))
            .collect();

        // Constrain that event_sig_bytes equals expected_sig_bytes
        // Both should be 32 bytes
        assert_eq!(event_sig_bytes.len(), 32, "Event signature should be 32 bytes");
        assert_eq!(expected_sig_bytes.len(), 32, "Expected signature should be 32 bytes");

        for (actual, expected) in event_sig_bytes.iter().zip(expected_sig_bytes.iter()) {
            ctx_gate.constrain_equal(actual, expected);
        }
        println!("   ✓ Verified event signature");

        // 11. Verify contract address
        // Load expected contract address from inputs
        let expected_address = &self.inputs.event_data.contract_address;
        let expected_address_bytes: Vec<AssignedValue<Fr>> = expected_address
            .iter()
            .map(|&byte| ctx_gate.load_witness(Fr::from(byte as u64)))
            .collect();

        // Constrain that address_bytes equals expected_address_bytes
        // Address should be 20 bytes
        assert_eq!(address_bytes.len(), 20, "Contract address should be 20 bytes");
        assert_eq!(expected_address_bytes.len(), 20, "Expected address should be 20 bytes");

        for (actual, expected) in address_bytes.iter().zip(expected_address_bytes.iter()) {
            ctx_gate.constrain_equal(actual, expected);
        }
        println!("   ✓ Verified contract address");

        // 12. Convert bytes to field elements for public outputs
        // Topics are already 32 bytes each (uint256 in Solidity)
        // We need to convert them from bytes to a single field element

        // Get the gate chip for arithmetic operations
        let gate = chip.gate();

        // Helper function to convert bytes (big-endian) to field element
        let mut bytes_to_field = |bytes: &[AssignedValue<Fr>]| -> AssignedValue<Fr> {
            // Convert bytes to field element using Horner's method
            // value = bytes[0] * 256^(n-1) + bytes[1] * 256^(n-2) + ... + bytes[n-1]
            let mut result = ctx_gate.load_zero();
            let base = ctx_gate.load_constant(Fr::from(256));

            for byte in bytes.iter() {
                // result = result * 256 + byte
                result = gate.mul_add(ctx_gate, result, base, *byte);
            }
            result
        };

        // Convert depositId (32 bytes from topics[1])
        let deposit_id_field = bytes_to_field(deposit_id_bytes);
        println!("   ✓ Converted depositId to field element");

        // Convert sender (32 bytes from topics[2], but only last 20 bytes are the address)
        // Ethereum addresses are 20 bytes, but stored as uint256 (32 bytes) in topics
        // The first 12 bytes should be zero, last 20 bytes are the address
        let sender_field = bytes_to_field(sender_bytes);
        println!("   ✓ Converted sender to field element");

        // Convert amount (first 32 bytes of data)
        // We need to extract the first 32 bytes from data_bytes
        let amount_bytes = &data_bytes[0..32.min(data_bytes.len())];
        let amount_field = bytes_to_field(amount_bytes);
        println!("   ✓ Converted amount to field element");

        // Convert contract address (20 bytes)
        let contract_address_field = bytes_to_field(address_bytes);
        println!("   ✓ Converted contract address to field element");

        // 13. Expose public outputs
        // The public inputs will be verified by the Solidity verifier
        // Order: [depositId, sender, amount, contract_address]
        let public_instances = builder.public_instances();
        public_instances[0].push(deposit_id_field);
        public_instances[0].push(sender_field);
        public_instances[0].push(amount_field);
        public_instances[0].push(contract_address_field);

        println!("   ✓ Exposed public outputs:");
        println!("     - depositId");
        println!("     - sender");
        println!("     - amount");
        println!("     - contract_address");

        println!("   ✓ Phase 1 complete!");
    }
}

/// Helper trait for converting ReceiptProof to MPTInput
trait ToMPTInput {
    fn to_mpt_input(&self, tx_index: u64) -> axiom_eth::mpt::MPTInput;
}

impl ToMPTInput for ReceiptProof {
    fn to_mpt_input(&self, tx_index: u64) -> axiom_eth::mpt::MPTInput {
        use axiom_eth::mpt::MPTInput;
        use ethers_core::types::H256;
        use rlp::RlpStream;

        // Encode transaction index as RLP (this is the key in the receipt trie)
        let mut rlp_stream = RlpStream::new();
        rlp_stream.append(&tx_index);
        let path_bytes = rlp_stream.out().to_vec();
        let path_len = path_bytes.len();

        MPTInput {
            path: axiom_eth::mpt::PathBytes(path_bytes),
            value: self.receipt_rlp.clone(),
            root_hash: H256::from_slice(&self.receipt_root),
            proof: self.proof_nodes.clone(),
            slot_is_empty: false,
            value_max_byte_len: MAX_DATA_BYTE_LEN * 2, // Conservative estimate
            max_depth: RECEIPT_PF_MAX_DEPTH,
            max_key_byte_len: 32,
            key_byte_len: Some(path_len),
        }
    }
}

/// Implement CircuitMetadata for proof generation compatibility
impl CircuitMetadata for DepositEventCircuitV2 {
    /// This circuit does not use aggregation, so no accumulator
    const HAS_ACCUMULATOR: bool = false;

    /// Number of public instance columns
    /// We expose: [depositId, sender, amount, contract_address]
    fn num_instance(&self) -> Vec<usize> {
        vec![4] // 4 public outputs in a single instance column
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DepositEventData;

    #[test]
    fn test_circuit_creation() {
        let event_data = DepositEventData {
            block_number: 12345,
            transaction_index: 0,
            log_index: 0,
            deposit_id: 42,
            sender: [1u8; 20],
            amount: 1000000000000000000,
            timestamp: 1234567890,
            contract_address: [2u8; 20],
        };

        let receipt_proof = ReceiptProof {
            receipt_rlp: vec![],
            proof_nodes: vec![],
            receipt_root: [0u8; 32],
            block_header_rlp: vec![],
        };

        let input = DepositProofInput {
            event_data,
            receipt_proof,
        };

        let circuit = DepositEventCircuitV2::new_with_defaults(input, Chain::Sepolia);
        assert_eq!(circuit.params.max_data_byte_len, MAX_DATA_BYTE_LEN);
        assert_eq!(circuit.params.max_log_num, MAX_LOG_NUM);
    }
}

