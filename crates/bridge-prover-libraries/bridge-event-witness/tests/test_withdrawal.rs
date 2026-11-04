//! exercise the BOC walk + event decode against the first
//! captured `WithdrawalInitiated` BOC from
//! `crates/bridge-circuits/bridge-event-prove-circuit/withdrawals.txt`.
//!
//! The fixture is inlined here (rather than read from disk) so the test is
//! hermetic — no path coupling, no flaky CI if that file is regenerated.
//! The constants are pinned and any drift will surface as a field-level
//! assertion failure.

use bridge_event_witness::schema::SCHEMA_VERSION;
use bridge_event_witness::{
    export_from_event_boc_base64, BlockContextInput,
};

/// First record from `withdrawals.txt`, captured by
/// `crates/bridge-prover-libraries/python/capture_withdrawal_fixtures.py`.
const EVENT_BOC_B64: &str = "te6ccgEBBAEA3QABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAAAFcmro7a1gAQKYcogE/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAqjanAAAAAAAAAAAAAAAAAA9CQAAAAAM/sIKpBaiVFRIk9tk70HToFjOGOAMCAEOAEIdoOfRZocIfPbLTU7IlRS1PBWIn4bOQDPFUR0DaXn+QACh0LTXMZjTAUyklo7hEvEVORDj0Tg==";

const EXPECTED_TOKEN_ID: u32 = 3;
const EXPECTED_RECIPIENT_HEX: &str = "742d35cc6634c0532925a3b844bc454e4438f44e";
/// dst_chain_id = 11155111 = 0xAA36A7, padded to 32 BE bytes.
const EXPECTED_DST_CHAIN_ID_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000aa36a7";
/// The EVM bridge the capture bound chain 11155111 to.
const EXPECTED_DST_BRIDGE_HEX: &str = "3fb082a905a895151224f6d93bd074e816338638";
/// amount = 1_000_000 = 0xF4240, padded to 16 BE bytes.
const EXPECTED_AMOUNT_HEX: &str = "000000000000000000000000000f4240";

#[test]
fn first_withdrawal_decodes_to_expected_fields() {
    // Block-level context is fabricated here — the per-tx exporter doesn't
    // care about its actual values, only that they pass through to the
    // schema verbatim.
    let ctx = BlockContextInput {
        block_id: [0x42u8; 32],
        block_seq_no: 4711,
        account_dapp_id: [0xAAu8; 32],
        account_id: [0xBBu8; 32],
        envelope_hash: [0xCCu8; 32],
    };

    let w = export_from_event_boc_base64(EVENT_BOC_B64, &ctx)
        .expect("export must succeed on captured fixture");

    assert_eq!(w.schema_version, SCHEMA_VERSION);

    assert_eq!(w.event.token_id, EXPECTED_TOKEN_ID);
    assert_eq!(w.event.recipient_hex, EXPECTED_RECIPIENT_HEX);
    assert_eq!(w.event.dst_chain_id_hex, EXPECTED_DST_CHAIN_ID_HEX);
    assert_eq!(w.event.amount_hex, EXPECTED_AMOUNT_HEX);
    assert_eq!(w.event.dst_bridge_hex, EXPECTED_DST_BRIDGE_HEX);
    // sender_hex is the 34-byte cell payload after the 2-byte d1+d2 header,
    // i.e. the tail of entries[3].cell_repr_data_hex. Check both the length
    // invariant and that the decoded field matches the raw cell walk.
    assert_eq!(w.event.sender_hex.len(), 68, "sender_hex length");
    assert_eq!(
        w.event.sender_hex,
        &w.entries[3].cell_repr_data_hex[4..],
        "sender_hex must equal entries[3] payload after d1+d2 header",
    );

    // Structural invariants the circuit depends on:
    assert_eq!(w.entries[0].refs_count, 1, "wrapper refs_count");
    assert_eq!(w.entries[1].refs_count, 2, "body refs_count");
    assert_eq!(w.entries[2].refs_count, 0, "recipient refs_count");
    assert_eq!(w.entries[3].refs_count, 0, "sender refs_count");

    // Body cell length = 146 bytes = 292 hex chars.
    assert_eq!(w.entries[1].cell_repr_data_hex.len(), 292);
    // Recipient cell length = 22 bytes = 44 hex chars.
    assert_eq!(w.entries[2].cell_repr_data_hex.len(), 44);
    // Sender cell length = 36 bytes = 72 hex chars.
    assert_eq!(w.entries[3].cell_repr_data_hex.len(), 72);

    // Daemon-side slots stay None on the per-tx exporter.
    assert!(w.events_tree_proof.is_none());
    assert!(w.block_tree_proof.is_none());
    assert!(w.anchor.is_none());

    // Block context passes through verbatim.
    assert_eq!(w.block_seq_no, 4711);
    assert_eq!(w.block_id_hex, hex::encode([0x42u8; 32]));
    assert_eq!(
        w.block_context.account_dapp_id_hex,
        hex::encode([0xAAu8; 32])
    );

    // Event message hash should be 32 bytes hex = 64 chars.
    assert_eq!(w.event_message_hash_hex.len(), 64);
}

/// Round-trip the witness through JSON to catch any non-serializable fields.
#[test]
fn witness_roundtrips_through_json() {
    let ctx = BlockContextInput {
        block_id: [0u8; 32],
        block_seq_no: 0,
        account_dapp_id: [0u8; 32],
        account_id: [0u8; 32],
        envelope_hash: [0u8; 32],
    };
    let w = export_from_event_boc_base64(EVENT_BOC_B64, &ctx).unwrap();
    let json = serde_json::to_string(&w).expect("serialize");
    let _round: bridge_event_witness::schema::PrivateWitness =
        serde_json::from_str(&json).expect("deserialize");
}

#[test]
fn decoding_the_event_alone_gives_the_fields_the_exporter_gives() {
    let ctx = BlockContextInput {
        block_id: [0u8; 32],
        block_seq_no: 0,
        account_dapp_id: [0u8; 32],
        account_id: [0u8; 32],
        envelope_hash: [0u8; 32],
    };
    let witness = export_from_event_boc_base64(EVENT_BOC_B64, &ctx).unwrap();
    let event = bridge_event_witness::decode_event_boc_base64(EVENT_BOC_B64).unwrap();
    assert_eq!(event.amount_hex, witness.event.amount_hex);
    assert_eq!(event.recipient_hex, witness.event.recipient_hex);
    assert_eq!(event.dst_chain_id_hex, witness.event.dst_chain_id_hex);
    assert_eq!(event.token_id, witness.event.token_id);
    assert_eq!(event.dst_bridge_hex, witness.event.dst_bridge_hex);
    assert_eq!(event.sender_hex, witness.event.sender_hex);
}
