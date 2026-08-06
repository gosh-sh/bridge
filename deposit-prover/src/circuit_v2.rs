//! Deposit Event Proof Circuit - Axiom-eth Integration
//!
//! This circuit uses axiom-eth to prove that a Deposit event was emitted on
//! Ethereum. It follows the EthCircuitInstructions pattern from axiom-eth.

use axiom_eth::{
    mpt::MPTChip,
    receipt::{EthReceiptChip, EthReceiptChipParams, EthReceiptInputAssigned, EthReceiptWitness},
    rlc::{circuit::builder::RlcCircuitBuilder, FIRST_PHASE},
    rlp::{evaluate_byte_array, types::RlpArrayWitness},
    transaction::{
        EthTransactionChip, EthTransactionChipParams, EthTransactionInputAssigned,
        EthTransactionWitness,
    },
    utils::{build_utils::aggregation::CircuitMetadata, eth_circuit::EthCircuitInstructions},
};
use ethers_core::{types::Chain, utils::keccak256};
use halo2_base::{
    gates::{GateInstructions, RangeInstructions},
    halo2_proofs::halo2curves::bn256::Fr,
    utils::ScalarField,
    AssignedValue, Context,
};

use crate::types::{DepositProofInput, ReceiptProof, TransactionProof};

/// Circuit parameters (OPTION B+: Ultra-aggressively optimized to reduce
/// verifier size)
pub const MAX_DATA_BYTE_LEN: usize = 128; // Max event data length (reduced from 256)
pub const MAX_LOG_NUM: usize = 3; // Max number of logs in receipt (OPTION B+: ultra-aggressive)
pub const TOPIC_NUM_BOUNDS: (usize, usize) = (0, 4); // Min/max topics per log
pub const RECEIPT_PF_MAX_DEPTH: usize = 10; // Max MPT proof depth

/// Public-input layout, in slot order. This is the wire contract the AN-side
/// `USDCBridge._parsePublicInputs` reads by fixed offset, so it must not drift
/// silently — `deposit_circuit_declares_twelve_public_inputs` and
/// `num_instance_matches_the_declared_layout` pin it.
///
/// `promiseCommit` is appended by `EthCircuitImpl`, not by
/// `virtual_assign_phase0`, which is why the circuit sets 11 values but
/// declares 12.
pub const DEPOSIT_PUBLIC_INPUT_LAYOUT: [&str; 12] = [
    "depositId",
    "sender",
    "amount",
    "contractAddress",
    "chainId",
    "dappIdHigh",
    "dappIdLow",
    "anAccountHigh",
    "anAccountLow",
    "blockHashHigh",
    "blockHashLow",
    "promiseCommit",
];

/// Number of BN254 `Fr` public inputs the deposit proof carries.
pub const DEPOSIT_NUM_PUBLIC_INPUTS: usize = DEPOSIT_PUBLIC_INPUT_LAYOUT.len();

/// Slot of the proven L1 `chainId` (Track-2 chain binding).
pub const PI_CHAIN_ID: usize = 4;
/// Max depth of the transactions-trie MPT proof (mirrors receipt path).
pub const TX_PF_MAX_DEPTH: usize = 10;
/// Max calldata bytes for the enclosing EIP-1559 tx (deposit ABI is small).
pub const MAX_TX_CALLDATA_BYTE_LEN: usize = 256;
/// Max RLP-encoded access-list length (deposit txs typically have none).
pub const MAX_TX_ACCESS_LIST_LEN: usize = 64;
/// EIP-1559 type byte / circuit `transaction_type` value.
pub const EIP1559_TX_TYPE: u64 = 2;

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
/// witness.
///
/// Sized for the Prague/Isthmus 21-field header table: axiom-eth's
/// Cancun/Ecotone `MAINNET_HEADER_FIELDS_MAX_BYTES` (20 slots, 668 B) plus the
/// EIP-7685 `requestsHash`, and with `gasLimit` widened to 8 bytes so Arbitrum
/// One's 2^50 limit fits. Covers every shape the supported chains emit today:
/// Arbitrum (16 fields, no `withdrawalsRoot`), post-Shanghai (17), OP Stack
/// Ecotone (20: + `blobGasUsed` / `excessBlobGas` / `parentBeaconBlockRoot`),
/// and Prague / OP Isthmus (21: + `requestsHash` — Base, Mantle, World Chain,
/// OP Mainnet and Sepolia are all here as of 2026-07).
///
/// The value is the RLP-encoded worst case implied by
/// [`BLOCK_HEADER_MAX_FIELD_LENS`]: each field costs `max_len` plus its
/// string prefix, and the list itself costs a 3-byte prefix.
///
/// A field slot that is too narrow is a **liveness** cliff, not a soundness
/// hole: the RLP decode simply fails, so no deposit in such a block can ever be
/// proven, and — since `withdraw()` was retired in Phase 4.3 — its funds are
/// stuck. That asymmetry is why the integer slots are sized to the protocol
/// maximum (8 bytes) rather than to observed values: widening them costs 12
/// witness bytes and no extra keccak round (705 and 717 both need 6), whereas
/// discovering later that one was too narrow costs a VK rotation plus a
/// shellnet redeploy (BC-D06).
///
/// The receipt + MPT proof are already fixed-size (axiom-eth pads them to
/// `value_max_byte_len` / `max_depth`).
pub const MAX_BLOCK_HEADER_BYTES: usize = 717;

