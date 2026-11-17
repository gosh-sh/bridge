use alloy::{
    primitives::{B256, U256},
    sol_types::SolError,
};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::{
    bridge::{AckiNackiBridge, DryRunOutcome, EthBridgeClient, NacklState},
    error::RelayerError,
};

#[async_trait]
pub trait NacklPayoutBridge: Send + Sync {
    async fn nackl_state(&self) -> Result<NacklState, RelayerError>;
    async fn dry_run_execute_nackl_payout(
        &self,
        payout_id: U256,
    ) -> Result<DryRunOutcome, RelayerError>;
    async fn execute_nackl_payout(&self, payout_id: U256) -> Result<B256, RelayerError>;
}

#[async_trait]
impl<P, N> NacklPayoutBridge for EthBridgeClient<P, N>
where
    P: alloy::providers::Provider<N> + Clone + Send + Sync + 'static,
    N: alloy::network::Network,
{
    async fn nackl_state(&self) -> Result<NacklState, RelayerError> {
        EthBridgeClient::nackl_state(self).await
    }

    async fn dry_run_execute_nackl_payout(
        &self,
        payout_id: U256,
    ) -> Result<DryRunOutcome, RelayerError> {
        EthBridgeClient::dry_run_execute_nackl_payout(self, payout_id).await
    }

    async fn execute_nackl_payout(&self, payout_id: U256) -> Result<B256, RelayerError> {
        EthBridgeClient::execute_nackl_payout(self, payout_id).await
    }
}

#[derive(Debug, Default)]
pub struct NacklPayoutScanState {
    pub executed: u64,
}

const MAX_PAYOUTS_PER_PASS: usize = 64;

fn reverted_with(reason: &str, selector: [u8; 4]) -> bool {
    reason
        .to_ascii_lowercase()
        .contains(&format!("0x{}", hex::encode(selector)))
}

