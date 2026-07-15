//! exercise the BOC walk + event decode against the first
//! captured `WithdrawalInitiated` BOC from
//! `acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/withdrawals.txt`.
//!
//! The fixture is inlined here (rather than read from disk) so the test is
//! hermetic — no path coupling to a sibling repo, no flaky CI if that file
//! is regenerated. The constants are pinned and any drift will surface as a
//! field-level assertion failure.

use bridge_event_witness::schema::SCHEMA_VERSION;
use bridge_event_witness::{
    export_from_event_boc_base64, BlockContextInput,
};

/// First record from `withdrawals.txt`, captured by
/// `acki-nacki/tests/exchange/generate_withdrawals.py`.
const EVENT_BOC_B64: &str = "te6ccgEBBAEAyQABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAAIZdmoHXvdgAQJwPIOJWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAA9CQAAAAAIDAgBDgAm3yq/MUPIYsiAFU9xmlVK1j7ShCFBTfqlHoaqgQg+0MAAodC01zGY0wFMpJaO4RLxFTkQ49E4=";

const EXPECTED_TOKEN_ID: u32 = 2;
const EXPECTED_RECIPIENT_HEX: &str = "742d35cc6634c0532925a3b844bc454e4438f44e";
/// dst_chain_id = 1, padded to 32 BE bytes.
const EXPECTED_DST_CHAIN_ID_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000001";
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

    // Structural invariants the circuit depends on:
    assert_eq!(w.entries[0].refs_count, 1, "wrapper refs_count");
    assert_eq!(w.entries[1].refs_count, 2, "body refs_count");
    assert_eq!(w.entries[2].refs_count, 0, "recipient refs_count");
    assert_eq!(w.entries[3].refs_count, 0, "sender refs_count");

    // Body cell length = 126 bytes = 252 hex chars.
    assert_eq!(w.entries[1].cell_repr_data_hex.len(), 252);
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
