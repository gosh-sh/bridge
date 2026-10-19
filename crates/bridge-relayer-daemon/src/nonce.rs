//! Nonce and fees for the relayer's own transactions.
//!
//! alloy's default nonce manager reads the nonce once and then counts
//! locally, advancing before the broadcast: a send the RPC lost left a gap,
//! every later transaction queued behind it, and the bridge stood still until
//! the daemon was restarted. Here the nonce is read from the chain before
//! every transaction, and the relayer remembers the last transaction it sent
//! from each address:
//!
//! * mined (`latest` is past it), or nothing remembered: the `pending` count,
//!   so transactions already waiting from the account are queued behind, not
//!   replaced;
//! * not mined, and a transaction waiting at its nonce in the pool of the node
//!   asked: the same nonce with fees 25 % above the last plan, which replaces
//!   it (a replacement must pay at least 10 % more), but never more than
//!   [`MAX_REPLACEMENT_FEE_MULTIPLE`] times the current estimate. Whatever
//!   waits at that nonce is taken for ours: on a key shared with someone else
//!   it can be theirs, sent after our send was lost;
//! * not mined, and no transaction waiting at that nonce: the send was lost, so
//!   the same nonce again at the current fee estimate.
//!
//! A plan is made only once the transaction's gas estimate has succeeded,
//! so a call that would revert leaves the nonce and fees alone. It is
//! remembered before the send, so a send cut short — a timeout, a dropped
//! future — still leaves the next attempt knowing its nonce.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use alloy::{
    network::Network, primitives::Address, providers::Provider, transports::TransportResult,
};
use tracing::{info, warn};

/// The most a replacement pays, as a multiple of the current EIP-1559
/// estimate — for the fee cap and the tip alike. Without it every
/// replacement of a transaction that stays unmined raised the fees by
/// another quarter: 87 times what the first send paid after 20 of them.
pub const MAX_REPLACEMENT_FEE_MULTIPLE: u128 = 4;

/// Nonce and EIP-1559 fees for one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxPlan {
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// The transaction replaces one of ours still waiting at this nonce.
    pub replaces: bool,
}

/// What the relayer last sent from an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sent {
    pub(crate) nonce: u64,
    pub(crate) max_fee_per_gas: u128,
    pub(crate) max_priority_fee_per_gas: u128,
}

fn last_sent() -> &'static Mutex<HashMap<Address, Sent>> {
    static LAST_SENT: OnceLock<Mutex<HashMap<Address, Sent>>> = OnceLock::new();
    LAST_SENT.get_or_init(Default::default)
}

/// The nonce and fees for the next transaction from `sender`, from the
/// `latest` and `pending` counts, the fee estimate and what was sent last.
pub async fn plan_transaction<P, N>(provider: &P, sender: Address) -> TransportResult<TxPlan>
where
    P: Provider<N>,
    N: Network,
{
    let latest = provider.get_transaction_count(sender).latest().await?;
    let pending = provider.get_transaction_count(sender).pending().await?;
    let fees = provider.estimate_eip1559_fees().await?;
    let last = last_sent()
        .lock()
        .ok()
        .and_then(|sent| sent.get(&sender).copied());
    let plan = decide(
        last,
        latest,
        pending,
        fees.max_fee_per_gas,
        fees.max_priority_fee_per_gas,
    );
    if plan.replaces {
        warn!(
            %sender,
            nonce = plan.nonce,
            max_fee_per_gas = plan.max_fee_per_gas,
            max_priority_fee_per_gas = plan.max_priority_fee_per_gas,
            "our last transaction is still waiting unmined; replacing it at the same nonce with higher fees"
        );
    } else if last.is_some_and(|t| t.nonce >= latest) {
        info!(
            %sender,
            nonce = plan.nonce,
            "our last transaction never reached a block or a pool; sending again at its nonce"
        );
    } else if pending > latest {
        info!(
            %sender,
            latest,
            pending,
            "other transactions from this account are waiting; queuing after them"
        );
    }
    Ok(plan)
}

/// Records `plan` as the last transaction sent from `sender`.
pub fn remember(sender: Address, plan: &TxPlan) {
    if let Ok(mut sent) = last_sent().lock() {
        sent.insert(sender, Sent {
            nonce: plan.nonce,
            max_fee_per_gas: plan.max_fee_per_gas,
            max_priority_fee_per_gas: plan.max_priority_fee_per_gas,
        });
    }
}

