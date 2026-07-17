//! Deposit Event Proof Circuit - Axiom-eth Integration
//!
//! This circuit uses axiom-eth to prove that a Deposit event was emitted on
//! Ethereum. It follows the EthCircuitInstructions pattern from axiom-eth.

use axiom_eth::{
    mpt::MPTChip,
    receipt::{EthReceiptChip, EthReceiptChipParams, EthReceiptInputAssigned, EthReceiptWitness},
    rlc::{circuit::builder::RlcCircuitBuilder, FIRST_PHASE},
    rlp::types::RlpArrayWitness,
    utils::{build_utils::aggregation::CircuitMetadata, eth_circuit::EthCircuitInstructions},
};
use ethers_core::{types::Chain, utils::keccak256};
use halo2_base::{
    gates::GateInstructions, halo2_proofs::halo2curves::bn256::Fr, utils::ScalarField,
    AssignedValue, Context,
};

use crate::types::{DepositProofInput, ReceiptProof};

/// Audit PoC hook: corrupt MPT key witness before `MPTInput::assign`.
///
/// Production prove paths must leave this `None`. Used to test whether
/// `max_key_byte_len > axiom-eth reference (3)` accepts non-zero padding bytes
/// while `key_byte_len` claims a shorter prefix.
#[derive(Clone, Debug, Default)]
pub struct MptWitnessMutation {
    /// Override `MPTInput.max_key_byte_len` (PoC: 4 vs axiom-eth reference 3).
    pub max_key_byte_len: Option<usize>,
    /// After honest `assign`, overwrite `key_bytes[idx]` (simulates padding-slot garbage).
    pub corrupt_key_byte_at: Option<(usize, u8)>,
}

impl MptWitnessMutation {
    pub fn apply_input(&self, mpt_input: &mut axiom_eth::mpt::MPTInput) {
        if let Some(max) = self.max_key_byte_len {
            mpt_input.max_key_byte_len = max;
        }
    }

    pub fn apply_assigned<F: ScalarField>(
        &self,
        proof: &mut axiom_eth::mpt::MPTProof<F>,
        ctx: &mut Context<F>,
    ) {
        if let Some((idx, byte)) = self.corrupt_key_byte_at {
            proof.key_bytes[idx] = ctx.load_witness(F::from(byte as u64));
        }
    }
}

/// Circuit parameters (OPTION B+: Ultra-aggressively optimized to reduce
/// verifier size)
pub const MAX_DATA_BYTE_LEN: usize = 128; // Max event data length (reduced from 256)
pub const MAX_LOG_NUM: usize = 3; // Max number of logs in receipt (OPTION B+: ultra-aggressive)
pub const TOPIC_NUM_BOUNDS: (usize, usize) = (0, 4); // Min/max topics per log
pub const RECEIPT_PF_MAX_DEPTH: usize = 10; // Max MPT proof depth

/// Fixed maximum block-header RLP length, in bytes.
///
/// UNIVERSAL-VK INVARIANT: the block header is the only input the circuit loads
/// at its *natural* length. Different blocks have different header lengths (the
/// `number` / `gasUsed` / `baseFeePerGas` fields use a variable number of bytes),
/// so loading it raw made the constraint system — and therefore the verifying
/// key — witness-dependent. We instead load a FIXED-size, zero-padded witness
/// vector of this length so `keccak_var_len` and `decompose_rlp_array_*` emit an
/// identical number of cells / copy-constraints for every block. The true header
/// length is still bound cryptographically via the `keccak_var_len` length
/// witness. Post-Shanghai mainnet headers (17 fields, through `withdrawalsRoot`)
/// are ~540-640 bytes; 640 covers them with margin and matches the 17-entry
/// `block_header_max_field_lens` table below. The receipt + MPT proof are already
/// fixed-size (axiom-eth pads them to `value_max_byte_len` / `max_depth`).
pub const MAX_BLOCK_HEADER_BYTES: usize = 640;

/// Expected event signature:
/// keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)") This is
/// computed off-circuit and used as a constant.
///
/// NOTE: the two new non-indexed fields (`int8 anWorkchain`, `bytes32
/// anAccount`) sit in the log data between `amount` (word 0) and `timestamp`
/// (now word 3). The circuit's existing parsing of `amount`/`sender`/
/// `depositId`/`contractAddress`/`blockHash` is unaffected — only `timestamp`
/// (which the circuit does not expose) moved.
pub fn get_deposit_event_signature() -> [u8; 32] {
    keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")
}