/// Per-field max byte lengths for `decompose_rlp_array_*`. Slots 0–19 follow
/// axiom-eth `MAINNET_HEADER_FIELDS_MAX_BYTES` (Cancun/Ecotone) except
/// `gasLimit`; slot 20 is the Prague / OP Isthmus `requestsHash`.
pub const BLOCK_HEADER_MAX_FIELD_LENS: [usize; 21] = [
    32,  // 0: parentHash
    32,  // 1: ommersHash
    20,  // 2: beneficiary (coinbase)
    32,  // 3: stateRoot
    32,  // 4: transactionsRoot
    32,  // 5: receiptsRoot
    256, // 6: logsBloom
    7,   // 7: difficulty
    8,   // 8: number (BC-D06: was 4 → 2^32 blocks; Arbitrum One is at 4.9e8)
    8,   // 9: gasLimit (Arbitrum One runs at 2^50 — 7 bytes)
    8,   // 10: gasUsed (BC-D06: was 4, while slot 9 allows 2^64)
    8,   // 11: timestamp (BC-D06: was 4 → overflows in 2106)
    32,  // 12: extraData (mainnet / OP Stack max)
    32,  // 13: mixHash / prevRandao
    8,   // 14: nonce
    32,  // 15: baseFeePerGas (post-London)
    32,  // 16: withdrawalsRoot (post-Shanghai)
    8,   // 17: blobGasUsed (post-Cancun / Ecotone)
    8,   // 18: excessBlobGas (post-Cancun / Ecotone)
    32,  // 19: parentBeaconBlockRoot (post-Cancun / Ecotone)
    32,  // 20: requestsHash (post-Prague / OP Isthmus, EIP-7685)
];

/// Worst-case RLP length implied by [`BLOCK_HEADER_MAX_FIELD_LENS`], so a
/// future slot change can't silently outgrow [`MAX_BLOCK_HEADER_BYTES`].
const fn max_block_header_rlp_len() -> usize {
    let mut payload = 0;
    let mut i = 0;
    while i < BLOCK_HEADER_MAX_FIELD_LENS.len() {
        let len = BLOCK_HEADER_MAX_FIELD_LENS[i];
        // Single-byte strings < 0x80 encode bare, but no header field is
        // capped at 1 byte, so every field carries a prefix.
        let prefix = if len <= 55 {
            1
        } else if len <= 255 {
            2
        } else {
            3
        };
        payload += len + prefix;
        i += 1;
    }
    // Payload > 255 → 0xf9 + two length bytes.
    payload + 3
}

const _: () = assert!(max_block_header_rlp_len() == MAX_BLOCK_HEADER_BYTES);

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
}

impl DepositEventCircuitV2 {
    /// Create a new circuit with the given inputs and configuration.
    ///
    /// # Arguments
    ///
    /// * `inputs` - The deposit proof input containing event data and receipt
    ///   proof
    /// * `config` - Circuit configuration (from prover module). It carries no
    ///   chain selector: the proven `chainId` is public input [`PI_CHAIN_ID`],
    ///   and the AN-side allowlist binds `(chainId → expected bridge Fr)`. The
    ///   CLI `--chain-id` flag only picks which RPC to fetch witnesses from and
    ///   is cross-checked against the witness by
    ///   [`DepositProofInput::require_chain_id`].
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
        }
    }
}

