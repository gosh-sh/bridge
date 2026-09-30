//! Finding an operation's transaction when the wallet's answer was lost
//! (or never comes, as with EIP-681). Bound by nonce slot; see `binding`.

use std::time::Duration;

use alloy_primitives::{Address, B256};

use crate::deposit::{
    binding::{decide, nonce_verdict, Binding, Candidate, Claims, NonceObs, NonceVerdict},
    evm::{BlockTag, EvmRead, DEPOSIT_TOPIC0},
    retry::{deadline_after, once, until},
    store::OpRecord,
    ui::Ui,
};

/// What the search for an operation's transaction concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// This transaction is the operation's, with the nonce the search itself
    /// read: the caller records it as is.
    Bound {
        /// The transaction hash.
        tx_hash: B256,
        /// The sender's nonce the transaction used.
        nonce: u64,
    },
    /// More than one transaction may be the operation's; the user names one.
    Ambiguous {
        /// The transactions that may be the operation's.
        hashes: Vec<B256>,
        /// Why none was bound.
        why: String,
    },
    /// Nothing found before the window closed; the outcome stays unknown.
    NotFound,
    /// The operation's nonce slot was used in finalized state, and the
    /// search through that state found no deposit of this operation there:
    /// its deposit can no longer execute.
    NonceConsumed,
}

/// Every bridge `Deposit` of the operation's sender from the block the
/// request was made at through `to_block`, with its transaction and receipt.
/// A log whose transaction or receipt the node does not return right now is
/// an error: the search is incomplete, and the caller reads again.
pub async fn candidates(
    evm: &dyn EvmRead,
    op: &OpRecord,
    bridge: Address,
    to_block: u64,
) -> anyhow::Result<Vec<Candidate>> {
    let (Some(from), Some(q)) = (op.from, op.request.as_ref()) else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for l in evm
        .deposit_logs(bridge, from, q.from_block, to_block)
        .await?
    {
        // Skipping such a log could turn a found deposit into "nothing was
        // deposited" once the slot is used in finalized state. A log that a
        // reorg removed is gone from the next log query anyway.
        let incomplete = |what: &str| {
            anyhow::anyhow!(
                "the node lists a Deposit of {:#x} but not its {what}; searching again",
                l.tx_hash
            )
        };
        let tx = evm
            .transaction(l.tx_hash)
            .await?
            .ok_or_else(|| incomplete("transaction"))?;
        let r = evm
            .receipt(l.tx_hash)
            .await?
            .ok_or_else(|| incomplete("receipt"))?;
        let has_bridge_deposit = r
            .logs
            .iter()
            .any(|x| x.address == bridge && x.topics.first() == Some(&DEPOSIT_TOPIC0));
        out.push(Candidate {
            tx_hash: l.tx_hash,
            nonce: tx.nonce,
            from: tx.from,
            input: tx.input,
            status_ok: r.status,
            has_bridge_deposit,
        });
    }
    Ok(out)
}