/// Helper function to convert bytes (big-endian) to field element using
/// Horner's method This matches the circuit's bytes_to_field() logic
fn bytes_to_field<F: ScalarField>(
    ctx: &mut Context<F>,
    gate: &impl GateInstructions<F>,
    bytes: &[AssignedValue<F>],
) -> AssignedValue<F> {
    let mut result = ctx.load_constant(F::ZERO);
    let base = ctx.load_constant(F::from(256));

    for &byte in bytes.iter() {
        result = gate.mul_add(ctx, result, base, byte);
    }

    result
}

/// Deposit event circuit using axiom-eth
#[derive(Clone)]
pub struct DepositEventCircuitV2 {
    pub inputs: DepositProofInput,
    pub params: EthReceiptChipParams,
    /// Audit-only MPT witness corruption (`None` in production).
    pub mpt_mutation: Option<MptWitnessMutation>,
}

impl DepositEventCircuitV2 {
    /// Create a new circuit with the given inputs and configuration.
    ///
    /// # Arguments
    ///
    /// * `inputs` - The deposit proof input containing event data and receipt
    ///   proof
    /// * `config` - Circuit configuration (from prover module)
    pub fn new(inputs: DepositProofInput, config: &crate::prover::CircuitConfig) -> Self {
        let params = EthReceiptChipParams {
            max_data_byte_len: config.max_data_byte_len,
            max_log_num: config.max_log_num,
            topic_num_bounds: config.topic_num_bounds,
            network: Some(Chain::Mainnet), // Default to mainnet
        };
        Self {
            inputs,
            params,
            mpt_mutation: None,
        }
    }

    /// Create a new circuit with default parameters (for backward
    /// compatibility).
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
            mpt_mutation: None,
        }
    }
}

/// Output from Phase 0 (MPT verification + RLP decoding + block hash)
#[derive(Clone)]
pub struct Phase0Output {
    pub receipt_witness: EthReceiptWitness<Fr>,
    pub log_index: AssignedValue<Fr>,
    pub block_hash_bytes: Vec<AssignedValue<Fr>>, // 32 bytes from keccak256(block_header_rlp)
    pub receipts_root_bytes: Vec<AssignedValue<Fr>>, // 32 bytes from block header field 5
    pub mpt_root_bytes: Vec<AssignedValue<Fr>>,   // 32 bytes from MPT proof
    pub block_header_witness: RlpArrayWitness<Fr>, /* Block header RLP witness for Phase 1
                                                   * verification */
    // FIX BC-CIRCUIT-004: Store public instance values for Phase 1 verification
    pub deposit_id_phase0: AssignedValue<Fr>,
    pub sender_phase0: AssignedValue<Fr>,
    pub amount_phase0: AssignedValue<Fr>,
    pub contract_address_phase0: AssignedValue<Fr>,
    // AN-recipient binding (2026-06-02): the Acki Nacki destination account
    // carried by the `Deposit` event (`bytes32 anAccount`). An EVM address is
    // not a valid AN recipient, so the destination account is bound here as
    // public inputs and credited on the AN side. The `dappId` public input
    // (which replaced `anWorkchain` on 2026-06-02) is a config-supplied tag and
    // is NOT verified against event data in Phase 1, so it is not stored here.
    pub an_account_high_phase0: AssignedValue<Fr>,
    pub an_account_low_phase0: AssignedValue<Fr>,
    pub block_hash_high_phase0: AssignedValue<Fr>,
    pub block_hash_low_phase0: AssignedValue<Fr>,
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
        println!(
            "   ✓ Loaded tx_index: {}",
            self.inputs.event_data.transaction_index
        );

        // 2. Convert receipt proof to MPTInput and assign
        let mut mpt_input = self.inputs.receipt_proof.to_mpt_input(
            self.inputs.event_data.transaction_index,
            self.params.max_data_byte_len,
            self.params.max_log_num,
        );
        if let Some(mutation) = &self.mpt_mutation {
            mutation.apply_input(&mut mpt_input);
        }
        let mut proof = mpt_input.assign(ctx);
        if let Some(mutation) = &self.mpt_mutation {
            mutation.apply_assigned(&mut proof, ctx);
        }

        // 3. Create receipt input
        let rc_input = EthReceiptInputAssigned {
            tx_idx,
            proof,
        };

        // 4. Parse receipt proof (verifies MPT inclusion)
        let receipt_witness = chip.parse_receipt_proof_phase0(ctx, rc_input);
        println!("   ✓ Verified MPT inclusion proof");

        // 5. Load log index
        let log_index = ctx.load_witness(Fr::from(self.inputs.event_data.log_index as u64));
        println!(
            "   ✓ Loaded log_index: {}",
            self.inputs.event_data.log_index
        );

        // 6. Parse block header RLP and compute block hash (MUST be in Phase 0 for
        //    Keccak)
        println!("🔧 Parsing block header and computing block hash...");

