//! Step 9: was THIS deposit credited? Decided only by the deposit's
//! identity `(chainId, contractAddr, depositId)`. A balance proves
//! nothing (two equal deposits share a base; the recipient may spend),
//! and a voucher proves nothing (its constructor only sends
//! `confirmDeposit`).
//!
//! Primary path: the voucher's deploy transaction → its `confirmDeposit`
//! to the bridge → the bridge transaction that ran it → the ECC[3]
//! transfer to the recipient and `DepositFinalized` → the transfer's
//! delivery. Fallback, in parallel: the bridge's `DepositFinalized`
//! events filtered by identity. The bridge pins `dappId` to zero.

use std::time::Duration;

use alloy_primitives::U256;
use serde_json::{json, Value};

use crate::{
    deposit::{
        an::{AccStatus, AnRead, ExtDir, TxView},
        identity::BRIDGE_ABI,
        retry::{deadline_after, once, until},
        store::CreditInfo,
        ui::Ui,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// The external address the bridge emits `DepositFinalized` to (619).
pub const DEPOSIT_FINALIZED_DST: &str =
    ":000000000000000000000000000000000000000000000000000000000000026b";

/// A deposit as the bridge's `confirmDeposit` and `DepositFinalized` name
/// it: the proven inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `depositId` of the EVM `Deposit` event.
    pub deposit_id: U256,
    /// The EVM bridge contract, as a number.
    pub contract: U256,
    /// The EVM chain id.
    pub chain_id: u64,
    /// The amount, in micro-USDC.
    pub amount: u128,
    /// The Acki Nacki recipient's account id.
    pub account: [u8; 32],
}

/// A decoded ABI integer: the SDK writes a `uint256` as `0x` and 64 hex
/// digits and a smaller one in decimal.
fn u256(v: &Value) -> Option<U256> {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64().map(U256::from))
}

impl Identity {
    /// Whether decoded `confirmDeposit` or `DepositFinalized` values are
    /// this deposit's: every field equal, and `dappId` zero, as the bridge
    /// pins it.
    pub fn matches(&self, v: &Value) -> bool {
        u256(&v["depositId"]) == Some(self.deposit_id)
            && u256(&v["contractAddr"]) == Some(self.contract)
            && u256(&v["chainId"]) == Some(U256::from(self.chain_id))
            && u256(&v["amount"]) == Some(U256::from(self.amount))
            && u256(&v["anAccount"]) == Some(U256::from_be_bytes(self.account))
            && u256(&v["dappId"]) == Some(U256::ZERO)
    }
}

/// What one look at the chain says about the credit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credit {
    /// The whole chain is there, up to the delivered transfer.
    Confirmed(CreditInfo),
    /// The bridge transaction that ran this deposit's `confirmDeposit`
    /// aborted: the voucher is spent and nothing was minted.
    Aborted {
        /// That transaction.
        bridge_tx: String,
    },
    /// Not visible (yet): a message in transit, a lagging node, or nothing
    /// finalized this deposit.
    NotYet,
}

/// Whether `dst` is the account `account`, in any workchain form.
fn is_recipient(dst: &str, account: &[u8; 32]) -> bool {
    dst.rsplit(':')
        .next()
        .is_some_and(|h| h.eq_ignore_ascii_case(&hex::encode(account)))
}

