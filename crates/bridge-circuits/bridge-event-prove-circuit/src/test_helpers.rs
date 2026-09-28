//! Test / synthetic-witness helpers for `bridge-event-prove-circuit`.
//!
//! Most helpers consume **real BOCs** captured from a local Acki Nacki node
//! by `acki-nacki/tests/exchange/generate_withdrawals.py` and shipped as
//! `bridge-event-prove-circuit/withdrawals.txt`. There is no synthetic
//! event-BOC builder — fixtures must originate from the live contract — but
//! the surrounding two-level Merkle witnesses and dense chain are built
//! synthetically by [`build_two_level_tree`] / [`build_dense_chain`].
//!
//! This module is exposed publicly (not `#[cfg(test)]`) so downstream crates
//! such as `bridge-prover-lib::keys::ensure_event_keys` can reuse the same
//! deterministic synthetic-witness path for halo2 keygen.

use rand::{rngs::StdRng, SeedableRng};

use crate::{
    boc_helper::*,
    event_data_helper::{read_withdrawals_from_file, WithdrawalRecord},
    event_primitives::{
        be_bytes_to_fr, poseidon_hash_96_native, ABI_EVENT_ID, BODY_CELL_LEN, EVENT_ABI_PREFIX_END,
        EVENT_ABI_PREFIX_START, EVENT_TOKEN_ID_END, EVENT_TOKEN_ID_START, RECIPIENT_CELL_LEN,
        RECIPIENT_HALF_LEN, RECIPIENT_HI_END, RECIPIENT_HI_START, RECIPIENT_LEN_FIXED,
        RECIPIENT_LO_END, RECIPIENT_LO_START, SENDER_CELL_LEN,
    },
    multi_hop_proof::BridgeMultiHopProof,
    multi_hop_witness::{
        block_merkle_leaf_proof, block_merkle_root, proof_block_ref_inner_path_native,
        proof_block_refs_root_native, BlockWitness, HopWitness, BLOCK_MERKLE_DEPTH,
        BLOCK_MERKLE_LEAF_COUNT, H_HOPS_PER_PROOF, MAX_PROOF_BLOCK_REFS_DEPTH,
    },
};

/// Captured-fixture file contents, baked into the binary so downstream
/// crates don't need filesystem access to drive keygen / selftest paths.
pub const EMBEDDED_WITHDRAWALS_TXT: &str = include_str!("../withdrawals.txt");
use dense_balanced_tree::{
    dense_merkle_proof, dense_merkle_root, PoseidonHasher as DensePoseidonHasher,
};
use gosh_dense_balanced_tree::{bytes_to_fr, fr_to_bytes, DenseChainLink, MAX_CHAIN_LEN};
use halo2_base::{gates::circuit::BaseCircuitParams, halo2_proofs::halo2curves::bn256::Fr};
use rand::Rng;
use tvm_block::{Deserializable, Message, Serializable};

pub const K: u32 = 19;

/// Conservative base-circuit params for first-cut tests. Dark-dex eventually
/// tightened these to K=14 with ~110 advice columns; that tuning is future
/// work for this crate.
pub fn base_circuit_params() -> BaseCircuitParams {
    BaseCircuitParams {
        k: K as usize,
        num_advice_per_phase: vec![16],
        num_fixed: 1,
        num_lookup_advice_per_phase: vec![2],
        lookup_bits: Some(18),
        num_instance_columns: 1,
    }
}

pub fn ceil_log2(n: usize) -> usize {
    assert!(n > 0);
    if n == 1 {
        return 0;
    }
    let mut k = 0usize;
    let mut v = 1usize;
    while v < n {
        v <<= 1;
        k += 1;
    }
    k
}

/// All values extracted from a single parsed `WithdrawalInitiated` BOC,
/// ready to drive the circuit and the instance column.
pub struct WithdrawalFields {
    pub entries: [BocFlattenData; 4],
    pub repr_hash: [u8; 32],
    /// PUBLIC-instance Fr values, BE-decoded from the parsed BOC, matching
    /// the in-circuit `inner_product` extractions in
    /// `bridge_event_final_proof::synthesize`.
    pub token_id_val: Fr,
    pub amount_val: Fr,
    pub dst_chain_id_val: Fr,
    pub recipient_hi_val: Fr,
    pub recipient_lo_val: Fr,
    /// Sender's account_id parsed from `WithdrawalRecord::sender_addr`
    /// (the part after `0:` in `0:<32-byte hex>`). Cross-checked against
    /// the algebraic decode of `entries[3].cell_repr_data` bits [11..267)
    /// in `extract_withdrawal_fields` — both must agree, otherwise the
    /// fixture and the in-circuit `sender_acc_fr` derivation would
    /// silently disagree.
    pub sender_account_id: [u8; 32],
    /// Source-of-truth values from the generator log (cross-check the
    /// in-circuit derivation does not silently desync from what the
    /// contract emitted).
    pub source_dst_chain_id: u128,
    pub source_amount: u128,
    pub source_token_id: u32,
    pub source_recipient_hex: String,
}