        // Load block header RLP bytes into a FIXED-size, zero-padded witness
        // vector (see `MAX_BLOCK_HEADER_BYTES`). This is what makes the verifying
        // key universal: the number of witness cells, keccak rounds and RLP
        // decode constraints no longer depends on the block's natural header
        // length. The true length is bound via `keccak_var_len` below.
        let actual_header = &self.inputs.receipt_proof.block_header_rlp;
        assert!(
            actual_header.len() <= MAX_BLOCK_HEADER_BYTES,
            "block header RLP is {} bytes, exceeds MAX_BLOCK_HEADER_BYTES ({})",
            actual_header.len(),
            MAX_BLOCK_HEADER_BYTES
        );
        let block_header_rlp_bytes: Vec<AssignedValue<Fr>> = (0..MAX_BLOCK_HEADER_BYTES)
            .map(|i| {
                let byte = actual_header.get(i).copied().unwrap_or(0u8);
                ctx.load_witness(Fr::from(byte as u64))
            })
            .collect();

        // Compute block hash using Keccak (MUST be in Phase 0). The length
        // witness is the TRUE header length, so the keccak output binds only the
        // real header bytes even though the input vector is fixed-size.
        let keccak_chip = chip.keccak();
        let block_header_len = ctx.load_witness(Fr::from(actual_header.len() as u64));
        let block_hash_query = keccak_chip.keccak_var_len(
            ctx,
            block_header_rlp_bytes.clone(),
            block_header_len,
            0, // No minimum length
        );
        let block_hash_bytes = block_hash_query.output_bytes.as_ref().to_vec();
        println!("   ✓ Computed block hash (32 bytes)");

        // Parse block header RLP to extract receiptsRoot (field index 5)
        let rlp_chip = chip.rlp();
        let block_header_max_field_lens = vec![
            32,  // 0: parentHash
            32,  // 1: ommersHash
            32,  // 2: beneficiary
            32,  // 3: stateRoot
            32,  // 4: transactionsRoot
            32,  // 5: receiptsRoot ← WE NEED THIS
            256, // 6: logsBloom
            32,  // 7: difficulty
            32,  // 8: number
            32,  // 9: gasLimit
            32,  // 10: gasUsed
            32,  // 11: timestamp
            32,  // 12: extraData
            32,  // 13: mixHash
            8,   // 14: nonce
            32,  // 15: baseFeePerGas (post-London)
            32,  // 16: withdrawalsRoot (post-Shanghai, optional)
        ];

        let block_header_array = rlp_chip.decompose_rlp_array_phase0(
            ctx,
            block_header_rlp_bytes,
            &block_header_max_field_lens,
            true, // variable length (15-17 fields)
        );

        // Extract receiptsRoot (field 5)
        let receipts_root_bytes = block_header_array.field_witness[5].field_cells.to_vec();
        println!("   ✓ Extracted receiptsRoot from block header (32 bytes)");

        // Get MPT root from receipt proof
        let mpt_root_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .receipt_proof
            .receipt_root
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();

        // ============================================================================
        // FIX BC-CIRCUIT-004: Compute public instances in Phase 0
        // ============================================================================
        // We set public instances from witness data in Phase 0, then verify in Phase 1
        // that the RLP-parsed event data matches these values.
        //
        // This ensures the proof is cryptographically bound to all 6 public values.

        println!("🔧 Computing public instances in Phase 0...");

        let gate = chip.gate();

        // 1. depositId - from witness data
        let deposit_id_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .event_data
            .deposit_id
            .to_be_bytes()
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();
        let deposit_id_field = bytes_to_field(ctx, gate, &deposit_id_bytes);

        // 2. sender - from witness data (20 bytes, left-padded to 32 bytes)
        let mut sender_bytes_32 = vec![ctx.load_constant(Fr::zero()); 12]; // 12 zero bytes
        sender_bytes_32.extend(
            self.inputs
                .event_data
                .sender
                .iter()
                .map(|&byte| ctx.load_witness(Fr::from(byte as u64))),
        );
        let sender_field = bytes_to_field(ctx, gate, &sender_bytes_32);

        // 3. amount - from witness data (32 bytes)
        let amount_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .event_data
            .amount
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();
        let amount_field = bytes_to_field(ctx, gate, &amount_bytes);

        // 4. contractAddress - from witness data (20 bytes, left-padded to 32 bytes)
        let mut contract_address_bytes_32 = vec![ctx.load_constant(Fr::zero()); 12]; // 12 zero bytes
        contract_address_bytes_32.extend(
            self.inputs
                .event_data
                .contract_address
                .iter()
                .map(|&byte| ctx.load_witness(Fr::from(byte as u64))),
        );
        let contract_address_field = bytes_to_field(ctx, gate, &contract_address_bytes_32);

