//! Step 8: `finalizeDeposit`. Before every send — the first, a retry,
//! a resume — check whether someone already finalized it (the voucher is
//! deployed), whether the bridge's voucher code moved (then the stored
//! voucher address is stale and the credit is confirmed by events), and
//! whether the bridge is paused. A resend is safe on a bridge that
//! passed the version check: it cannot mint one deposit twice.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{anyhow, Context as _};
use rand::Rng as _;
use serde_json::json;

use crate::{
    deposit::{
        an::{AccStatus, AccountInfo, AnRead, AnSend},
        credit::{finalized_event, Identity},
        identity::BRIDGE_ABI,
        refusals::{react, Reaction},
        retry::{base_delay, jittered, until},
        store::{FinalizeInfo, OpRecord, OpStage, Store},
        ui::Ui,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// What the checks before a send decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreSend {
    /// Send; with `via_events` the voucher code moved and the credit is
    /// confirmed by the bridge's events.
    Send {
        /// The stored voucher address is stale.
        via_events: bool,
    },
    /// The voucher at the stored address is deployed: someone finalized it.
    VoucherDeployed,
    /// The bridge emitted DepositFinalized for this deposit.
    FinalizedByEvents,
    /// The bridge is paused: wait.
    Paused,
}

/// The checks before every send, in their order: a finalized deposit —
/// by the voucher under the stored code, by the bridge's event once the
/// code moved — is never sent again, and never held by a pause.
pub fn pre_send(
    stored_code_hash: [u8; 32],
    bridge_code_hash: [u8; 32],
    voucher: Option<&AccountInfo>,
    event_seen: bool,
    paused: bool,
) -> PreSend {
    // Before the pause: a deposit finalized before the bridge was paused is
    // credited already, and waiting for the pause would only hide it.
    if event_seen {
        return PreSend::FinalizedByEvents;
    }
    let code_moved = stored_code_hash != bridge_code_hash;
    // An account that merely exists does not count: anyone can prefund the
    // deterministic voucher address, and that does not stop its deploy.
    if !code_moved && voucher.is_some_and(|v| v.status == AccStatus::Active) {
        return PreSend::VoucherDeployed;
    }
    if paused {
        return PreSend::Paused;
    }
    PreSend::Send {
        via_events: code_moved,
    }
}

/// Where the run goes after step 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeExit {
    /// Step 9; with `via_events` only the bridge's events can confirm it.
    ToCredit {
        /// The stored voucher address is stale.
        via_events: bool,
    },
    /// The bridge does not know the anchored block: back to step 6.
    BackToAnchor,
}

/// What step 8 needs to know about the deposit and the bridge.
#[derive(Debug, Clone)]
pub struct FinCtx {
    /// The Acki Nacki bridge account id.
    pub bridge: [u8; 32],
    /// The dapp the bridge lives in.
    pub bridge_dapp: [u8; 32],
    /// The voucher account id, computed with `stored_code_hash`.
    pub voucher: [u8; 32],
    /// `getDepositVoucherCodeHash()` as stored in the operation.
    pub stored_code_hash: [u8; 32],
    /// The deposit as the bridge's DepositFinalized names it.
    pub identity: Identity,
    /// The deposit block time, in seconds: no DepositFinalized for this
    /// deposit is older, so the event search stops there.
    pub not_before: u64,
    /// Step 8's deadline, set by the caller when it entered the step: the
    /// reads it makes before calling in count against it too.
    pub deadline: Option<tokio::time::Instant>,
    /// How often a paused bridge is looked at again.
    pub poll: Duration,
}

/// What the deadline leaves behind. With a send in flight, or one that
/// answered without a verdict, the finalize may have executed: exit 34.
/// Otherwise the bridge was paused or unreadable the whole time: exit 31.
fn out_of_time(op: &str, in_doubt: bool) -> CliError {
    if in_doubt {
        CliError::deposit(
            ExitCode::CreditUnconfirmed,
            Stage::Finalize,
            Some(op),
            format!(
                "finalizeDeposit was sent and the time ran out before its outcome was known; \
                 continue with --resume {op}"
            ),
        )
    } else {
        CliError::deposit(
            ExitCode::AnWaitTimeout,
            Stage::Finalize,
            Some(op),
            format!(
                "the Acki Nacki bridge stayed paused or could not be read in time; continue with \
                 --resume {op}"
            ),
        )
    }
}