/// Parse a base64-encoded full ExtOut `Message` BOC into the 4 flattened
/// cells expected by the circuit, enforcing the structural invariants we
/// depend on (see `EVENT_LAYOUT_COMPARISON.md` §2).
pub fn parse_withdrawal_boc(event_boc_b64: &str) -> [BocFlattenData; 4] {
    let msg = Message::construct_from_base64(event_boc_b64)
        .expect("failed to parse event BOC from base64");
    let msg_cell = msg.serialize().expect("failed to serialize Message cell");
    let serialized =
        serialize_cells_tree_root_first(&msg_cell).expect("failed to flatten cell tree");

    assert_eq!(
        serialized.len(),
        4,
        "expected 4 cells in WithdrawalInitiated BOC (wrapper, body, recipient, sender); got {}",
        serialized.len(),
    );

    // entries[0] is the ExtOut wrapper, refs=1
    assert_eq!(
        serialized[0].refs_count, 1,
        "entries[0] (ExtOut wrapper) must have refs_count=1"
    );

    // entries[1] is the event body, refs=2, fixed length
    assert_eq!(
        serialized[1].refs_count, 2,
        "entries[1] (body) must have refs_count=2"
    );
    assert_eq!(
        serialized[1].cell_repr_data.len(),
        BODY_CELL_LEN,
        "body cell payload length mismatch (got {}, expected {})",
        serialized[1].cell_repr_data.len(),
        BODY_CELL_LEN,
    );

    // ABI event id at body[2..6) must be 0x3c838959
    let abi_slice = &serialized[1].cell_repr_data[EVENT_ABI_PREFIX_START..EVENT_ABI_PREFIX_END];
    assert_eq!(
        abi_slice, &ABI_EVENT_ID,
        "ABI event id mismatch at body[2..6) — expected WithdrawalInitiated 0x3c838959, got \
         {:02x?}",
        abi_slice,
    );

    // entries[2] is recipient, refs=0
    assert_eq!(
        serialized[2].refs_count, 0,
        "entries[2] (recipient) must have refs_count=0"
    );
    // For first-cut tests, recipient must be exactly 20 bytes.
    assert_eq!(
        serialized[2].cell_repr_data.len(),
        RECIPIENT_CELL_LEN,
        "recipient cell payload length mismatch (got {}, expected {} for 20-byte recipient)",
        serialized[2].cell_repr_data.len(),
        RECIPIENT_CELL_LEN,
    );

    // entries[3] is sender, refs=0, fixed 36 bytes (267-bit std_addr)
    assert_eq!(
        serialized[3].refs_count, 0,
        "entries[3] (sender) must have refs_count=0"
    );
    assert_eq!(
        serialized[3].cell_repr_data.len(),
        SENDER_CELL_LEN,
        "sender cell payload length mismatch (got {}, expected {})",
        serialized[3].cell_repr_data.len(),
        SENDER_CELL_LEN,
    );

    [
        serialized[0].clone(),
        serialized[1].clone(),
        serialized[2].clone(),
        serialized[3].clone(),
    ]
}

/// Native counterpart of the in-circuit algebraic decode of
/// `sender_acc_fr`. Returns the 32 BE-display-order `account_id` bytes
/// extracted from sender cell payload bits [11..267), per
/// EVENT_LAYOUT_COMPARISON.md §2.2.
///
///   account_id_byte[i] = (sender[3+i] & 0x1F) << 3 | (sender[4+i] >> 5)
pub fn decode_sender_account_id_from_cell(sender_cell_repr_data: &[u8]) -> [u8; 32] {
    assert_eq!(
        sender_cell_repr_data.len(),
        SENDER_CELL_LEN,
        "sender cell repr_data must be {} bytes",
        SENDER_CELL_LEN
    );
    let mut out = [0u8; 32];
    for i in 0..32 {
        let low5 = sender_cell_repr_data[3 + i] & 0x1F;
        let high3 = sender_cell_repr_data[4 + i] >> 5;
        out[i] = (low5 << 3) | high3;
    }
    out
}