/// Output from Phase 0 (MPT verification + RLP decoding + block hash)
#[derive(Clone)]
pub struct Phase0Output {
    pub receipt_witness: EthReceiptWitness<Fr>,
    pub tx_witness: EthTransactionWitness<Fr>,
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
        let mpt_input = self.inputs.receipt_proof.to_mpt_input(
            self.inputs.event_data.transaction_index,
            self.params.max_data_byte_len,
            self.params.max_log_num,
        );
        let proof = mpt_input.assign(ctx);

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

        // Parse block header RLP to extract receiptsRoot (field index 5).
        // 21-slot Prague/Isthmus table (see `BLOCK_HEADER_MAX_FIELD_LENS`).
        let rlp_chip = chip.rlp();
        let block_header_max_field_lens = BLOCK_HEADER_MAX_FIELD_LENS.to_vec();

        let block_header_array = rlp_chip.decompose_rlp_array_phase0(
            ctx,
            block_header_rlp_bytes,
            &block_header_max_field_lens,
            true, // variable length (16–21 fields)
        );

        // BC-D03: the length fed to `keccak_var_len` and the length the RLP
        // decoder derived from the list prefix must be the same number.
        // Without this the two views of the header are independent: the prover
        // can append arbitrary trailing bytes inside the zero-padded witness,
        // move `block_header_len` past them to shift `blockHash`, and still
        // have `decompose_rlp_array` read the same `receiptsRoot` from the
        // prefix-delimited list. That would let one event carry unboundedly
        // many valid `blockHash` public inputs — none of them a real block, but
        // it voids the "the proof commits to the producing block" claim.
        ctx.constrain_equal(&block_header_len, &block_header_array.rlp_len);
        println!("   ✓ Constrained keccak length == RLP list length (BC-D03)");

        // Extract receiptsRoot (field 5)
        let receipts_root_bytes = block_header_array.field_witness[5].field_cells.to_vec();
        println!("   ✓ Extracted receiptsRoot from block header (32 bytes)");

        // Extract transactionsRoot (field 4) — chain-binding MPT target
        let transactions_root_bytes = block_header_array.field_witness[4].field_cells.to_vec();
        println!("   ✓ Extracted transactionsRoot from block header (32 bytes)");

        // ====================================================================
        // Track 2: bind enclosing EIP-1559 tx (chain_id + to + same tx_index)
        // ====================================================================
        // `from` is NOT in the typed-tx RLP (ECDSA-only); binding the Deposit
        // `sender` topic to the same `tx_index` as this MPT proof closes that
        // loop without an in-circuit ecrecover.
        println!("🔧 Phase 0: Transaction MPT + chain_id binding...");
        assert!(
            !self.inputs.tx_proof.tx_bytes.is_empty(),
            "tx_proof.tx_bytes is empty — regenerate DepositProofInput with \
             generate_transaction_proof / fetch_deposit_proof (Track 2 chain binding)"
        );
        let tx_chip_params = EthTransactionChipParams {
            max_data_byte_len: MAX_TX_CALLDATA_BYTE_LEN,
            max_access_list_len: MAX_TX_ACCESS_LIST_LEN,
            enable_types: [false, false, true], // EIP-1559 only
            network: self.params.network,
        };
        let tx_chip = EthTransactionChip::new(mpt, tx_chip_params);
        let tx_mpt_input = self.inputs.tx_proof.to_mpt_input(
            self.inputs.event_data.transaction_index,
            MAX_TX_CALLDATA_BYTE_LEN,
            MAX_TX_ACCESS_LIST_LEN,
        );
        let tx_mpt_proof = tx_mpt_input.assign(ctx);
        // Reuse the same `tx_idx` AssignedValue as the receipt path.
        let tx_input = EthTransactionInputAssigned {
            transaction_index: tx_idx,
            proof: tx_mpt_proof,
        };
        let tx_witness = tx_chip.parse_transaction_proof_phase0(ctx, tx_input);

        // MPT root == block header transactionsRoot
        for (pf_byte, hdr_byte) in tx_witness
            .mpt_witness()
            .root_hash_bytes
            .iter()
            .zip(transactions_root_bytes.iter())
        {
            ctx.constrain_equal(pf_byte, hdr_byte);
        }
        println!("   ✓ Constrained tx MPT root == transactionsRoot");

