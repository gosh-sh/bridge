//! Contract ↔ driver global-history consistency checks (Alina plan §5.4).
//!
//! Check A — after each successful ack, the driver's `BridgeState` must match
//! what the ETH bridge just stored.
//! Check B — across ticks, on-chain anchors must not rewind or jump without
//! an explained BK-update.

use alloy::primitives::U256;
use bridge_prover_lib::bridge_state::BridgeState;

use crate::bridge::BridgeOnChainState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDrift {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
}

impl std::fmt::Display for HistoryDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: expected={}, actual={}",
            self.field, self.expected, self.actual
        )
    }
}

fn drift(
    field: &'static str,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> HistoryDrift {
    HistoryDrift {
        field,
        expected: expected.into(),
        actual: actual.into(),
    }
}

/// Check A — driver post-ack state vs freshly-read on-chain anchors.
///
/// Storage v2.0 (2026-08-04): the per-layer top-hash cross-check moved to
/// [`EthBridgeClient::submit_block`], which asserts on-chain
/// `expectedPrevAnchor(numLayers) == layerHashes[numLayers-1]` right after
/// the receipt lands. That is a *strictly stronger* invariant than the old
/// flat `storedPrevMaxLevelLayerHash` compare — and it uses the same block
/// pin (`receipt.block_number`) that avoids read-after-write RPC lag. This
/// helper therefore no longer touches
/// [`BridgeOnChainState::prev_max_level_layer_hash`] (which is now the
/// immutable genesis seed, not the runtime anchor).
/// `bk` is the set the prover must hold: `Some((commitment, last_bk))`
/// with `last_bk = None` during the hold window (only the outgoing
/// commitment is known on-chain). `None` skips the BK comparison.
pub fn check_history_consistency(
    expected: &BridgeState,
    actual: &BridgeOnChainState,
    bk: Option<(U256, Option<u64>)>,
) -> Result<(), HistoryDrift> {
    if expected.stored_last_seen_block_seq_no != actual.last_seen_block_seq_no {
        return Err(drift(
            "last_seen_block_seq_no",
            expected.stored_last_seen_block_seq_no.to_string(),
            actual.last_seen_block_seq_no.to_string(),
        ));
    }
    let Some((want_bk, want_last_bk)) = bk else {
        return Ok(());
    };
    let want_bk = want_bk.to_le_bytes::<32>();
    if expected.stored_bk_set_commitment != want_bk {
        return Err(drift(
            "bk_set_commitment",
            hex::encode(want_bk),
            hex::encode(expected.stored_bk_set_commitment),
        ));
    }
    if let Some(n) = want_last_bk {
        if expected.stored_last_bk_set_update_seq_no != n {
            return Err(drift(
                "last_bk_set_update_seq_no",
                n.to_string(),
                expected.stored_last_bk_set_update_seq_no.to_string(),
            ));
        }
    }
    Ok(())
}

/// Check B — monotonicity / unexpected actor detection vs last remembered
/// on-chain snapshot. Returns `Ok(())` when there is no prior memory.
pub fn check_chain_monotonicity(
    remembered: &BridgeOnChainState,
    actual: &BridgeOnChainState,
    // Max gap (in seq_no) another actor may advance between our ticks
    // before we halt. Caller derives it from the bundle stride (W·P = 1024 at
    // W=128, P=8); see `MAX_FORWARD_GAP`.
    max_forward_gap: u64,
) -> Result<(), HistoryDrift> {
    if actual.last_seen_block_seq_no < remembered.last_seen_block_seq_no {
        return Err(drift(
            "last_seen_block_seq_no_rewound",
            remembered.last_seen_block_seq_no.to_string(),
            actual.last_seen_block_seq_no.to_string(),
        ));
    }
    let gap = actual
        .last_seen_block_seq_no
        .saturating_sub(remembered.last_seen_block_seq_no);
    if gap > max_forward_gap {
        return Err(drift(
            "last_seen_block_seq_no_jumped",
            format!(
                "{} (gap≤{max_forward_gap})",
                remembered.last_seen_block_seq_no
            ),
            format!("{} (gap={gap})", actual.last_seen_block_seq_no),
        ));
    }
    if actual.bk_set_commitment != remembered.bk_set_commitment
        && actual.last_bk_set_update_seq_no == remembered.last_bk_set_update_seq_no
    {
        return Err(drift(
            "bk_set_commitment_without_update",
            format!("{:#x}", remembered.bk_set_commitment),
            format!("{:#x}", actual.bk_set_commitment),
        ));
    }
    Ok(())
}

/// Startup audit: remembered vs live chain must match exactly (no auto-heal).
pub fn check_startup_drift(
    remembered: &BridgeOnChainState,
    actual: &BridgeOnChainState,
) -> Result<(), HistoryDrift> {
    if remembered != actual {
        return Err(drift(
            "startup_on_chain_drift",
            format!("{remembered:?}"),
            format!("{actual:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy::primitives::U256;
    use bridge_prover_lib::bridge_state::BridgeState;

    use super::*;

    #[test]
    fn consistency_ok_on_matching_zeros() {
        let expected = BridgeState::new(128);
        let actual = BridgeOnChainState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: U256::ZERO,
            prev_bk_set_commitment: U256::ZERO,
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: 0,
        };
        assert!(check_history_consistency(&expected, &actual, Some((U256::ZERO, Some(0)))).is_ok());
    }

    #[test]
    fn monotonicity_detects_rewind() {
        let remembered = BridgeOnChainState {
            last_seen_block_seq_no: 10,
            bk_set_commitment: U256::from(1u64),
            prev_bk_set_commitment: U256::ZERO,
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: 0,
        };
        let actual = BridgeOnChainState {
            last_seen_block_seq_no: 9,
            ..remembered.clone()
        };
        let err = check_chain_monotonicity(&remembered, &actual, 2048).unwrap_err();
        assert_eq!(err.field, "last_seen_block_seq_no_rewound");
    }
}