/// Parse `sender_addr` of the form `0:<64-hex>` into a 32-byte account_id.
fn parse_sender_account_id(sender_addr: &str) -> [u8; 32] {
    let trimmed = sender_addr.trim();
    let (wc, hex_part) = trimmed.split_once(':').unwrap_or(("0", trimmed));
    let _ = wc; // workchain is not stored explicitly here — only account_id.
    let bytes = hex::decode(hex_part).expect("sender_addr must end with a hex-encoded account_id");
    assert_eq!(
        bytes.len(),
        32,
        "sender_addr account_id must be exactly 32 bytes, got {}",
        bytes.len()
    );
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// Extract everything the circuit and instance column need from a single
/// captured withdrawal.
pub fn extract_withdrawal_fields(rec: &WithdrawalRecord) -> WithdrawalFields {
    let entries = parse_withdrawal_boc(&rec.event_boc);
    let repr_hash = entries[0].repr_hash;

    // `parse_withdrawal_boc` has already asserted `BODY_CELL_LEN`, so the
    // body / recipient slices below are in-bounds.
    let body = &entries[1].cell_repr_data;
    let recipient_payload = &entries[2].cell_repr_data;

    let token_id_val = be_bytes_to_fr(&body[EVENT_TOKEN_ID_START..EVENT_TOKEN_ID_END]);

    // Cross-check the in-circuit-derived tokenId matches the value the
    // generator script saw being passed to the contract.
    assert_eq!(
        token_id_val,
        Fr::from(rec.token_id as u64),
        "parsed tokenId from BOC does not match source token_id"
    );

    // Cross-check the recipient bytes embedded in the recipient cell payload
    // match the recipient_hex the generator submitted. The recipient cell is
    // `d1 || d2 || 20 raw bytes` — bytes [2..22) hold the address.
    let parsed_recipient = &entries[2].cell_repr_data[2..2 + RECIPIENT_LEN_FIXED];
    let parsed_recipient_hex = hex::encode(parsed_recipient);
    assert_eq!(
        parsed_recipient_hex.to_lowercase(),
        rec.recipient_hex.to_lowercase(),
        "recipient bytes from BOC do not match source recipient_hex"
    );

    // Native BE-pack of the remaining public-instance fields. Mirrors the
    // in-circuit `gate.inner_product(..., be_powers)` in `synthesize`.
    let amount_val = be_bytes_to_fr(
        &body[crate::event_primitives::EVENT_AMOUNT_START
            ..crate::event_primitives::EVENT_AMOUNT_END],
    );
    let dst_chain_id_val = be_bytes_to_fr(
        &body[crate::event_primitives::EVENT_DST_CHAIN_ID_START
            ..crate::event_primitives::EVENT_DST_CHAIN_ID_END],
    );
    let recipient_hi_val = be_bytes_to_fr(&recipient_payload[RECIPIENT_HI_START..RECIPIENT_HI_END]);
    let recipient_lo_val = be_bytes_to_fr(&recipient_payload[RECIPIENT_LO_START..RECIPIENT_LO_END]);

    // Sanity-check half-length (compile-time constant — runtime check is a
    // belt-and-suspenders against later refactors).
    assert_eq!(RECIPIENT_HI_END - RECIPIENT_HI_START, RECIPIENT_HALF_LEN);

    let sender_account_id = parse_sender_account_id(&rec.sender_addr);

    // Cross-check: the BoC-derived account_id (algebraic decode of bits
    // [11..267) from entries[3].cell_repr_data) must match the value
    // parsed from the sender_addr text field — otherwise the fixture
    // and the in-circuit `sender_acc_fr` derivation would silently
    // disagree.
    let boc_account_id = decode_sender_account_id_from_cell(&entries[3].cell_repr_data);
    assert_eq!(
        boc_account_id, sender_account_id,
        "sender account_id mismatch: parsed-from-sender_addr={:02x?}, decoded-from-BOC={:02x?}",
        sender_account_id, boc_account_id,
    );

    WithdrawalFields {
        entries,
        repr_hash,
        token_id_val,
        amount_val,
        dst_chain_id_val,
        recipient_hi_val,
        recipient_lo_val,
        sender_account_id,
        source_dst_chain_id: rec.dst_chain_id,
        source_amount: rec.amount,
        source_token_id: rec.token_id,
        source_recipient_hex: rec.recipient_hex.clone(),
    }
}

/// Load and extract the first withdrawal from `withdrawals.txt`.
pub fn load_first_withdrawal() -> WithdrawalFields {
    let recs = read_withdrawals_from_file("withdrawals.txt");
    assert!(
        !recs.is_empty(),
        "withdrawals.txt must contain at least one entry"
    );
    extract_withdrawal_fields(&recs[0])
}

/// Same as [`load_first_withdrawal`] but parses [`EMBEDDED_WITHDRAWALS_TXT`]
/// instead of reading the file from disk — for downstream crates that need
/// a deterministic fixture without filesystem coupling.
pub fn load_first_withdrawal_embedded() -> WithdrawalFields {
    let recs = parse_withdrawals_from_str(EMBEDDED_WITHDRAWALS_TXT);
    assert!(
        !recs.is_empty(),
        "EMBEDDED_WITHDRAWALS_TXT must contain at least one entry"
    );
    extract_withdrawal_fields(&recs[0])
}

/// String-parser counterpart of
/// [`crate::event_data_helper::read_withdrawals_from_file`] — useful when
/// the fixture is embedded via `include_str!`. Format is identical (7 data
/// lines + 1 blank separator per record).
pub fn parse_withdrawals_from_str(content: &str) -> Vec<WithdrawalRecord> {
    let lines: Vec<&str> = content.lines().collect();
    let mut records = Vec::new();
    let mut i = 0;
    while i + 6 < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let dst_chain_id = lines[i].trim().parse::<u128>().expect("dst_chain_id u128");
        let recipient_hex = lines[i + 1].trim().to_string();
        let amount = lines[i + 2].trim().parse::<u128>().expect("amount u128");
        let token_id = lines[i + 3].trim().parse::<u32>().expect("token_id u32");
        let sender_addr = lines[i + 4].trim().to_string();
        let event_boc = lines[i + 5].trim().to_string();
        let event_block_id_hex = lines[i + 6].trim().to_string();
        records.push(WithdrawalRecord {
            dst_chain_id,
            recipient_hex,
            amount,
            token_id,
            sender_addr,
            event_boc,
            event_block_id_hex,
        });
        i += 8;
    }
    records
}

/// Two-level (events tree → block tree) random witnesses, with the captured
/// event's `repr_hash` injected at the events-tree leaf position 0.
pub struct TwoLevelWitnesses {
    pub account_dapp_id: [u8; 32],
    pub account_id: [u8; 32],
    pub block_id: [u8; 32],
    pub envelope_hash_bytes: [u8; 32],
    pub events_siblings: Vec<[u8; 32]>,
    pub events_pos: usize,
    pub block_siblings: Vec<[u8; 32]>,
    pub block_pos: usize,
    pub blocks_root_level_0: [u8; 32],
}

