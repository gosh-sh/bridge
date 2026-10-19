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
//!   so transactions other senders have waiting are left alone — the shellnet
//!   burner key is shared;
//! * not mined, and waiting in the pool of the node asked: the same nonce with
//!   fees 25 % above it, which replaces it (a replacement must pay at least 10
//!   % more);
//! * not mined, and no transaction waiting at that nonce: the send was lost, so
//!   the same nonce again at the current fee estimate.
//!
//! The plan is remembered before the send, so a send cut short — a timeout,
//! a dropped future — still leaves the next attempt knowing its nonce.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use alloy::{
    network::Network, primitives::Address, providers::Provider, transports::TransportResult,
};
use tracing::{info, warn};

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
                    fee.max(bump(t.max_fee_per_gas)),
                    tip.max(bump(t.max_priority_fee_per_gas)),
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
        /// The reply to each `eth_sendRawTransaction`, in turn; `None`
        /// never answers, like a send the RPC lost.
        replies: VecDeque<Option<Value>>,
        sent: Vec<TxEnvelope>,
    }

    fn chain(latest: u64, replies: Vec<Option<Value>>) -> Arc<Mutex<Chain>> {
        Arc::new(Mutex::new(Chain {
            latest,
            pending: latest,
            replies: replies.into(),
            sent: Vec::new(),
        }))
    }

    fn refused() -> Option<Value> {
        Some(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "error": {"code": -32000, "message": "insufficient funds for gas * price + value"},
        }))
    }

    fn playing(chain: Arc<Mutex<Chain>>) -> Responder {
        Arc::new(move |request: &Value| {
            let mut chain = chain.lock().unwrap();
            let ok = |v: Value| Some((200, json!({"jsonrpc": "2.0", "id": 0, "result": v})));
            match request["method"].as_str()? {
                "eth_chainId" => ok(json!("0xaa36a7")),
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
                "eth_estimateGas" => ok(json!("0x30d40")),
                "eth_sendRawTransaction" => {
                    let raw = hex::decode(request["params"][0].as_str()?).ok()?;
                    let tx = TxEnvelope::decode_2718(&mut raw.as_slice()).ok()?;
                    chain.sent.push(tx);
                    chain
                        .replies
                        .pop_front()
                        .flatten()
                        .map(|reply| (200, reply))
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

    async fn submit(bridge: &EthBridgeClient<impl alloy::providers::Provider + Clone>) {
        let outcome = bridge
            .submit_withdraw(&Bytes::from(vec![0u8; 4]), &inputs())
            .await
            .unwrap();
        assert!(
            matches!(outcome, WithdrawSubmitOutcome::Reverted {
                permanent: false,
                ..
            }),
            "{outcome:?}"
        );
    }

    /// The shellnet failure: the send of 421 was lost. alloy's cached
    /// manager sent the next transaction as 422, which waited for 421
    /// forever; it must go out as 421 again.
    #[tokio::test]
    async fn a_lost_send_does_not_move_the_nonce() {
        let chain = chain(421, vec![None, refused()]);
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
        let chain = chain(421, vec![refused(), refused()]);
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
        let chain = chain(421, vec![refused(), refused()]);
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
}