/// Given the bridge transaction that processed this deposit's
/// `confirmDeposit`: did it mint, and has the transfer been delivered?
///
/// Delivery is the recipient's transaction keeping ECC[3] of the amount.
/// That transaction may be aborted: at an address with no code yet there
/// is nothing to compute, and a transfer that does not bounce keeps its
/// currencies all the same.
pub async fn bridge_tx_verdict(
    an: &dyn AnRead,
    tx: &TxView,
    id: &Identity,
    via_events: bool,
) -> anyhow::Result<Credit> {
    if tx.aborted {
        return Ok(Credit::Aborted {
            bridge_tx: tx.hash.clone(),
        });
    }
    let event_ok = tx.out.iter().any(|m| {
        m.dst.ends_with(DEPOSIT_FINALIZED_DST)
            && m.body
                .as_deref()
                .and_then(|b| an.decode(BRIDGE_ABI, b, false))
                .is_some_and(|(n, v)| n == "DepositFinalized" && id.matches(&v))
    });
    let transfer = tx.out.iter().find(|m| {
        is_recipient(&m.dst, &id.account)
            && m.ecc.iter().any(|(c, v)| *c == 3 && *v == id.amount)
            && m.bounce == Some(false)
    });
    let (true, Some(t)) = (event_ok, transfer) else {
        return Ok(Credit::NotYet);
    };
    let Some(full) = an.message(&t.hash).await? else {
        return Ok(Credit::NotYet);
    };
    let Some(d) = full.dst_tx else {
        return Ok(Credit::NotYet);
    };
    let Some(dtx) = an.transaction(&d.hash).await? else {
        return Ok(Credit::NotYet);
    };
    if !dtx
        .ecc_delta
        .iter()
        .any(|(c, v)| *c == 3 && *v == id.amount as i128)
    {
        return Ok(Credit::NotYet);
    }
    Ok(Credit::Confirmed(CreditInfo {
        confirm_tx: tx.hash.clone(),
        delivery_tx: dtx.hash,
        via_events,
    }))
}

/// The primary path, from the voucher at its stored address: its deploy
/// transaction (from `NonExist` or `Uninit` to `Active`, not aborted; the
/// address may have been funded before, so it need not be the first),
/// that transaction's `confirmDeposit` to the bridge, and the bridge
/// transaction that ran it.
pub async fn by_voucher(
    an: &dyn AnRead,
    voucher: [u8; 32],
    bridge: [u8; 32],
    id: &Identity,
) -> anyhow::Result<Credit> {
    let mut after = None;
    let deploy = loop {
        let page = an.transactions(voucher, after.clone()).await?;
        if let Some(t) = page.items.into_iter().find(|t| {
            matches!(t.orig_status, AccStatus::NonExist | AccStatus::Uninit)
                && t.end_status == AccStatus::Active
                && !t.aborted
        }) {
            break t;
        }
        match page.cursor {
            Some(c) => after = Some(c),
            None => return Ok(Credit::NotYet),
        }
    };
    for m in &deploy.out_msgs {
        let Some(msg) = an.message(m).await? else {
            continue;
        };
        if !is_recipient(&msg.dst, &bridge) {
            continue;
        }
        let ours = msg
            .body
            .as_deref()
            .and_then(|b| an.decode(BRIDGE_ABI, b, true))
            .is_some_and(|(n, v)| n == "confirmDeposit" && id.matches(&v));
        if !ours {
            continue;
        }
        let Some(d) = msg.dst_tx else {
            return Ok(Credit::NotYet);
        };
        if d.aborted {
            return Ok(Credit::Aborted {
                bridge_tx: d.hash,
            });
        }
        let Some(tx) = an.transaction(&d.hash).await? else {
            return Ok(Credit::NotYet);
        };
        return bridge_tx_verdict(an, &tx, id, false).await;
    }
    Ok(Credit::NotYet)
}

/// The bridge's `DepositFinalized` for this deposit, if it emitted one: the
/// event message's hash. That the deposit is finalized, not yet that the
/// credit arrived — `by_events` follows the rest of the chain.
///
/// The events are read newest first, and the page that reaches back past
/// `not_before` (a time the event cannot predate) is the last one read. A
/// list that cannot be read is an error, never "no event".
pub async fn finalized_event(
    an: &dyn AnRead,
    bridge: [u8; 32],
    id: &Identity,
    not_before: u64,
) -> anyhow::Result<Option<String>> {
    let mut before = None;
    loop {
        let page = an.ext_messages(bridge, ExtDir::Out, before.clone()).await?;
        let mut older = false;
        for m in &page.items {
            older |= m.created_at < not_before;
            let ours = m.dst.ends_with(DEPOSIT_FINALIZED_DST)
                && m.body
                    .as_deref()
                    .and_then(|b| an.decode(BRIDGE_ABI, b, false))
                    .is_some_and(|(n, v)| n == "DepositFinalized" && id.matches(&v));
            if ours {
                return Ok(Some(m.hash.clone()));
            }
        }
        match (older, page.cursor) {
            (false, Some(c)) => before = Some(c),
            _ => return Ok(None),
        }
    }
}