/// Looks for the operation's transaction every `poll` until it is bound,
/// found ambiguous, or its slot is consumed in finalized state. `window`
/// bounds the whole search, every read and retry included; when it runs out
/// the answer is [`SearchOutcome::NotFound`]. Without a window the search
/// goes on until it has an answer.
pub async fn search(
    evm: &dyn EvmRead,
    op: &OpRecord,
    others: &Claims,
    bridge: Address,
    window: Option<Duration>,
    ui: &dyn Ui,
    poll: Duration,
) -> anyhow::Result<SearchOutcome> {
    // The window bounds the reads too: an RPC that keeps failing must not
    // keep an interactive run past --recovery-window-s.
    let deadline = window.map(deadline_after);
    let from = op
        .from
        .ok_or_else(|| anyhow::anyhow!("operation {} has no sender", op.op_id))?;
    let wallet_hash = match op.tx {
        None => op.request.as_ref().and_then(|q| q.wallet_hash),
        Some(_) => None,
    };
    loop {
        // The wallet answered with this hash before its nonce could be read.
        // That transaction is the operation's whatever it did: a revert
        // leaves no Deposit log to search for, and the receipt check closes
        // it. Until the node shows it, its nonce is unknown: while the user
        // confirmed it, another transaction may have taken the slot the
        // operation observed and pushed this one to the next nonce. A failed
        // read is retried, never a reason to look at the slot instead.
        let mut unseen_wallet_hash = None;
        if let Some(h) = wallet_hash.filter(|h| !others.tx_hashes.contains(h)) {
            let Some(seen) = until(
                ui,
                "reading the transaction the wallet returned",
                deadline,
                || evm.transaction(h),
            )
            .await
            else {
                return Ok(SearchOutcome::NotFound);
            };
            match seen {
                Some(t) if t.from == from => {
                    return Ok(SearchOutcome::Bound {
                        tx_hash: h,
                        nonce: t.nonce,
                    })
                },
                Some(_) => {},
                None => unseen_wallet_hash = Some(h),
            }
        }
        // The slot's finalized state before the finalized block: read later,
        // the block is at least as high as the state the count came from, so
        // a search through it covers whatever consumed the slot.
        let finalized_count = match op.tx {
            Some(_) => once(deadline, evm.tx_count(from, BlockTag::Finalized)).await,
            None => None,
        };
        let finalized_block = once(deadline, evm.header(BlockTag::Finalized))
            .await
            .flatten()
            .map(|h| h.number);
        let Some(head) = until(ui, "reading the chain head", deadline, || async {
            evm.header(BlockTag::Latest)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no latest block"))
        })
        .await
        else {
            return Ok(SearchOutcome::NotFound);
        };
        let to_block = head.number.max(finalized_block.unwrap_or(0));
        let Some(cands) = until(
            ui,
            "searching for the deposit transaction",
            deadline,
            || candidates(evm, op, bridge, to_block),
        )
        .await
        else {
            return Ok(SearchOutcome::NotFound);
        };
        match decide(op, &cands, others) {
            Binding::Bind(h) => {
                let nonce = cands
                    .iter()
                    .find(|c| c.tx_hash == h)
                    .map(|c| c.nonce)
                    .ok_or_else(|| {
                        anyhow::anyhow!("bound {h:#x}, which is not among the candidates")
                    })?;
                return Ok(match unseen_wallet_hash {
                    Some(w) if w != h => SearchOutcome::Ambiguous {
                        hashes: vec![w, h],
                        why: format!(
                            "the wallet returned {w:#x}, which the node does not show, and {h:#x} \
                             took the nonce this operation observed; either may be this \
                             operation's"
                        ),
                    },
                    _ => SearchOutcome::Bound {
                        tx_hash: h,
                        nonce,
                    },
                });
            },
            Binding::Ambiguous {
                hashes,
                why,
            } => {
                return Ok(SearchOutcome::Ambiguous {
                    hashes,
                    why,
                })
            },
            Binding::NotYet => {},
        }
        if let Some(t) = op.tx {
            let Some(latest_count) = until(ui, "reading the account nonce", deadline, || {
                evm.tx_count(from, BlockTag::Latest)
            })
            .await
            else {
                return Ok(SearchOutcome::NotFound);
            };
            // Without the finalized block the search bound is unknown, and so
            // is whether the count's block was searched: no verdict from it.
            let finalized_count = finalized_count.filter(|_| finalized_block.is_some());
            let v = nonce_verdict(t.tx_nonce, NonceObs {
                latest_count,
                finalized_count,
            });
            if v == NonceVerdict::ConsumedFinal {
                // The logs were searched through a block no lower than the one
                // the finalized count was read at; nothing of ours uses the slot.
                return Ok(SearchOutcome::NonceConsumed);
            }
            ui.status(&v.status_text(t.tx_nonce));
        } else {
            ui.status("searching for the deposit transaction from the wallet");
        }
        let now = tokio::time::Instant::now();
        match deadline {
            Some(d) if now >= d => return Ok(SearchOutcome::NotFound),
            Some(d) => tokio::time::sleep(poll.min(d - now)).await,
            None => tokio::time::sleep(poll).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy_primitives::{address, Address, B256, U256};

    use super::*;
    use crate::deposit::{
        evm::{deposit_calldata, DepositLogRef},
        store::{OpParams, OpRecord, OpStage, RequestInfo, TxClaim},
        testkit::*,
        ui::RecordingUi,
    };

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
    const FROM: Address = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
    const ACC: B256 = B256::repeat_byte(0xa3);

    fn op(nonce_before: u64, tx: Option<TxClaim>) -> OpRecord {
        let mut r = OpRecord::new(
            "A".into(),
            OpParams {
                chain_id: 11_155_111,
                bridge: BRIDGE,
                to: "x".into(),
                amount_units: 5,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            "bd44".into(),
        );
        r.from = Some(FROM);
        r.stage = if tx.is_some() {
            OpStage::Signed
        } else {
            OpStage::Requested
        };
        r.request = Some(RequestInfo {
            nonce_before,
            from_block: 90,
            calldata: deposit_calldata(5, ACC),
            wallet_hash: None,
        });
        r.tx = tx;
        r
    }

    fn put_deposit(evm: &FakeEvm, h: B256, nonce: u64, block: u64) {
        let b = header(block, block as u8);
        let log = deposit_log(BRIDGE, U256::from(1), FROM, 5, ACC, Some(0));
        evm.logs.lock().unwrap().push(DepositLogRef {
            tx_hash: h,
            block_number: block,
            block_hash: b.hash,
            log: log.clone(),
        });
        evm.txs.lock().unwrap().insert(
            h,
            deposit_tx(h, FROM, BRIDGE, nonce, 2, deposit_calldata(5, ACC)),
        );
        evm.script_receipt(h, vec![Some(deposit_receipt(h, &b, true, vec![log]))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_transaction_executed_after_the_window_is_found_on_resume() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(100, 1)]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 7);
        let ui = RecordingUi::new(true);
        let first = search(
            &evm,
            &op(7, None),
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(60)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(first, SearchOutcome::NotFound);
        put_deposit(&evm, B256::repeat_byte(4), 7, 120);
        evm.latest.set([header(130, 2)]);
        let resumed = search(
            &evm,
            &op(7, None),
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(resumed, SearchOutcome::Bound {
            tx_hash: B256::repeat_byte(4),
            nonce: 7
        });
    }

    #[tokio::test(start_paused = true)]
    async fn the_hash_the_wallet_returned_is_checked_before_any_log() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(100, 1)]);
        let h = B256::repeat_byte(0x33);
        // Reverted: there is no Deposit log for a log search to find.
        evm.txs.lock().unwrap().insert(
            h,
            deposit_tx(h, FROM, BRIDGE, 7, 2, deposit_calldata(5, ACC)),
        );
        let mut o = op(7, None);
        o.request.as_mut().unwrap().wallet_hash = Some(h);
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &o,
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::Bound {
            tx_hash: h,
            nonce: 7
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_replacement_that_deposited_another_amount_is_bound_not_nonce_consumed() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        let h = B256::repeat_byte(0x44);
        let b = header(150, 0x15);
        let log = deposit_log(BRIDGE, U256::from(1), FROM, 1, ACC, Some(0));
        evm.logs.lock().unwrap().push(DepositLogRef {
            tx_hash: h,
            block_number: 150,
            block_hash: b.hash,
            log: log.clone(),
        });
        evm.txs.lock().unwrap().insert(
            h,
            deposit_tx(h, FROM, BRIDGE, 7, 2, deposit_calldata(1, ACC)),
        );
        evm.script_receipt(h, vec![Some(deposit_receipt(h, &b, true, vec![log]))]);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(
            got,
            SearchOutcome::Bound {
                tx_hash: h,
                nonce: 7
            },
            "USDC moved in the slot: step 5 must report it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_in_a_finalized_block_consumes_the_slot() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.finalized.set([Some(header(190, 3))]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        let ui = RecordingUi::new(true);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::NonceConsumed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_finalized_above_the_head_read_is_bound_not_nonce_consumed() {
        // `latest` lags behind `finalized` (another backend of the provider,
        // or a head read before the deposit landed): the search must reach
        // the block the finalized count comes from.
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.finalized.set([Some(header(260, 2))]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        put_deposit(&evm, B256::repeat_byte(4), 7, 250);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::Bound {
            tx_hash: B256::repeat_byte(4),
            nonce: 7
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_the_node_momentarily_does_not_return_is_not_nonce_consumed() {
        // The slot is used in finalized state by a replacement that did
        // deposit; the node lists its log but answers `None` for the
        // transaction twice. That search is incomplete, not empty.
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.finalized.set([Some(header(190, 3))]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        put_deposit(&evm, B256::repeat_byte(0x44), 7, 150);
        evm.miss_tx_reads
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::Bound {
            tx_hash: B256::repeat_byte(0x44),
            nonce: 7
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_finalized_count_without_the_finalized_block_decides_nothing() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(60)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::NotFound, "the search bound is unknown");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unseen_wallet_hash_keeps_the_slot_from_binding_another_deposit() {
        // The wallet answered h. While the user confirmed it, another deposit
        // took nonce 7, and h went out with nonce 8; the node does not show h yet.
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        let other = B256::repeat_byte(0x44);
        put_deposit(&evm, other, 7, 150);
        let h = B256::repeat_byte(0x33);
        let mut o = op(7, None);
        o.request.as_mut().unwrap().wallet_hash = Some(h);
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &o,
            &Claims::default(),
            BRIDGE,
            None,
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        let SearchOutcome::Ambiguous {
            hashes,
            why,
        } = got
        else {
            panic!("{got:?}")
        };
        assert_eq!(hashes, vec![h, other]);
        assert!(why.contains("the wallet returned"), "{why}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_read_of_the_wallet_hash_is_retried_not_skipped() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        put_deposit(&evm, B256::repeat_byte(0x44), 7, 150);
        let h = B256::repeat_byte(0x33);
        evm.txs.lock().unwrap().insert(
            h,
            deposit_tx(h, FROM, BRIDGE, 8, 2, deposit_calldata(5, ACC)),
        );
        evm.fail_tx_reads
            .store(3, std::sync::atomic::Ordering::SeqCst);
        let mut o = op(7, None);
        o.request.as_mut().unwrap().wallet_hash = Some(h);
        let ui = RecordingUi::new(true);
        let got = search(
            &evm,
            &o,
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(600)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(
            got,
            SearchOutcome::Bound {
                tx_hash: h,
                nonce: 8
            },
            "not the deposit in the observed slot"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_rpc_that_keeps_failing_does_not_outlive_the_recovery_window() {
        let evm = FakeEvm::sepolia(); // no head: every read of it fails
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let got = search(
            &evm,
            &op(7, None),
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(60)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::NotFound);
        assert!(
            t0.elapsed() <= Duration::from_secs(60),
            "{:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_that_is_not_final_keeps_waiting_until_the_deposit_wins() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.finalized.set([Some(header(190, 3))]);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 7);
        let ui = RecordingUi::new(true);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(60)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(got, SearchOutcome::NotFound, "no verdict from `latest`");
        assert!(ui
            .statuses()
            .iter()
            .any(|s| s.contains("waiting for finality")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_log_whose_receipt_the_node_does_not_return_fails_the_search_not_skips_it() {
        let evm = FakeEvm::sepolia();
        let h = B256::repeat_byte(0x44);
        put_deposit(&evm, h, 7, 150);
        let log = deposit_log(BRIDGE, U256::from(1), FROM, 5, ACC, Some(0));
        let mined = deposit_receipt(h, &header(150, 150), true, vec![log]);
        evm.script_receipt(h, vec![None, Some(mined)]);
        let err = candidates(&evm, &op(7, None), BRIDGE, 200)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("receipt"), "{err:#}");
        // Once the node answers, the log is a candidate with the nonce its
        // transaction used.
        let got = candidates(&evm, &op(7, None), BRIDGE, 200).await.unwrap();
        assert_eq!(got, vec![Candidate {
            tx_hash: h,
            nonce: 7,
            from: FROM,
            input: deposit_calldata(5, ACC),
            status_ok: true,
            has_bridge_deposit: true,
        }]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_finalized_read_does_not_outlive_the_recovery_window() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(200, 1)]);
        evm.hang_finalized
            .store(true, std::sync::atomic::Ordering::SeqCst);
        evm.counts.lock().unwrap().insert((FROM, "latest"), 8);
        evm.counts.lock().unwrap().insert((FROM, "finalized"), 8);
        let tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let got = search(
            &evm,
            &op(7, tx),
            &Claims::default(),
            BRIDGE,
            Some(Duration::from_secs(30)),
            &ui,
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert_eq!(
            got,
            SearchOutcome::NotFound,
            "no finalized block, no verdict"
        );
        assert!(
            t0.elapsed() <= Duration::from_secs(30),
            "{:?}",
            t0.elapsed()
        );
    }
}