        // 5. dappId - Acki Nacki destination dApp identifier (UInt256), supplied
        //    from the bridge config (NOT from the Ethereum event). It replaced
        //    `anWorkchain` on 2026-06-02. A full UInt256 dappId can exceed the
        //    BN254 scalar modulus, so it is split into high/low 16-byte halves
        //    (mirrors the anAccount/block-hash split). These are witness values
        //    promoted to public instances; they are NOT constrained against
        //    event data — the AN-side `TokenBridge` checks them against its
        //    configured dappId, which is what binds the proof to a dApp.
        let dapp_id_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .dapp_id
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();
        let dapp_id_high = bytes_to_field(ctx, gate, &dapp_id_bytes[0..16]);
        let dapp_id_low = bytes_to_field(ctx, gate, &dapp_id_bytes[16..32]);

        // 6. anAccount - Acki Nacki destination account (256-bit), split into high/low
        //    16-byte halves (mirrors the block-hash split) so each fits a BN254 field
        //    element. Matches log data word 2.
        let an_account_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .event_data
            .an_account
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();
        let an_account_high = bytes_to_field(ctx, gate, &an_account_bytes[0..16]);
        let an_account_low = bytes_to_field(ctx, gate, &an_account_bytes[16..32]);

        // 7. blockHashHigh - from block hash (first 16 bytes)
        let block_hash_high = bytes_to_field(ctx, gate, &block_hash_bytes[0..16]);

        // 8. blockHashLow - from block hash (last 16 bytes)
        let block_hash_low = bytes_to_field(ctx, gate, &block_hash_bytes[16..32]);

        // Set public instances BEFORE promise_commit is added.
        // Layout (10 user values + promise_commit appended by EthCircuitImpl):
        //   [depositId, sender, amount, contractAddress,
        //    dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
        //    blockHashHigh, blockHashLow, (promiseCommit)]
        // All except dappId{High,Low} are verified in Phase 1 against the
        // RLP-parsed event data; dappId is a config-supplied tag (see above).
        let public_instances = vec![
            deposit_id_field,
            sender_field,
            amount_field,
            contract_address_field,
            dapp_id_high,
            dapp_id_low,
            an_account_high,
            an_account_low,
            block_hash_high,
            block_hash_low,
        ];

        builder.base.assigned_instances[0] = public_instances;

        println!("   ✓ Set 10 public instances in Phase 0");
        println!("   (promise_commit will be appended automatically)");
        println!("   (Phase 1 will verify these match the RLP-parsed event data)");