        // transaction_type == 2 (EIP-1559)
        let eip1559 = ctx.load_constant(Fr::from(EIP1559_TX_TYPE));
        ctx.constrain_equal(&tx_witness.transaction_type, &eip1559);
        println!("   ✓ Constrained tx type == EIP-1559 (0x02)");

        // Extract EIP-1559 chain_id (RLP field 0) and expose as a public input.
        // Not constrained to a VK-baked constant — AN allowlists (chainId →
        // expected bridge Fr); the relayer sanity-checks against eth_chainId.
        let chain_id_idx = ctx.load_constant(Fr::from(0u64));
        let chain_id_field =
            tx_chip.extract_field(ctx, tx_witness.clone(), chain_id_idx);
        let chain_id_val = evaluate_byte_array(
            ctx,
            tx_chip.gate(),
            &chain_id_field.field_bytes,
            chain_id_field.len,
        );
        println!("   ✓ Extracted chain_id (public input, not VK-constrained)");

        // BC-D02: `tx.to == contractAddress` used to be constrained here as
        // defence-in-depth on the emitter. It is REMOVED, deliberately.
        //
        // It bought nothing. The emitter is already pinned independently — the
        // log's RLP-decoded `address` is constrained equal to the
        // `contractAddress` public instance in Phase 1 — and `chain_id`'s
        // binding does not involve `to` at all: this tx is the *same* tx as the
        // receipt because both MPT proofs are opened at the same `tx_idx`
        // AssignedValue against roots taken from the same header.
        //
        // What it did cost was every deposit that does not call the bridge
        // directly: a Safe or any multisig (`to` = wallet), ERC-4337 (`to` =
        // EntryPoint), EIP-7702, and any router. `deposit()` accepts those
        // calls, so their funds entered the bridge and then could not be
        // proven — and the legacy `withdraw()` refund path was retired in
        // Phase 4.3, leaving no way out short of `emergencyWithdrawAll`.

        // MPT root the receipt inclusion was ACTUALLY verified against, taken from
        // `receipt_witness` (set by `parse_receipt_proof_phase0` above) rather than
        // reloaded as a fresh, prover-controlled witness from
        // `receipt_proof.receipt_root`. This is what binds the proven receipt to
        // *this* block's header: in Phase 1 it is constrained byte-for-byte equal to
        // the header's receiptsRoot (field 5). Mirrors the tx path
        // (`tx_witness.mpt_witness().root_hash_bytes` == transactionsRoot).
        //
        // Review finding #1 (CRITICAL): previously `mpt_root_bytes` was an
        // independent witness only tied to the header receiptsRoot, never to the
        // root the MPT chip verified against — so a prover could prove a real
        // receipt under root R_A while binding the header (and thus the blockHash
        // public input) of an unrelated block B, decoupling the event from its block.
        let mpt_root_bytes: Vec<AssignedValue<Fr>> =
            receipt_witness.mpt_witness().root_hash_bytes.to_vec();

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

        // 5. chainId - extracted EIP-1559 RLP field 0 (already computed above).
        //    Exposed as a public input so USDCBridge can allowlist
        //    (chainId → expected bridge Fr). Not constrained to a constant.

        // 6. dappId - Acki Nacki destination dApp identifier (UInt256), supplied
        //    from the bridge config (NOT from the Ethereum event). It replaced
        //    `anWorkchain` on 2026-06-02. A full UInt256 dappId can exceed the
        //    BN254 scalar modulus, so it is split into high/low 16-byte halves
        //    (mirrors the anAccount/block-hash split). These are witness values
        //    promoted to public instances; they are NOT constrained against
        //    event data — the AN-side `TokenBridge` checks them against its
        //    configured dappId, which is what binds the proof to a dApp.
        //
        //    BC-D05: unlike every other 32-byte input here, `dappId` has no
        //    Phase-1 counterpart to be constrained against, so these witnesses
        //    are otherwise entirely free. `bytes_to_field` does not range-check,
        //    so without the loop below each "byte" could be any field element
        //    and `dappIdHigh`/`dappIdLow` would not be 128-bit halves at all —
        //    while the AN side reassembles them as `(fr[5] << 128) | fr[6]`.
        //    An attacker gains nothing (dappId is freely chosen and checked
        //    against the configured value), but the public input must mean what
        //    the layout says it means.
        let dapp_id_bytes: Vec<AssignedValue<Fr>> = self
            .inputs
            .dapp_id
            .iter()
            .map(|&byte| ctx.load_witness(Fr::from(byte as u64)))
            .collect();
        let range = chip.range();
        for byte in dapp_id_bytes.iter() {
            range.range_check(ctx, *byte, 8);
        }
        let dapp_id_high = bytes_to_field(ctx, gate, &dapp_id_bytes[0..16]);
        let dapp_id_low = bytes_to_field(ctx, gate, &dapp_id_bytes[16..32]);