/// What shows that somebody finalized a deposit already: this run before
/// it stopped, an earlier run, or the operator's relayer.
#[derive(Debug, Clone)]
pub struct FinalizedCheck {
    /// The voucher code hash the operation recorded, as hex.
    pub stored_code: String,
    /// The voucher's address under that code.
    pub voucher: [u8; 32],
    /// The deposit.
    pub identity: Identity,
    /// The deposit block's time in seconds, which no `DepositFinalized` of
    /// this deposit predates; 0 when it is not known, and the events are
    /// then paged further back.
    pub not_before: u64,
}

/// How hard [`finalized_already`] looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Before a `finalizeDeposit` or a proof that has to be built again:
    /// the events are read whenever the voucher code moved or no proof is
    /// on disk (a new proof costs minutes), retried until the deadline.
    BeforeSend {
        /// A proof is on disk.
        have_proof: bool,
    },
    /// On a poll of the anchor wait: one attempt at each read, and the
    /// events only when the voucher code moved.
    Poll,
}

/// Whether somebody finalized the deposit already, and if so whether step
/// 9 confirms it by events (`Some(true)`) or through the voucher
/// (`Some(false)`). The voucher at the stored address shows it while the
/// bridge keeps the voucher code the address was computed with. After a
/// code change the deposit is finalized through another address, and only
/// the bridge's `DepositFinalized` events show it. Every read is bounded by
/// `deadline`; one that fails or runs out is "not known", never
/// "finalized".
pub async fn finalized_already(
    an: &dyn AnRead,
    ui: &dyn Ui,
    bridge: [u8; 32],
    check: &FinalizedCheck,
    look: Look,
    deadline: Option<tokio::time::Instant>,
) -> Option<bool> {
    let code = once(
        deadline,
        an.run_getter(bridge, BRIDGE_ABI, "getDepositVoucherCodeHash", json!({})),
    )
    .await;
    // Unknown counts as moved: then the events decide, not a stale address.
    let stored = check.stored_code.trim_start_matches("0x");
    let moved = code.and_then(|c| {
        c["value0"]
            .as_str()
            .map(|h| h.trim_start_matches("0x").to_ascii_lowercase())
    }) != Some(stored.to_ascii_lowercase());
    if !moved {
        let v = once(deadline, an.account(check.voucher)).await.flatten();
        if v.is_some_and(|a| a.status == AccStatus::Active) {
            return Some(false);
        }
        // With a proof on disk, step 8's own look before every send is
        // enough; in the anchor wait, the next poll looks again.
        if !matches!(look, Look::BeforeSend {
            have_proof: false
        }) {
            return None;
        }
    }
    let event = || finalized_event(an, bridge, &check.identity, check.not_before);
    // The event is enough to stop proving and sending; step 9 follows the
    // rest of the chain to the delivered transfer.
    let found = match look {
        Look::Poll => once(deadline, event()).await,
        Look::BeforeSend {
            have_proof,
        } => {
            let found = until(
                ui,
                "reading the bridge's DepositFinalized events",
                deadline,
                event,
            )
            .await;
            if found.is_none() {
                ui.warn(if have_proof {
                    "the bridge's DepositFinalized events could not be read; sending \
                     finalizeDeposit"
                } else {
                    "the bridge's DepositFinalized events could not be read; building the proof \
                     again"
                });
            }
            found
        },
    };
    found.flatten().map(|_| true)
}

/// The fallback path, which does not depend on the voucher's address: the
/// deposit's `DepositFinalized`, the bridge transaction that emitted it,
/// and that transaction's transfer.
pub async fn by_events(
    an: &dyn AnRead,
    bridge: [u8; 32],
    id: &Identity,
    not_before: u64,
) -> anyhow::Result<Credit> {
    let Some(event) = finalized_event(an, bridge, id, not_before).await? else {
        return Ok(Credit::NotYet);
    };
    let Some(full) = an.message(&event).await? else {
        return Ok(Credit::NotYet);
    };
    let Some(src) = full.src_tx else {
        return Ok(Credit::NotYet);
    };
    let Some(tx) = an.transaction(&src).await? else {
        return Ok(Credit::NotYet);
    };
    bridge_tx_verdict(an, &tx, id, true).await
}

