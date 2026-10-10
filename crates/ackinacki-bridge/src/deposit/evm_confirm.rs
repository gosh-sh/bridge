//! Step 5: is the deposit on chain, and is it the one requested?
//!
//! No verdict is taken from the first receipt. A reorg can re-execute the
//! same transaction in another block with another outcome, so a verdict
//! needs `--confirmations` of depth and a second reading of the same
//! block; a NEGATIVE verdict (exit 22, exit 35) needs the block to be
//! finalized as well, because it releases the lock that keeps a second
//! deposit from being requested.
//!
//! A receipt the node gave incompletely decides nothing either: it is read
//! again, like any read that failed.

use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};

use crate::deposit::{
    evm::{parse_deposit_log, BlockTag, EvmRead, LogLite, ReceiptLite, TxLite, DEPOSIT_TOPIC0},
    limits::{check_receipt_bounds, check_tx_shape, ShapeViolation, TxShape},
    log_index::receipt_log_index,
    retry,
    ui::Ui,
};

/// The deposit the wallet was asked to broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expect {
    /// The EVM bridge the deposit goes to.
    pub bridge: Address,
    /// The paired wallet's address.
    pub from: Address,
    /// The requested `deposit(amount, 0, account)` calldata.
    pub calldata: Bytes,
    /// The requested amount, in USDC units.
    pub amount: u64,
    /// The requested Acki Nacki recipient.
    pub account: B256,
    /// Blocks on top of the deposit's, its own included, before a verdict.
    pub confirmations: u64,
}

/// One reading of the chain about the deposit transaction.
#[expect(
    clippy::large_enum_variant,
    reason = "one of these exists per poll, judged and dropped. Boxing the receipt and the \
              transaction would add two allocations per poll for a size that is never multiplied."
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The transaction has no receipt yet.
    NoReceipt {
        /// The node still shows the transaction.
        tx_known: bool,
    },
    /// The transaction is mined.
    Receipt {
        /// Its receipt.
        receipt: ReceiptLite,
        /// The transaction itself.
        tx: TxLite,
        /// The latest block number.
        head: u64,
        /// The finalized height, read before the receipt.
        finalized: Option<u64>,
        /// The canonical block hash at the receipt's height, read after the
        /// receipt; only when that height is at or below `finalized`.
        canonical: Option<B256>,
    },
}

/// The requested deposit, as mined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The bridge's id of the deposit.
    pub deposit_id: U256,
    /// The block that holds it.
    pub block_number: u64,
    /// That block's hash.
    pub block_hash: B256,
    /// The `Deposit` log's `logIndex`: its index among all the logs of the
    /// block.
    pub block_log_index: u64,
    /// The `Deposit` log's position in the receipt: what the prover reads.
    pub receipt_log_index: u64,
    /// The RLP length of the transaction's access list.
    pub access_list_rlp_len: usize,
}

/// Why the transaction is not a deposit this CLI goes on with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Negative {
    /// The transaction reverted.
    Reverted,
    /// It succeeded without a `Deposit` event of the bridge.
    NoDeposit,
    /// It deposited something other than what was requested.
    Mismatch(String),
    /// It is the requested deposit, but the circuit cannot prove it.
    Unprovable(ShapeViolation),
}

/// What one observation allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The transaction is known but not mined yet.
    WaitMined,
    /// Neither a receipt nor the transaction: it left the chain.
    LostTx,
    /// Mined, not deep enough yet.
    WaitDepth {
        /// Blocks so far, the deposit's own included.
        have: u64,
        /// Blocks required.
        need: u64,
    },
    /// Depth reached for the first time: remember this block hash and read
    /// again.
    Settle(B256),
    /// The block changed since the last reading: start over.
    Restart,
    /// The node's answer lacks something the decision needs; read again.
    Reread(String),
    /// Negative so far, but its block is not final yet.
    WaitFinality(Negative),
    /// Negative, in a finalized block.
    Final(Negative),
    /// The requested deposit, settled.
    Confirmed(Found),
}

/// The start of every mismatch message, as the user sees it.
const DIFFERS: &str = "the wallet broadcast a deposit that differs from the request";

/// What a settled receipt says about the deposit.
enum Classified {
    /// The requested deposit.
    Found(Found),
    /// Something else; see [`Negative`].
    Negative(Negative),
    /// The node's answer lacks something the decision needs.
    Incomplete(String),
}