/// Nothing left this process on this attempt. Exit 32 — unless an earlier
/// send of this step has no verdict: that one may have executed, exit 34.
/// Either way the record says Finalizing, so --resume starts with the
/// checks before a send.
fn not_sent(op: &str, earlier_in_doubt: bool, what: String) -> CliError {
    if earlier_in_doubt {
        CliError::deposit(
            ExitCode::CreditUnconfirmed,
            Stage::Finalize,
            Some(op),
            format!(
                "{what}; an earlier finalizeDeposit of this run has no known outcome and may have \
                 executed. Continue with --resume {op}"
            ),
        )
    } else {
        CliError::deposit(
            ExitCode::DepositProofFailed,
            Stage::Finalize,
            Some(op),
            format!(
                "{what}; nothing reached Acki Nacki and the deposit is on the EVM bridge. \
                 Continue with --resume {op}"
            ),
        )
    }
}

/// `getDepositVoucherCodeHash()`. An answer that is not 32 bytes of hex
/// is a failed read, never a changed code.
async fn bridge_code_hash(an: &dyn AnRead, bridge: [u8; 32]) -> anyhow::Result<[u8; 32]> {
    let v = an
        .run_getter(bridge, BRIDGE_ABI, "getDepositVoucherCodeHash", json!({}))
        .await
        .context("could not read getDepositVoucherCodeHash")?;
    let mut h = [0u8; 32];
    v["value0"]
        .as_str()
        .and_then(|s| hex::decode_to_slice(s.trim_start_matches("0x"), &mut h).ok())
        .ok_or_else(|| anyhow!("getDepositVoucherCodeHash answered {v}"))?;
    Ok(h)
}

/// `isPaused()`. An answer that is not a boolean is a failed read.
async fn is_paused(an: &dyn AnRead, bridge: [u8; 32]) -> anyhow::Result<bool> {
    let v = an
        .run_getter(bridge, BRIDGE_ABI, "isPaused", json!({}))
        .await
        .context("could not read isPaused")?;
    v["value0"]
        .as_bool()
        .ok_or_else(|| anyhow!("isPaused answered {v}"))
}

/// Step 8. Refusals are exit 33; a message that never left is exit 32, or
/// 34 after a send in doubt. The whole step — reads, pauses, sends — is
/// under `cx.deadline`: out of time with a send in flight or in doubt is
/// exit 34, otherwise exit 31. The record says Finalizing before every
/// send, so --resume starts with the voucher check either way.
#[allow(clippy::too_many_arguments)]
pub async fn finalize(
    an: &dyn AnRead,
    send: &dyn AnSend,
    store: &Store,
    rec: &mut OpRecord,
    cx: &FinCtx,
    proof: &[u8],
    pi: &[u8],
    ui: &dyn Ui,
) -> CliResult<FinalizeExit> {
    let op = rec.op_id.clone();
    // The whole step is under the deadline: the reads, the pauses, the
    // backoff and the sends themselves. A send cut by it may still execute.
    let in_doubt = AtomicBool::new(false);
    let steps = attempts(an, send, store, rec, cx, proof, pi, ui, &in_doubt);
    match cx.deadline {
        None => steps.await,
        Some(d) => match tokio::time::timeout_at(d, steps).await {
            Ok(r) => r,
            Err(_) => Err(out_of_time(&op, in_doubt.load(Ordering::SeqCst))),
        },
    }
}