        Phase0Output {
            receipt_witness,
            log_index,
            block_hash_bytes,
            receipts_root_bytes,
            mpt_root_bytes,
            block_header_witness: block_header_array,
            // Store Phase 0 values for verification in Phase 1
            deposit_id_phase0: deposit_id_field,
            sender_phase0: sender_field,
            amount_phase0: amount_field,
            contract_address_phase0: contract_address_field,
            an_account_high_phase0: an_account_high,
            an_account_low_phase0: an_account_low,
            block_hash_high_phase0: block_hash_high,
            block_hash_low_phase0: block_hash_low,
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

        // 1. Verify block header RLP in Phase 1 (RLC verification)
        // This ensures the block header RLP encoding is cryptographically sound
        let rlp_chip = chip.rlp();
        let _block_header_trace = rlp_chip.decompose_rlp_array_phase1(
            (ctx_gate, ctx_rlc),
            phase0_output.block_header_witness,
            true, // variable length (15-17 fields)
        );
        println!("   ✓ Verified block header RLC");

        // 2. Parse receipt in phase 1 (RLC verification)
        let _receipt_trace = chip
            .parse_receipt_proof_phase1((ctx_gate, ctx_rlc), phase0_output.receipt_witness.clone());
        println!("   ✓ Verified receipt RLC");

        // 3. Extract the specific log at log_index
        let log_witness = chip.extract_receipt_log(
            ctx_gate,
            &phase0_output.receipt_witness,
            phase0_output.log_index,
        );
        println!(
            "   ✓ Extracted log at index {}",
            self.inputs.event_data.log_index
        );
        println!("   Log length: {:?}", log_witness.log_len);
        println!(
            "   Log bytes (first 16): {:?}",
            &log_witness.log_bytes[0..16.min(log_witness.log_bytes.len())]
        );

        // 4. Parse log RLP structure: [address, topics[], data]

        // Log structure has 3 fields: address (20 bytes), topics (array of 32-byte
        // hashes), data (variable). Max lengths: address=20,
        // topics=4*32+overhead=150, data=128 (4 ABI words:
        // amount + anWorkchain + anAccount + timestamp).
        let log_max_field_lens = [20, 150, 128];

        let log_array = rlp_chip.decompose_rlp_array_phase0(
            ctx_gate,
            log_witness.log_bytes.clone(),
            &log_max_field_lens,
            false, // fixed length (always 3 fields)
        );
        println!("   ✓ Parsed log RLP structure");

        // 5. Extract address (field 0)
        let address_bytes = &log_array.field_witness[0].field_cells;
        println!("   Address bytes: {} bytes", address_bytes.len());

        // Debug: print actual address bytes
        if address_bytes.len() >= 20 {
            let addr_vals: Vec<u64> = address_bytes[0..20]
                .iter()
                .map(|v| v.value().get_lower_64())
                .collect();
            println!("   Address (hex): {:02x?}", addr_vals);
        }

        // 6. Extract topics (field 1)
        // Topics are NOT an RLP list - they're just concatenated RLP-encoded 32-byte
        // strings Each topic is: 0xa0 (1 byte RLP prefix) + 32 bytes of data =
        // 33 bytes total For Deposit event: [event_sig, depositId, sender] = 3
        // topics = 99 bytes total
        let topics_rlp = &log_array.field_witness[1].field_cells;
        println!("   Topics field: {} bytes", topics_rlp.len());

        // Manually extract each topic by slicing the concatenated bytes
        // Topic 0 (event signature): bytes 0-32 (skip byte 0 which is 0xa0)
        // Topic 1 (depositId): bytes 33-65 (skip byte 33 which is 0xa0)
        // Topic 2 (sender): bytes 66-98 (skip byte 66 which is 0xa0)

        // 7. Extract event signature (topic 0: bytes 1-32, skipping byte 0 which is
        //    0xa0)
        let event_sig_bytes: Vec<AssignedValue<Fr>> = if topics_rlp.len() >= 33 {
            topics_rlp[1..33].to_vec()
        } else {
            vec![]
        };

        // Debug: print actual event signature
        if event_sig_bytes.len() >= 16 {
            let sig_vals: Vec<u64> = event_sig_bytes[0..16]
                .iter()
                .map(|v| v.value().get_lower_64())
                .collect();
            println!("   Event sig (first 16 bytes): {:02x?}", &sig_vals[0..16]);
        }

        // 8. Extract depositId (topic 1: bytes 34-65, skipping byte 33 which is 0xa0)
        let deposit_id_bytes: Vec<AssignedValue<Fr>> = if topics_rlp.len() >= 66 {
            topics_rlp[34..66].to_vec()
        } else {
            vec![]
        };
        println!("   DepositId: {} bytes", deposit_id_bytes.len());

        // 9. Extract sender (topic 2: bytes 67-98, skipping byte 66 which is 0xa0)
        let sender_bytes: Vec<AssignedValue<Fr>> = if topics_rlp.len() >= 99 {
            topics_rlp[67..99].to_vec()
        } else {
            vec![]
        };
        println!("   Sender: {} bytes", sender_bytes.len());

        // 10. Extract data field (field 2)
        // Data contains 4 ABI words:
        //   [amount (32), anWorkchain (32), anAccount (32), timestamp (32)]
        let data_bytes = &log_array.field_witness[2].field_cells;
        println!("   Data: {} bytes", data_bytes.len());

        // 11. Verify event signature
        // Load expected event signature as constant
        let expected_sig = get_deposit_event_signature();
        println!("   Expected event signature: {:02x?}", &expected_sig[0..16]);
        let expected_sig_bytes: Vec<AssignedValue<Fr>> = expected_sig
            .iter()
            .map(|&byte| ctx_gate.load_constant(Fr::from(byte as u64)))
            .collect();

        // Constrain that event_sig_bytes equals expected_sig_bytes
        // Both should be 32 bytes
        if event_sig_bytes.len() != 32 {
            println!(
                "   WARNING: Event signature is {} bytes, expected 32",
                event_sig_bytes.len()
            );
        }
        assert_eq!(
            expected_sig_bytes.len(),
            32,
            "Expected signature should be 32 bytes"
        );

        // Constrain equality for all 32 bytes
        let min_len = event_sig_bytes.len().min(expected_sig_bytes.len());
        for i in 0..min_len {
            ctx_gate.constrain_equal(&event_sig_bytes[i], &expected_sig_bytes[i]);
        }
        println!("   ✓ Verified event signature ({} bytes)", min_len);

        // 12. Sanity-check the extracted contract address length.
        //
        // NOTE: We intentionally do NOT load `self.inputs.event_data.contract_address`
        // as an in-circuit CONSTANT to constrain `address_bytes` against. Doing so
        // (the previous implementation) injected the per-deposit contract address
        // into the single fixed column, which made the verifying key depend on the
        // contract address — so a VK built for one bridge address would not verify
        // a deposit from another, and "one embedded VK verifies every deposit"
        // silently broke. The address binding is fully preserved WITHOUT a constant:
        // the RLP-extracted address (`address_bytes` -> `contract_address_field`,
        // below) is constrained equal to the Phase-0 PUBLIC INSTANCE
        // `contract_address_phase0` (public input #3), and the AN-side
        // `TokenBridge.finalizeDeposit` checks that public input against the bridge's
        // configured deposit-source address. Keeping the address out of the fixed
        // column makes the VK witness-independent (see
        // `docs/deposit_vk_witness_independence.md`).
        assert_eq!(
            address_bytes.len(),
            20,
            "Contract address should be 20 bytes"
        );
        println!("   ✓ Extracted contract address (bound to public input in Phase 1)");

        // 13. Convert bytes to field elements for public outputs
        // Topics are already 32 bytes each (uint256 in Solidity)
        // We need to convert them from bytes to a single field element

        // Get the gate chip for arithmetic operations
        let gate = chip.gate();

        // Convert depositId (32 bytes from topics[1])
        let deposit_id_field = bytes_to_field(ctx_gate, gate, &deposit_id_bytes);
        println!("   ✓ Converted depositId to field element");

        // Convert sender (32 bytes from topics[2], but only last 20 bytes are the
        // address) Ethereum addresses are 20 bytes, but stored as uint256 (32
        // bytes) in topics The first 12 bytes should be zero, last 20 bytes are
        // the address
        let sender_field = bytes_to_field(ctx_gate, gate, &sender_bytes);
        println!("   ✓ Converted sender to field element");

        // Convert amount (data word 0: bytes 0..32)
        let amount_bytes = &data_bytes[0..32.min(data_bytes.len())];
        let amount_field = bytes_to_field(ctx_gate, gate, amount_bytes);
        println!("   ✓ Converted amount to field element");

        // Data word 1 (bytes 32..64) is the event's `anWorkchain` field. As of
        // 2026-06-02 the circuit no longer binds it (the `dappId` config tag
        // took its public-input slot), so it is intentionally not parsed here.

        // Convert anAccount (data word 2: bytes 64..96), split high/low halves.
        let an_account_high_phase1 = bytes_to_field(ctx_gate, gate, &data_bytes[64..80]);
        let an_account_low_phase1 = bytes_to_field(ctx_gate, gate, &data_bytes[80..96]);
        println!("   ✓ Converted anAccount to field elements");

        // Convert contract address (20 bytes)
        let contract_address_field = bytes_to_field(ctx_gate, gate, address_bytes);
        println!("   ✓ Converted contract address to field element");

        // 14. Verify block header receiptsRoot matches MPT root
        // (Block hash and receiptsRoot were already computed in Phase 0)
        println!("🔧 Verifying block header...");

        // Convert receiptsRoot from Phase 0 to field element
        let receipts_root_field =
            bytes_to_field(ctx_gate, gate, &phase0_output.receipts_root_bytes);

        // Convert MPT root from Phase 0 to field element
        let mpt_root_field = bytes_to_field(ctx_gate, gate, &phase0_output.mpt_root_bytes);

        // CRITICAL: Constrain that receiptsRoot from block header equals MPT root
        ctx_gate.constrain_equal(&receipts_root_field, &mpt_root_field);
        println!("   ✓ Verified receiptsRoot matches MPT root");

        // 15. Use block hash from Phase 0
        println!("🔧 Using block hash from Phase 0...");
        let block_hash_bytes = &phase0_output.block_hash_bytes;

        // Convert block hash to field elements (split into two 128-bit chunks)
        // Block hash is 32 bytes = 256 bits, but field elements are ~254 bits
        // So we split it into two 128-bit (16-byte) chunks
        let block_hash_high_phase1 = bytes_to_field(ctx_gate, gate, &block_hash_bytes[0..16]);
        let block_hash_low_phase1 = bytes_to_field(ctx_gate, gate, &block_hash_bytes[16..32]);

        println!("   ✓ Converted block hash to field elements");

        // ============================================================================
        // FIX BC-CIRCUIT-004: Verify Phase 1 values match Phase 0 public instances
        // ============================================================================
        // The public instances were set in Phase 0 from witness data.
        // Now we verify that the RLP-parsed event data matches those public instances.
        //
        // This ensures:
        // 1. The proof is cryptographically bound to the public instances (Phase 0)
        // 2. The public instances match the actual RLP-verified event data (Phase 1)

        println!("🔧 Verifying Phase 1 extracted values match Phase 0 public instances...");

        // Constrain that Phase 1 values equal Phase 0 values
        ctx_gate.constrain_equal(&deposit_id_field, &phase0_output.deposit_id_phase0);
        ctx_gate.constrain_equal(&sender_field, &phase0_output.sender_phase0);
        ctx_gate.constrain_equal(&amount_field, &phase0_output.amount_phase0);
        ctx_gate.constrain_equal(
            &contract_address_field,
            &phase0_output.contract_address_phase0,
        );
        // dappId is config-supplied (not in the event) and therefore not
        // constrained against RLP-parsed data here.
        ctx_gate.constrain_equal(
            &an_account_high_phase1,
            &phase0_output.an_account_high_phase0,
        );
        ctx_gate.constrain_equal(&an_account_low_phase1, &phase0_output.an_account_low_phase0);
        ctx_gate.constrain_equal(
            &block_hash_high_phase1,
            &phase0_output.block_hash_high_phase0,
        );
        ctx_gate.constrain_equal(&block_hash_low_phase1, &phase0_output.block_hash_low_phase0);

        println!("   ✓ Verified event-bound public instances match RLP-verified event data");
        println!("   ✓ Phase 1 complete!");
    }
}

