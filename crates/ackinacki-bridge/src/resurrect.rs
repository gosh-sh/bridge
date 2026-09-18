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
//! 1. **[`wait_for_coverage`]** — poll the contract until the covering bundle
//!    for the just-fired burn lands (i.e. `storedLastSeenBlockSeqNo >=` the
//!    strictly-next multiple of the stride). Returns the resurrected
//!    `BridgeState` snapshot at the moment coverage was observed. This is the
//!    state the enricher needs.
//! 2. **[`covering_bundle_seq_no`]** — the pure math: the strictly-next
//!    multiple of the anchoring stride. A burn that lands exactly on a boundary
//!    is covered by the *next* bundle — the root sitting at that boundary
//!    belongs to the previous batch. Same rule as
//!    `real_chain_builder::l1_anchor_boundaries`. Exposed for tests and for the
//!    orchestrator to log the target.
//!
//! **Anchoring assumption.** L1 stride is 1024 seq_nos (`W·P`); L2 stride
//! is 16 384 seq_nos (`W²`). `AnchorLayerMode::Auto` picks L1 here — if
//! the deploy is L2-anchored, an operator running the CLI in `auto` mode
//! will wait forever (L1 windows never fill). Explicit `--anchor-layer 2`
//! is required against L2 deploys.

use std::time::{Duration, Instant};

use alloy::{network::Network, providers::Provider};
use anyhow::{Context, Result};
use bridge_event_witness::AnchorLayerMode;
use bridge_prover_lib::{bridge_state::BridgeState, AnchorMode};
use bridge_relayer_daemon::bridge::{EthBridgeClient, HISTORY_PROOF_WINDOW};
use tracing::info;

/// Resolve the anchoring stride the CLI should wait against. See module
/// docstring for the Auto-vs-Explicit-2 caveat.
pub fn stride_for(anchor: AnchorLayerMode) -> u64 {
    match anchor {
        // A floor, not a claim about the layer the proof will use. Under `Auto`
        // the layer is chosen per event, after this wait, by
        // `resolve_anchor_layer` — and bounded there by
        // `bridge_state.num_active_layers()`, so it never names a layer whose
        // on-chain window is empty. That bound is the whole difference from an
        // explicit `3`, which returns before the check (`enrich.rs:388`) and can
        // stamp a layer nothing publishes; hence the CLI refusal. Waiting on the
        // shortest stride is the safe side of wrong: the boundary always
        // arrives, and an escalated anchor is already in place when it does
        // (T_n ≤ e + W^n, far inside its window).
        AnchorLayerMode::Auto => AnchorMode::L1.stride(),
        AnchorLayerMode::Explicit(1) => AnchorMode::L1.stride(),
        AnchorLayerMode::Explicit(2) => AnchorMode::L2.stride(),
        // `ackinacki-bridge`'s CLI refuses `> 2`; `bridge-relayer-daemon`
        // accepts any `n ≥ 1`. Safety does not depend on this arm: the daemon
        // never calls `stride_for`, and an explicit 3 dies in enrich at
        // `slot_for_event_height`. Kept total, and L1 for the reason above.
        AnchorLayerMode::Explicit(_) => AnchorMode::L1.stride(),
    }
}

/// Numeric anchor level for `BridgeState::from_contract`. The state
/// mirror stamps this into `BridgeState.anchor_level`; the contract
/// does not persist it, so callers pick the level they'll run the
/// enricher at. `Auto` maps to 1 (the enricher escalates internally).
///
/// So on an `Auto` run this field is a floor, and a proof may well be anchored
/// higher than the 1 recorded here. Nothing reads it back to size a window —
/// treat it as provenance, not as the layer in force.
pub fn anchor_level_for(anchor: AnchorLayerMode) -> u8 {
    match anchor {
        AnchorLayerMode::Auto => 1,
        AnchorLayerMode::Explicit(n) => n.max(1),
    }
}

/// The covering bundle is the strictly-next multiple of `stride`.
/// `l1_anchor_boundaries` uses the same rule: for an event at seq 1024
/// (`W·P`), `K = 2048`, because the root at key block 1024 covers the
/// *previous* batch. Rounding "at or after" waits one bundle too early
/// and `resolve_anchor_layer` then probes a `K` the chain may not have
/// produced yet.
pub fn covering_bundle_seq_no(burn_seq_no: u64, stride: u64) -> u64 {
    debug_assert!(stride > 0);
    if burn_seq_no == 0 {
        return 0;
    }
    (burn_seq_no / stride) * stride + stride
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
                "BridgeState::from_contract failed — on-chain layer window shape does not match \
                 HISTORY_PROOF_WINDOW (128)",
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
        assert_eq!(covering_bundle_seq_no(1024, s), 2048);
        assert_eq!(covering_bundle_seq_no(1025, s), 2048);
        let (_, k, _) = bridge_prover_lib::real_chain_builder::l1_anchor_boundaries(1024, 128, 8);
        assert_eq!(covering_bundle_seq_no(1024, s), k);
    }

    #[test]
    fn covering_bundle_zero_is_zero() {
        assert_eq!(
            covering_bundle_seq_no(0, bridge_prover_lib::BUNDLE_STRIDE_L1),
            0
        );
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