/// The rules of the module docs, without the RPC: `latest` and `pending`
/// are the account's transaction counts, `fee` and `tip` the current
/// EIP-1559 estimate.
pub(crate) fn decide(
    last: Option<Sent>,
    latest: u64,
    pending: u64,
    fee: u128,
    tip: u128,
) -> TxPlan {
    let pending = pending.max(latest);
    let (nonce, fee, tip, replaces) = match last {
        Some(t) if t.nonce >= latest => {
            // Ours is not mined. A gap below it (`pending` short of its
            // nonce) is filled first.
            let nonce = t.nonce.min(pending);
            let replaces = nonce == t.nonce && pending > t.nonce;
            if replaces {
                (
                    nonce,
                    replacement(t.max_fee_per_gas, fee),
                    replacement(t.max_priority_fee_per_gas, tip),
                    true,
                )
            } else {
                (nonce, fee, tip, false)
            }
        },
        _ => (pending, fee, tip, false),
    };
    TxPlan {
        nonce,
        max_fee_per_gas: fee.max(tip),
        max_priority_fee_per_gas: tip,
        replaces,
    }
}

/// What a replacement pays where the replaced transaction paid `last` and
/// the estimate is now `estimate`: `last` raised by [`bump`], at least the
/// estimate and at most [`MAX_REPLACEMENT_FEE_MULTIPLE`] times it. Held at
/// the cap, a replacement no longer pays the 10 % more a node asks for; the
/// node refuses it, and the transaction it would replace keeps waiting at
/// several times the going rate.
fn replacement(last: u128, estimate: u128) -> u128 {
    bump(last).clamp(
        estimate,
        estimate.saturating_mul(MAX_REPLACEMENT_FEE_MULTIPLE),
    )
}

