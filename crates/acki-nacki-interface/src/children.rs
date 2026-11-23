//! Parsing of blockchain-API responses into [`ChildTransaction`]s, kept free of
//! the `tvm-sdk` feature so it is testable in the default build.

use std::future::Future;

use serde_json::Value;

use crate::{
    error::Result,
    types::{ChildOutcome, ChildTransaction, Deploy, TxHash},
};

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

/// The newest deploy on one page of an account's transactions (from `NonExist`
/// or `Uninit` to `Active`, not aborted), and the cursor of the older page.
fn parse_deploy_page(res: &Value) -> (Option<Deploy>, Option<String>) {
    let Some(page) = res.pointer("/blockchain/account/transactions") else {
        return (None, None);
    };
    let edges = page
        .get("edges")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let deploy = edges
        .iter()
        .rev()
        .filter_map(|e| e.get("node"))
        .filter(|n| {
            matches!(
                n.get("orig_status_name").and_then(|v| v.as_str()),
                Some("NonExist" | "Uninit")
            ) && n.get("end_status_name").and_then(|v| v.as_str()) == Some("Active")
                && !n.get("aborted").and_then(|v| v.as_bool()).unwrap_or(true)
        })
        .find_map(|n| {
            Some(Deploy {
                tx_hash: tx_hash(n.get("id")?.as_str()?)?,
                now: n.get("now").and_then(Value::as_u64).unwrap_or(0),
            })
        });
    let older = page
        .pointer("/pageInfo/hasPreviousPage")
        .and_then(Value::as_bool)
        .filter(|more| *more)
        .and_then(|_| page.pointer("/pageInfo/startCursor")?.as_str())
        .map(str::to_owned);
    (deploy, older)
}

/// The account's latest deploy, read page by page back from its newest
/// transaction: any number of later transactions can sit on top of it.
/// `page` fetches the transactions before a cursor, the newest without one.
pub(crate) async fn last_deploy<F, Fut>(mut page: F) -> Result<Option<Deploy>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let mut before = None;
    loop {
        let (deploy, older) = parse_deploy_page(&page(before.clone()).await?);
        if deploy.is_some() {
            return Ok(deploy);
        }
        match older {
            Some(cursor) if before.as_ref() != Some(&cursor) => before = Some(cursor),
            _ => return Ok(None),
        }
    }
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

    fn tx(id: u8, orig: &str, end: &str, aborted: bool) -> Value {
        json!({ "node": { "id": hex::encode([id; 32]), "aborted": aborted,
                          "orig_status_name": orig, "end_status_name": end } })
    }

    fn page(edges: Vec<Value>, older: Option<&str>) -> Value {
        json!({ "blockchain": { "account": { "transactions": {
            "edges": edges,
            "pageInfo": { "hasPreviousPage": older.is_some(), "startCursor": older.unwrap_or("c") },
        } } } })
    }

    async fn walk(pages: Vec<(Option<&str>, Value)>) -> (Option<TxHash>, usize) {
        let mut reads = 0;
        let found = last_deploy(|before| {
            let (expected, answer) = pages[reads].clone();
            reads += 1;
            assert_eq!(before.as_deref(), expected);
            async move { Ok(answer) }
        })
        .await
        .unwrap();
        (found.map(|d| d.tx_hash), reads)
    }

    #[tokio::test]
    async fn a_deploy_behind_a_full_page_of_later_transactions_is_found() {
        let later = (0..50).map(|_| tx(9, "Active", "Active", true)).collect();
        let (found, reads) = walk(vec![
            (None, page(later, Some("c1"))),
            (
                Some("c1"),
                page(vec![tx(1, "Uninit", "Active", false)], None),
            ),
        ])
        .await;
        assert_eq!(found, Some([1; 32]));
        assert_eq!(reads, 2);
    }

    #[tokio::test]
    async fn the_newest_deploy_wins_and_older_pages_are_not_read() {
        let (found, reads) = walk(vec![(
            None,
            page(
                vec![
                    tx(1, "NonExist", "Active", false),
                    tx(2, "Active", "NonExist", false),
                    tx(3, "NonExist", "Active", false),
                    tx(4, "Active", "Active", false),
                ],
                Some("c1"),
            ),
        )])
        .await;
        assert_eq!(found, Some([3; 32]));
        assert_eq!(reads, 1);
    }

    #[tokio::test]
    async fn an_aborted_deploy_is_not_a_deploy_and_the_history_ends() {
        let (found, reads) = walk(vec![(
            None,
            page(vec![tx(1, "NonExist", "Active", true)], None),
        )])
        .await;
        assert_eq!(found, None);
        assert_eq!(reads, 1);
    }

    #[tokio::test]
    async fn a_cursor_that_does_not_move_ends_the_walk() {
        let (found, reads) = walk(vec![
            (None, page(vec![], Some("c1"))),
            (Some("c1"), page(vec![], Some("c1"))),
        ])
        .await;
        assert_eq!(found, None);
        assert_eq!(reads, 2);
    }

    #[tokio::test]
    async fn an_answer_without_the_account_finds_nothing() {
        let (found, reads) = walk(vec![(None, json!({ "blockchain": { "account": null } }))]).await;
        assert_eq!(found, None);
        assert_eq!(reads, 1);
    }
}