/// Classifies a settled receipt against the request.
fn classify(receipt: &ReceiptLite, tx: &TxLite, exp: &Expect) -> Classified {
    if !receipt.status {
        return Classified::Negative(Negative::Reverted);
    }
    // Every log of a mined receipt has its `logIndex`; one without it comes
    // from an incomplete view of the block.
    let Some(block_log_indices) = receipt
        .logs
        .iter()
        .map(|l| l.block_log_index)
        .collect::<Option<Vec<u64>>>()
    else {
        return Classified::Incomplete(format!(
            "the node returned the receipt of {} with a log that has no logIndex",
            receipt.tx_hash
        ));
    };
    let ours: Vec<(usize, &LogLite)> = receipt
        .logs
        .iter()
        .enumerate()
        .filter(|(_, l)| l.address == exp.bridge && l.topics.first() == Some(&DEPOSIT_TOPIC0))
        .collect();
    let [(position, log)] = ours.as_slice() else {
        return Classified::Negative(if ours.is_empty() {
            Negative::NoDeposit
        } else {
            Negative::Mismatch(format!(
                "{DIFFERS}: the transaction emitted {} Deposit events",
                ours.len()
            ))
        });
    };
    // The bridge always emits the whole event; one that does not decode
    // lost a topic or data on the way.
    let Some(ev) = parse_deposit_log(log) else {
        return Classified::Incomplete(format!(
            "the node returned the bridge's Deposit log of {} with {} topics and {} bytes of \
             data, which do not decode",
            receipt.tx_hash,
            log.topics.len(),
            log.data.len()
        ));
    };
    if ev.sender != exp.from || ev.amount != U256::from(exp.amount) || ev.an_account != exp.account
    {
        return Classified::Negative(Negative::Mismatch(format!(
            "{DIFFERS}: {} units to account {:#x} from {} were deposited, {} units to {:#x} from \
             {} were requested",
            ev.amount, ev.an_account, ev.sender, exp.amount, exp.account, exp.from
        )));
    }
    if tx.input != exp.calldata {
        return Classified::Negative(Negative::Mismatch(format!(
            "{DIFFERS}: the Deposit event matches, but the transaction to {} carries {} bytes of \
             calldata that are not the requested deposit call",
            tx.to
                .map(|a| a.to_string())
                .unwrap_or_else(|| "no address".into()),
            tx.input.len()
        )));
    }
    let max_log_data = receipt.logs.iter().map(|l| l.data.len()).max().unwrap_or(0);
    if let Err(v) = check_receipt_bounds(receipt.logs.len(), max_log_data) {
        return Classified::Negative(Negative::Unprovable(v));
    }
    if let Err(v) = check_tx_shape(
        &TxShape {
            tx_type: tx.tx_type,
            to: tx.to,
            input_len: tx.input.len(),
            access_list_rlp_len: tx.access_list_rlp_len,
        },
        exp.bridge,
    ) {
        return Classified::Negative(Negative::Unprovable(v));
    }
    let block_log_index = block_log_indices[*position];
    // The prover finds the log by its receipt position. The lookup by
    // `logIndex` lands elsewhere only if the node numbered two logs alike.
    let indices: Vec<Option<u64>> = block_log_indices.iter().copied().map(Some).collect();
    match receipt_log_index(&indices, block_log_index) {
        Ok(i) if i == *position as u64 => {},
        _ => {
            return Classified::Incomplete(format!(
                "the node returned the receipt of {} with logIndex {block_log_index} on more than \
                 one log",
                receipt.tx_hash
            ));
        },
    }
    Classified::Found(Found {
        deposit_id: ev.deposit_id,
        block_number: receipt.block_number,
        block_hash: receipt.block_hash,
        block_log_index,
        receipt_log_index: *position as u64,
        access_list_rlp_len: tx.access_list_rlp_len,
    })
}