/// 25 % more, and at least one wei more.
fn bump(value: u128) -> u128 {
    value.saturating_add(value / 4).saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE: u128 = 2_000_000_000;
    const TIP: u128 = 1_000_000_000;

    fn sent(nonce: u64, fee: u128, tip: u128) -> Option<Sent> {
        Some(Sent {
            nonce,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: tip,
        })
    }

    #[test]
    fn nothing_remembered_takes_the_pending_count() {
        let plan = decide(None, 421, 421, FEE, TIP);
        assert_eq!(plan, TxPlan {
            nonce: 421,
            max_fee_per_gas: FEE,
            max_priority_fee_per_gas: TIP,
            replaces: false,
        });
        // Someone else's transaction is waiting at 421: queue after it.
        assert_eq!(decide(None, 421, 422, FEE, TIP).nonce, 422);
    }

    #[test]
    fn a_mined_transaction_moves_on_to_the_pending_count() {
        let plan = decide(sent(421, FEE, TIP), 422, 422, FEE, TIP);
        assert_eq!((plan.nonce, plan.replaces), (422, false));
    }

    /// The failure of the shellnet run: the send of 421 was lost, so no node
    /// counts it. The next transaction must take 421 again, not 422.
    #[test]
    fn a_lost_send_is_sent_again_at_its_nonce() {
        let plan = decide(sent(421, FEE, TIP), 421, 421, FEE, TIP);
        assert_eq!(plan, TxPlan {
            nonce: 421,
            max_fee_per_gas: FEE,
            max_priority_fee_per_gas: TIP,
            replaces: false,
        });
    }

    #[test]
    fn a_waiting_transaction_is_replaced_with_fees_a_quarter_higher() {
        let plan = decide(sent(421, FEE, TIP), 421, 422, FEE, TIP);
        assert_eq!(plan.nonce, 421);
        assert!(plan.replaces);
        assert_eq!(plan.max_fee_per_gas, FEE + FEE / 4 + 1);
        assert_eq!(plan.max_priority_fee_per_gas, TIP + TIP / 4 + 1);
        // Above the 10 % a replacement has to pay.
        assert!(plan.max_fee_per_gas * 10 >= FEE * 11);
        assert!(plan.max_priority_fee_per_gas * 10 >= TIP * 11);
    }

    #[test]
    fn a_replacement_never_pays_less_than_the_current_estimate() {
        let plan = decide(sent(421, FEE, TIP), 421, 422, 10 * FEE, 10 * TIP);
        assert_eq!(plan.max_fee_per_gas, 10 * FEE);
        assert_eq!(plan.max_priority_fee_per_gas, 10 * TIP);
    }

    #[test]
    fn repeated_replacements_keep_raising_the_fees() {
        let first = decide(sent(421, FEE, TIP), 421, 422, FEE, TIP);
        let second = decide(
            sent(421, first.max_fee_per_gas, first.max_priority_fee_per_gas),
            421,
            422,
            FEE,
            TIP,
        );
        assert!(second.max_fee_per_gas > first.max_fee_per_gas);
        assert!(second.max_priority_fee_per_gas > first.max_priority_fee_per_gas);
    }

    /// The review's case: one transaction stays unmined and is replaced
    /// again and again. The fees stop at the cap instead of compounding.
    #[test]
    fn repeated_replacements_stop_at_four_times_the_estimate() {
        let mut last = sent(421, FEE, TIP);
        for _ in 0..50 {
            let plan = decide(last, 421, 422, FEE, TIP);
            assert!(plan.replaces);
            assert!(plan.max_fee_per_gas <= MAX_REPLACEMENT_FEE_MULTIPLE * FEE);
            assert!(plan.max_priority_fee_per_gas <= MAX_REPLACEMENT_FEE_MULTIPLE * TIP);
            last = sent(421, plan.max_fee_per_gas, plan.max_priority_fee_per_gas);
        }
        let held = decide(last, 421, 422, FEE, TIP);
        assert_eq!(held.max_fee_per_gas, MAX_REPLACEMENT_FEE_MULTIPLE * FEE);
        assert_eq!(
            held.max_priority_fee_per_gas,
            MAX_REPLACEMENT_FEE_MULTIPLE * TIP
        );
    }

    /// A transaction sent when fees were high is replaced at no more than
    /// the cap of today's estimate, even when that is below what it pays.
    #[test]
    fn the_cap_follows_the_current_estimate() {
        let plan = decide(sent(421, 20 * FEE, 20 * TIP), 421, 422, FEE, TIP);
        assert_eq!(plan.max_fee_per_gas, MAX_REPLACEMENT_FEE_MULTIPLE * FEE);
        assert_eq!(
            plan.max_priority_fee_per_gas,
            MAX_REPLACEMENT_FEE_MULTIPLE * TIP
        );
    }

    #[test]
    fn a_gap_below_the_remembered_nonce_is_filled_first() {
        let plan = decide(sent(423, FEE, TIP), 421, 421, FEE, TIP);
        assert_eq!((plan.nonce, plan.replaces), (421, false));
    }

    #[test]
    fn the_fee_cap_is_never_below_the_tip() {
        let plan = decide(None, 0, 0, TIP / 2, TIP);
        assert_eq!(plan.max_fee_per_gas, TIP);
    }

    #[test]
    fn a_pending_count_behind_latest_is_read_as_latest() {
        assert_eq!(decide(None, 421, 420, FEE, TIP).nonce, 421);
    }
}

/// `withdrawByProof` through the real provider and bridge client against a
/// local endpoint that plays the chain: the nonce of every signed
/// transaction it receives is read back from the raw bytes.
#[cfg(test)]
mod end_to_end {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use alloy::{
        consensus::{Transaction, TxEnvelope},
        eips::eip2718::Decodable2718,
        primitives::{hex, Address, Bytes, U256},
        signers::local::PrivateKeySigner,
    };
    use serde_json::{json, Value};

    use crate::{
        bridge::{EthBridgeClient, WithdrawSubmitOutcome},
        eth_rpc::{
            test_support::{endpoint, Responder},
            wallet_provider_with_timeout,
        },
        withdrawal::WithdrawalPublicInputs,
    };

    struct Chain {
        latest: u64,
        pending: u64,
        /// `eth_estimateGas` answers with a revert, as for a proof whose
        /// anchor the bridge does not hold yet.
        estimate_reverts: bool,
        /// What happens to each `eth_sendRawTransaction`, in turn.
        replies: VecDeque<Reply>,
        sent: Vec<TxEnvelope>,
    }

    /// What the endpoint does with one `eth_sendRawTransaction`.
    #[derive(Clone, Copy)]
    enum Reply {
        /// Never answers, like a send the RPC lost.
        Lost,
        /// Refuses it, as for an account that cannot pay.
        Refused,
        /// Takes it; no receipt ever comes.
        Accepted,
    }

