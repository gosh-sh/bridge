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
    utils::{assign_vec, eth_circuit::EthCircuitInstructions},
};
use ethers_core::types::Chain;
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

/// Deposit event circuit using axiom-eth
#[derive(Clone)]
pub struct DepositEventCircuitV2 {
    pub inputs: DepositProofInput,
    pub params: EthReceiptChipParams,
}

impl DepositEventCircuitV2 {
    pub fn new(inputs: DepositProofInput, network: Chain) -> Self {
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

        // TODO: Verify event signature matches keccak256("Deposit(uint256,address,uint256,uint256)")
        // TODO: Verify contract address matches expected bridge contract
        // TODO: Convert bytes to field elements for public outputs
        // TODO: Expose public outputs: depositId, sender, amount, contract_address

        println!("   ✓ Phase 1 complete (log parsing done, verification TODO)");
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

        let circuit = DepositEventCircuitV2::new(input, Chain::Sepolia);
        assert_eq!(circuit.params.max_data_byte_len, MAX_DATA_BYTE_LEN);
        assert_eq!(circuit.params.max_log_num, MAX_LOG_NUM);
    }
}