pub async fn nackl_payout_scan_once<B>(
    bridge: &B,
    dry_run: bool,
    st: &mut NacklPayoutScanState,
) -> bool
where
    B: NacklPayoutBridge + ?Sized,
{
    use AckiNackiBridge as A;
    for _ in 0..MAX_PAYOUTS_PER_PASS {
        let state = match bridge.nackl_state().await {
            Ok(s) => s,
            Err(e) => {
                debug!(error = %e, "nacklState unavailable; skipping the NACKL payout lane");
                return false;
            },
        };
        if state.handed_over || state.pending_count.is_zero() || state.next_payout.is_zero() {
            return false;
        }
        let payout_id = state.next_payout;
        match bridge.dry_run_execute_nackl_payout(payout_id).await {
            Ok(DryRunOutcome::WouldSucceed) if dry_run => {
                info!(%payout_id, "dry-run: executeNacklPayout would succeed");
                return false;
            },
            Ok(DryRunOutcome::WouldSucceed) => match bridge.execute_nackl_payout(payout_id).await {
                Ok(tx_hash) => {
                    info!(%payout_id, ?tx_hash, "NACKL payout EXECUTED");
                    st.executed += 1;
                },
                Err(e) => {
                    warn!(%payout_id, error = %e, "executeNacklPayout failed; will retry");
                    return true;
                },
            },
            Ok(DryRunOutcome::WouldRevert {
                reason,
            }) => {
                if reverted_with(&reason, A::NacklPayoutAlreadyExecuted::SELECTOR)
                    || reverted_with(&reason, A::NacklPayoutNotNext::SELECTOR)
                {
                    debug!(%payout_id, %reason, "the next NACKL payout moved; reading it again");
                } else if reverted_with(&reason, A::NacklPayoutNotDue::SELECTOR)
                    || reverted_with(&reason, A::NacklWindowFull::SELECTOR)
                {
                    debug!(%payout_id, %reason, "the next NACKL payout is not executable yet");
                    return false;
                } else if reverted_with(&reason, A::NacklPayoutUnknown::SELECTOR)
                    || reverted_with(&reason, A::BridgePaused::SELECTOR)
                    || reverted_with(&reason, A::NacklMinterGone::SELECTOR)
                {
                    return false;
                } else {
                    warn!(%payout_id, %reason, "executeNacklPayout dry-run reverted; will retry");
                    return true;
                }
            },
            Err(e) => {
                warn!(%payout_id, error = %e, "executeNacklPayout dry-run failed; will retry");
                return true;
            },
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };

    use super::*;

    struct FakeBridge {
        queue: Mutex<VecDeque<u64>>,
        reverts: Mutex<BTreeMap<u64, [u8; 4]>>,
        stale_next: Mutex<Option<u64>>,
        sent: Mutex<Vec<u64>>,
        handed_over: bool,
    }

    #[async_trait]
    impl NacklPayoutBridge for FakeBridge {
        async fn nackl_state(&self) -> Result<NacklState, RelayerError> {
            let q = self.queue.lock().unwrap();
            let next = self
                .stale_next
                .lock()
                .unwrap()
                .take()
                .or(q.front().copied())
                .unwrap_or(0);
            Ok(NacklState {
                daily_limit: U256::ZERO,
                delay_threshold: U256::ZERO,
                window_used: U256::ZERO,
                accepted: U256::ZERO,
                minted: U256::ZERO,
                pending_count: U256::from(q.len()),
                payout_count: U256::from(q.back().copied().unwrap_or(0)),
                handed_over: self.handed_over,
                next_payout: U256::from(next),
            })
        }

        async fn dry_run_execute_nackl_payout(
            &self,
            payout_id: U256,
        ) -> Result<DryRunOutcome, RelayerError> {
            let id: u64 = payout_id.to();
            if self.queue.lock().unwrap().front() != Some(&id) {
                return Ok(DryRunOutcome::WouldRevert {
                    reason: format!(
                        "execution reverted: 0x{}",
                        hex::encode(AckiNackiBridge::NacklPayoutAlreadyExecuted::SELECTOR)
                    ),
                });
            }
            Ok(match self.reverts.lock().unwrap().get(&id) {
                Some(sel) => DryRunOutcome::WouldRevert {
                    reason: format!("execution reverted: 0x{}", hex::encode(sel)),
                },
                None => DryRunOutcome::WouldSucceed,
            })
        }

        async fn execute_nackl_payout(&self, payout_id: U256) -> Result<B256, RelayerError> {
            let id: u64 = payout_id.to();
            self.sent.lock().unwrap().push(id);
            assert_eq!(self.queue.lock().unwrap().pop_front(), Some(id));
            Ok(B256::ZERO)
        }
    }

    fn fake(queue: &[u64], reverts: &[(u64, [u8; 4])]) -> FakeBridge {
        FakeBridge {
            queue: Mutex::new(queue.iter().copied().collect()),
            reverts: Mutex::new(reverts.iter().copied().collect()),
            stale_next: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            handed_over: false,
        }
    }

    #[tokio::test]
    async fn executes_the_queue_in_order() {
        let b = fake(&[3, 4, 7], &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert_eq!(*b.sent.lock().unwrap(), vec![3, 4, 7]);
        assert_eq!(st.executed, 3);
    }

    #[tokio::test]
    async fn a_full_window_stops_the_pass_and_nothing_behind_it_is_sent() {
        let b = fake(&[1, 2, 3], &[(
            2,
            AckiNackiBridge::NacklWindowFull::SELECTOR,
        )]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert_eq!(*b.sent.lock().unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn a_payout_not_due_yet_stops_the_pass() {
        let b = fake(&[1, 2], &[(
            1,
            AckiNackiBridge::NacklPayoutNotDue::SELECTOR,
        )]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_payout_executed_by_someone_else_is_read_again() {
        let b = fake(&[2], &[]);
        *b.stale_next.lock().unwrap() = Some(1);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert_eq!(*b.sent.lock().unwrap(), vec![2]);
    }

    #[tokio::test]
    async fn dry_run_sends_nothing() {
        let b = fake(&[1], &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, true, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn nothing_pending_reads_no_payout() {
        let b = fake(&[], &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_handed_over_minter_reads_no_payout() {
        let mut b = fake(&[1], &[]);
        b.handed_over = true;
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_paused_bridge_stops_the_pass_without_backoff() {
        let b = fake(&[1, 2], &[(1, AckiNackiBridge::BridgePaused::SELECTOR)]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_revert_backs_off() {
        let b = fake(&[1], &[(1, [0xde, 0xad, 0xbe, 0xef])]);
        let mut st = NacklPayoutScanState::default();
        assert!(nackl_payout_scan_once(&b, false, &mut st).await);
    }

    #[tokio::test]
    async fn a_pass_is_bounded() {
        let ids: Vec<u64> = (1..=100).collect();
        let b = fake(&ids, &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert_eq!(st.executed, MAX_PAYOUTS_PER_PASS as u64);
    }
}
