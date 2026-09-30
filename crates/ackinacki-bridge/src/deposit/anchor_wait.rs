//! Step 6: wait until the bridge accepts the deposit's block, the block
//! is finalized, the receipt in it is still ours and the bridge is not
//! paused. While the block is not final, every cycle re-reads the
//! receipt: after a reorg the anchor for the old block never comes. Then,
//! before the anchor and the pause, every cycle looks whether somebody
//! finalized the deposit already: a credited deposit must not wait for a
//! pause or an anchor it no longer needs.
//!
//! `--anchor-timeout-s` bounds the whole wait, reads and their retries
//! included; the grace period after the anchor has its own bound,
//! `--relayer-grace-s`.

use std::{
    sync::{Mutex, PoisonError},
    time::Duration,
};

use alloy_primitives::B256;
use anyhow::{anyhow, Context as _};
use serde_json::json;

use crate::{
    deposit::{
        an::{AccStatus, AnRead},
        credit::{self, FinalizedCheck, Look},
        evm::{BlockTag, EvmRead},
        identity::BRIDGE_ABI,
        lc_readiness::{self, AnchorPlan, LcFailure},
        retry::{deadline_after, once, until},
        ui::Ui,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// What the wait has settled so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaitState {
    /// The deposit's block is final and still holds the receipt, so the
    /// receipt is no longer re-read.
    pub finalized_same: bool,
    /// The bridge accepted the block on the last poll.
    pub was_anchored: bool,
}

/// One poll of the chains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    /// The block hash in the deposit's receipt: `None` when the receipt
    /// was not re-read, `Some(None)` when the node has no receipt.
    pub receipt: Option<Option<B256>>,
    /// `finalized`, read before the receipt, is at or past the deposit's
    /// height.
    pub block_finalized: bool,
    /// The canonical block hash at the deposit's height, read after the
    /// receipt; only when that height is finalized and the receipt is ours.
    pub canonical: Option<B256>,
    /// The bridge accepts the deposit's block.
    pub anchored: bool,
    /// The bridge is paused; read only once the block is anchored.
    pub paused: bool,
    /// The light client's last known verdict; `None` when it is not
    /// watched or no re-check has been read yet.
    pub lc: Option<Result<(), LcFailure>>,
}

/// Why the bridge can stop accepting a block it had accepted.
pub const ANCHOR_LOST_CAUSES: &str = "the owner withdrew the anchor; an updateCode of the bridge \
                                      wiped all anchors, the allowlist and the pause flag; a \
                                      light-client anchor older than a year expired";

/// What one poll decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitStep {
    /// The receipt moved to another block: confirm the deposit again.
    BackToConfirm,
    /// The node has no receipt: search for the transaction by its nonce.
    Search,
    /// Keep waiting; the text says for what.
    Wait(String),
    /// The bridge stopped accepting the block; the text is the warning.
    AnchorLost(String),
    /// Anchored, final, still ours and not paused: prove it.
    Proceed,
}

/// Decides one poll, as the wait does around its look for a deposit
/// finalized already. The receipt is checked while the block is not final
/// with the receipt in it; then the anchor, then finality, then the pause.
#[cfg(test)]
pub fn step(
    st: &mut WaitState,
    t: &Tick,
    stored: B256,
    plan: &AnchorPlan,
    chain_id: u64,
) -> WaitStep {
    receipt_step(st, t, stored).unwrap_or_else(|| anchor_step(st, t, stored, plan, chain_id))
}

/// The receipt's part of a poll: back to step 5 or to the search when the
/// receipt moved or is gone, nothing while it is still ours. Once the
/// block is final with the receipt in it, `st` says so and the receipt is
/// no longer read.
pub fn receipt_step(st: &mut WaitState, t: &Tick, stored: B256) -> Option<WaitStep> {
    if st.finalized_same {
        return None;
    }
    match t.receipt {
        Some(None) => Some(WaitStep::Search),
        Some(Some(h)) if h != stored => Some(WaitStep::BackToConfirm),
        // Final only if the finalized chain has this very block at its height.
        Some(Some(_)) if t.block_finalized => match t.canonical {
            Some(c) if c == stored => {
                st.finalized_same = true;
                None
            },
            Some(_) => Some(WaitStep::BackToConfirm),
            None => None,
        },
        _ => None,
    }
}

/// The rest of a poll, once the receipt is still ours: the anchor, then
/// finality, then the pause.
pub fn anchor_step(
    st: &mut WaitState,
    t: &Tick,
    stored: B256,
    plan: &AnchorPlan,
    chain_id: u64,
) -> WaitStep {
    if st.was_anchored && !t.anchored {
        st.was_anchored = false;
        return WaitStep::AnchorLost(format!(
            "the bridge no longer accepts the deposit's block; waiting again. Possible causes: \
             {ANCHOR_LOST_CAUSES}"
        ));
    }
    if !t.anchored {
        let base = plan.status_line(stored);
        return WaitStep::Wait(match (plan, &t.lc) {
            (
                AnchorPlan::LightClient,
                Some(Err(
                    e @ (LcFailure::AncestryStale {
                        ..
                    }
                    | LcFailure::HeadStale {
                        ..
                    }),
                )),
            ) => format!(
                "{base} — but the light client's ancestry has stopped ({e}); the operator must \
                 restart it, and the bridge owner cannot help once owner anchors are off"
            ),
            (
                AnchorPlan::Owner {
                    ..
                },
                _,
            ) => format!(
                "{base}; the owner's call: setAcceptedBlockHash({chain_id}, {stored:#x}, true)"
            ),
            _ => base,
        });
    }
    st.was_anchored = true;
    if !st.finalized_same {
        return WaitStep::Wait(
            "the block is anchored; waiting for it to be finalized on the EVM side".into(),
        );
    }
    if t.paused {
        return WaitStep::Wait(
            "the bridge is paused by its owner; deposits finalize once it is lifted".into(),
        );
    }
    WaitStep::Proceed
}