/// Decides what `obs` allows, given the block hash of the previous reading
/// that reached depth (`seen`).
pub fn judge(obs: &Observation, seen: Option<B256>, exp: &Expect) -> Verdict {
    let (receipt, tx, head, finalized, canonical) = match obs {
        Observation::NoReceipt {
            tx_known: true,
        } => return Verdict::WaitMined,
        Observation::NoReceipt {
            tx_known: false,
        } => return Verdict::LostTx,
        Observation::Receipt {
            receipt,
            tx,
            head,
            finalized,
            canonical,
        } => (receipt, tx, *head, *finalized, *canonical),
    };
    let have = head.saturating_sub(receipt.block_number) + 1;
    if have < exp.confirmations {
        return Verdict::WaitDepth {
            have,
            need: exp.confirmations,
        };
    }
    match seen {
        None => return Verdict::Settle(receipt.block_hash),
        Some(h) if h != receipt.block_hash => return Verdict::Restart,
        Some(_) => {},
    }
    match classify(receipt, tx, exp) {
        Classified::Found(found) => Verdict::Confirmed(found),
        Classified::Incomplete(why) => Verdict::Reread(why),
        // A final height is not enough: the receipt may have been read before
        // a reorg that `finalized` was read after. The verdict stands only if
        // the canonical block at that height is the receipt's block.
        Classified::Negative(neg) => match (finalized, canonical) {
            (Some(f), Some(c)) if f >= receipt.block_number && c == receipt.block_hash => {
                Verdict::Final(neg)
            },
            (Some(f), Some(_)) if f >= receipt.block_number => Verdict::Restart,
            _ => Verdict::WaitFinality(neg),
        },
    }
}

/// Reads the chain about `tx_hash`: `finalized` first, the block at the
/// receipt's height last. If that block is still the receipt's, and its
/// height is at or below a head that was final before the receipt was
/// read, the receipt is the final one.
///
/// `finalized` and the canonical block are needed only for a negative
/// verdict, so each is one bounded attempt and a failure leaves it
/// unknown. Every other read fails the whole observation.
pub async fn observe(evm: &dyn EvmRead, tx_hash: B256) -> anyhow::Result<Observation> {
    let finalized = retry::once(None, evm.header(BlockTag::Finalized))
        .await
        .flatten()
        .map(|h| h.number);
    let Some(receipt) = evm.receipt(tx_hash).await? else {
        return Ok(Observation::NoReceipt {
            tx_known: evm.transaction(tx_hash).await?.is_some(),
        });
    };
    let tx = evm.transaction(tx_hash).await?.ok_or_else(|| {
        anyhow::anyhow!("the node has a receipt but no transaction for {tx_hash}")
    })?;
    let head = evm
        .header(BlockTag::Latest)
        .await?
        .map(|h| h.number)
        .unwrap_or(receipt.block_number);
    let canonical = match finalized {
        Some(f) if f >= receipt.block_number => {
            retry::once(None, evm.header(BlockTag::Number(receipt.block_number)))
                .await
                .flatten()
                .map(|h| h.hash)
        },
        _ => None,
    };
    Ok(Observation::Receipt {
        receipt,
        tx,
        head,
        finalized,
        canonical,
    })
}

/// How step 5 ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The requested deposit, settled.
    Confirmed(Found),
    /// A negative verdict in a finalized block.
    Final(Negative),
    /// The transaction left the chain; search for it by its nonce.
    Lost,
}

/// What the retries of this step report as.
const READING: &str = "reading the deposit receipt";