    fn chain(latest: u64, replies: Vec<Reply>) -> Arc<Mutex<Chain>> {
        Arc::new(Mutex::new(Chain {
            latest,
            pending: latest,
            estimate_reverts: false,
            replies: replies.into(),
            sent: Vec::new(),
        }))
    }

    fn playing(chain: Arc<Mutex<Chain>>) -> Responder {
        Arc::new(move |request: &Value| {
            let mut chain = chain.lock().unwrap();
            let ok = |v: Value| Some((200, json!({"jsonrpc": "2.0", "id": 0, "result": v})));
            let error = |code: i64, message: &str| {
                Some((
                    200,
                    json!({"jsonrpc": "2.0", "id": 0, "error": {"code": code, "message": message}}),
                ))
            };
            match request["method"].as_str()? {
                "eth_chainId" => ok(json!("0xaa36a7")),
                "eth_blockNumber" => ok(json!("0x10")),
                "eth_getTransactionCount" => {
                    let count = if request["params"][1] == "pending" {
                        chain.pending
                    } else {
                        chain.latest
                    };
                    ok(json!(format!("{count:#x}")))
                },
                "eth_feeHistory" => ok(json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]],
                })),
                "eth_estimateGas" if chain.estimate_reverts => error(3, "execution reverted"),
                "eth_estimateGas" => ok(json!("0x30d40")),
                "eth_sendRawTransaction" => {
                    let raw = hex::decode(request["params"][0].as_str()?).ok()?;
                    let tx = TxEnvelope::decode_2718(&mut raw.as_slice()).ok()?;
                    let hash = *tx.tx_hash();
                    chain.sent.push(tx);
                    match chain.replies.pop_front()? {
                        Reply::Lost => None,
                        Reply::Refused => {
                            error(-32000, "insufficient funds for gas * price + value")
                        },
                        Reply::Accepted => ok(json!(hash)),
                    }
                },
                _ => ok(Value::Null),
            }
        })
    }

    async fn bridge(
        chain: Arc<Mutex<Chain>>,
        key: u8,
    ) -> EthBridgeClient<impl alloy::providers::Provider + Clone> {
        let (url, _) = endpoint(playing(chain)).await;
        let signer: PrivateKeySigner = format!("0x{}", hex::encode([key; 32])).parse().unwrap();
        let sender = signer.address();
        let provider = wallet_provider_with_timeout(&[url], signer, Duration::from_millis(500))
            .await
            .unwrap();
        EthBridgeClient::new(Address::repeat_byte(0x53), provider).with_sender(sender)
    }

    fn inputs() -> WithdrawalPublicInputs {
        WithdrawalPublicInputs {
            token_id: U256::from(3),
            amount: U256::from(1),
            recipient_hi: U256::ZERO,
            recipient_lo: U256::ZERO,
            dst_chain_id: U256::from(11_155_111),
            sender_acc_fr: U256::ZERO,
            dapp_fr: U256::ZERO,
            acc_fr: U256::ZERO,
            nullifier: U256::from(7),
            final_root: U256::from(9),
            anchor_layer: U256::from(1),
        }
    }

    async fn submit(bridge: &EthBridgeClient<impl alloy::providers::Provider + Clone>) -> String {
        let outcome = bridge
            .submit_withdraw(&Bytes::from(vec![0u8; 4]), &inputs())
            .await
            .unwrap();
        match outcome {
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent: false,
            } => reason,
            other => panic!("{other:?}"),
        }
    }

    /// The shellnet failure: the send of 421 was lost. alloy's cached
    /// manager sent the next transaction as 422, which waited for 421
    /// forever; it must go out as 421 again.
    #[tokio::test]
    async fn a_lost_send_does_not_move_the_nonce() {
        let chain = chain(421, vec![Reply::Lost, Reply::Refused]);
        let bridge = bridge(chain.clone(), 0x21).await;
        submit(&bridge).await;
        submit(&bridge).await;
        let chain = chain.lock().unwrap();
        let nonces: Vec<u64> = chain.sent.iter().map(|tx| tx.nonce()).collect();
        assert_eq!(nonces, [421, 421]);
        assert_eq!(
            chain.sent[1].max_fee_per_gas(),
            chain.sent[0].max_fee_per_gas()
        );
    }

    #[tokio::test]
    async fn a_transaction_still_waiting_is_replaced_with_higher_fees() {
        let chain = chain(421, vec![Reply::Refused, Reply::Refused]);
        let bridge = bridge(chain.clone(), 0x22).await;
        submit(&bridge).await;
        // Our 421 now waits in the pool, unmined.
        chain.lock().unwrap().pending = 422;
        submit(&bridge).await;
        let chain = chain.lock().unwrap();
        let (first, second) = (&chain.sent[0], &chain.sent[1]);
        assert_eq!((first.nonce(), second.nonce()), (421, 421));
        assert!(second.max_fee_per_gas() >= first.max_fee_per_gas() * 5 / 4);
        assert!(
            second.max_priority_fee_per_gas().unwrap()
                >= first.max_priority_fee_per_gas().unwrap() * 5 / 4
        );
    }

    #[tokio::test]
    async fn a_mined_transaction_lets_the_next_one_move_on() {
        let chain = chain(421, vec![Reply::Refused, Reply::Refused]);
        let bridge = bridge(chain.clone(), 0x23).await;
        submit(&bridge).await;
        {
            let mut chain = chain.lock().unwrap();
            chain.latest = 422;
            chain.pending = 422;
        }
        submit(&bridge).await;
        let nonces: Vec<u64> = chain
            .lock()
            .unwrap()
            .sent
            .iter()
            .map(|tx| tx.nonce())
            .collect();
        assert_eq!(nonces, [421, 422]);
    }

    /// The review's case in `daemon-bridge`: our 421 waits unmined while
    /// the withdraw leg tries a proof whose anchor the bridge does not hold
    /// yet, and its gas estimate reverts. Those attempts send nothing and
    /// leave the plan alone, so the replacement that follows pays a quarter
    /// more than 421 did, not a quarter more for every failed attempt.
    #[tokio::test]
    async fn a_failed_gas_estimate_leaves_the_nonce_and_fees_alone() {
        let chain = chain(421, vec![Reply::Refused, Reply::Refused]);
        let bridge = bridge(chain.clone(), 0x24).await;
        submit(&bridge).await;
        {
            let mut chain = chain.lock().unwrap();
            chain.pending = 422;
            chain.estimate_reverts = true;
        }
        for _ in 0..20 {
            let reason = submit(&bridge).await;
            assert!(reason.contains("execution reverted"), "{reason}");
        }
        chain.lock().unwrap().estimate_reverts = false;
        submit(&bridge).await;
        let chain = chain.lock().unwrap();
        assert_eq!(chain.sent.len(), 2, "a failed estimate sends nothing");
        let (first, second) = (&chain.sent[0], &chain.sent[1]);
        assert_eq!((first.nonce(), second.nonce()), (421, 421));
        let fee = first.max_fee_per_gas();
        let tip = first.max_priority_fee_per_gas().unwrap();
        assert_eq!(second.max_fee_per_gas(), fee + fee / 4 + 1);
        assert_eq!(second.max_priority_fee_per_gas(), Some(tip + tip / 4 + 1));
        // The estimate is the gas limit; `send()` does not estimate again.
        assert_eq!(second.gas_limit(), 200_000);
    }

    /// `ackinacki-bridge withdraw` and the one-shot commands wait for the
    /// receipt as long as it takes: a slow payout is not a failed one.
    #[tokio::test]
    async fn without_a_receipt_timeout_the_receipt_is_waited_for() {
        let chain = chain(421, vec![Reply::Accepted]);
        let bridge = bridge(chain.clone(), 0x25).await;
        let waiting = tokio::time::timeout(
            Duration::from_secs(2),
            bridge.submit_withdraw(&Bytes::from(vec![0u8; 4]), &inputs()),
        )
        .await;
        assert!(waiting.is_err(), "{waiting:?}");
        assert_eq!(chain.lock().unwrap().sent.len(), 1);
    }

    /// The daemons stop waiting, and the reason names the transaction.
    #[tokio::test]
    async fn the_daemons_receipt_timeout_names_the_transaction() {
        let chain = chain(421, vec![Reply::Accepted]);
        let bridge = bridge(chain.clone(), 0x26)
            .await
            .with_withdraw_receipt_timeout(Duration::from_millis(300));
        let reason = tokio::time::timeout(Duration::from_secs(10), submit(&bridge))
            .await
            .expect("the receipt timeout ends the wait");
        let hash = *chain.lock().unwrap().sent[0].tx_hash();
        assert!(reason.starts_with("tx confirmation error:"), "{reason}");
        assert!(reason.contains(&hash.to_string()), "{reason}");
    }
}