/// Step 9: follows both paths every `poll` until one confirms the credit
/// or sees the confirmation abort (exit 37), or `timeout` passes (exit 34).
/// With `via_events_only` (the voucher code changed, so the stored address
/// is stale) only the events are read. The timeout covers every read and
/// every pause; a read that fails is reported and counts as "not visible",
/// never as a verdict.
#[allow(clippy::too_many_arguments)]
pub async fn confirm(
    an: &dyn AnRead,
    bridge: [u8; 32],
    voucher: [u8; 32],
    id: &Identity,
    via_events_only: bool,
    not_before: u64,
    timeout: Duration,
    poll: Duration,
    op_id: &str,
    ui: &dyn Ui,
) -> CliResult<CreditInfo> {
    // One attempt per path per round, both bounded by one deadline: a path
    // that keeps failing (or hangs) must neither hide the other's answer nor
    // outlive --credit-timeout-s.
    let deadline = deadline_after(timeout);
    let mut failures = 0u32;
    loop {
        let (v, e) = tokio::join!(
            async {
                if via_events_only {
                    Ok(Ok(Credit::NotYet))
                } else {
                    tokio::time::timeout_at(deadline, by_voucher(an, voucher, bridge, id)).await
                }
            },
            tokio::time::timeout_at(deadline, by_events(an, bridge, id, not_before)),
        );
        let mut read = |r: Result<anyhow::Result<Credit>, tokio::time::error::Elapsed>,
                        what: &str| match r {
            Ok(Ok(c)) => c,
            Ok(Err(err)) => {
                failures += 1;
                ui.retry(what, failures, &format!("{err:#}"));
                Credit::NotYet
            },
            Err(_) => Credit::NotYet,
        };
        let v = read(v, "following the deposit voucher");
        let e = read(e, "reading the bridge's DepositFinalized events");
        match (v, e) {
            (Credit::Confirmed(c), _) | (_, Credit::Confirmed(c)) => return Ok(c),
            (
                Credit::Aborted {
                    bridge_tx,
                },
                _,
            )
            | (
                _,
                Credit::Aborted {
                    bridge_tx,
                },
            ) => {
                return Err(CliError::deposit(
                    ExitCode::CreditAborted,
                    Stage::Credit,
                    Some(op_id),
                    format!(
                        "the deposit voucher exists, but the bridge transaction {bridge_tx} that \
                         should have minted the credit aborted. The voucher is spent and the \
                         deposit cannot be finalized again; only the bridge operator can pay it \
                         out (mintAndSend). Give them operation {op_id} and transaction \
                         {bridge_tx}"
                    ),
                ));
            },
            _ => {},
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(CliError::deposit(
                ExitCode::CreditUnconfirmed,
                Stage::Credit,
                Some(op_id),
                format!(
                    "finalizeDeposit was sent and the credit could not be confirmed yet (a \
                     message in transit, a lagging or failing GraphQL); continue with --resume \
                     {op_id}"
                ),
            ));
        }
        ui.status(if via_events_only {
            "waiting for the credit: DepositFinalized → transfer → delivery"
        } else {
            "waiting for the credit: voucher → confirmDeposit → transfer → delivery"
        });
        tokio::time::sleep(poll.min(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use alloy_primitives::U256;
    use serde_json::json;

    use super::*;
    use crate::deposit::{an::*, testkit::*};

    const BRIDGE: [u8; 32] = [0x1a; 32];
    const VOUCHER: [u8; 32] = [0xee; 32];
    const ACC: [u8; 32] = [0xa3; 32];

    fn id(n: u64) -> Identity {
        Identity {
            deposit_id: U256::from(n),
            contract: U256::from(0xcdfdu64),
            chain_id: 11_155_111,
            amount: 1_000_000,
            account: ACC,
        }
    }

    fn fields(i: &Identity, dapp: &str) -> serde_json::Value {
        json!({ "depositId": i.deposit_id.to_string(), "contractAddr": i.contract.to_string(), "dappId": dapp,
                "chainId": i.chain_id.to_string(), "amount": i.amount.to_string(), "anAccount": format!("0x{}", hex::encode(i.account)) })
    }

    /// Voucher history: optional pre-funding transfer, then the deploy whose
    /// out message is confirmDeposit(i); the bridge transaction that ran it
    /// mints (or aborts), and the transfer is delivered.
    fn chain(an: &FakeAn, i: &Identity, prefunded: bool, bridge_aborted: bool) {
        let mut txs = vec![];
        if prefunded {
            txs.push(TxListItem {
                hash: "fund".into(),
                now: 1,
                orig_status: AccStatus::NonExist,
                end_status: AccStatus::Uninit,
                aborted: false,
                out_msgs: vec![],
            });
        }
        txs.push(TxListItem {
            hash: "deploy".into(),
            now: 2,
            orig_status: if prefunded {
                AccStatus::Uninit
            } else {
                AccStatus::NonExist
            },
            end_status: AccStatus::Active,
            aborted: false,
            out_msgs: vec!["confirm".into()],
        });
        an.txs.lock().unwrap().insert(VOUCHER, txs);
        let mut confirm = msg(
            "confirm",
            "Internal",
            &format!("0:{}", hex::encode(VOUCHER)),
            &format!("0:{}", hex::encode(BRIDGE)),
            Some("B-confirm"),
        );
        confirm.dst_tx = Some(TxRef {
            hash: "btx".into(),
            aborted: bridge_aborted,
            account: format!("0:{}", hex::encode(BRIDGE)),
            now: 3,
        });
        an.messages
            .lock()
            .unwrap()
            .insert("confirm".into(), confirm);
        an.bodies.lock().unwrap().insert(
            "B-confirm".into(),
            ("confirmDeposit".into(), fields(i, "0")),
        );
        let mut transfer = msg(
            "xfer",
            "Internal",
            &format!("0:{}", hex::encode(BRIDGE)),
            &format!("0:{}", hex::encode(ACC)),
            None,
        );
        transfer.ecc = vec![(3, i.amount)];
        transfer.bounce = Some(false);
        let mut event = msg(
            "ev",
            "ExtOut",
            &format!("0:{}", hex::encode(BRIDGE)),
            DEPOSIT_FINALIZED_DST,
            Some("B-event"),
        );
        event.created_at = 3;
        an.bodies.lock().unwrap().insert(
            "B-event".into(),
            ("DepositFinalized".into(), fields(i, "0")),
        );
        an.transactions
            .lock()
            .unwrap()
            .insert("btx".into(), TxView {
                hash: "btx".into(),
                aborted: bridge_aborted,
                exit_code: Some(if bridge_aborted { 100 } else { 0 }),
                account: String::new(),
                now: 3,
                out: if bridge_aborted {
                    vec![]
                } else {
                    vec![transfer.clone(), event.clone()]
                },
                ecc_delta: vec![],
            });
        let mut delivered = transfer.clone();
        delivered.dst_tx = Some(TxRef {
            hash: "rtx".into(),
            aborted: false,
            account: format!("0:{}", hex::encode(ACC)),
            now: 4,
        });
        an.messages.lock().unwrap().insert("xfer".into(), delivered);
        an.transactions
            .lock()
            .unwrap()
            .insert("rtx".into(), TxView {
                hash: "rtx".into(),
                aborted: false,
                exit_code: Some(0),
                account: String::new(),
                now: 4,
                out: vec![],
                ecc_delta: vec![(3, i.amount as i128)],
            });
        let mut ev_full = event;
        ev_full.src_tx = Some("btx".into());
        an.messages
            .lock()
            .unwrap()
            .insert("ev".into(), ev_full.clone());
        an.ext_out
            .lock()
            .unwrap()
            .entry(BRIDGE)
            .or_default()
            .push(ev_full);
    }

    #[tokio::test]
    async fn the_chain_from_the_voucher_confirms_the_credit() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        let Credit::Confirmed(c) = by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap() else {
            panic!()
        };
        assert_eq!(
            (c.confirm_tx.as_str(), c.delivery_tx.as_str(), c.via_events),
            ("btx", "rtx", false)
        );
    }

    #[tokio::test]
    async fn a_prefunded_voucher_address_is_paged_past() {
        let an = FakeAn::default();
        chain(&an, &id(0), true, false);
        assert!(matches!(
            by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
            Credit::Confirmed(_)
        ));
    }

    #[tokio::test]
    async fn an_aborted_confirm_is_exit_37() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, true);
        assert!(matches!(
            by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
            Credit::Aborted { .. }
        ));
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = confirm(
            &an,
            BRIDGE,
            VOUCHER,
            &id(0),
            false,
            0,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(1),
            "OP",
            &ui,
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::CreditAborted);
    }

    #[tokio::test]
    async fn another_deposit_of_the_same_amount_does_not_confirm_this_one() {
        let an = FakeAn::default();
        chain(&an, &id(1), false, false);
        // The voucher address of deposit 0 has no history; the only event is deposit
        // 1's.
        an.txs.lock().unwrap().remove(&VOUCHER);
        assert_eq!(
            by_events(&an, BRIDGE, &id(0), 0).await.unwrap(),
            Credit::NotYet
        );
        assert!(matches!(
            by_events(&an, BRIDGE, &id(1), 0).await.unwrap(),
            Credit::Confirmed(_)
        ));
    }

    #[tokio::test]
    async fn the_event_alone_says_finalized_before_the_credit_is_visible() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        an.messages.lock().unwrap().remove("xfer"); // the transfer is not delivered yet
        assert_eq!(
            finalized_event(&an, BRIDGE, &id(0), 0)
                .await
                .unwrap()
                .as_deref(),
            Some("ev")
        );
        assert_eq!(
            by_events(&an, BRIDGE, &id(0), 0).await.unwrap(),
            Credit::NotYet
        );
        assert_eq!(finalized_event(&an, BRIDGE, &id(1), 0).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_nonzero_dapp_in_the_event_is_not_ours() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        an.bodies.lock().unwrap().insert(
            "B-event".into(),
            ("DepositFinalized".into(), fields(&id(0), "1")),
        );
        an.txs.lock().unwrap().remove(&VOUCHER);
        assert_eq!(
            by_events(&an, BRIDGE, &id(0), 0).await.unwrap(),
            Credit::NotYet
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_voucher_path_that_keeps_failing_does_not_hide_the_events() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        an.fail_transactions
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let c = confirm(
            &an,
            BRIDGE,
            VOUCHER,
            &id(0),
            false,
            0,
            std::time::Duration::from_secs(300),
            std::time::Duration::from_secs(10),
            "OP",
            &ui,
        )
        .await
        .unwrap();
        assert!(c.via_events);
    }

    #[tokio::test(start_paused = true)]
    async fn reads_that_keep_failing_still_end_at_the_credit_timeout() {
        let an = FakeAn::default();
        an.fail_transactions
            .store(true, std::sync::atomic::Ordering::SeqCst);
        an.fail_ext_messages
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let e = confirm(
            &an,
            BRIDGE,
            VOUCHER,
            &id(0),
            false,
            0,
            std::time::Duration::from_secs(300),
            std::time::Duration::from_secs(10),
            "OP",
            &ui,
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::CreditUnconfirmed);
        assert!(t0.elapsed() <= std::time::Duration::from_secs(301));
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_visible_until_the_timeout_is_exit_34() {
        let an = FakeAn::default();
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let e = confirm(
            &an,
            BRIDGE,
            VOUCHER,
            &id(0),
            false,
            0,
            std::time::Duration::from_secs(300),
            std::time::Duration::from_secs(10),
            "OP",
            &ui,
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::CreditUnconfirmed);
    }

    /// A transfer that does not bounce, to an address with no code yet, runs
    /// a transaction that aborts (there is nothing to compute) and still
    /// keeps the currencies: that is a delivered credit.
    #[tokio::test]
    async fn a_recipient_without_code_is_credited_by_an_aborted_transaction() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        if let Some(d) = an
            .messages
            .lock()
            .unwrap()
            .get_mut("xfer")
            .and_then(|m| m.dst_tx.as_mut())
        {
            d.aborted = true;
        }
        if let Some(t) = an.transactions.lock().unwrap().get_mut("rtx") {
            t.aborted = true;
            t.exit_code = None;
        }
        assert!(matches!(
            by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
            Credit::Confirmed(_)
        ));
        assert!(matches!(
            by_events(&an, BRIDGE, &id(0), 0).await.unwrap(),
            Credit::Confirmed(_)
        ));
    }

    #[tokio::test]
    async fn a_transfer_that_did_not_stay_with_the_recipient_is_not_the_credit() {
        // The recipient's transaction shows no ECC[3] of the amount.
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        if let Some(t) = an.transactions.lock().unwrap().get_mut("rtx") {
            t.ecc_delta = vec![];
        }
        assert_eq!(
            by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
            Credit::NotYet
        );
        // The bridge's credit does not bounce, and a transfer whose flag
        // was not read is not known to be it.
        for bounce in [Some(true), None] {
            let an = FakeAn::default();
            chain(&an, &id(0), false, false);
            if let Some(t) = an.transactions.lock().unwrap().get_mut("btx") {
                t.out[0].bounce = bounce;
            }
            assert_eq!(
                by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
                Credit::NotYet,
                "{bounce:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_voucher_history_is_read_page_by_page_to_its_deploy() {
        let an = FakeAn::default();
        chain(&an, &id(0), true, false);
        an.page_size.store(1, Ordering::SeqCst);
        assert!(matches!(
            by_voucher(&an, VOUCHER, BRIDGE, &id(0)).await.unwrap(),
            Credit::Confirmed(_)
        ));
        assert_eq!(an.pages_read.load(Ordering::SeqCst), 2);
    }

    /// Adds, newest first, an event of deposit `n` created at `at`.
    fn other_event(an: &FakeAn, n: u64, at: u64) {
        let body = format!("B-event-{n}");
        let mut m = msg(
            &format!("ev{n}"),
            "ExtOut",
            &format!("0:{}", hex::encode(BRIDGE)),
            DEPOSIT_FINALIZED_DST,
            Some(&body),
        );
        m.created_at = at;
        an.bodies
            .lock()
            .unwrap()
            .insert(body, ("DepositFinalized".into(), fields(&id(n), "0")));
        an.ext_out
            .lock()
            .unwrap()
            .entry(BRIDGE)
            .or_default()
            .insert(0, m);
    }

    #[tokio::test]
    async fn the_event_search_pages_back_no_further_than_not_before() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false); // this deposit's event, created at 3
        other_event(&an, 2, 5);
        other_event(&an, 1, 10);
        an.page_size.store(1, Ordering::SeqCst);
        // Newer events of other deposits are paged past.
        assert_eq!(
            finalized_event(&an, BRIDGE, &id(0), 0)
                .await
                .unwrap()
                .as_deref(),
            Some("ev")
        );
        assert_eq!(an.pages_read.load(Ordering::SeqCst), 3);
        // The page holding an event older than `not_before` is the last read.
        an.pages_read.store(0, Ordering::SeqCst);
        assert_eq!(finalized_event(&an, BRIDGE, &id(0), 9).await.unwrap(), None);
        assert_eq!(an.pages_read.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_event_list_that_cannot_be_read_is_an_error_not_no_event() {
        let an = FakeAn::default();
        chain(&an, &id(0), false, false);
        an.fail_ext_messages.store(true, Ordering::SeqCst);
        assert!(finalized_event(&an, BRIDGE, &id(0), 0).await.is_err());
        assert!(by_events(&an, BRIDGE, &id(0), 0).await.is_err());
    }

    /// The body of `confirmDeposit(i)` with `dapp`, encoded by the SDK as
    /// the voucher sends it.
    async fn confirm_body(i: &Identity, amount: u128, dapp: u8) -> String {
        use tvm_client::abi::{
            encode_message_body, Abi, CallSet, ParamsOfEncodeMessageBody, Signer,
        };
        encode_message_body(
            crate::deposit::identity::offline_context(),
            ParamsOfEncodeMessageBody {
                abi: Abi::Json(BRIDGE_ABI.into()),
                call_set: CallSet {
                    function_name: "confirmDeposit".into(),
                    header: None,
                    input: Some(json!({
                        "depositId": i.deposit_id.to_string(),
                        "contractAddr": format!("{:#x}", i.contract),
                        "dappId": dapp,
                        "chainId": i.chain_id,
                        "amount": amount.to_string(),
                        "anAccount": format!("0x{}", hex::encode(i.account)),
                    })),
                },
                is_internal: true,
                signer: Signer::None,
                processing_try_index: None,
                address: None,
                signature_id: None,
            },
        )
        .await
        .unwrap()
        .body
    }

    /// The SDK decodes a uint256 as 64 hex digits and a uint128 in decimal.
    #[tokio::test]
    async fn the_identity_matches_what_the_sdk_decodes() {
        let i = Identity {
            deposit_id: U256::from(7u64),
            contract: U256::from_be_slice(&[0xcd; 20]),
            chain_id: 11_155_111,
            amount: 25_000_000,
            account: ACC,
        };
        // Decoding is local: nothing is contacted.
        let an = LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        let decode = |body: String| an.decode(BRIDGE_ABI, &body, true).unwrap();
        let (name, v) = decode(confirm_body(&i, i.amount, 0).await);
        assert_eq!(name, "confirmDeposit");
        assert!(i.matches(&v), "{v}");
        assert!(!i.matches(&decode(confirm_body(&i, i.amount + 1, 0).await).1));
        assert!(!i.matches(&decode(confirm_body(&i, i.amount, 1).await).1));
        let other = Identity {
            deposit_id: U256::from(8u64),
            ..i.clone()
        };
        assert!(!other.matches(&v));
    }

    /// The newest `DepositFinalized` of the shellnet bridge, confirmed by
    /// its events and, while the voucher code is the one this build knows,
    /// through its voucher.
    #[tokio::test]
    #[ignore = "reads shellnet; run by hand"]
    async fn live_shellnet_confirms_its_newest_finalized_deposit() {
        let an = LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        let bridge = [0x1a; 32];
        let mut before = None;
        let (event, v) = 'found: loop {
            let page = an.ext_messages(bridge, ExtDir::Out, before).await.unwrap();
            for m in &page.items {
                if !m.dst.ends_with(DEPOSIT_FINALIZED_DST) {
                    continue;
                }
                if let Some(("DepositFinalized", v)) = m
                    .body
                    .as_deref()
                    .and_then(|b| an.decode(BRIDGE_ABI, b, false))
                    .as_ref()
                    .map(|(n, v)| (n.as_str(), v.clone()))
                {
                    break 'found (m.clone(), v);
                }
            }
            before = Some(page.cursor.expect(
                "no DepositFinalized in the bridge's history: no deposit was finalized on this \
                 network yet",
            ));
        };
        println!("{} {v}", event.hash);
        let u = |k: &str| v[k].as_str().unwrap().parse::<U256>().unwrap();
        let i = Identity {
            deposit_id: u("depositId"),
            contract: u("contractAddr"),
            chain_id: u("chainId").to::<u64>(),
            amount: u("amount").to::<u128>(),
            account: u("anAccount").to_be_bytes(),
        };
        assert!(i.matches(&v));
        assert_eq!(
            finalized_event(&an, bridge, &i, event.created_at)
                .await
                .unwrap()
                .as_deref(),
            Some(event.hash.as_str())
        );
        let by_ev = by_events(&an, bridge, &i, event.created_at).await.unwrap();
        println!("by events: {by_ev:?}");
        assert!(matches!(by_ev, Credit::Confirmed(_)), "{by_ev:?}");
        let ctx = crate::deposit::identity::offline_context();
        let voucher = crate::deposit::identity::voucher_account_id(
            &ctx,
            &crate::deposit::identity::DepositIdentity {
                deposit_id: i.deposit_id,
                contract: alloy_primitives::Address::from_slice(
                    &i.contract.to_be_bytes::<32>()[12..],
                ),
                chain_id: i.chain_id,
            },
        )
        .unwrap();
        let by_v = by_voucher(&an, voucher, bridge, &i).await.unwrap();
        println!("voucher {}: {by_v:?}", hex::encode(voucher));
        assert!(matches!(by_v, Credit::Confirmed(_)), "{by_v:?}");
    }
}