/// Helper trait for converting ReceiptProof to MPTInput
trait ToMPTInput {
    fn to_mpt_input(
        &self,
        tx_index: u64,
        max_data_byte_len: usize,
        max_log_num: usize,
    ) -> axiom_eth::mpt::MPTInput;
}

impl ToMPTInput for ReceiptProof {
    fn to_mpt_input(
        &self,
        tx_index: u64,
        max_data_byte_len: usize,
        max_log_num: usize,
    ) -> axiom_eth::mpt::MPTInput {
        use axiom_eth::mpt::MPTInput;
        use ethers_core::types::H256;

        // Encode transaction index as RLP (this is the key in the receipt trie)
        let path_bytes = crate::rlp_utils::encode_tx_index(tx_index);
        let path_len = path_bytes.len();

        // Calculate value_max_byte_len using axiom-eth's formula
        // This is the maximum size of the RLP-encoded receipt
        // Formula from axiom-eth/src/receipt/mod.rs:calc_max_val_len
        let max_topic_num = TOPIC_NUM_BOUNDS.1; // max topics = 4
        let max_log_len = 3 + 21 + 3 + 33 * max_topic_num + 3 + max_data_byte_len + 1;
        let value_max_byte_len = 4 + 33 + 33 + 259 + 4 + max_log_num * max_log_len;

        MPTInput {
            path: axiom_eth::mpt::PathBytes(path_bytes),
            value: self.receipt_rlp.clone(),
            root_hash: H256::from_slice(&self.receipt_root),
            proof: self.proof_nodes.clone(),
            slot_is_empty: false,
            value_max_byte_len,
            max_depth: RECEIPT_PF_MAX_DEPTH,
            // Receipt trie keys are RLP(tx_index):
            // RLP encoding for integers:
            // - tx_index 0-127: 1 byte (the value itself, no prefix)
            // - tx_index 128-255: 2 bytes (0x81 prefix + 1 value byte)
            // - tx_index 256-65535: 3 bytes (0x82 prefix + 2 value bytes)
            // - tx_index 65536-16777215: 4 bytes (0x83 prefix + 3 value bytes)
            //
            // axiom-eth uses TRANSACTION_IDX_MAX_LEN = 2 (supports up to 65535 txs)
            // Formula: max_key_byte_len = 1 + max_rlp_len_len(2) + 2
            //                           = 1 + 0 + 2 = 3
            // where max_rlp_len_len(2) = 0 because 2 <= 55 (no length-of-length bytes)
            //
            // Canonical (QC-PROV-03 closed 2026-07-17): must match axiom-eth receipt reference.
            max_key_byte_len: 3,
            key_byte_len: Some(path_len),
        }
    }
}