/// Watches `tx_hash` until a verdict: every `poll` while waiting, at once
/// after the first reading that reached depth. Failed reads are retried
/// without end; no verdict is taken from one.
pub async fn confirm(
    evm: &dyn EvmRead,
    tx_hash: B256,
    exp: &Expect,
    ui: &dyn Ui,
    poll: Duration,
) -> anyhow::Result<Outcome> {
    let mut seen: Option<B256> = None;
    let mut rereads = 0u32;
    loop {
        let obs = retry::transient(ui, READING, || observe(evm, tx_hash)).await;
        let verdict = judge(&obs, seen, exp);
        rereads = match verdict {
            Verdict::Reread(_) => rereads + 1,
            _ => 0,
        };
        match verdict {
            Verdict::Confirmed(f) => return Ok(Outcome::Confirmed(f)),
            Verdict::Final(n) => return Ok(Outcome::Final(n)),
            Verdict::LostTx => return Ok(Outcome::Lost),
            Verdict::Settle(h) => {
                seen = Some(h);
                continue;
            },
            Verdict::Restart => {
                ui.warn("the deposit's block was replaced by a reorg; checking it again");
                seen = None;
            },
            Verdict::Reread(why) => ui.retry(READING, rereads, &why),
            Verdict::WaitMined => ui.status("waiting for the deposit to be mined"),
            Verdict::WaitDepth {
                have,
                need,
            } => ui.status(&format!("waiting for confirmations: {have}/{need}")),
            Verdict::WaitFinality(n) => ui.status(&format!(
                "the deposit looks {} — waiting for its block to be finalized before deciding",
                match n {
                    Negative::Reverted => "reverted".to_string(),
                    Negative::NoDeposit => "to have no Deposit event".to_string(),
                    Negative::Mismatch(_) => "different from the request".to_string(),
                    Negative::Unprovable(v) => format!("unprovable ({v})"),
                }
            )),
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::*;
    use crate::deposit::{
        evm::{deposit_calldata, LogLite},
        testkit::*,
    };

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
    const USDC: Address = address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
    const FROM: Address = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
    const ACC: B256 = B256::repeat_byte(0xa3);
    const TX: B256 = B256::repeat_byte(0x77);

    fn exp() -> Expect {
        Expect {
            bridge: BRIDGE,
            from: FROM,
            calldata: deposit_calldata(12_500_000, ACC),
            amount: 12_500_000,
            account: ACC,
            confirmations: 12,
        }
    }

    fn obs(
        status: bool,
        logs: Vec<LogLite>,
        tx_type: u8,
        input: Bytes,
        head: u64,
        fin: Option<u64>,
    ) -> Observation {
        let b = header(100, 1);
        Observation::Receipt {
            receipt: deposit_receipt(TX, &b, status, logs),
            tx: deposit_tx(TX, FROM, BRIDGE, 7, tx_type, input),
            head,
            finalized: fin,
            // The finalized chain has the receipt's block, when it reaches it.
            canonical: fin.filter(|f| *f >= 100).map(|_| b.hash),
        }
    }

    fn good_logs() -> Vec<LogLite> {
        vec![
            transfer_log(USDC),
            deposit_log(BRIDGE, U256::from(5), FROM, 12_500_000, ACC, Some(41)),
        ]
    }

    #[test]
    fn nothing_is_decided_before_depth_and_a_second_reading() {
        let o = obs(true, good_logs(), 2, exp().calldata, 105, Some(90));
        assert!(matches!(judge(&o, None, &exp()), Verdict::WaitDepth {
            have: 6,
            need: 12
        }));
        let o = obs(true, good_logs(), 2, exp().calldata, 111, Some(90));
        assert_eq!(
            judge(&o, None, &exp()),
            Verdict::Settle(B256::repeat_byte(1))
        );
        assert_eq!(
            judge(&o, Some(B256::repeat_byte(9)), &exp()),
            Verdict::Restart
        );
    }

    #[test]
    fn a_good_deposit_is_confirmed_with_both_log_indices() {
        let o = obs(true, good_logs(), 2, exp().calldata, 111, Some(90));
        let Verdict::Confirmed(f) = judge(&o, Some(B256::repeat_byte(1)), &exp()) else {
            panic!()
        };
        assert_eq!(f.deposit_id, U256::from(5));
        assert_eq!(f.block_log_index, 41);
        assert_eq!(f.receipt_log_index, 1);
    }

    #[test]
    fn negative_outcomes_wait_for_finality() {
        let seen = Some(B256::repeat_byte(1));
        let reverted = obs(false, vec![], 2, exp().calldata, 111, Some(99));
        assert_eq!(
            judge(&reverted, seen, &exp()),
            Verdict::WaitFinality(Negative::Reverted)
        );
        let reverted_fin = obs(false, vec![], 2, exp().calldata, 111, Some(100));
        assert_eq!(
            judge(&reverted_fin, seen, &exp()),
            Verdict::Final(Negative::Reverted)
        );
        // A reverted legacy transaction is exit 22, not exit 35.
        let legacy_reverted = obs(false, vec![], 0, exp().calldata, 111, Some(100));
        assert_eq!(
            judge(&legacy_reverted, seen, &exp()),
            Verdict::Final(Negative::Reverted)
        );
        let no_log = obs(
            true,
            vec![transfer_log(USDC)],
            2,
            exp().calldata,
            111,
            Some(100),
        );
        assert_eq!(
            judge(&no_log, seen, &exp()),
            Verdict::Final(Negative::NoDeposit)
        );
        let legacy = obs(true, good_logs(), 0, exp().calldata, 111, Some(100));
        assert!(matches!(
            judge(&legacy, seen, &exp()),
            Verdict::Final(Negative::Unprovable(_))
        ));
    }

    #[test]
    fn a_receipt_whose_block_the_finalized_chain_lacks_decides_nothing() {
        // The receipt was read before a reorg, `finalized` after it: the
        // height is final, the block is not the receipt's.
        let seen = Some(B256::repeat_byte(1));
        let Observation::Receipt {
            receipt,
            tx,
            ..
        } = obs(false, vec![], 2, exp().calldata, 111, Some(100))
        else {
            unreachable!()
        };
        let replaced = Observation::Receipt {
            receipt: receipt.clone(),
            tx: tx.clone(),
            head: 111,
            finalized: Some(100),
            canonical: Some(B256::repeat_byte(2)),
        };
        assert_eq!(judge(&replaced, seen, &exp()), Verdict::Restart);
        let unread = Observation::Receipt {
            receipt,
            tx,
            head: 111,
            finalized: Some(100),
            canonical: None,
        };
        assert_eq!(
            judge(&unread, seen, &exp()),
            Verdict::WaitFinality(Negative::Reverted)
        );
    }

    #[test]
    fn an_rpc_without_finalized_never_gives_a_negative_verdict() {
        let reverted = obs(false, vec![], 2, exp().calldata, 10_000, None);
        assert_eq!(
            judge(&reverted, Some(B256::repeat_byte(1)), &exp()),
            Verdict::WaitFinality(Negative::Reverted)
        );
    }

    #[test]
    fn a_deposit_that_differs_from_the_request_is_a_mismatch() {
        let seen = Some(B256::repeat_byte(1));
        let other_amount = vec![deposit_log(BRIDGE, U256::from(5), FROM, 1, ACC, Some(41))];
        let o = obs(
            true,
            other_amount,
            2,
            deposit_calldata(1, ACC),
            111,
            Some(100),
        );
        assert!(matches!(
            judge(&o, seen, &exp()),
            Verdict::Final(Negative::Mismatch(_))
        ));
        let other_account = vec![deposit_log(
            BRIDGE,
            U256::from(5),
            FROM,
            12_500_000,
            B256::repeat_byte(1),
            Some(41),
        )];
        let o = obs(
            true,
            other_account,
            2,
            deposit_calldata(12_500_000, B256::repeat_byte(1)),
            111,
            Some(99),
        );
        assert!(matches!(
            judge(&o, seen, &exp()),
            Verdict::WaitFinality(Negative::Mismatch(_))
        ));
    }

    #[test]
    fn a_mismatch_names_what_was_broadcast_next_to_what_was_requested() {
        let logs = vec![deposit_log(BRIDGE, U256::from(5), FROM, 1, ACC, Some(41))];
        let o = obs(true, logs, 2, deposit_calldata(1, ACC), 111, Some(100));
        let Verdict::Final(Negative::Mismatch(m)) = judge(&o, Some(B256::repeat_byte(1)), &exp())
        else {
            panic!()
        };
        assert!(
            m.starts_with("the wallet broadcast a deposit that differs from the request"),
            "{m}"
        );
        assert!(m.contains("1 units") && m.contains("12500000 units"), "{m}");
    }

    #[test]
    fn calldata_other_than_requested_is_a_mismatch_even_with_a_matching_log() {
        let mut input = exp().calldata.to_vec();
        input.push(0);
        let o = obs(true, good_logs(), 2, Bytes::from(input), 111, Some(100));
        assert!(matches!(
            judge(&o, Some(B256::repeat_byte(1)), &exp()),
            Verdict::Final(Negative::Mismatch(_))
        ));
    }

    #[test]
    fn two_deposits_in_one_transaction_are_a_mismatch() {
        let mut logs = good_logs();
        logs.push(deposit_log(
            BRIDGE,
            U256::from(6),
            FROM,
            12_500_000,
            ACC,
            Some(42),
        ));
        let o = obs(true, logs, 2, exp().calldata, 111, Some(100));
        let Verdict::Final(Negative::Mismatch(m)) = judge(&o, Some(B256::repeat_byte(1)), &exp())
        else {
            panic!()
        };
        assert!(m.contains("2 Deposit events"), "{m}");
    }

    #[test]
    fn a_receipt_the_node_gave_incompletely_is_read_again_never_a_mismatch() {
        // Every case sits in a finalized block the canonical chain agrees
        // on, so a negative verdict here would be final: exit 35 for a
        // deposit that may well be the requested one.
        let seen = Some(B256::repeat_byte(1));
        let good = deposit_log(BRIDGE, U256::from(5), FROM, 12_500_000, ACC, Some(41));
        let no_index = LogLite {
            block_log_index: None,
            ..good.clone()
        };
        let other_amount_no_index = LogLite {
            block_log_index: None,
            ..deposit_log(BRIDGE, U256::from(5), FROM, 1, ACC, Some(41))
        };
        let transfer_no_index = LogLite {
            block_log_index: None,
            ..transfer_log(USDC)
        };
        let truncated = LogLite {
            data: good.data.slice(..64),
            ..good.clone()
        };
        let one_topic_short = LogLite {
            topics: good.topics[..2].to_vec(),
            ..good.clone()
        };
        let same_index = LogLite {
            block_log_index: Some(41),
            ..transfer_log(USDC)
        };
        let cases: [(&str, Vec<LogLite>); 6] = [
            ("Deposit log without logIndex", vec![
                transfer_log(USDC),
                no_index,
            ]),
            ("differing Deposit log without logIndex", vec![
                transfer_log(USDC),
                other_amount_no_index,
            ]),
            ("another log without logIndex", vec![
                transfer_no_index,
                good.clone(),
            ]),
            ("Deposit data cut short", vec![
                transfer_log(USDC),
                truncated,
            ]),
            ("Deposit topic missing", vec![
                transfer_log(USDC),
                one_topic_short,
            ]),
            ("two logs with one logIndex", vec![same_index, good]),
        ];
        for (what, logs) in cases {
            let o = obs(true, logs, 2, exp().calldata, 111, Some(100));
            assert!(
                matches!(judge(&o, seen, &exp()), Verdict::Reread(_)),
                "{what}: {:?}",
                judge(&o, seen, &exp())
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_log_without_its_index_is_read_again_until_the_node_gives_it() {
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        let bare = vec![
            transfer_log(USDC),
            deposit_log(BRIDGE, U256::from(5), FROM, 12_500_000, ACC, None),
        ];
        evm.txs
            .lock()
            .unwrap()
            .insert(TX, deposit_tx(TX, FROM, BRIDGE, 7, 2, exp().calldata));
        evm.script_receipt(TX, vec![
            Some(deposit_receipt(TX, &b, true, bare.clone())),
            Some(deposit_receipt(TX, &b, true, bare.clone())),
            Some(deposit_receipt(TX, &b, true, bare)),
            Some(deposit_receipt(TX, &b, true, good_logs())),
        ]);
        // The block is final and canonical: nothing but the missing index
        // stands between this read and a terminal verdict.
        evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
        evm.latest.set([header(111, 9)]);
        evm.finalized.set([Some(header(105, 8))]);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = confirm(&evm, TX, &exp(), &ui, std::time::Duration::from_secs(12))
            .await
            .unwrap();
        let Outcome::Confirmed(f) = out else {
            panic!("{out:?}")
        };
        assert_eq!((f.block_log_index, f.receipt_log_index), (41, 1));
        let retries: Vec<_> = ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                crate::deposit::ui::UiEvent::Retry(_, n, why) => Some((n, why)),
                _ => None,
            })
            .collect();
        assert_eq!(retries.len(), 2, "{retries:?}");
        assert_eq!(retries[1].0, 2);
        assert!(retries[0].1.contains("logIndex"), "{retries:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_mismatch_that_a_reorg_replaces_with_the_requested_deposit_goes_on() {
        let evm = FakeEvm::sepolia();
        let old = header(100, 1);
        let new = header(100, 2);
        let wrong = vec![
            transfer_log(USDC),
            deposit_log(BRIDGE, U256::from(5), FROM, 1, ACC, Some(41)),
        ];
        evm.txs
            .lock()
            .unwrap()
            .insert(TX, deposit_tx(TX, FROM, BRIDGE, 7, 2, exp().calldata));
        evm.script_receipt(TX, vec![
            Some(deposit_receipt(TX, &old, true, wrong.clone())),
            Some(deposit_receipt(TX, &old, true, wrong)),
            Some(deposit_receipt(TX, &new, true, good_logs())),
        ]);
        evm.latest.set([header(111, 9)]);
        evm.finalized.set([Some(header(95, 8))]); // the old block never gets finalized
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = confirm(&evm, TX, &exp(), &ui, std::time::Duration::from_secs(12))
            .await
            .unwrap();
        let Outcome::Confirmed(f) = out else {
            panic!("{out:?}")
        };
        assert_eq!(f.block_hash, B256::repeat_byte(2));
        assert!(ui
            .statuses()
            .iter()
            .any(|s| s.contains("different from the request")));
    }

    #[test]
    fn a_lost_transaction_goes_to_the_search() {
        assert_eq!(
            judge(
                &Observation::NoReceipt {
                    tx_known: true
                },
                None,
                &exp()
            ),
            Verdict::WaitMined
        );
        assert_eq!(
            judge(
                &Observation::NoReceipt {
                    tx_known: false
                },
                None,
                &exp()
            ),
            Verdict::LostTx
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_reverted_receipt_next_to_a_finalized_head_is_not_exit_22() {
        // The RPC still serves the receipt from block 100 (hash 1) while its
        // finalized chain has block 100 with hash 2, where the deposit succeeded.
        let evm = FakeEvm::sepolia();
        let old = header(100, 1);
        let new = header(100, 2);
        evm.txs
            .lock()
            .unwrap()
            .insert(TX, deposit_tx(TX, FROM, BRIDGE, 7, 2, exp().calldata));
        evm.script_receipt(TX, vec![
            Some(deposit_receipt(TX, &old, false, vec![])),
            Some(deposit_receipt(TX, &old, false, vec![])),
            Some(deposit_receipt(TX, &new, true, good_logs())),
        ]);
        evm.by_hash.lock().unwrap().insert(new.hash, new.clone()); // the canonical block 100
        evm.latest.set([header(111, 9)]);
        evm.finalized.set([Some(header(105, 8))]);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = confirm(&evm, TX, &exp(), &ui, std::time::Duration::from_secs(12))
            .await
            .unwrap();
        let Outcome::Confirmed(f) = out else {
            panic!("{out:?}")
        };
        assert_eq!(f.block_hash, B256::repeat_byte(2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_revert_that_a_reorg_turns_into_success_is_not_exit_22() {
        let evm = FakeEvm::sepolia();
        let old = header(100, 1);
        let new = header(100, 2);
        evm.txs
            .lock()
            .unwrap()
            .insert(TX, deposit_tx(TX, FROM, BRIDGE, 7, 2, exp().calldata));
        evm.script_receipt(TX, vec![
            Some(deposit_receipt(TX, &old, false, vec![])),
            Some(deposit_receipt(TX, &old, false, vec![])),
            Some(deposit_receipt(TX, &new, true, good_logs())),
        ]);
        evm.latest.set([header(111, 9)]);
        evm.finalized.set([Some(header(95, 8))]);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = confirm(&evm, TX, &exp(), &ui, std::time::Duration::from_secs(12))
            .await
            .unwrap();
        let Outcome::Confirmed(f) = out else {
            panic!("{out:?}")
        };
        assert_eq!(f.block_hash, B256::repeat_byte(2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_finalized_read_neither_holds_up_a_good_deposit_nor_decides_a_revert() {
        let b = header(100, 1);
        let chain = |status: bool, logs: Vec<LogLite>| {
            let evm = FakeEvm::sepolia();
            evm.txs
                .lock()
                .unwrap()
                .insert(TX, deposit_tx(TX, FROM, BRIDGE, 7, 2, exp().calldata));
            evm.script_receipt(TX, vec![Some(deposit_receipt(TX, &b, status, logs))]);
            evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
            evm.latest.set([header(111, 9)]);
            evm.finalized.set([Some(header(105, 8))]);
            evm.hang_finalized
                .store(true, std::sync::atomic::Ordering::SeqCst);
            evm
        };
        let poll = std::time::Duration::from_secs(12);
        let hour = std::time::Duration::from_secs(3600);

        let evm = chain(true, good_logs());
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = tokio::time::timeout(hour, confirm(&evm, TX, &exp(), &ui, poll))
            .await
            .expect("finality is not needed to confirm a good deposit")
            .unwrap();
        assert!(matches!(out, Outcome::Confirmed(_)), "{out:?}");

        let evm = chain(false, vec![]);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = tokio::time::timeout(hour, confirm(&evm, TX, &exp(), &ui, poll)).await;
        assert!(out.is_err(), "decided without knowing finality: {out:?}");
        assert!(
            ui.statuses().iter().any(|s| s.contains("looks reverted")),
            "{:?}",
            ui.statuses()
        );
    }
}