/// How the wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitExit {
    /// Anchored, final and not paused, and no voucher in the grace period:
    /// build the proof here.
    Proceed,
    /// The voucher was deployed in the grace period: the operator's
    /// relayer is finalizing; confirm the credit.
    VoucherDeployed,
    /// Somebody finalized the deposit before the wait was through, found
    /// on a poll before the anchor and the pause were read: confirm the
    /// credit, by events when only they showed it.
    Finalized {
        /// Found by the bridge's `DepositFinalized` events: the recorded
        /// voucher address is stale.
        via_events: bool,
    },
    /// The receipt moved to another block: back to the EVM confirmation.
    BackToConfirm,
    /// The node has no receipt: search for the transaction by its nonce.
    Search,
}

/// What the wait needs to know about the deposit and the bridge.
#[derive(Debug, Clone)]
pub struct WaitCtx {
    /// The deposit transaction.
    pub tx_hash: B256,
    /// The block the deposit was confirmed in.
    pub block_hash: B256,
    /// That block's number.
    pub block_number: u64,
    /// The EVM chain id.
    pub chain_id: u64,
    /// The Acki Nacki bridge.
    pub bridge: [u8; 32],
    /// What shows that the deposit was finalized already, the deposit's
    /// voucher address among it.
    pub finalized: FinalizedCheck,
    /// The bridge's light client, if it names one.
    pub light_client: Option<[u8; 32]>,
    /// Who is expected to anchor the block.
    pub plan: AnchorPlan,
    /// `--anchor-timeout-s`; `None` waits without end.
    pub timeout: Option<Duration>,
    /// `--relayer-grace-s`.
    pub grace: Duration,
    /// The pause between two polls.
    pub poll: Duration,
    /// The operation, for `--resume`.
    pub op_id: String,
}

/// How many polls apart the light client is re-checked while it is the
/// one expected to anchor the block.
const LC_RECHECK_EVERY: u64 = 10;

/// What a light-client re-check that could not be read is reported as.
const LC_RECHECK: &str = "re-checking the light client";

/// The longest pause between two voucher reads in the grace period.
const VOUCHER_POLL: Duration = Duration::from_secs(15);

/// A bool getter of the bridge, retried until `deadline`. An answer that
/// is not a bool is a failed read, never a `false`.
async fn flag(
    an: &dyn AnRead,
    ui: &dyn Ui,
    deadline: Option<tokio::time::Instant>,
    bridge: [u8; 32],
    f: &'static str,
    input: serde_json::Value,
) -> Option<bool> {
    until(ui, "reading the Acki Nacki bridge", deadline, || async {
        let v = an
            .run_getter(bridge, BRIDGE_ABI, f, input.clone())
            .await
            .with_context(|| format!("could not read {f}"))?;
        v["value0"]
            .as_bool()
            .ok_or_else(|| anyhow!("{f} answered {v}"))
    })
    .await
}

/// Waits for the anchor, then gives the operator's relayer the grace
/// period. `--anchor-timeout-s` bounds everything up to the anchor —
/// reads, their retries, the light-client re-checks, the looks for a
/// deposit finalized already — and ends it with exit 31; the grace period
/// is bounded by `--relayer-grace-s` alone.
pub async fn wait(
    evm: &dyn EvmRead,
    an: &dyn AnRead,
    cx: &WaitCtx,
    ui: &dyn Ui,
) -> CliResult<WaitExit> {
    // Reads count against --anchor-timeout-s too: an RPC that keeps failing
    // must not make the timeout unreachable. The whole wait is under the
    // deadline, not only the reads that retry: the light-client check pages
    // through messages, and one hung call must not outlive it either.
    let deadline = cx.timeout.map(deadline_after);
    let last = Mutex::new(None);
    let anchor = until_anchored(evm, an, cx, ui, deadline, &last);
    let reached = match deadline {
        Some(d) => tokio::time::timeout_at(d, anchor).await.ok().flatten(),
        None => anchor.await,
    };
    let Some(reached) = reached else {
        let last = last.into_inner().unwrap_or_else(PoisonError::into_inner);
        return Err(timed_out(cx, last));
    };
    match reached {
        WaitExit::Proceed => {},
        other => return Ok(other),
    }
    // The operator's relayer may finalize it; do not spend 20 minutes proving in
    // parallel.
    let grace_end = deadline_after(cx.grace);
    while tokio::time::Instant::now() < grace_end {
        // An unreadable voucher only ends the grace period early: we prove.
        let Some(v) = until(
            ui,
            "looking for the deposit voucher",
            Some(grace_end),
            || an.account(cx.finalized.voucher),
        )
        .await
        else {
            break;
        };
        if v.is_some_and(|a| a.status == AccStatus::Active) {
            return Ok(WaitExit::VoucherDeployed);
        }
        ui.status("block anchored; giving the operator's relayer a moment before proving here");
        tokio::time::sleep_until((tokio::time::Instant::now() + VOUCHER_POLL).min(grace_end)).await;
    }
    Ok(WaitExit::Proceed)
}