        // 7. anAccount - Acki Nacki destination account (256-bit), split into high/low
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

        // 8. blockHashHigh - from block hash (first 16 bytes)
        let block_hash_high = bytes_to_field(ctx, gate, &block_hash_bytes[0..16]);

        // 9. blockHashLow - from block hash (last 16 bytes)
        let block_hash_low = bytes_to_field(ctx, gate, &block_hash_bytes[16..32]);

        // Set public instances BEFORE promise_commit is added.
        // Layout (11 user values + promise_commit appended by EthCircuitImpl):
        //   [depositId, sender, amount, contractAddress, chainId,
        //    dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
        //    blockHashHigh, blockHashLow, (promiseCommit)]
        // All except dappId{High,Low} and chainId are verified in Phase 1
        // against the RLP-parsed event data; chainId is bound via tx MPT +
        // EIP-1559 decode; dappId is a config-supplied tag (see above).
        let public_instances = vec![
            deposit_id_field,
            sender_field,
            amount_field,
            contract_address_field,
            chain_id_val,
            dapp_id_high,
            dapp_id_low,
            an_account_high,
            an_account_low,
            block_hash_high,
            block_hash_low,
        ];

        builder.base.assigned_instances[0] = public_instances;

        println!("   ✓ Set 11 explicit public instances in Phase 0 (chainId at PI[4])");
        println!("   (promise_commit is appended automatically → 12 total, matches num_instance())");
        println!("   (Phase 1 will verify these match the RLP-parsed event data)");

        Phase0Output {
            receipt_witness,
            tx_witness,
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
            true, // variable length (15–20 fields)
        );
        println!("   ✓ Verified block header RLC");

        // 2. Parse receipt in phase 1 (RLC verification)
        let _receipt_trace = chip
            .parse_receipt_proof_phase1((ctx_gate, ctx_rlc), phase0_output.receipt_witness.clone());
        println!("   ✓ Verified receipt RLC");

        // 2b. Transaction RLC (chain-binding witness from Phase 0)
        let tx_chip_params = EthTransactionChipParams {
            max_data_byte_len: MAX_TX_CALLDATA_BYTE_LEN,
            max_access_list_len: MAX_TX_ACCESS_LIST_LEN,
            enable_types: [false, false, true],
            network: self.params.network,
        };
        let tx_chip = EthTransactionChip::new(mpt, tx_chip_params);
        let _tx_trace =
            tx_chip.parse_transaction_proof_phase1((ctx_gate, ctx_rlc), phase0_output.tx_witness);
        println!("   ✓ Verified transaction RLC");

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

        // BC-D04: pin the payload lengths of the two variable-length log fields.
        //
        // Everything below reads `topics_rlp` and `data_bytes` at FIXED offsets
        // into `field_cells`, which is zero-padded up to `log_max_field_lens`.
        // Without these two constraints a log with fewer topics — or a shorter
        // data payload — decodes fine, and `depositId` / `sender` / `anAccount`
        // get read out of the padding instead of out of the event. The event
        // signature check at topic 0 does not help: it only says *something* at
        // offset 1..33 equals the signature.
        //
        //   topics: 3 × (0xa0 prefix + 32) = 99   [event_sig, depositId, sender]
        //   data:   4 × 32 = 128                  [amount, anWorkchain,
        //                                          anAccount, timestamp]
        let expected_topics_len = ctx_gate.load_constant(Fr::from(99u64));
        ctx_gate.constrain_equal(&log_array.field_witness[1].field_len, &expected_topics_len);
        let expected_data_len = ctx_gate.load_constant(Fr::from(128u64));
        ctx_gate.constrain_equal(&log_array.field_witness[2].field_len, &expected_data_len);
        println!("   ✓ Constrained topics len == 99, data len == 128 (BC-D04)");

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
        // review finding #2: `bytes_to_field` folds 32 BE bytes into one Fr, which is
        // injective only for values below the BN254 scalar modulus. depositId is the
        // AN-side anti-replay anchor; pin its top byte to zero (value < 2^248 « p) so
        // two distinct on-chain ids can never collide mod p.
        if deposit_id_bytes.len() == 32 {
            let zero = ctx_gate.load_constant(Fr::zero());
            ctx_gate.constrain_equal(&deposit_id_bytes[0], &zero);
        }
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
        // review finding #2: the AN contract reads `amount` as uint128. Require the
        // high 16 bytes to be zero (amount < 2^128 « p): this makes the 32-byte→Fr
        // fold injective AND rejects any deposit whose amount would silently truncate
        // to uint128 on the contract side.
        {
            let zero = ctx_gate.load_constant(Fr::zero());
            for b in amount_bytes.iter().take(16) {
                ctx_gate.constrain_equal(b, &zero);
            }
        }
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

