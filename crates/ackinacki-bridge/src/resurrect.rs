//! Resurrect `BridgeState` from the on-chain `AckiNackiBridge` contract.
//!
//! The third-party CLI has no local `prover_state.json` — the daemon that
//! feeds `verifyBlock` is running on some other host. Everything the
//! enricher needs (per-layer rolling windows + the four scalar fields)
//! lives in contract storage though, so we can rebuild `BridgeState`
//! byte-for-byte via
//! [`EthBridgeClient::read_full_state`] + [`BridgeState::from_contract`].
//!
//! Two responsibilities:
//! 1. **[`wait_for_coverage`]** — poll the contract until the covering
//!    bundle for the just-fired burn lands (i.e.
//!    `storedLastSeenBlockSeqNo >= ceil(burn_seq_no / stride) * stride`).
//!    Returns the resurrected `BridgeState` snapshot at the moment
//!    coverage was observed. This is the state the enricher needs.
//! 2. **[`covering_bundle_seq_no`]** — the pure math: round the burn
//!    seq_no up to the next multiple of the anchoring stride. Exposed
//!    for tests and for the orchestrator to log the target.
//!
//! **Anchoring assumption.** L1 stride is 1024 seq_nos (`W·P`); L2 stride
//! is 16 384 seq_nos (`W²`). `AnchorLayerMode::Auto` picks L1 here — if
//! the deploy is L2-anchored, an operator running the CLI in `auto` mode
//! will wait forever (L1 windows never fill). Explicit `--anchor-layer 2`
//! is required against L2 deploys.

use std::time::{Duration, Instant};

use alloy::{network::Network, providers::Provider};
use anyhow::{Context, Result};
use tracing::info;

use bridge_event_witness::AnchorLayerMode;
use bridge_prover_lib::bridge_state::BridgeState;
use bridge_prover_lib::AnchorMode;
use bridge_relayer_daemon::bridge::{EthBridgeClient, HISTORY_PROOF_WINDOW};

/// Resolve the anchoring stride the CLI should wait against. See module
/// docstring for the Auto-vs-Explicit-2 caveat.
pub fn stride_for(anchor: AnchorLayerMode) -> u64 {
    match anchor {
        AnchorLayerMode::Auto => AnchorMode::L1.stride(),
        AnchorLayerMode::Explicit(1) => AnchorMode::L1.stride(),
        AnchorLayerMode::Explicit(2) => AnchorMode::L2.stride(),
        // Higher layers are not currently deployed. Fall back to L1 —
        // the enricher will escalate via Auto internally if it needs to.
        AnchorLayerMode::Explicit(_) => AnchorMode::L1.stride(),
    }
}

/// Numeric anchor level for `BridgeState::from_contract`. The state
/// mirror stamps this into `BridgeState.anchor_level`; the contract
/// does not persist it, so callers pick the level they'll run the
/// enricher at. `Auto` maps to 1 (the enricher escalates internally).
pub fn anchor_level_for(anchor: AnchorLayerMode) -> u8 {
    match anchor {
        AnchorLayerMode::Auto => 1,
        AnchorLayerMode::Explicit(n) => n.max(1),
    }
}

/// The next multiple of `stride` at or after `burn_seq_no`. A burn that
/// lands exactly on a stride boundary is already covered by that
/// boundary's bundle (no need to round up).
pub fn covering_bundle_seq_no(burn_seq_no: u64, stride: u64) -> u64 {
    debug_assert!(stride > 0);
    if burn_seq_no == 0 {
        return 0;
    }
    burn_seq_no.div_ceil(stride) * stride
}

/// Poll `AckiNackiBridge` until it has advanced past `target_seq_no`
/// (the covering bundle for the burn), then return the resurrected
/// `BridgeState` snapshot at that moment. Errors on RPC failure or
/// deadline expiry.
///
/// The returned state is byte-for-byte the contract mirror — safe to
/// hand straight to
/// [`bridge_relayer_daemon::withdraw_e2e::run_once_with_state`].
pub async fn wait_for_coverage<P, N>(
    client: &EthBridgeClient<P, N>,
    anchor: AnchorLayerMode,
    target_seq_no: u64,
    poll_interval: Duration,
    total_wait: Duration,
) -> Result<BridgeState>
where
    P: Provider<N> + Clone,
    N: Network,
{
    let deadline = Instant::now() + total_wait;
    let level = anchor_level_for(anchor);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let cfs = client
            .read_full_state()
            .await
            .context("EthBridgeClient::read_full_state")?;
        let observed = cfs.last_seen_block_seq_no;
        info!(
            attempt,
            observed_last_seen = observed,
            target_covering_seq_no = target_seq_no,
            anchor_level = level,
            "polled AckiNackiBridge",
        );
        if observed >= target_seq_no {
            let state = BridgeState::from_contract(cfs, HISTORY_PROOF_WINDOW, level).context(
                "BridgeState::from_contract failed — on-chain layer window shape does not \
                 match HISTORY_PROOF_WINDOW (128)",
            )?;
            info!(
                observed_last_seen = observed,
                stored_last_seen_block_seq_no = state.stored_last_seen_block_seq_no,
                num_active_layers = state.num_active_layers(),
                "coverage reached — resurrected BridgeState from contract",
            );
            return Ok(state);
        }
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "wait_for_coverage: gave up after {attempt} polls — contract at \
                 last_seen={observed}, need >= {target_seq_no} (stride={}, wait_budget={:?})",
                stride_for(anchor),
                total_wait,
            );
        }
        let remaining = deadline.duration_since(now);
        let sleep = poll_interval.min(remaining);
        info!(
            attempt,
            observed_last_seen = observed,
            target_covering_seq_no = target_seq_no,
            sleep_s = sleep.as_secs(),
            remaining_s = remaining.as_secs(),
            "coverage not reached yet — sleeping before next poll",
        );
        tokio::time::sleep(sleep).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covering_bundle_rounds_up_l1() {
        let s = bridge_prover_lib::BUNDLE_STRIDE_L1; // 1024
        assert_eq!(covering_bundle_seq_no(1, s), 1024);
        assert_eq!(covering_bundle_seq_no(1023, s), 1024);
        assert_eq!(covering_bundle_seq_no(1024, s), 1024);
        assert_eq!(covering_bundle_seq_no(1025, s), 2048);
    }

    #[test]
    fn covering_bundle_zero_is_zero() {
        assert_eq!(covering_bundle_seq_no(0, bridge_prover_lib::BUNDLE_STRIDE_L1), 0);
    }

    #[test]
    fn stride_mapping() {
        assert_eq!(stride_for(AnchorLayerMode::Auto), AnchorMode::L1.stride());
        assert_eq!(
            stride_for(AnchorLayerMode::Explicit(1)),
            AnchorMode::L1.stride()
        );
        assert_eq!(
            stride_for(AnchorLayerMode::Explicit(2)),
            AnchorMode::L2.stride()
        );
    }

    #[test]
    fn anchor_level_mapping() {
        assert_eq!(anchor_level_for(AnchorLayerMode::Auto), 1);
        assert_eq!(anchor_level_for(AnchorLayerMode::Explicit(1)), 1);
        assert_eq!(anchor_level_for(AnchorLayerMode::Explicit(2)), 2);
    }
}
