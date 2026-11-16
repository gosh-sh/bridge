
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
    pub first_open: u64,
    pub executed: u64,
}

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
    let state = match bridge.nackl_state().await {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "nacklState unavailable; skipping the NACKL payout lane");
            return false;
        },
    };
    if state.handed_over || state.pending_count.is_zero() {
        return false;
    }
    let count: u64 = state.payout_count.try_into().unwrap_or(u64::MAX);
    st.first_open = st.first_open.max(1);
    let mut id = st.first_open;
    while id <= count {
        let payout_id = U256::from(id);
        match bridge.dry_run_execute_nackl_payout(payout_id).await {
            Ok(DryRunOutcome::WouldSucceed) if dry_run => {
                info!(%payout_id, "dry-run: executeNacklPayout would succeed");
            },
            Ok(DryRunOutcome::WouldSucceed) => match bridge.execute_nackl_payout(payout_id).await {
                Ok(tx_hash) => {
                    info!(%payout_id, ?tx_hash, "NACKL payout EXECUTED");
                    st.executed += 1;
                    if id == st.first_open {
                        st.first_open = id + 1;
                    }
                },
                Err(e) => {
                    warn!(%payout_id, error = %e, "executeNacklPayout failed; will retry");
                    return true;
                },
            },
            Ok(DryRunOutcome::WouldRevert {
                reason,
            }) => {
                if reverted_with(&reason, A::NacklPayoutAlreadyExecuted::SELECTOR) {
                    if id == st.first_open {
                        st.first_open = id + 1;
                    }
                } else if reverted_with(&reason, A::NacklPayoutNotDue::SELECTOR)
                    || reverted_with(&reason, A::NacklWindowFull::SELECTOR)
                {
                    debug!(%payout_id, %reason, "NACKL payout not executable yet");
                } else if reverted_with(&reason, A::NacklPayoutUnknown::SELECTOR) {
                    break;
                } else if reverted_with(&reason, A::BridgePaused::SELECTOR)
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
        id += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use super::*;

    struct FakeBridge {
        state: NacklState,
        reverts: Mutex<BTreeMap<u64, Option<[u8; 4]>>>,
        sent: Mutex<Vec<u64>>,
    }

    fn state(pending: u64, count: u64) -> NacklState {
        NacklState {
            daily_limit: U256::ZERO,
            delay_threshold: U256::ZERO,
            window_used: U256::ZERO,
            accepted: U256::ZERO,
            minted: U256::ZERO,
            pending_count: U256::from(pending),
            payout_count: U256::from(count),
            handed_over: false,
        }
    }

    #[async_trait]
    impl NacklPayoutBridge for FakeBridge {
        async fn nackl_state(&self) -> Result<NacklState, RelayerError> {
            Ok(self.state)
        }

        async fn dry_run_execute_nackl_payout(
            &self,
            payout_id: U256,
        ) -> Result<DryRunOutcome, RelayerError> {
            let id: u64 = payout_id.to();
            Ok(match self.reverts.lock().unwrap().get(&id) {
                Some(Some(sel)) => DryRunOutcome::WouldRevert {
                    reason: format!("execution reverted: 0x{}", hex::encode(sel)),
                },
                Some(None) => DryRunOutcome::WouldRevert {
                    reason: format!(
                        "execution reverted: 0x{}",
                        hex::encode(AckiNackiBridge::NacklPayoutAlreadyExecuted::SELECTOR)
                    ),
                },
                None => DryRunOutcome::WouldSucceed,
            })
        }

        async fn execute_nackl_payout(&self, payout_id: U256) -> Result<B256, RelayerError> {
            let id: u64 = payout_id.to();
            self.sent.lock().unwrap().push(id);
            self.reverts.lock().unwrap().insert(id, None);
            Ok(B256::ZERO)
        }
    }

    fn fake(pending: u64, count: u64, reverts: &[(u64, Option<[u8; 4]>)]) -> FakeBridge {
        FakeBridge {
            state: state(pending, count),
            reverts: Mutex::new(reverts.iter().copied().collect()),
            sent: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn executes_due_payouts_and_skips_the_rest() {
        let b = fake(2, 4, &[
            (1, None),
            (2, Some(AckiNackiBridge::NacklPayoutNotDue::SELECTOR)),
            (4, Some(AckiNackiBridge::NacklWindowFull::SELECTOR)),
        ]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert_eq!(*b.sent.lock().unwrap(), vec![3]);
        assert_eq!(st.executed, 1);
        assert_eq!(st.first_open, 2);
    }

    #[tokio::test]
    async fn dry_run_sends_nothing() {
        let b = fake(1, 1, &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, true, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn nothing_pending_reads_no_payout() {
        let b = fake(0, 3, &[]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_paused_bridge_stops_the_pass_without_backoff() {
        let b = fake(1, 2, &[(1, Some(AckiNackiBridge::BridgePaused::SELECTOR))]);
        let mut st = NacklPayoutScanState::default();
        assert!(!nackl_payout_scan_once(&b, false, &mut st).await);
        assert!(b.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_revert_backs_off() {
        let b = fake(1, 1, &[(1, Some([0xde, 0xad, 0xbe, 0xef]))]);
        let mut st = NacklPayoutScanState::default();
        assert!(nackl_payout_scan_once(&b, false, &mut st).await);
    }
}