        // CRITICAL: bind the block-header receiptsRoot to the root the MPT chip
        // actually verified the receipt inclusion against (`mpt_root_bytes`, set in
        // Phase 0 from `receipt_witness.mpt_witness().root_hash_bytes`).
        //
        // Compare byte-for-byte rather than folding each 32-byte root into a single
        // Fr with `bytes_to_field`: a keccak root can exceed the BN254 scalar
        // modulus, so the fold is non-injective and two distinct roots could collide
        // mod p (review finding #2). Byte-wise equality is exact and mirrors the
        // transactionsRoot binding on the tx path.
        assert_eq!(phase0_output.receipts_root_bytes.len(), 32);
        assert_eq!(phase0_output.mpt_root_bytes.len(), 32);
        for (hdr_byte, mpt_byte) in phase0_output
            .receipts_root_bytes
            .iter()
            .zip(phase0_output.mpt_root_bytes.iter())
        {
            ctx_gate.constrain_equal(hdr_byte, mpt_byte);
        }
        println!("   ✓ Verified receiptsRoot == MPT-verified receipt root (byte-wise)");

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
            // Receipt trie keys are RLP(tx_index). Must match axiom-eth's
            // receipt/tx providers (`max_key_byte_len: 3`) and
            // `TRANSACTION_IDX_MAX_LEN = 2` (tx_index ≤ 65535):
            //   max_key_byte_len = 1 + max_rlp_len_len(2) + 2 = 3
            // where max_rlp_len_len(2) = 0 because 2 ≤ 55.
            //
            // Using 4 (or 32, the storage-trie keccak key width) desyncs the
            // MPT chip padding from axiom-eth's RLP key decomposition and
            // breaks MockProver / under-constrained padding slots.
            // Storage tries use 32; receipt/tx tries use 3 — never mix them.
            max_key_byte_len: 3,
            key_byte_len: Some(path_len),
        }
    }
}