pub fn build_two_level_tree(
    repr_hash: &[u8; 32],
    rng: &mut impl Rng,
    dense_hasher: &DensePoseidonHasher,
    num_events_leaves: usize,
    num_block_leaves: usize,
) -> TwoLevelWitnesses {
    let mut dapp_id = [0u8; 32];
    let mut account_id_b = [0u8; 32];
    let mut block_id = [0u8; 32];
    let mut envelope_hash = [0u8; 32];
    rng.fill(&mut dapp_id);
    rng.fill(&mut account_id_b);
    rng.fill(&mut block_id);
    rng.fill(&mut envelope_hash);

    let ext_msg_leaf = poseidon_hash_96_native(&dapp_id, &account_id_b, repr_hash);

    let mut events_leaves = vec![[0u8; 32]; num_events_leaves];
    events_leaves[0] = ext_msg_leaf;
    for i in 1..num_events_leaves {
        rng.fill(&mut events_leaves[i]);
    }
    let events_root = dense_merkle_root(dense_hasher, &events_leaves);
    let events_siblings = dense_merkle_proof(dense_hasher, &events_leaves, 0);

    let block_leaf = poseidon_hash_96_native(&block_id, &envelope_hash, &events_root);

    let mut block_leaves = vec![[0u8; 32]; num_block_leaves];
    block_leaves[0] = block_leaf;
    for i in 1..num_block_leaves {
        rng.fill(&mut block_leaves[i]);
    }
    let blocks_root = dense_merkle_root(dense_hasher, &block_leaves);
    let block_siblings = dense_merkle_proof(dense_hasher, &block_leaves, 0);

    TwoLevelWitnesses {
        account_dapp_id: dapp_id,
        account_id: account_id_b,
        block_id,
        envelope_hash_bytes: envelope_hash,
        events_siblings,
        events_pos: 0,
        block_siblings,
        block_pos: 0,
        blocks_root_level_0: blocks_root,
    }
}

/// Build a chain of `chain_len` dense balanced trees on top of an initial leaf,
/// padding to `MAX_CHAIN_LEN` with inactive links. Returns the chain and the
/// final root.
pub fn build_dense_chain(
    initial_leaf_bytes: [u8; 32],
    chain_len: usize,
    leaves_per_tree: usize,
) -> (Vec<DenseChainLink>, [u8; 32]) {
    use dense_balanced_tree::dense_merkle_verify;
    use rand::{rngs::StdRng, SeedableRng};

    assert!(chain_len <= MAX_CHAIN_LEN);
    let dense_hasher = DensePoseidonHasher::new();
    let mut rng = StdRng::seed_from_u64(123);
    let mut chain = Vec::with_capacity(MAX_CHAIN_LEN);
    let mut current_leaf_bytes = initial_leaf_bytes;

    for t in 0..chain_len {
        let mut leaves = vec![[0u8; 32]; leaves_per_tree];
        leaves[0] = current_leaf_bytes;
        for i in 1..leaves_per_tree {
            rng.fill(&mut leaves[i]);
        }

        let root_hash = dense_merkle_root(&dense_hasher, &leaves);
        let siblings = dense_merkle_proof(&dense_hasher, &leaves, 0);

        assert!(
            dense_merkle_verify(&dense_hasher, &root_hash, &leaves[0], 0, &siblings),
            "Chain link {}: native verification failed",
            t
        );

        chain.push(DenseChainLink {
            active: true,
            siblings,
            position: 0,
            leaf_native: current_leaf_bytes,
        });

        let root_fr = bytes_to_fr(&root_hash);
        current_leaf_bytes = fr_to_bytes(root_fr);
    }

    let final_root_bytes = current_leaf_bytes;
    let depth = if chain.is_empty() {
        ceil_log2(leaves_per_tree)
    } else {
        chain[0].siblings.len()
    };

    while chain.len() < MAX_CHAIN_LEN {
        chain.push(DenseChainLink::inactive(final_root_bytes, depth));
    }

    (chain, final_root_bytes)
}

/// Native nullifier — mirrors the in-circuit `hash_fix_len_array` call in
/// `BridgeEventFinalProof::synthesize`. Used by tests and downstream
/// orchestrators to predict the public-instance `PUB_NULLIFIER` value.
///
/// `events_pos` is appended so two identical `WithdrawalInitiated` events
/// in the same AN block produce distinct nullifiers. The value MUST be the
/// same `events_pos` used to build the events-tree merkle proof (bound to
/// the walker's direction bits inside the circuit).
pub fn nullifier_native(
    block_id_fr: Fr,
    token_id: Fr,
    amount: Fr,
    recipient_hi: Fr,
    recipient_lo: Fr,
    sender_acc_fr: Fr,
    events_pos: Fr,
) -> Fr {
    crate::poseidon::poseidon_hash(&[
        block_id_fr,
        token_id,
        amount,
        recipient_hi,
        recipient_lo,
        sender_acc_fr,
        events_pos,
    ])
}

/// Bundled leading public-input Fr values, in `PUB_*` slot order.
/// Constructed by [`compute_leading_public_inputs`] and consumed by
/// [`make_final_proof_instances`].
#[derive(Clone, Copy, Debug)]
pub struct LeadingPublicInputs {
    pub token_id: Fr,
    pub amount: Fr,
    pub recipient_hi: Fr,
    pub recipient_lo: Fr,
    pub dst_chain_id: Fr,
    pub sender_acc_fr: Fr,
    pub dapp_fr: Fr,
    pub acc_fr: Fr,
    pub nullifier: Fr,
}

impl LeadingPublicInputs {
    pub fn to_vec(self) -> Vec<Fr> {
        vec![
            self.token_id,
            self.amount,
            self.recipient_hi,
            self.recipient_lo,
            self.dst_chain_id,
            self.sender_acc_fr,
            self.dapp_fr,
            self.acc_fr,
            self.nullifier,
        ]
    }
}

