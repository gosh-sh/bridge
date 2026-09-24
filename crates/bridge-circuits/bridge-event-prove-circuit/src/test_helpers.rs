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

use crate::boc_helper::*;
use crate::bridge_event_prove_circuit::{
    be_bytes_to_fr, poseidon_hash_96_native, BridgeEventProveCircuit, ABI_EVENT_ID,
    BODY_CELL_LEN, EVENT_ABI_PREFIX_END, EVENT_ABI_PREFIX_START, EVENT_TOKEN_ID_END,
    EVENT_TOKEN_ID_START, RECIPIENT_CELL_LEN, RECIPIENT_HALF_LEN, RECIPIENT_HI_END,
    RECIPIENT_HI_START, RECIPIENT_LEN_FIXED, RECIPIENT_LO_END, RECIPIENT_LO_START,
    SENDER_CELL_LEN, TOTAL_PUBLIC_INPUTS,
};
use crate::event_data_helper::{read_withdrawals_from_file, WithdrawalRecord};
use rand::rngs::StdRng;
use rand::SeedableRng;

/// Captured-fixture file contents, baked into the binary so downstream
/// crates don't need filesystem access to drive keygen / selftest paths.
pub const EMBEDDED_WITHDRAWALS_TXT: &str = include_str!("../withdrawals.txt");
use dense_balanced_tree::{
    dense_merkle_proof, dense_merkle_root, PoseidonHasher as DensePoseidonHasher,
};
use gosh_dense_balanced_tree::{bytes_to_fr, fr_to_bytes, DenseChainLink, MAX_CHAIN_LEN};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
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
    /// `bridge_event_prove_circuit::synthesize`.
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
    /// Source-of-truth values from the generator log (cross-check the in-circuit
    /// derivation does not silently desync from what the contract emitted).
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
        "ABI event id mismatch at body[2..6) — expected WithdrawalInitiated 0x3c838959, got {:02x?}",
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
    let bytes = hex::decode(hex_part)
        .expect("sender_addr must end with a hex-encoded account_id");
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
        &body[crate::bridge_event_prove_circuit::EVENT_AMOUNT_START
            ..crate::bridge_event_prove_circuit::EVENT_AMOUNT_END],
    );
    let dst_chain_id_val = be_bytes_to_fr(
        &body[crate::bridge_event_prove_circuit::EVENT_DST_CHAIN_ID_START
            ..crate::bridge_event_prove_circuit::EVENT_DST_CHAIN_ID_END],
    );
    let recipient_hi_val =
        be_bytes_to_fr(&recipient_payload[RECIPIENT_HI_START..RECIPIENT_HI_END]);
    let recipient_lo_val =
        be_bytes_to_fr(&recipient_payload[RECIPIENT_LO_START..RECIPIENT_LO_END]);

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
    use rand::rngs::StdRng;
    use rand::SeedableRng;

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
/// `BridgeEventProveCircuit::synthesize`. Used by tests and downstream
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
/// [`make_instances`].
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

/// Concatenate `[leading_public_inputs..., final_root, anchor_layer]` into
/// a single instance vector matching the circuit's `assigned_instances`
/// order.
///
/// `anchor_layer` is the 1-indexed layer index the on-chain verifier uses
/// to route to the correct `_layerWindows[layer]` history window and is
/// range-checked `1..=MAX_ANCHOR_LAYER` inside the circuit.
pub fn make_instances(
    leading: LeadingPublicInputs,
    final_root: Fr,
    anchor_layer: Fr,
) -> Vec<Fr> {
    let mut v = Vec::with_capacity(TOTAL_PUBLIC_INPUTS);
    v.extend(leading.to_vec());
    v.push(final_root);
    v.push(anchor_layer);
    v
}

/// One-shot synthetic-witness builder. Wires together
/// [`load_first_withdrawal_embedded`] + [`build_two_level_tree`] +
/// [`build_dense_chain`] (T=1) into a ready-to-keygen circuit and its
/// public-instance vector. Deterministic given `seed`.
///
/// Intended for `bridge-prover-lib::keys::ensure_event_keys` and for a
/// `--selftest` mode in the `bridge-event-prove` binary that does not require
/// a daemon-supplied fixture.
pub fn build_synthetic_event_keygen_inputs(
    seed: u64,
) -> (BridgeEventProveCircuit, Vec<Fr>) {
    let w = load_first_withdrawal_embedded();
    let dense_hasher = DensePoseidonHasher::new();
    let mut rng = StdRng::seed_from_u64(seed);

    let tw = build_two_level_tree(&w.repr_hash, &mut rng, &dense_hasher, 128, 130);
    let (dense_chain, final_root_bytes) =
        build_dense_chain(tw.blocks_root_level_0, 1, 130);
    let final_root_fr = bytes_to_fr(&final_root_bytes);

    let params = base_circuit_params();
    // Synthetic anchor_layer must sit inside `1..=MAX_ANCHOR_LAYER`; the
    // exact value doesn't matter for keygen shape — pick the smallest
    // legal one so the range checks always pass on the reference witness.
    let anchor_layer: u8 = 1;
    let events_pos = tw.events_pos;
    let circuit = BridgeEventProveCircuit::new(
        w.entries.clone(),
        tw.events_siblings,
        events_pos,
        tw.account_dapp_id,
        tw.account_id,
        tw.block_id,
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
    let instances = make_instances(leading, final_root_fr, Fr::from(anchor_layer as u64));

    (circuit, instances)
}