/// Implement CircuitMetadata for proof generation compatibility
impl CircuitMetadata for DepositEventCircuitV2 {
    /// This circuit does not use aggregation, so no accumulator
    const HAS_ACCUMULATOR: bool = false;

    /// Number of public instance columns.
    ///
    /// We expose 11 public inputs (10 user values + promise_commit, which is
    /// appended automatically by EthCircuitImpl at the end of Phase 0):
    /// [depositId, sender, amount, contractAddress,
    ///  dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
    ///  blockHashHigh, blockHashLow, promiseCommit]
    ///
    /// `dappIdHigh`/`dappIdLow` (the UInt256 Acki Nacki dApp identifier) replaced
    /// the single `anWorkchain` slot on 2026-06-02. dappId is a config-supplied
    /// tag — it is not bound to event data in-circuit; the AN-side
    /// `TokenBridge.finalizeDeposit` checks it against its configured dappId.
    /// `anAccountHigh`/`anAccountLow` remain the event-bound AN recipient account.
    /// The `ZKHALO2VERIFYWITHVK` consumer is VK-driven (it reads this count from
    /// the VkBlob), and `TokenBridge.finalizeDeposit` builds the public-inputs
    /// cell from the same 11-scalar layout.
    fn num_instance(&self) -> Vec<usize> {
        vec![11] // 10 user values + 1 promise_commit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DepositEventData;

    #[test]
    fn test_circuit_creation() {
        // FIX BC-TYPES-001: amount is now [u8; 32] instead of u64
        let mut amount = [0u8; 32];
        // 1 ETH = 1000000000000000000 wei = 0x0DE0B6B3A7640000
        amount[24..32].copy_from_slice(&1000000000000000000u64.to_be_bytes());

        let event_data = DepositEventData {
            block_number: 12345,
            transaction_index: 0,
            log_index: 0,
            deposit_id: 42,
            sender: [1u8; 20],
            amount,
            an_workchain: 0,
            an_account: [3u8; 32],
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
            dapp_id: [7u8; 32],
        };

        let circuit = DepositEventCircuitV2::new_with_defaults(input, Chain::Sepolia);
        assert_eq!(circuit.params.max_data_byte_len, MAX_DATA_BYTE_LEN);
        assert_eq!(circuit.params.max_log_num, MAX_LOG_NUM);
    }

    #[test]
    fn test_deposit_event_signature() {
        let sig = get_deposit_event_signature();
        assert_eq!(sig.len(), 32);
        // keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")
        let expected = [
            0x8d, 0x5d, 0x06, 0x06, 0x73, 0xb2, 0x7f, 0xac, 0x84, 0xd5, 0x6e, 0xe2, 0x62, 0xfe,
            0x8d, 0xcc, 0xad, 0x60, 0xd1, 0x98, 0xae, 0x11, 0x76, 0x60, 0x63, 0xf1, 0x12, 0xa9,
            0xbe, 0x3d, 0x37, 0xee,
        ];
        assert_eq!(sig, expected);
    }

    /// BC-CIRCUIT-004 Test: Verify that public instances are properly included
    /// in the proof
    ///
    /// This test verifies the fix for BC-CIRCUIT-004 where public instances
    /// were not being constrained. The fix ensures that:
    /// 1. All 6 user values are set as public instances in Phase 0
    /// 2. promise_commit is automatically appended (7th instance)
    /// 3. Phase 1 constrains the public instances to equal RLP-verified data
    ///
    /// Expected behavior:
    /// - instances[0] should contain 7 values: [depositId, sender, amount,
    ///   contractAddress, blockHashHigh, blockHashLow, promise_commit]
    #[test]
    #[ignore] // Requires real proof data
    fn test_bc_circuit_004_instances_in_proof() {
        use std::fs;

        use snark_verifier_sdk::Snark;

        use crate::types::DepositProofOutput;

        // Load a real proof from e2e test data
        let proof_path = "../e2e_attack_test_data/valid_proof.json";
        if !std::path::Path::new(proof_path).exists() {
            println!("Skipping test: proof file not found");
            return;
        }

        let json_str = fs::read_to_string(proof_path).expect("Failed to read proof file");
        let proof_output: DepositProofOutput =
            serde_json::from_str(&json_str).expect("Failed to parse proof JSON");

        // Deserialize SNARK
        let snark: Snark =
            bincode::deserialize(&proof_output.proof).expect("Failed to deserialize SNARK");

        println!("SNARK instances:");
        println!("  Number of instance columns: {}", snark.instances.len());
        assert_eq!(snark.instances.len(), 1, "Should have 1 instance column");

        println!("  Column 0: {} instances", snark.instances[0].len());

        // 11-input layout: [depositId, sender, amount, contractAddress,
        // dappIdHigh, dappIdLow, anAccountHigh, anAccountLow, blockHashHigh,
        // blockHashLow, promise_commit]
        assert_eq!(
            snark.instances[0].len(),
            11,
            "Should have 11 instances: [depositId, sender, amount, contractAddress, dappIdHigh, \
             dappIdLow, anAccountHigh, anAccountLow, blockHashHigh, blockHashLow, promise_commit]"
        );

        println!("✅ Proof contains all 11 instances!");
    }
}