/// Compute the 9 leading public inputs natively from a parsed withdrawal
/// + the two-level tree witnesses + the sender's `account_id`. The
/// `sender_account_id` must match the algebraic decode of the BoC sender
/// cell (cross-checked in [`extract_withdrawal_fields`]).
///
/// `events_pos` participates in the nullifier preimage so two identical
/// `WithdrawalInitiated` events in the same AN block produce distinct
/// nullifiers. It MUST be the same `events_pos` used to build the
/// events-tree merkle proof passed to the circuit.
pub fn compute_leading_public_inputs(
    w: &WithdrawalFields,
    block_id: &[u8; 32],
    account_dapp_id: &[u8; 32],
    account_id: &[u8; 32],
    sender_account_id: &[u8; 32],
    events_pos: usize,
) -> LeadingPublicInputs {
    let block_id_fr = bytes_to_fr(block_id);
    let dapp_fr = bytes_to_fr(account_dapp_id);
    let acc_fr = bytes_to_fr(account_id);
    let sender_acc_fr = bytes_to_fr(sender_account_id);
    let events_pos_fr = Fr::from(events_pos as u64);
    let nullifier = nullifier_native(
        block_id_fr,
        w.token_id_val,
        w.amount_val,
        w.recipient_hi_val,
        w.recipient_lo_val,
        sender_acc_fr,
        events_pos_fr,
    );
    LeadingPublicInputs {
        token_id: w.token_id_val,
        amount: w.amount_val,
        recipient_hi: w.recipient_hi_val,
        recipient_lo: w.recipient_lo_val,
        dst_chain_id: w.dst_chain_id_val,
        sender_acc_fr,
        dapp_fr,
        acc_fr,
        nullifier,
    }
}

// `make_instances` (the legacy 11-slot single-thread instance packer) and
// `build_synthetic_event_keygen_inputs` (its one-shot circuit builder) were
// removed together with the single-thread event-prove circuit. Every current
// caller uses `make_final_proof_instances` /
// `build_synthetic_final_proof_keygen_inputs` (13-slot layout) below.

// ─── Multi-thread `BridgeEventFinalProof` helpers ────────────────────────────
//
// These parallel the single-thread helpers above but populate the extra
// witnesses the L8 opening needs — most importantly the `h07_sibling` (opaque
// left aggregate of the depth-4 block-id SHA tree) and a `block_id` that IS
// the SHA depth-4 opening of `ext_out_root` against that sibling. See
// `block_id_tree.rs` for the arithmetic; the helper below inverts the direction
// by *computing* `block_id` from a random ext_out_root + random h07_sibling so
// the MockProver witness is internally consistent.

/// Two-level witnesses for `BridgeEventFinalProof`. Extends
/// [`TwoLevelWitnesses`] with the L8-opening sibling.
pub struct FinalProofTwoLevelWitnesses {
    pub account_dapp_id: [u8; 32],
    pub account_id: [u8; 32],
    /// Block-id computed from `ext_out_root` (events tree root) and
    /// `h07_sibling` via the SHA depth-4 opening. The MockProver's L8-opening
    /// gadget will reconstruct exactly this value in-circuit.
    pub block_id: [u8; 32],
    pub envelope_hash_bytes: [u8; 32],
    pub events_siblings: Vec<[u8; 32]>,
    pub events_pos: usize,
    pub block_siblings: Vec<[u8; 32]>,
    pub block_pos: usize,
    pub blocks_root_level_0: [u8; 32],
    /// Opaque SHA-256 sibling for the depth-4 block-id tree — leaves 0..=7
    /// aggregate. Random in synthetic witnesses; fetched from GQL in
    /// production.
    pub h07_sibling: [u8; 32],
}

/// Build a same-thread two-level tree witness. Unlike `build_two_level_tree`,
/// this variant derives `block_id` deterministically from the events root
/// and a random `h07_sibling` so the in-circuit L8 opening reconstructs it.
pub fn build_final_proof_two_level_tree(
    repr_hash: &[u8; 32],
    rng: &mut impl Rng,
    dense_hasher: &DensePoseidonHasher,
    num_events_leaves: usize,
    num_block_leaves: usize,
) -> FinalProofTwoLevelWitnesses {
    use crate::{
        block_id_tree::compute_block_id_from_l8_native, event_primitives::poseidon_hash_96_native,
    };

    let mut dapp_id = [0u8; 32];
    let mut account_id_b = [0u8; 32];
    let mut envelope_hash = [0u8; 32];
    let mut h07_sibling = [0u8; 32];
    rng.fill(&mut dapp_id);
    rng.fill(&mut account_id_b);
    rng.fill(&mut envelope_hash);
    rng.fill(&mut h07_sibling);

    let ext_msg_leaf = poseidon_hash_96_native(&dapp_id, &account_id_b, repr_hash);

    let mut events_leaves = vec![[0u8; 32]; num_events_leaves];
    events_leaves[0] = ext_msg_leaf;
    for i in 1..num_events_leaves {
        rng.fill(&mut events_leaves[i]);
    }
    let events_root = dense_merkle_root(dense_hasher, &events_leaves);
    let events_siblings = dense_merkle_proof(dense_hasher, &events_leaves, 0);

    // ext_out_root (Fr) → 32-byte native form → feed to L8 opening
    let ext_out_root_bytes = fr_to_bytes(bytes_to_fr(&events_root));
    // Note: for the events-tree side, `events_root` is already computed as
    // a 32-byte SHA-of-Poseidon-outputs image; the DEX convention treats
    // `walk_dense_merkle_bind_pos`'s output directly as the ext_out_root Fr.
    // The circuit converts that Fr back to bytes via `fr_to_bytes` inside
    // synthesize; we mirror the exact same rebuild here so the witness
    // stays consistent bit-for-bit with the in-circuit path.
    let block_id = compute_block_id_from_l8_native(&ext_out_root_bytes, &h07_sibling);

    let block_leaf = poseidon_hash_96_native(&block_id, &envelope_hash, &events_root);
    let mut block_leaves = vec![[0u8; 32]; num_block_leaves];
    block_leaves[0] = block_leaf;
    for i in 1..num_block_leaves {
        rng.fill(&mut block_leaves[i]);
    }
    let blocks_root = dense_merkle_root(dense_hasher, &block_leaves);
    let block_siblings = dense_merkle_proof(dense_hasher, &block_leaves, 0);

    FinalProofTwoLevelWitnesses {
        account_dapp_id: dapp_id,
        account_id: account_id_b,
        block_id,
        envelope_hash_bytes: envelope_hash,
        events_siblings,
        events_pos: 0,
        block_siblings,
        block_pos: 0,
        blocks_root_level_0: blocks_root,
        h07_sibling,
    }
}

