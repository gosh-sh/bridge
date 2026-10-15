//! Parsing of blockchain-API responses into [`ChildTransaction`]s, kept free of
//! the `tvm-sdk` feature so it is testable in the default build.

use serde_json::Value;

use crate::types::{ChildOutcome, ChildTransaction, TxHash};

fn tx_hash(id: &str) -> Option<TxHash> {
    let bytes = hex::decode(id.trim_start_matches("0x")).ok()?;
    if bytes.len() > 32 {
        return None;
    }
    let mut hash = [0u8; 32];
    hash[32 - bytes.len()..].copy_from_slice(&bytes);
    Some(hash)
}

fn child_outcome(tx: &Value) -> Option<ChildOutcome> {
    Some(ChildOutcome {
        tx_hash: tx_hash(tx.get("id")?.as_str()?)?,
        exit_code: tx
            .pointer("/compute/exit_code")
            .and_then(|v| v.as_i64())
            .map(|c| c as i32),
        aborted: tx.get("aborted").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Internal out-messages of a `blockchain { transaction { out_messages } }`
/// response, with the destination outcome where the API linked it.
pub(crate) fn parse_internal_children(res: &Value) -> Vec<ChildTransaction> {
    let Some(msgs) = res
        .pointer("/blockchain/transaction/out_messages")
        .and_then(|v| v.as_array())
    else {
        return vec![];
    };
    msgs.iter()
        .filter(|m| m.get("msg_type_name").and_then(|v| v.as_str()) == Some("Internal"))
        .filter_map(|m| {
            Some(ChildTransaction {
                message_id: m.get("id")?.as_str()?.to_owned(),
                dst: m.get("dst")?.as_str()?.to_owned(),
                outcome: m
                    .get("dst_transaction")
                    .filter(|t| !t.is_null())
                    .and_then(child_outcome),
            })
        })
        .collect()
}

/// The destination transaction of a `blockchain { message { dst_transaction }
/// }` response.
pub(crate) fn parse_message_dst_transaction(res: &Value) -> Option<ChildOutcome> {
    res.pointer("/blockchain/message/dst_transaction")
        .filter(|t| !t.is_null())
        .and_then(child_outcome)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Shapes as shellnet returned them on 2026-10-12 for a `finalizeDeposit`
    /// (bridge tx `4b722bbe…`) and its voucher deploy: null nested under
    /// `out_messages`, linked when the message is queried by hash.
    #[test]
    fn unlinked_child_resolves_through_the_message_query() {
        let msg = "9aa9755e3e9f89560c482a5a2baca77e29aa456d13a5b49f1619ca3d83ca927a";
        let voucher_tx = "cc1acf80002f680f159fa993f27c6cb9c8759f69aaff76e6a72c1c97ef1a69e8";
        let parent = json!({ "blockchain": { "transaction": { "out_messages": [
            { "id": "ffff", "dst": ":0000000000000000000000000000000000000000000000000000000000000268",
              "msg_type_name": "ExtOut", "dst_transaction": null },
            { "id": msg, "dst": "0:ffd5c4e7770b73e67ad8a44c828c0d2ee2e642cbbff53096ffd0c68ed0f9a80e",
              "msg_type_name": "Internal", "dst_transaction": null },
        ] } } });
        let children = parse_internal_children(&parent);
        assert_eq!(children.len(), 1, "external out-messages are not children");
        assert_eq!(children[0].message_id, msg);
        assert!(children[0].outcome.is_none());

        let message = json!({ "blockchain": { "message": { "dst_transaction": {
            "id": voucher_tx, "aborted": false, "compute": { "exit_code": 0 } } } } });
        let outcome = parse_message_dst_transaction(&message).unwrap();
        assert_eq!(hex::encode(outcome.tx_hash), voucher_tx);
        assert!(outcome.is_success());

        let unexecuted = json!({ "blockchain": { "message": { "dst_transaction": null } } });
        assert_eq!(parse_message_dst_transaction(&unexecuted), None);
    }

    #[test]
    fn linked_dst_transaction_is_used_directly() {
        let parent = json!({ "blockchain": { "transaction": { "out_messages": [
            { "id": "m1", "dst": "0:01", "msg_type_name": "Internal",
              "dst_transaction": { "id": "bb".repeat(32), "aborted": true,
                                   "compute": { "exit_code": 51 } } },
        ] } } });
        let outcome = parse_internal_children(&parent)[0].outcome.clone().unwrap();
        assert_eq!(outcome.exit_code, Some(51));
        assert!(!outcome.is_success());
    }
}