/// Exit 31, with the last status the wait showed, if any.
fn timed_out(cx: &WaitCtx, last: Option<String>) -> CliError {
    let last = last
        .map(|s| format!("; last status: {s}"))
        .unwrap_or_default();
    CliError::deposit(
        ExitCode::AnWaitTimeout,
        Stage::Anchor,
        Some(&cx.op_id),
        format!(
            "the deposit is on the EVM bridge; the Acki Nacki side did not accept block {:#x} in \
             time (or could not be read){last}. Continue with --resume {}",
            cx.block_hash, cx.op_id
        ),
    )
}

/// The anchor loop: `None` when a read gave up at the deadline. Every
/// status it shows is kept in `last`, for the timeout's message.
async fn until_anchored(
    evm: &dyn EvmRead,
    an: &dyn AnRead,
    cx: &WaitCtx,
    ui: &dyn Ui,
    deadline: Option<tokio::time::Instant>,
    last: &Mutex<Option<String>>,
) -> Option<WaitExit> {
    let mut st = WaitState::default();
    let mut cycle = 0u64;
    // The light client's last verdict stays until a later re-check reads a
    // new one: a re-check that fails decides nothing.
    let mut lc = None;
    let mut lc_due = 0u64;
    let mut lc_failures = 0u32;
    loop {
        // `finalized` before the receipt and the block at the deposit's height
        // after it, as in step 5: a final height is not enough, the receipt
        // may have been read before a reorg that `finalized` was read after.
        // Unknown is "not finalized yet": the wait goes on.
        let finalized = once(deadline, evm.header(BlockTag::Finalized))
            .await
            .flatten()
            .map(|h| h.number);
        let block_finalized = finalized.is_some_and(|f| f >= cx.block_number);
        let (receipt, canonical) = if st.finalized_same {
            (None, None)
        } else {
            let r = until(ui, "re-reading the deposit receipt", deadline, || {
                evm.receipt(cx.tx_hash)
            })
            .await?;
            let r = r.map(|r| r.block_hash);
            let canonical = if block_finalized && r == Some(cx.block_hash) {
                once(deadline, evm.header(BlockTag::Number(cx.block_number)))
                    .await
                    .flatten()
                    .map(|h| h.hash)
            } else {
                None
            };
            (Some(r), canonical)
        };
        let mut t = Tick {
            receipt,
            block_finalized,
            canonical,
            anchored: false,
            paused: false,
            lc: None,
        };
        // A receipt that moved goes back to step 5 first: the deposit id,
        // and with it the voucher address, may be different there.
        match receipt_step(&mut st, &t, cx.block_hash) {
            Some(WaitStep::Search) => return Some(WaitExit::Search),
            Some(_) => return Some(WaitExit::BackToConfirm),
            None => {},
        }
        // Before the anchor and the pause: a deposit the operator's relayer
        // finalized while this run was not looking needs neither. One
        // attempt at each read; one that fails is looked at again next
        // cycle.
        if let Some(via_events) =
            credit::finalized_already(an, ui, cx.bridge, &cx.finalized, Look::Poll, deadline).await
        {
            return Some(WaitExit::Finalized {
                via_events,
            });
        }
        let block = json!({ "chainId": cx.chain_id.to_string(), "blockHash": format!("{:#x}", cx.block_hash) });
        let anchored = flag(an, ui, deadline, cx.bridge, "isAcceptedBlockHash", block).await?;
        let paused = anchored && flag(an, ui, deadline, cx.bridge, "isPaused", json!({})).await?;
        // Only when owner anchors are off: then nobody else can anchor the
        // block, so the light client's whole history is read (no page cap).
        // With them on, the status names the owner's call whatever the light
        // client's state, and nothing is re-checked.
        if let (false, AnchorPlan::LightClient, Some(lc_id)) = (anchored, cx.plan, cx.light_client)
        {
            if cycle >= lc_due {
                match lc_readiness::observe(an, evm, cx.bridge, lc_id, cx.chain_id, ui, None).await
                {
                    Ok(o) => {
                        lc = Some(lc_readiness::judge(&o, cx.chain_id));
                        lc_failures = 0;
                        lc_due = cycle + LC_RECHECK_EVERY;
                    },
                    Err(e) => {
                        lc_failures = lc_failures.saturating_add(1);
                        ui.retry(LC_RECHECK, lc_failures, &format!("{e:#}"));
                        lc_due = cycle + 1;
                    },
                }
            }
        }
        t.anchored = anchored;
        t.paused = paused;
        t.lc = lc.clone();
        match anchor_step(&mut st, &t, cx.block_hash, &cx.plan, cx.chain_id) {
            WaitStep::BackToConfirm => return Some(WaitExit::BackToConfirm),
            WaitStep::Search => return Some(WaitExit::Search),
            WaitStep::AnchorLost(w) => ui.warn(&w),
            WaitStep::Wait(s) => {
                ui.status(&s);
                *last.lock().unwrap_or_else(PoisonError::into_inner) = Some(s);
            },
            WaitStep::Proceed => return Some(WaitExit::Proceed),
        }
        cycle += 1;
        tokio::time::sleep(cx.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;

    use super::*;

    const STORED: B256 = B256::repeat_byte(1);
    const OWNER: AnchorPlan = AnchorPlan::Owner {
        lc_ready: false,
    };

    fn tick(receipt: Option<Option<B256>>, fin: bool, anchored: bool, paused: bool) -> Tick {
        // The finalized chain has the stored block, when it reaches it.
        Tick {
            receipt,
            block_finalized: fin,
            canonical: fin.then_some(STORED),
            anchored,
            paused,
            lc: None,
        }
    }

    #[test]
    fn a_reorg_during_the_wait_goes_back_to_step_5() {
        let mut st = WaitState::default();
        assert_eq!(
            step(
                &mut st,
                &tick(Some(Some(B256::repeat_byte(2))), false, false, false),
                STORED,
                &OWNER,
                1
            ),
            WaitStep::BackToConfirm
        );
        let mut st = WaitState::default();
        assert_eq!(
            step(
                &mut st,
                &tick(Some(None), false, false, false),
                STORED,
                &OWNER,
                1
            ),
            WaitStep::Search
        );
    }

    #[test]
    fn an_anchor_before_finality_does_not_start_the_proof() {
        let mut st = WaitState::default();
        assert!(
            matches!(step(&mut st, &tick(Some(Some(STORED)), false, true, false), STORED, &OWNER, 1), WaitStep::Wait(s) if s.contains("finalized"))
        );
        assert_eq!(
            step(
                &mut st,
                &tick(Some(Some(STORED)), true, true, false),
                STORED,
                &OWNER,
                1
            ),
            WaitStep::Proceed
        );
    }

    #[test]
    fn a_final_height_with_another_block_on_it_goes_back_to_step_5() {
        // The receipt was read before a reorg, `finalized` after it.
        let mut st = WaitState::default();
        let t = Tick {
            canonical: Some(B256::repeat_byte(2)),
            ..tick(Some(Some(STORED)), true, true, false)
        };
        assert_eq!(
            step(&mut st, &t, STORED, &OWNER, 1),
            WaitStep::BackToConfirm
        );
        assert!(!st.finalized_same);
        // Unread: nothing is settled, the receipt is read again next cycle.
        let t = Tick {
            canonical: None,
            ..tick(Some(Some(STORED)), true, true, false)
        };
        assert!(matches!(
            step(&mut st, &t, STORED, &OWNER, 1),
            WaitStep::Wait(_)
        ));
        assert!(!st.finalized_same);
    }

    #[test]
    fn once_finalized_the_receipt_is_no_longer_read() {
        let mut st = WaitState::default();
        let _ = step(
            &mut st,
            &tick(Some(Some(STORED)), true, false, false),
            STORED,
            &OWNER,
            1,
        );
        assert!(st.finalized_same);
        assert!(matches!(
            step(&mut st, &tick(None, true, false, false), STORED, &OWNER, 1),
            WaitStep::Wait(_)
        ));
    }

    #[test]
    fn a_withdrawn_anchor_warns_and_waits_again() {
        let mut st = WaitState {
            finalized_same: true,
            was_anchored: true,
        };
        let s = step(&mut st, &tick(None, true, false, false), STORED, &OWNER, 1);
        assert!(matches!(s, WaitStep::AnchorLost(w) if w.contains("updateCode")));
    }

    #[test]
    fn a_paused_bridge_holds_the_proof() {
        let mut st = WaitState {
            finalized_same: true,
            was_anchored: false,
        };
        assert!(
            matches!(step(&mut st, &tick(None, true, true, true), STORED, &OWNER, 1), WaitStep::Wait(s) if s.contains("paused"))
        );
    }

    #[test]
    fn the_owner_status_names_the_call_to_make() {
        let mut st = WaitState {
            finalized_same: true,
            was_anchored: false,
        };
        let WaitStep::Wait(s) = step(
            &mut st,
            &tick(None, true, false, false),
            STORED,
            &OWNER,
            11_155_111,
        ) else {
            panic!()
        };
        assert!(s.contains("setAcceptedBlockHash(11155111, 0x0101"), "{s}");
    }

    #[test]
    fn a_stopped_ancestry_is_said_plainly() {
        let mut st = WaitState {
            finalized_same: true,
            was_anchored: false,
        };
        let t = Tick {
            lc: Some(Err(LcFailure::AncestryStale {
                lag_s: 900,
            })),
            ..tick(None, true, false, false)
        };
        let WaitStep::Wait(s) = step(&mut st, &t, STORED, &AnchorPlan::LightClient, 1) else {
            panic!()
        };
        assert!(
            s.contains("ancestry has stopped") && s.contains("operator"),
            "{s}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_relayer_wins_during_the_grace_period() {
        use crate::deposit::{
            an::{AccStatus, AccountInfo},
            testkit::*,
        };
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        evm.script_receipt(B256::repeat_byte(7), vec![Some(deposit_receipt(
            B256::repeat_byte(7),
            &b,
            true,
            vec![],
        ))]);
        evm.txs.lock().unwrap().insert(
            B256::repeat_byte(7),
            deposit_tx(
                B256::repeat_byte(7),
                Default::default(),
                Default::default(),
                1,
                2,
                Default::default(),
            ),
        );
        evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
        evm.latest.set([header(200, 9)]);
        evm.finalized.set([Some(header(150, 8))]);
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(b.hash, 2); // accepted on the third poll
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        an.accounts.lock().unwrap().insert([0xee; 32], AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        });
        let cx = WaitCtx {
            tx_hash: B256::repeat_byte(7),
            block_hash: b.hash,
            block_number: 100,
            chain_id: 11_155_111,
            bridge: [0x1a; 32],
            finalized: finalized_check(),
            light_client: None,
            plan: AnchorPlan::Owner {
                lc_ready: false,
            },
            timeout: None,
            grace: std::time::Duration::from_secs(120),
            poll: std::time::Duration::from_secs(30),
            op_id: "OP".into(),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        assert_eq!(
            wait(&evm, &an, &cx, &ui).await.unwrap(),
            WaitExit::VoucherDeployed
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failing_reads_do_not_outlive_the_anchor_timeout() {
        use crate::deposit::testkit::*;
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        evm.script_receipt(B256::repeat_byte(7), vec![Some(deposit_receipt(
            B256::repeat_byte(7),
            &b,
            true,
            vec![],
        ))]);
        evm.finalized.set([Some(header(150, 8))]);
        let an = FakeAn::default();
        an.fail_getters
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let cx = WaitCtx {
            tx_hash: B256::repeat_byte(7),
            block_hash: b.hash,
            block_number: 100,
            chain_id: 11_155_111,
            bridge: [0x1a; 32],
            finalized: finalized_check(),
            light_client: None,
            plan: AnchorPlan::Owner {
                lc_ready: false,
            },
            timeout: Some(std::time::Duration::from_secs(300)),
            grace: std::time::Duration::from_secs(120),
            poll: std::time::Duration::from_secs(30),
            op_id: "OP".into(),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            wait(&evm, &an, &cx, &ui).await.unwrap_err().exit_code(),
            crate::errors::ExitCode::AnWaitTimeout
        );
        assert!(t0.elapsed() <= std::time::Duration::from_secs(300));
    }

    #[tokio::test(start_paused = true)]
    async fn a_receipt_from_a_block_the_finalized_chain_replaced_is_not_proved() {
        // The node still serves the receipt from block 100 (hash 1), which the
        // owner anchored before the reorg; the finalized chain has block 100
        // with hash 2.
        use crate::deposit::testkit::*;
        let evm = FakeEvm::sepolia();
        let old = header(100, 1);
        let new = header(100, 2);
        evm.script_receipt(B256::repeat_byte(7), vec![Some(deposit_receipt(
            B256::repeat_byte(7),
            &old,
            true,
            vec![],
        ))]);
        evm.by_hash.lock().unwrap().insert(new.hash, new.clone());
        evm.latest.set([header(200, 9)]);
        evm.finalized.set([Some(header(150, 8))]);
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(old.hash, 0);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        let cx = WaitCtx {
            tx_hash: B256::repeat_byte(7),
            block_hash: old.hash,
            block_number: 100,
            chain_id: 11_155_111,
            bridge: [0x1a; 32],
            finalized: finalized_check(),
            light_client: None,
            plan: AnchorPlan::Owner {
                lc_ready: false,
            },
            timeout: Some(std::time::Duration::from_secs(300)),
            grace: std::time::Duration::from_secs(1),
            poll: std::time::Duration::from_secs(30),
            op_id: "OP".into(),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        assert_eq!(
            wait(&evm, &an, &cx, &ui).await.unwrap(),
            WaitExit::BackToConfirm
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_read_outside_the_retries_does_not_outlive_the_anchor_timeout() {
        // `getConfig` is the light-client re-check's first read, made once
        // and without a retry around it: only the step's own deadline can
        // end it.
        use crate::deposit::testkit::*;
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        evm.script_receipt(B256::repeat_byte(7), vec![Some(deposit_receipt(
            B256::repeat_byte(7),
            &b,
            true,
            vec![],
        ))]);
        evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
        evm.finalized.set([Some(header(150, 8))]);
        let an = FakeAn::default();
        an.hang_getter_after
            .lock()
            .unwrap()
            .insert("getConfig".into(), 0);
        let cx = WaitCtx {
            tx_hash: B256::repeat_byte(7),
            block_hash: b.hash,
            block_number: 100,
            chain_id: 11_155_111,
            bridge: [0x1a; 32],
            finalized: finalized_check(),
            light_client: Some([0x1c; 32]),
            plan: AnchorPlan::LightClient,
            timeout: Some(std::time::Duration::from_secs(300)),
            grace: std::time::Duration::from_secs(120),
            poll: std::time::Duration::from_secs(30),
            op_id: "OP".into(),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(3600),
            wait(&evm, &an, &cx, &ui),
        )
        .await
        .expect("the anchor timeout must end the wait");
        assert_eq!(
            r.unwrap_err().exit_code(),
            crate::errors::ExitCode::AnWaitTimeout
        );
        assert!(
            t0.elapsed() <= std::time::Duration::from_secs(300),
            "{:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_anchor_until_the_timeout_is_exit_31() {
        use crate::deposit::testkit::*;
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        evm.script_receipt(B256::repeat_byte(7), vec![Some(deposit_receipt(
            B256::repeat_byte(7),
            &b,
            true,
            vec![],
        ))]);
        evm.txs.lock().unwrap().insert(
            B256::repeat_byte(7),
            deposit_tx(
                B256::repeat_byte(7),
                Default::default(),
                Default::default(),
                1,
                2,
                Default::default(),
            ),
        );
        evm.latest.set([header(200, 9)]);
        evm.finalized.set([Some(header(150, 8))]);
        let an = FakeAn::default(); // no block is ever accepted
        let cx = WaitCtx {
            tx_hash: B256::repeat_byte(7),
            block_hash: b.hash,
            block_number: 100,
            chain_id: 11_155_111,
            bridge: [0x1a; 32],
            finalized: finalized_check(),
            light_client: None,
            plan: AnchorPlan::Owner {
                lc_ready: false,
            },
            timeout: Some(std::time::Duration::from_secs(300)),
            grace: std::time::Duration::from_secs(120),
            poll: std::time::Duration::from_secs(30),
            op_id: "OP".into(),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        assert_eq!(
            wait(&evm, &an, &cx, &ui).await.unwrap_err().exit_code(),
            crate::errors::ExitCode::AnWaitTimeout
        );
    }

    // The loop against the fakes, beyond the cases above.

    use std::time::Duration;

    use crate::deposit::{
        an::{AccStatus, AccountInfo, MsgView, TxRef},
        evm::Header,
        lc_readiness::ANCESTRY_ACCEPTED_DST,
        testkit::*,
        ui::{RecordingUi, UiEvent},
    };

    const TX: B256 = B256::repeat_byte(7);
    const BRIDGE: [u8; 32] = [0x1a; 32];
    const VOUCHER: [u8; 32] = [0xee; 32];
    const LC: [u8; 32] = [0x1c; 32];

    /// The deposit's block 100 (hash 1), final under `finalized` 150, with
    /// the receipt still in it.
    fn final_deposit() -> (FakeEvm, Header) {
        let evm = FakeEvm::sepolia();
        let b = header(100, 1);
        evm.script_receipt(TX, vec![Some(deposit_receipt(TX, &b, true, vec![]))]);
        evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
        evm.finalized.set([Some(header(150, 8))]);
        (evm, b)
    }

    fn ctx(b: &Header, plan: AnchorPlan, timeout: Option<u64>) -> WaitCtx {
        WaitCtx {
            tx_hash: TX,
            block_hash: b.hash,
            block_number: b.number,
            chain_id: 11_155_111,
            bridge: BRIDGE,
            finalized: finalized_check(),
            light_client: matches!(plan, AnchorPlan::LightClient).then_some(LC),
            plan,
            timeout: timeout.map(Duration::from_secs),
            grace: Duration::from_secs(120),
            poll: Duration::from_secs(30),
            op_id: "OP".into(),
        }
    }

    fn deployed_voucher(an: &FakeAn) {
        an.accounts.lock().unwrap().insert(VOUCHER, AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        });
    }

    /// The voucher code the operation recorded.
    const CODE: &str = "c0de";

    /// The voucher [`VOUCHER`] under [`CODE`], for a deposit whose events
    /// no test here emits.
    fn finalized_check() -> FinalizedCheck {
        FinalizedCheck {
            stored_code: CODE.into(),
            voucher: VOUCHER,
            identity: credit::Identity {
                deposit_id: alloy_primitives::U256::from(6),
                contract: alloy_primitives::U256::from(0xb1),
                chain_id: 11_155_111,
                amount: 12_500_000,
                account: [0xa3; 32],
            },
            not_before: 0,
        }
    }

    /// The bridge names `codes` as its voucher code, one per read, the
    /// last one from then on.
    fn voucher_codes(an: &FakeAn, codes: &[&str]) {
        let answers = codes
            .iter()
            .map(|c| serde_json::json!({ "value0": format!("0x{c}") }))
            .collect();
        an.getter(BRIDGE, "getDepositVoucherCodeHash", answers);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_finalized_before_the_anchor_ends_the_wait_at_the_first_poll() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default(); // no block is ever accepted
        voucher_codes(&an, &[CODE]);
        deployed_voucher(&an);
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            wait(
                &evm,
                &an,
                &ctx(&b, OWNER, Some(300)),
                &RecordingUi::new(true)
            )
            .await
            .unwrap(),
            WaitExit::Finalized {
                via_events: false
            }
        );
        assert_eq!(t0.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_finalized_during_a_pause_ends_the_wait_without_it_being_lifted() {
        // Anchored and paused for good; the voucher shows up on the third
        // poll, once the bridge names the recorded code again.
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(b.hash, 0);
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": true}),
        ]);
        voucher_codes(&an, &["0ther", "0ther", CODE]);
        deployed_voucher(&an);
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            wait(&evm, &an, &ctx(&b, OWNER, None), &ui).await.unwrap(),
            WaitExit::Finalized {
                via_events: false
            }
        );
        assert_eq!(t0.elapsed(), Duration::from_secs(60));
        assert!(
            ui.statuses().iter().any(|s| s.contains("paused")),
            "{:?}",
            ui.statuses()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_receipt_that_moved_goes_back_to_step_5_before_the_finalized_look() {
        // The voucher at the recorded address belongs to the deposit id of
        // the old block; step 5 reads the one the deposit has now.
        let b = header(100, 1);
        let evm = FakeEvm::sepolia();
        evm.script_receipt(TX, vec![Some(deposit_receipt(
            TX,
            &header(101, 2),
            true,
            vec![],
        ))]);
        evm.finalized.set([Some(header(90, 8))]);
        let an = FakeAn::default();
        voucher_codes(&an, &[CODE]);
        deployed_voucher(&an);
        assert_eq!(
            wait(
                &evm,
                &an,
                &ctx(&b, OWNER, Some(300)),
                &RecordingUi::new(true)
            )
            .await
            .unwrap(),
            WaitExit::BackToConfirm
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_voucher_read_that_fails_is_not_a_finalized_deposit() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(b.hash, 1);
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        voucher_codes(&an, &[CODE]);
        deployed_voucher(&an);
        an.failing_accounts.lock().unwrap().insert(VOUCHER);
        // Not known is not finalized: the wait goes on to the anchor, and
        // the grace period cannot read the voucher either.
        assert_eq!(
            wait(
                &evm,
                &an,
                &ctx(&b, OWNER, Some(300)),
                &RecordingUi::new(true)
            )
            .await
            .unwrap(),
            WaitExit::Proceed
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pause_and_a_withdrawn_anchor_hold_the_proof_until_both_clear() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        // Anchored and paused; the anchor withdrawn; anchored again and the
        // pause lifted.
        an.accepted_seq
            .lock()
            .unwrap()
            .insert(b.hash, Script::new([true, false, true]));
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": true}),
            serde_json::json!({"value0": false}),
        ]);
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let cx = ctx(&b, OWNER, None);
        assert_eq!(wait(&evm, &an, &cx, &ui).await.unwrap(), WaitExit::Proceed);
        assert!(
            ui.statuses().iter().any(|s| s.contains("paused")),
            "{:?}",
            ui.statuses()
        );
        assert!(
            ui.warnings().iter().any(|w| w.contains("updateCode")),
            "{:?}",
            ui.warnings()
        );
        // Two polls before the anchor held, then the whole grace period
        // with no voucher.
        assert_eq!(t0.elapsed(), Duration::from_secs(60) + cx.grace);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bridge_that_stays_paused_ends_in_exit_31_saying_so() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(b.hash, 0);
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": true}),
        ]);
        let ui = RecordingUi::new(true);
        let e = wait(&evm, &an, &ctx(&b, OWNER, Some(300)), &ui)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::AnWaitTimeout);
        assert!(e.to_string().contains("paused"), "{e}");
        assert!(e.to_string().contains("--resume OP"), "{e}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_receipt_that_moves_or_vanishes_on_a_later_poll_ends_the_wait() {
        // Block 100 is not final yet, so every poll reads the receipt again.
        // A reorg moves the deposit on the third poll, and the owner anchors
        // only the block it moved to.
        let b = header(100, 1);
        let moved = header(101, 2);
        let evm = FakeEvm::sepolia();
        evm.script_receipt(TX, vec![
            Some(deposit_receipt(TX, &b, true, vec![])),
            Some(deposit_receipt(TX, &b, true, vec![])),
            Some(deposit_receipt(TX, &moved, true, vec![])),
        ]);
        evm.finalized.set([Some(header(90, 8))]);
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(moved.hash, 0);
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            wait(&evm, &an, &ctx(&b, OWNER, Some(3600)), &ui)
                .await
                .unwrap(),
            WaitExit::BackToConfirm
        );
        assert_eq!(t0.elapsed(), Duration::from_secs(60));

        // The receipt is gone on the second poll: search by the nonce.
        let evm = FakeEvm::sepolia();
        evm.script_receipt(TX, vec![Some(deposit_receipt(TX, &b, true, vec![])), None]);
        evm.finalized.set([Some(header(90, 8))]);
        let an = FakeAn::default();
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            wait(&evm, &an, &ctx(&b, OWNER, Some(3600)), &ui)
                .await
                .unwrap(),
            WaitExit::Search
        );
        assert_eq!(t0.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_flag_answer_that_is_no_bool_is_read_again() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        an.accepted.lock().unwrap().insert(b.hash, 0);
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": "maybe"}),
            serde_json::json!({"value0": false}),
        ]);
        deployed_voucher(&an);
        let ui = RecordingUi::new(true);
        assert_eq!(
            wait(&evm, &an, &ctx(&b, OWNER, None), &ui).await.unwrap(),
            WaitExit::VoucherDeployed
        );
        let retries: Vec<_> = ui
            .events()
            .into_iter()
            .filter(|e| matches!(e, UiEvent::Retry(_, _, err) if err.contains("isPaused")))
            .collect();
        assert_eq!(retries.len(), 1, "{:?}", ui.events());
    }

    #[tokio::test(start_paused = true)]
    async fn a_light_client_read_that_fails_is_read_again_next_cycle() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        an.failing_getters
            .lock()
            .unwrap()
            .insert("getConfig".into());
        an.accepted.lock().unwrap().insert(b.hash, 2); // accepted on the third poll
        an.getter(BRIDGE, "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        deployed_voucher(&an);
        let ui = RecordingUi::new(true);
        assert_eq!(
            wait(
                &evm,
                &an,
                &ctx(&b, AnchorPlan::LightClient, Some(3600)),
                &ui
            )
            .await
            .unwrap(),
            WaitExit::VoucherDeployed
        );
        // No verdict from the failed read; it is logged and made again on
        // each of the two polls before the anchor.
        let attempts: Vec<u32> = ui
            .events()
            .into_iter()
            .filter_map(|e| match e {
                UiEvent::Retry(what, n, err) if err.contains("getConfig") => {
                    assert_eq!(what, LC_RECHECK);
                    Some(n)
                },
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![1, 2]);
        assert!(!ui
            .statuses()
            .iter()
            .any(|s| s.contains("ancestry has stopped")));
    }

    /// A light client whose checks 1 and 2 and the switch-off pass, but
    /// whose newest ancestry's checkpoint trails its head by 900 s.
    fn stopped_ancestry(evm: &FakeEvm, an: &FakeAn) {
        let fin_ts = header(150, 8).timestamp;
        let head = Header {
            number: 140,
            hash: B256::repeat_byte(0x10),
            parent_hash: B256::repeat_byte(0x0f),
            timestamp: fin_ts - 120,
        };
        let parent = Header {
            number: 130,
            hash: B256::repeat_byte(0x50),
            parent_hash: B256::repeat_byte(0x4f),
            timestamp: head.timestamp - 912,
        };
        let checkpoint = Header {
            number: 131,
            hash: B256::repeat_byte(0x30),
            parent_hash: parent.hash,
            timestamp: head.timestamp - 900,
        };
        let t_flip = parent.timestamp - 1000;
        for h in [&head, &parent, &checkpoint] {
            evm.by_hash.lock().unwrap().insert(h.hash, h.clone());
        }
        an.getter(LC, "getConfig", vec![
            serde_json::json!({ "l1ChainId": "11155111" }),
        ]);
        an.getter(LC, "getHead", vec![
            serde_json::json!({ "executionBlockHash": format!("{:#x}", head.hash) }),
        ]);
        an.accepted.lock().unwrap().insert(head.hash, 0);
        an.accepted.lock().unwrap().insert(parent.hash, 0);
        let bridge = format!("0:{}", hex::encode(BRIDGE));
        let flip = msg("flip", "ExtIn", "", &bridge, Some("B-disable"));
        an.ext_in.lock().unwrap().insert(BRIDGE, vec![flip.clone()]);
        an.messages.lock().unwrap().insert("flip".into(), MsgView {
            dst_tx: Some(TxRef {
                hash: "flip-tx".into(),
                aborted: false,
                account: bridge,
                now: t_flip,
            }),
            ..flip
        });
        let mut anc = msg(
            "anc",
            "ExtOut",
            &format!("0:{}", hex::encode(LC)),
            ANCESTRY_ACCEPTED_DST,
            Some("B-anc"),
        );
        anc.created_at = t_flip + 500;
        an.ext_out.lock().unwrap().insert(LC, vec![anc]);
        an.bodies.lock().unwrap().extend([
            ("B-disable".to_string(), ("disableOwnerAnchors".to_string(), serde_json::json!({}))),
            (
                "B-anc".to_string(),
                (
                    "AncestryAccepted".to_string(),
                    serde_json::json!({ "checkpointHash": format!("{:#x}", checkpoint.hash), "hashesAdded": "31" }),
                ),
            ),
        ]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_ancestry_stays_in_the_status_between_re_checks() {
        let (evm, b) = final_deposit();
        let an = FakeAn::default();
        stopped_ancestry(&evm, &an);
        let ui = RecordingUi::new(true);
        let e = wait(&evm, &an, &ctx(&b, AnchorPlan::LightClient, Some(700)), &ui)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::AnWaitTimeout);
        assert!(e.to_string().contains("ancestry has stopped"), "{e}");
        let statuses = ui.statuses();
        // Polls at 0, 30, …, 690 s; the light client is re-checked on the
        // first and every tenth.
        let checks = statuses
            .iter()
            .filter(|s| s.starts_with("checking whether the light client"))
            .count();
        assert_eq!(checks, 3, "{statuses:?}");
        let waits: Vec<_> = statuses
            .iter()
            .filter(|s| s.starts_with("waiting for the light client"))
            .collect();
        assert_eq!(waits.len(), 24, "{statuses:?}");
        assert!(
            waits.iter().all(|s| s.contains("ancestry has stopped")),
            "{waits:?}"
        );
    }
}