/// Cross-thread variant: `x_block_id` is derived from the L8 opening (as in
/// same-thread) but the *block-leaf* stored in the block tree is keyed off a
/// caller-supplied `y_block_id`. This mirrors what the future L7-walk
/// circuit will bind together — for now it lets the FinalProof MockProver
/// exercise `is_same_thread = false` with a well-formed witness.
pub fn build_final_proof_two_level_tree_cross(
    repr_hash: &[u8; 32],
    rng: &mut impl Rng,
    dense_hasher: &DensePoseidonHasher,
    num_events_leaves: usize,
    num_block_leaves: usize,
    y_block_id: [u8; 32],
) -> FinalProofTwoLevelWitnesses {
    use crate::{
        block_id_tree::compute_block_id_from_l8_native, event_primitives::poseidon_hash_96_native,
    };

    let mut dapp_id = [0u8; 32];
    let mut account_id_b = [0u8; 32];
    let mut envelope_hash = [0u8; 32];
    let mut h07_sibling = [0u8; 32];
    rng.fill(&mut dapp_id);
    rng.fill(&mut account_id_b);
    rng.fill(&mut envelope_hash);
    rng.fill(&mut h07_sibling);

    let ext_msg_leaf = poseidon_hash_96_native(&dapp_id, &account_id_b, repr_hash);

    let mut events_leaves = vec![[0u8; 32]; num_events_leaves];
    events_leaves[0] = ext_msg_leaf;
    for i in 1..num_events_leaves {
        rng.fill(&mut events_leaves[i]);
    }
    let events_root = dense_merkle_root(dense_hasher, &events_leaves);
    let events_siblings = dense_merkle_proof(dense_hasher, &events_leaves, 0);

    let ext_out_root_bytes = fr_to_bytes(bytes_to_fr(&events_root));
    let x_block_id = compute_block_id_from_l8_native(&ext_out_root_bytes, &h07_sibling);

    // Block leaf is Y-side: uses the caller's y_block_id but the same
    // ext_out_root / envelope_hash. In production this is what the Y-block
    // *would* contain — the L7 walker circuit (Commit 4) enforces the
    // cross-thread binding; here we just wire the witness consistently.
    let block_leaf = poseidon_hash_96_native(&y_block_id, &envelope_hash, &events_root);
    let mut block_leaves = vec![[0u8; 32]; num_block_leaves];
    block_leaves[0] = block_leaf;
    for i in 1..num_block_leaves {
        rng.fill(&mut block_leaves[i]);
    }
    let blocks_root = dense_merkle_root(dense_hasher, &block_leaves);
    let block_siblings = dense_merkle_proof(dense_hasher, &block_leaves, 0);

    FinalProofTwoLevelWitnesses {
        account_dapp_id: dapp_id,
        account_id: account_id_b,
        block_id: x_block_id, // .block_id here means x_block_id
        envelope_hash_bytes: envelope_hash,
        events_siblings,
        events_pos: 0,
        block_siblings,
        block_pos: 0,
        blocks_root_level_0: blocks_root,
        h07_sibling,
    }
}

/// Instance vector for `BridgeEventFinalProof`: 11 leading slots +
/// `final_root` + `anchor_layer` + `x_block_id_fr` + `y_block_id_fr`.
pub fn make_final_proof_instances(
    leading: LeadingPublicInputs,
    final_root: Fr,
    anchor_layer: Fr,
    x_block_id_fr: Fr,
    y_block_id_fr: Fr,
) -> Vec<Fr> {
    let mut v = Vec::with_capacity(crate::bridge_event_final_proof::TOTAL_PUBLIC_INPUTS);
    v.extend(leading.to_vec());
    v.push(final_root);
    v.push(anchor_layer);
    v.push(x_block_id_fr);
    v.push(y_block_id_fr);
    v
}