/// The checks and sends of step 8, until one decides. `in_doubt` says
/// whether a send of this step may have executed: set when a send starts,
/// kept after an answer without a verdict, and put back to what it was
/// only by a refusal before accept, which settles the one send it answers.
#[allow(clippy::too_many_arguments)]
async fn attempts(
    an: &dyn AnRead,
    send: &dyn AnSend,
    store: &Store,
    rec: &mut OpRecord,
    cx: &FinCtx,
    proof: &[u8],
    pi: &[u8],
    ui: &dyn Ui,
    in_doubt: &AtomicBool,
) -> CliResult<FinalizeExit> {
    let op = rec.op_id.clone();
    let deadline = cx.deadline;
    let timed_out = || out_of_time(&op, in_doubt.load(Ordering::SeqCst));
    let mut retries = 0u32;
    let mut warned = false;
    loop {
        let bridge_code = until(
            ui,
            "reading the bridge's voucher code hash",
            deadline,
            || bridge_code_hash(an, cx.bridge),
        )
        .await
        .ok_or_else(timed_out)?;
        let moved = cx.stored_code_hash != bridge_code;
        // After a code change the stored voucher address is stale, and only
        // the bridge's DepositFinalized shows whether the deposit is finalized.
        // Read before every send, and first: a send whose outcome was unknown
        // may have executed, and the bridge refuses a second one with a code
        // this step would take for a refusal.
        let event_seen = if moved {
            until(
                ui,
                "reading the bridge's DepositFinalized events",
                deadline,
                || finalized_event(an, cx.bridge, &cx.identity, cx.not_before),
            )
            .await
            .ok_or_else(timed_out)?
            .is_some()
        } else {
            false
        };
        // The stored voucher address means something only under the stored code.
        let voucher = if moved {
            None
        } else {
            until(ui, "looking for the deposit voucher", deadline, || {
                an.account(cx.voucher)
            })
            .await
            .ok_or_else(timed_out)?
        };
        // A deposit finalized already is not held by the pause flag: it is not
        // read then, so a pause, or a getter that hangs, cannot keep the run
        // from step 9.
        let finalized = event_seen
            || voucher
                .as_ref()
                .is_some_and(|v| v.status == AccStatus::Active);
        let paused = !finalized
            && until(ui, "reading the bridge pause flag", deadline, || {
                is_paused(an, cx.bridge)
            })
            .await
            .ok_or_else(timed_out)?;
        let via_events = match pre_send(
            cx.stored_code_hash,
            bridge_code,
            voucher.as_ref(),
            event_seen,
            paused,
        ) {
            PreSend::VoucherDeployed => {
                return Ok(FinalizeExit::ToCredit {
                    via_events: false,
                })
            },
            PreSend::FinalizedByEvents => {
                return Ok(FinalizeExit::ToCredit {
                    via_events: true,
                })
            },
            PreSend::Paused => {
                // The deadline around the whole step ends this wait.
                ui.status("the bridge is paused by its owner; deposits finalize once it is lifted");
                tokio::time::sleep(cx.poll).await;
                continue;
            },
            PreSend::Send {
                via_events,
            } => via_events,
        };
        if via_events && !warned {
            warned = true;
            ui.warn(
                "the bridge's voucher code changed since this deposit was checked; the credit \
                 will be confirmed by the bridge's DepositFinalized event",
            );
        }
        if rec.stage != OpStage::Finalizing {
            rec.stage = OpStage::Finalizing;
            rec.finalize = Some(FinalizeInfo {
                voucher_code_hash: hex::encode(cx.stored_code_hash),
                voucher_account: hex::encode(cx.voucher),
                sends: 0,
            });
        }
        if let Some(f) = rec.finalize.as_mut() {
            f.sends += 1;
        }
        // Nothing of this attempt has left yet: a record that cannot be
        // written is a local failure like a message that cannot be encoded.
        store.write(rec).map_err(|e| {
            not_sent(
                &op,
                in_doubt.load(Ordering::SeqCst),
                format!("cannot record the finalize attempt: {e}"),
            )
        })?;
        let earlier = in_doubt.swap(true, Ordering::SeqCst);
        let outcome = send
            .send_finalize(cx.bridge, cx.bridge_dapp, proof, pi)
            .await;
        match react(&outcome) {
            Reaction::ToCredit => {
                return Ok(FinalizeExit::ToCredit {
                    via_events,
                })
            },
            Reaction::BackToAnchor => return Ok(FinalizeExit::BackToAnchor),
            Reaction::WaitPaused => {
                // Refused before accept: this send did not execute. An earlier
                // one without a verdict still may have.
                in_doubt.store(earlier, Ordering::SeqCst);
                ui.status("the bridge is paused by its owner; deposits finalize once it is lifted");
                tokio::time::sleep(cx.poll).await;
            },
            Reaction::Refused {
                code,
                hint,
            } => {
                return Err(CliError::deposit(
                    ExitCode::FinalizeRefused,
                    Stage::Finalize,
                    Some(&op),
                    format!("finalizeDeposit refused with exit code {code}: {hint}"),
                ));
            },
            // Never retried: sending the same message again fails the same way.
            Reaction::NotSent {
                message,
            } => {
                return Err(not_sent(
                    &op,
                    earlier,
                    format!("finalizeDeposit could not be sent: {message}"),
                ));
            },
            // No verdict: it may have executed, so it stays in doubt.
            Reaction::Retry => {
                ui.retry(
                    "sending finalizeDeposit",
                    retries + 1,
                    &format!("{outcome:?}"),
                );
                let unit: f64 = rand::thread_rng().gen_range(-1.0..=1.0);
                tokio::time::sleep(jittered(base_delay(retries), unit)).await;
                retries = retries.saturating_add(1);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deposit::an::{AccStatus, AccountInfo};

    const H: [u8; 32] = [0xbd; 32];

    #[test]
    fn a_deployed_voucher_means_do_not_send() {
        let v = AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        };
        assert_eq!(
            pre_send(H, H, Some(&v), false, false),
            PreSend::VoucherDeployed
        );
    }

    #[test]
    fn a_prefunded_uninit_voucher_address_does_not_stop_the_send() {
        let v = AccountInfo {
            status: AccStatus::Uninit,
            dapp_id: None,
            ecc3: 0,
        };
        assert_eq!(pre_send(H, H, Some(&v), false, false), PreSend::Send {
            via_events: false
        });
    }

    #[test]
    fn a_changed_voucher_code_sends_and_confirms_by_events() {
        let v = AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        };
        assert_eq!(
            pre_send(H, [1; 32], Some(&v), false, false),
            PreSend::Send {
                via_events: true
            }
        );
    }

    #[test]
    fn a_changed_code_with_the_deposit_in_the_events_neither_sends_nor_waits() {
        // Finalized through the new voucher code; a pause after that must not
        // hold a deposit that is already credited.
        assert_eq!(
            pre_send(H, [1; 32], None, true, true),
            PreSend::FinalizedByEvents
        );
        assert_eq!(
            pre_send(H, [1; 32], None, true, false),
            PreSend::FinalizedByEvents
        );
    }

    #[test]
    fn a_paused_bridge_waits() {
        assert_eq!(pre_send(H, H, None, false, true), PreSend::Paused);
    }

    #[tokio::test(start_paused = true)]
    async fn the_record_says_finalizing_before_the_first_send() {
        use crate::deposit::{refusals::FinalizeSend, store::*, testkit::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 1,
                bridge: Default::default(),
                to: "x".into(),
                amount_units: 1,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            hex::encode(H),
        );
        rec.stage = OpStage::Proved;
        rec.tx = Some(TxClaim {
            tx_hash: Default::default(),
            tx_nonce: 0,
        });
        rec.deposit = Some(DepositInfo {
            deposit_id: Default::default(),
            block_number: 1,
            block_hash: Default::default(),
            block_log_index: 0,
            receipt_log_index: 0,
            access_list_rlp_len: 1,
            voucher_account: hex::encode([0xee; 32]),
        });
        store.write(&mut rec).unwrap();
        let an = FakeAn::default();
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", hex::encode(H))}),
        ]);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        an.sends.lock().unwrap().extend([
            FinalizeSend::Rejected {
                exit_code: Some(231),
                message: "paused".into(),
            },
            FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: false,
                exit_code: Some(0),
            },
        ]);
        let cx = FinCtx {
            bridge: [0x1a; 32],
            bridge_dapp: [0; 32],
            voucher: [0xee; 32],
            stored_code_hash: H,
            identity: ident(),
            not_before: 0,
            deadline: None,
            poll: std::time::Duration::from_secs(30),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::ToCredit {
            via_events: false
        });
        assert_eq!(store.load(&rec.op_id).unwrap().stage, OpStage::Finalizing);
        assert_eq!(*an.sent.lock().unwrap(), 2);
    }

    fn ident() -> crate::deposit::credit::Identity {
        crate::deposit::credit::Identity {
            deposit_id: alloy_primitives::U256::from(5),
            contract: alloy_primitives::U256::from(0xcdfdu64),
            chain_id: 1,
            amount: 1,
            account: [0xa3; 32],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_resend_after_an_unknown_outcome_looks_at_the_events_first_when_the_code_moved() {
        // The first send answers without a verdict but executed; with the
        // voucher code changed, only the bridge's event shows it. A second
        // send would be refused by the bridge as a duplicate. The pause
        // getter hangs after its first answer: the event must not wait for it.
        use crate::deposit::{credit::DEPOSIT_FINALIZED_DST, refusals::FinalizeSend, testkit::*};
        let d = tempfile::tempdir().unwrap();
        let store = crate::deposit::store::Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let an = FakeAn::default();
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", "11".repeat(32))}),
        ]);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        an.sends.lock().unwrap().push_back(FinalizeSend::Unknown {
            message: "timeout".into(),
        });
        let i = ident();
        an.bodies.lock().unwrap().insert("B-ev".into(), ("DepositFinalized".into(), serde_json::json!({
            "depositId": i.deposit_id.to_string(), "contractAddr": i.contract.to_string(), "dappId": "0",
            "chainId": i.chain_id.to_string(), "amount": i.amount.to_string(), "anAccount": format!("0x{}", hex::encode(i.account)),
        })));
        an.on_send_ext_out.lock().unwrap().push((
            [0x1a; 32],
            msg(
                "ev",
                "ExtOut",
                &format!("0:{}", hex::encode([0x1a; 32])),
                DEPOSIT_FINALIZED_DST,
                Some("B-ev"),
            ),
        ));
        an.hang_getter_after
            .lock()
            .unwrap()
            .insert("isPaused".into(), 1);
        let cx = FinCtx {
            bridge: [0x1a; 32],
            bridge_dapp: [0; 32],
            voucher: [0xee; 32],
            stored_code_hash: H,
            identity: i,
            not_before: 0,
            deadline: Some(tokio::time::Instant::now() + std::time::Duration::from_secs(600)),
            poll: std::time::Duration::from_secs(30),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::ToCredit {
            via_events: true
        });
        assert_eq!(*an.sent.lock().unwrap(), 1, "no second send");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "{:?}",
            t0.elapsed()
        );
    }

    fn proved(store: &crate::deposit::store::Store) -> crate::deposit::store::OpRecord {
        use crate::deposit::store::*;
        let mut rec = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 1,
                bridge: Default::default(),
                to: "x".into(),
                amount_units: 1,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            hex::encode(H),
        );
        rec.stage = OpStage::Proved;
        rec.tx = Some(TxClaim {
            tx_hash: Default::default(),
            tx_nonce: 0,
        });
        rec.deposit = Some(DepositInfo {
            deposit_id: Default::default(),
            block_number: 1,
            block_hash: Default::default(),
            block_log_index: 0,
            receipt_log_index: 0,
            access_list_rlp_len: 1,
            voucher_account: hex::encode([0xee; 32]),
        });
        store.write(&mut rec).unwrap();
        rec
    }

    #[tokio::test(start_paused = true)]
    async fn a_pause_does_not_outlive_the_anchor_timeout() {
        use crate::deposit::{store::*, testkit::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let an = FakeAn::default();
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", hex::encode(H))}),
        ]);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": true}),
        ]);
        let cx = FinCtx {
            bridge: [0x1a; 32],
            bridge_dapp: [0; 32],
            voucher: [0xee; 32],
            stored_code_hash: H,
            identity: ident(),
            not_before: 0,
            deadline: Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1)),
            poll: std::time::Duration::from_secs(30),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::AnWaitTimeout);
        assert_eq!(
            t0.elapsed(),
            std::time::Duration::from_secs(1),
            "not after a whole poll"
        );
        assert_eq!(*an.sent.lock().unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_cut_by_the_deadline_is_exit_34_and_resumable() {
        use crate::deposit::{refusals::FinalizeSend, store::*, testkit::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let an = FakeAn::default();
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", hex::encode(H))}),
        ]);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        an.sends.lock().unwrap().push_back(FinalizeSend::Executed {
            tx_id: "t".into(),
            aborted: false,
            exit_code: Some(0),
        });
        *an.send_delay.lock().unwrap() = std::time::Duration::from_secs(10);
        let cx = FinCtx {
            bridge: [0x1a; 32],
            bridge_dapp: [0; 32],
            voucher: [0xee; 32],
            stored_code_hash: H,
            identity: ident(),
            not_before: 0,
            deadline: Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1)),
            poll: std::time::Duration::from_secs(30),
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::CreditUnconfirmed,
            "the send may have executed: {e}"
        );
        assert!(e.to_string().contains("--resume"), "{e}");
        assert_eq!(t0.elapsed(), std::time::Duration::from_secs(1));
        let back = store.load(&rec.op_id).unwrap();
        assert_eq!(
            back.stage,
            OpStage::Finalizing,
            "a resume starts with the voucher check"
        );
        assert_eq!(back.finalize.unwrap().sends, 1);
    }

    /// A fake bridge with the stored voucher code, not paused, answering
    /// `sends` in order, and a step context with `deadline_s` left: a
    /// regression that retries forever fails instead of spinning.
    fn plain(
        sends: impl IntoIterator<Item = crate::deposit::refusals::FinalizeSend>,
        deadline_s: u64,
    ) -> (crate::deposit::testkit::FakeAn, FinCtx) {
        let an = crate::deposit::testkit::FakeAn::default();
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", hex::encode(H))}),
        ]);
        an.getter([0x1a; 32], "isPaused", vec![
            serde_json::json!({"value0": false}),
        ]);
        an.sends.lock().unwrap().extend(sends);
        let cx = FinCtx {
            bridge: [0x1a; 32],
            bridge_dapp: [0; 32],
            voucher: [0xee; 32],
            stored_code_hash: H,
            identity: ident(),
            not_before: 0,
            deadline: Some(
                tokio::time::Instant::now() + std::time::Duration::from_secs(deadline_s),
            ),
            poll: std::time::Duration::from_secs(30),
        };
        (an, cx)
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_that_never_left_ends_the_step_with_exit_32() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::NotSent {
                message: "encode finalizeDeposit: bad proof bytes".into(),
            }],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::DepositProofFailed,
            "{e}"
        );
        assert!(
            e.to_string()
                .contains("encode finalizeDeposit: bad proof bytes"),
            "{e}"
        );
        assert!(e.to_string().contains("--resume"), "{e}");
        assert_eq!(*an.sent.lock().unwrap(), 1, "never retried");
        assert_eq!(
            t0.elapsed(),
            std::time::Duration::ZERO,
            "no backoff before giving up"
        );
        assert_eq!(
            store.load(&rec.op_id).unwrap().stage,
            OpStage::Finalizing,
            "a resume starts with the checks"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_that_never_left_after_a_send_in_doubt_is_exit_34() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [
                FinalizeSend::Unknown {
                    message: "timeout".into(),
                },
                FinalizeSend::NotSent {
                    message: "encode finalizeDeposit: bad proof bytes".into(),
                },
            ],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::CreditUnconfirmed,
            "the first send may have executed: {e}"
        );
        assert!(
            e.to_string()
                .contains("encode finalizeDeposit: bad proof bytes"),
            "{e}"
        );
        assert!(e.to_string().contains("--resume"), "{e}");
        assert_eq!(*an.sent.lock().unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_before_accept_does_not_settle_an_earlier_send_in_doubt() {
        // The 231 answers the second send only; the first, without a verdict,
        // may still have executed.
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [
                FinalizeSend::Unknown {
                    message: "timeout".into(),
                },
                FinalizeSend::Rejected {
                    exit_code: Some(231),
                    message: "paused".into(),
                },
                FinalizeSend::NotSent {
                    message: "encode finalizeDeposit: bad proof bytes".into(),
                },
            ],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::CreditUnconfirmed,
            "{e}"
        );
        assert_eq!(*an.sent.lock().unwrap(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_after_a_send_in_doubt_and_a_refusal_is_exit_34() {
        // The pause wait after the 231 is cut by the deadline; the first send
        // still has no verdict.
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [
                FinalizeSend::Unknown {
                    message: "timeout".into(),
                },
                FinalizeSend::Rejected {
                    exit_code: Some(231),
                    message: "paused".into(),
                },
            ],
            20,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::CreditUnconfirmed,
            "{e}"
        );
        assert_eq!(*an.sent.lock().unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_after_a_refusal_before_accept_is_exit_31() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Rejected {
                exit_code: Some(231),
                message: "paused".into(),
            }],
            10,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::AnWaitTimeout,
            "nothing executed: {e}"
        );
        assert!(e.to_string().contains("--resume"), "{e}");
        assert_eq!(t0.elapsed(), std::time::Duration::from_secs(10));
        assert_eq!(*an.sent.lock().unwrap(), 1);
        assert!(
            ui.statuses().iter().any(|s| s.contains("paused")),
            "{:?}",
            ui.statuses()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_block_goes_back_to_the_anchor() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: true,
                exit_code: Some(224),
            }],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::BackToAnchor);
        assert_eq!(*an.sent.lock().unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_proof_is_exit_33_with_the_verification_key_hint() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: true,
                exit_code: Some(220),
            }],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::FinalizeRefused,
            "{e}"
        );
        assert!(e.to_string().contains("220"), "{e}");
        assert!(e.to_string().contains("verification key"), "{e}");
        assert_eq!(*an.sent.lock().unwrap(), 1, "a refusal is not retried");
    }

    #[tokio::test(start_paused = true)]
    async fn a_deployed_voucher_skips_the_send_and_the_pause() {
        // Someone finalized it already: nothing is sent, and a pause flag that
        // cannot be read does not hold the run.
        use crate::deposit::{an::*, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain([], 600);
        an.accounts.lock().unwrap().insert([0xee; 32], AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        });
        an.failing_getters.lock().unwrap().insert("isPaused".into());
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::ToCredit {
            via_events: false
        });
        assert_eq!(*an.sent.lock().unwrap(), 0);
        assert_eq!(
            store.load(&rec.op_id).unwrap().stage,
            OpStage::Proved,
            "nothing was sent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prefunded_uninit_voucher_address_is_still_sent_to() {
        use crate::deposit::{an::*, refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: false,
                exit_code: Some(0),
            }],
            600,
        );
        an.accounts.lock().unwrap().insert([0xee; 32], AccountInfo {
            status: AccStatus::Uninit,
            dapp_id: None,
            ecc3: 0,
        });
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::ToCredit {
            via_events: false
        });
        assert_eq!(*an.sent.lock().unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_list_that_cannot_be_read_is_not_no_event() {
        // With the voucher code moved, only the events show a finalized
        // deposit; a failed read of them must not let a send through.
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: false,
                exit_code: Some(0),
            }],
            120,
        );
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": format!("0x{}", "11".repeat(32))}),
        ]);
        an.fail_ext_messages
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::AnWaitTimeout, "{e}");
        assert_eq!(*an.sent.lock().unwrap(), 0);
        assert!(
            ui.events()
                .iter()
                .any(|ev| matches!(ev, crate::deposit::ui::UiEvent::Retry(..))),
            "every failed read is reported"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_voucher_code_hash_is_not_a_moved_code() {
        // A malformed answer is a failed read: retried, never taken for a
        // changed code (which would switch the confirmation path).
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: false,
                exit_code: Some(0),
            }],
            600,
        );
        an.getter([0x1a; 32], "getDepositVoucherCodeHash", vec![
            serde_json::json!({"value0": "0xbd"}),
            serde_json::json!({"value0": format!("0x{}", hex::encode(H))}),
        ]);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let out = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap();
        assert_eq!(out, FinalizeExit::ToCredit {
            via_events: false
        });
        assert_eq!(*an.sent.lock().unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_record_that_cannot_be_written_stops_before_the_send() {
        use crate::deposit::{refusals::FinalizeSend, store::*};
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(d.path()).unwrap();
        let mut rec = proved(&store);
        std::fs::remove_dir_all(d.path()).unwrap();
        let (an, cx) = plain(
            [FinalizeSend::Executed {
                tx_id: "t".into(),
                aborted: false,
                exit_code: Some(0),
            }],
            600,
        );
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = finalize(&an, &an, &store, &mut rec, &cx, b"p", b"i", &ui)
            .await
            .unwrap_err();
        assert_eq!(
            e.exit_code(),
            crate::errors::ExitCode::DepositProofFailed,
            "nothing was sent: {e}"
        );
        assert!(e.to_string().contains("--resume"), "{e}");
        assert_eq!(*an.sent.lock().unwrap(), 0);
    }
}