impl TransactionProof {
    /// Convert to axiom-eth `MPTInput` for the transactions trie.
    fn to_mpt_input(
        &self,
        tx_index: u64,
        max_data_byte_len: usize,
        max_access_list_len: usize,
    ) -> axiom_eth::mpt::MPTInput {
        use axiom_eth::{mpt::MPTInput, transaction::calc_max_val_len};
        use ethers_core::types::H256;

        let path_bytes = crate::rlp_utils::encode_tx_index(tx_index);
        let path_len = path_bytes.len();
        let value_max_byte_len =
            calc_max_val_len(max_data_byte_len, max_access_list_len, [false, false, true]);

        MPTInput {
            path: axiom_eth::mpt::PathBytes(path_bytes),
            value: self.tx_bytes.clone(),
            root_hash: H256::from_slice(&self.transactions_root),
            proof: self.proof_nodes.clone(),
            slot_is_empty: false,
            value_max_byte_len,
            max_depth: TX_PF_MAX_DEPTH,
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
    /// We expose 12 public inputs (11 user values + promise_commit, which is
    /// appended automatically by EthCircuitImpl at the end of Phase 0):
    /// [depositId, sender, amount, contractAddress, chainId,
    ///  dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
    ///  blockHashHigh, blockHashLow, promiseCommit]
    ///
    /// `chainId` is the EIP-1559 RLP field-0 value extracted from the enclosing
    /// tx (MPT-bound under `transactionsRoot`). It is **not** VK-baked — the
    /// AN-side `USDCBridge` allowlists `(chainId → expected bridge Fr)`.
    /// `dappIdHigh`/`dappIdLow` (the UInt256 Acki Nacki dApp identifier) replaced
    /// the single `anWorkchain` slot on 2026-06-02. dappId is a config-supplied
    /// tag — it is not bound to event data in-circuit; the AN-side
    /// `TokenBridge.finalizeDeposit` checks it against its configured dappId.
    /// `anAccountHigh`/`anAccountLow` remain the event-bound AN recipient account.
    /// The `ZKHALO2VERIFYWITHVK` consumer is VK-driven (it reads this count from
    /// the VkBlob), and `TokenBridge.finalizeDeposit` builds the public-inputs
    /// cell from the same 12-scalar layout.
    fn num_instance(&self) -> Vec<usize> {
        vec![DEPOSIT_NUM_PUBLIC_INPUTS] // 11 user values + 1 promise_commit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{supported_chains::CHAIN_ID_SEPOLIA, types::DepositEventData};

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
            chain_id: 11155111,
        };

        let receipt_proof = ReceiptProof {
            receipt_rlp: vec![],
            proof_nodes: vec![],
            receipt_root: [0u8; 32],
            block_header_rlp: vec![],
        };

        let tx_proof = TransactionProof {
            tx_bytes: vec![0x02],
            proof_nodes: vec![],
            transactions_root: [0u8; 32],
        };

        let input = DepositProofInput {
            event_data,
            receipt_proof,
            tx_proof,
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

    /// The public-input layout is the wire contract the AN-side
    /// `USDCBridge._parsePublicInputs` reads by fixed offset, and it has moved
    /// three times in one quarter (7 → 10 → 11 → 12). This pins the width the
    /// circuit declares.
    ///
    /// Deliberately witness-free: the previous version of this test loaded
    /// `../e2e_attack_test_data/valid_proof.json`, which is `.gitignore`d, and
    /// was additionally `#[ignore]`d — so it never ran for anyone, and its
    /// doc-comment still described the 7-input layout while its assert demanded
    /// 12 (BC-D08).
    #[test]
    fn deposit_circuit_declares_twelve_public_inputs() {
        assert_eq!(
            DEPOSIT_PUBLIC_INPUT_LAYOUT.len(),
            DEPOSIT_NUM_PUBLIC_INPUTS,
            "layout table and declared width disagree"
        );
        assert_eq!(
            DEPOSIT_PUBLIC_INPUT_LAYOUT[PI_CHAIN_ID], "chainId",
            "chainId must stay at slot {PI_CHAIN_ID}"
        );
        assert_eq!(
            *DEPOSIT_PUBLIC_INPUT_LAYOUT.last().unwrap(),
            "promiseCommit",
            "promise_commit is appended by EthCircuitImpl and must stay last"
        );
    }

    /// `num_instance()` is what `keygen_vk` bakes into the VK, so it — not the
    /// layout table — is what a verifier will enforce. Keep them equal.
    #[test]
    fn num_instance_matches_the_declared_layout() {
        let circuit = DepositEventCircuitV2::new_with_defaults(minimal_input(), Chain::Sepolia);
        assert_eq!(
            circuit.num_instance(),
            vec![DEPOSIT_NUM_PUBLIC_INPUTS],
            "num_instance() drifted from DEPOSIT_PUBLIC_INPUT_LAYOUT"
        );
    }

    /// Shape-only `DepositProofInput` — enough to construct the circuit struct
    /// and read its declared instance count. Not provable.
    fn minimal_input() -> DepositProofInput {
        DepositProofInput {
            event_data: DepositEventData {
                block_number: 1,
                transaction_index: 0,
                log_index: 0,
                deposit_id: 0,
                sender: [0u8; 20],
                amount: [0u8; 32],
                an_workchain: 0,
                an_account: [0u8; 32],
                timestamp: 0,
                contract_address: [0u8; 20],
                chain_id: CHAIN_ID_SEPOLIA,
            },
            receipt_proof: ReceiptProof {
                receipt_rlp: vec![],
                proof_nodes: vec![],
                receipt_root: [0u8; 32],
                block_header_rlp: vec![],
            },
            tx_proof: TransactionProof {
                tx_bytes: vec![0x02],
                proof_nodes: vec![],
                transactions_root: [0u8; 32],
            },
            dapp_id: [0u8; 32],
        }
    }
}