/// One-shot synthetic-witness builder for [`BridgeEventFinalProof`]. Successor
/// of the removed legacy `build_synthetic_event_keygen_inputs` — wires
/// together [`load_first_withdrawal_embedded`] +
/// [`build_final_proof_two_level_tree`] + [`build_dense_chain`] (T=1) into
/// a ready-to-keygen circuit and its 13-slot public-instance vector.
/// Deterministic given `seed`.
///
/// The synthetic witness is always same-thread (`is_same_thread = true`,
/// `y_block_id == x_block_id`); the anchor layer is the smallest legal
/// value (1) so the range checks always pass on the reference witness.
///
/// Consumed by `bridge-prover-lib::keys::event::ensure_keys` and by the
/// `--selftest` mode of the `bridge-event-prove` binary.
pub fn build_synthetic_final_proof_keygen_inputs(
    seed: u64,
) -> (
    crate::bridge_event_final_proof::BridgeEventFinalProof,
    Vec<Fr>,
) {
    let w = load_first_withdrawal_embedded();
    let dense_hasher = DensePoseidonHasher::new();
    let mut rng = StdRng::seed_from_u64(seed);

    let tw = build_final_proof_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
    let (dense_chain, final_root_bytes) = build_dense_chain(tw.blocks_root_level_0, 1, 130);
    let final_root_fr = bytes_to_fr(&final_root_bytes);

    let params = base_circuit_params();
    // Synthetic anchor_layer must sit inside `1..=MAX_ANCHOR_LAYER`; pick
    // the smallest legal one so the range checks always pass on the
    // reference witness.
    let anchor_layer: u8 = 1;
    let events_pos = tw.events_pos;
    let circuit = crate::bridge_event_final_proof::BridgeEventFinalProof::new(
        w.entries.clone(),
        tw.events_siblings,
        events_pos,
        tw.account_dapp_id,
        tw.account_id,
        tw.block_id, // x_block_id
        tw.block_id, // y_block_id (same-thread)
        tw.h07_sibling,
        true, // is_same_thread
        tw.envelope_hash_bytes,
        tw.block_siblings,
        tw.block_pos,
        dense_chain,
        1,
        anchor_layer,
        params,
    );

    let leading: LeadingPublicInputs = compute_leading_public_inputs(
        &w,
        &tw.block_id,
        &tw.account_dapp_id,
        &tw.account_id,
        &w.sender_account_id,
        events_pos,
    );
    let block_id_fr = bytes_to_fr(&tw.block_id);
    let instances = make_final_proof_instances(
        leading,
        final_root_fr,
        Fr::from(anchor_layer as u64),
        block_id_fr,
        block_id_fr,
    );

    (circuit, instances)
}

// ─── Multi-thread `BridgeMultiHopProof` helpers ──────────────────────────────
//
// Mirror the same shape as the FinalProof helpers above: a synthetic-witness
// builder that produces a ready-to-keygen `BridgeMultiHopProof` circuit plus
// its 2-slot public-instance vector. Consumed by
// `bridge-prover-lib::keys::multi_hop::MultiHopKeyManager::ensure_keys`.
//
// The three per-hop constructors (`make_active_hop`, `make_inactive_hop`,
// `synth_hops`) are the exact witness shapes used by the in-crate
// `multi_hop_proof::tests::all_active_hops_mock_prover` and its siblings —
// promoted here as `pub` for downstream consumption. The circuit-side test
// module keeps its private copies to avoid churning the passing tests.

/// K used by the multi-hop MockProver tests and the `MultiHopKeyManager`
/// keygen. Matches `multi_hop_proof::tests::test_params(K=17)`.
pub const MULTI_HOP_K: u32 = 17;

/// Deterministic placeholder for the slot-0 same-thread parent — never opened
/// by the circuit (spec §5.1) but must be a well-defined value so native
/// `proof_block_refs_root_native` is reproducible.
pub const SLOT0_PARENT_PLACEHOLDER: [u8; 32] = [0xF0; 32];

/// Base-circuit params for `BridgeMultiHopProof` — mirrors
/// `multi_hop_proof::tests::test_params(K=17)`. 25 advice / 9 lookup-advice
/// columns per phase, lookup_bits=16.
///
/// Advice-column budget: MockProver at K=17, H_HOPS_PER_PROOF=1 sweep
/// (2026-09-27) gave 20 FAIL, 21 FAIL, 22 PASS, 25 PASS, 30 PASS. Set to
/// 25 for parity with `historical-layer-hashes-movement-checker-circuit`
/// (identical SHA compression count = 8, fewer PIs) and 3-col safety
/// margin over the 22-col floor.
///
/// The prior 50-column budget dated from `H_HOPS_PER_PROOF=2` (16 SHA
/// compressions per snark). Dropping H to 1 halves the SHA workload, which
/// halves the col count, which drops the aggregated Yul verifier by ~12 KB
/// (from the rejected 33 213 B at 50 cols to ~21 KB at 25 cols) — bringing
/// it into EIP-170 with margin. See
/// `crates/bridge-circuits/docs/CIRCUIT_COMPLEXITY_COMPARISON.md` and
/// `docs/SHA256_INVOCATIONS.md` §4.
pub fn multi_hop_base_circuit_params() -> BaseCircuitParams {
    BaseCircuitParams {
        k: MULTI_HOP_K as usize,
        num_advice_per_phase: vec![25],
        num_fixed: 1,
        num_lookup_advice_per_phase: vec![9],
        lookup_bits: Some(16),
        num_instance_columns: 1,
    }
}

/// Build one active hop under Direction (a) semantics: given
/// `older_ref_id` (the block the current hop's `proof_block_refs[1]` points
/// at), derive the current (newer) block whose L7 contains that ref. Returns
/// the fully-populated `HopWitness` — `hop.hop_start_block_id ==
/// current_block_id` (newer), `hop.hop_end_block_id == older_ref_id` (older)
/// — and the derived `current_block_id` so callers can chain multiple hops.
pub fn make_active_hop_public(
    older_ref_id: [u8; 32],
    sentinel_byte: u8,
) -> (HopWitness, [u8; 32]) {
    let proof_block_refs: Vec<[u8; 32]> = vec![SLOT0_PARENT_PLACEHOLDER, older_ref_id];
    let l7 = proof_block_refs_root_native(&proof_block_refs);

    let mut leaves = [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT];
    for (j, slot) in leaves.iter_mut().enumerate().take(7) {
        *slot = [sentinel_byte; 32];
        slot[0] = j as u8;
    }
    leaves[7] = l7;

    let block_id = block_merkle_root(&leaves);
    let block_merkle_leaf_proof_l7 = block_merkle_leaf_proof(&leaves, 7);
    let ref_index = 1usize;
    let (proof_block_ref_inner_path, refs_tree_depth) =
        proof_block_ref_inner_path_native(&proof_block_refs, ref_index);

    let hop = HopWitness {
        is_active: true,
        block: BlockWitness {
            block_id,
            block_merkle_tree_leaves: leaves,
            proof_block_refs,
        },
        block_merkle_leaf_proof_l7,
        ref_index,
        refs_tree_depth,
        proof_block_ref_inner_path,
        hop_start_block_id: block_id,
        hop_end_block_id: older_ref_id,
    };
    (hop, block_id)
}

/// Build an inactive padding hop carrying `pad_bid` on both endpoints.
pub fn make_inactive_hop_public(pad_bid: [u8; 32]) -> HopWitness {
    let proof_block_refs: Vec<[u8; 32]> = vec![SLOT0_PARENT_PLACEHOLDER, pad_bid];
    let (proof_block_ref_inner_path, refs_tree_depth) =
        proof_block_ref_inner_path_native(&proof_block_refs, 1);
    HopWitness {
        is_active: false,
        block: BlockWitness {
            block_id: pad_bid,
            block_merkle_tree_leaves: [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT],
            proof_block_refs,
        },
        block_merkle_leaf_proof_l7: [[0u8; 32]; BLOCK_MERKLE_DEPTH],
        ref_index: 1,
        refs_tree_depth,
        proof_block_ref_inner_path,
        hop_start_block_id: pad_bid,
        hop_end_block_id: pad_bid,
    }
}

/// Build `H_HOPS_PER_PROOF` hops under Direction (a) semantics: `seed_bytes`
/// is the *oldest* block-id (the anchor `Y`). We construct the chain
/// oldest→newest — each iteration derives a newer block whose L7 references
/// the previous older one — then reverse so `hops[0].hop_start = X` (newest
/// event block) and the last active hop's `hop_end = seed_bytes` (`Y`). Any
/// tail slot is inactive padding carrying `Y` on both endpoints. `k_active`
/// must be `<= H_HOPS_PER_PROOF`.
pub fn synth_hops_public(
    seed_bytes: [u8; 32],
    k_active: usize,
) -> [HopWitness; H_HOPS_PER_PROOF] {
    assert!(k_active <= H_HOPS_PER_PROOF);
    let mut chain: Vec<HopWitness> = Vec::with_capacity(k_active);
    let mut older_bid = seed_bytes;
    for i in 0..k_active {
        let (hop, newer_bid) = make_active_hop_public(older_bid, 0x10 + i as u8);
        chain.push(hop);
        older_bid = newer_bid;
    }
    // Reverse so index 0 is the newest hop (start = X) and the last active
    // hop's end is `seed_bytes` (Y). At k_active=0 this is a no-op.
    chain.reverse();
    let terminal_older = if chain.is_empty() {
        seed_bytes
    } else {
        chain.last().unwrap().hop_end_block_id
    };
    while chain.len() < H_HOPS_PER_PROOF {
        chain.push(make_inactive_hop_public(terminal_older));
    }
    chain
        .try_into()
        .unwrap_or_else(|v: Vec<HopWitness>| panic!("hop slot count {}", v.len()))
}

/// One-shot synthetic-witness builder for [`BridgeMultiHopProof`]. Produces a
/// ready-to-keygen circuit and its 2-slot public-instance vector
/// `[hop_start_block_id_fr, hop_end_block_id_fr]`. Deterministic given `seed`.
///
/// The synthetic chain is `H_HOPS_PER_PROOF` active hops (fully packed,
/// no padding) so keygen exercises every active-path constraint. Consumed by
/// `bridge-prover-lib::keys::multi_hop::MultiHopKeyManager::ensure_keys`.
pub fn build_synthetic_multi_hop_keygen_inputs(seed: u64) -> (BridgeMultiHopProof, Vec<Fr>) {
    let _ = MAX_PROOF_BLOCK_REFS_DEPTH; // silence unused-import; keeps re-export stable
    let genesis: [u8; 32] = {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut b = [0u8; 32];
        rng.fill(&mut b);
        b
    };
    let hops = synth_hops_public(genesis, H_HOPS_PER_PROOF);
    let first_start = hops[0].hop_start_block_id;
    let last_end = hops[H_HOPS_PER_PROOF - 1].hop_end_block_id;

    let params = multi_hop_base_circuit_params();
    let circuit = BridgeMultiHopProof::new(hops, params);
    let instances = vec![bytes_to_fr(&first_start), bytes_to_fr(&last_end)];
    (circuit, instances)
}
