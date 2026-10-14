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

impl HistoryDrift {
    /// `true` for drift signals that usually mean the RPC endpoint we read
    /// from is lagging behind our most recent write (classic read-after-write
    /// staleness on load-balanced public endpoints — publicnode's balancer
    /// has observable 3–30 s backend-propagation lag). The reads are at
    /// `latest`, not at a finalized block, so a `_rewound` signal can also be
    /// a reorg that dropped our transaction; the tick loop therefore retries
    /// it only for a bounded time ([`crate::Relayer`] reports it as drift once
    /// it outlasts the lag window).
    ///
    /// Jumps, orphan commitments and uncovered rotations are NOT transient
    /// and keep their fatal semantics — a stale backend serves an older
    /// snapshot, not a different timeline.
    pub fn is_transient_rpc_lag(&self) -> bool {
        matches!(
            self.field,
            "last_seen_block_seq_no_rewound" | "last_bk_set_update_seq_no_rewound"
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
///
/// Its last check predates the bundle-aware `applyBkSetUpdate` gate: it
/// rejects a second rotation while the previous N is ahead of `last_seen`,
/// which the contract allows whenever the next bundle boundary is above
/// that N. The relayer uses [`check_chain_monotonicity_with_stride`].
pub fn check_chain_monotonicity(
    remembered: &BridgeOnChainState,
    actual: &BridgeOnChainState,
    // Max gap (in seq_no) either cursor may advance between the snapshot and
    // a fresh read before we halt; the relayer's own check bounds only
    // `last_seen`, by four bundles (`Relayer::max_forward_gap`).
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
    // Rotation cursor. First apply may lead `last_seen`; a second apply
    // while that N is still uncovered is rejected here (the pre-bundle-aware
    // rule — see the doc comment).
    if actual.last_bk_set_update_seq_no < remembered.last_bk_set_update_seq_no {
        return Err(drift(
            "last_bk_set_update_seq_no_rewound",
            remembered.last_bk_set_update_seq_no.to_string(),
            actual.last_bk_set_update_seq_no.to_string(),
        ));
    }
    let bk_gap = actual
        .last_bk_set_update_seq_no
        .saturating_sub(remembered.last_bk_set_update_seq_no);
    if bk_gap > max_forward_gap {
        return Err(drift(
            "last_bk_set_update_seq_no_jumped",
            format!(
                "{} (gap≤{max_forward_gap})",
                remembered.last_bk_set_update_seq_no
            ),
            format!("{} (gap={bk_gap})", actual.last_bk_set_update_seq_no),
        ));
    }
    if actual.last_bk_set_update_seq_no > remembered.last_bk_set_update_seq_no
        && remembered.last_bk_set_update_seq_no > actual.last_seen_block_seq_no
    {
        return Err(drift(
            "last_bk_set_update_seq_no_uncovered",
            format!(
                "previous N={} still ahead of last_seen={}",
                remembered.last_bk_set_update_seq_no, actual.last_seen_block_seq_no
            ),
            actual.last_bk_set_update_seq_no.to_string(),
        ));
    }
    Ok(())
}

/// The bundle boundary `AckiNackiBridge.applyBkSetUpdate` treats as owed:
/// the next multiple of `bundle_stride` strictly above `last_seen`.
fn next_owed_boundary(last_seen: u64, bundle_stride: u64) -> u64 {
    let stride = bundle_stride.max(1);
    ((last_seen / stride) + 1) * stride
}

/// Check B as the relayer runs it, with the contract's own rotation gate.
///
/// - `last_seen` never moves back (`_rewound`, transient on a lagging RPC) and
///   moves at most `max_forward_gap` between the snapshot and a fresh read
///   (`_jumped`). A `verifyBlock` moves it one bundle, so one that landed
///   without its receipt leaves the snapshot one bundle behind.
/// - The commitment changes only with a rotation.
/// - The rotation cursor never moves back, and moves forward any distance:
///   rotations follow Acki Nacki's epochs, not bundles, so a rotation that
///   landed without its receipt can be far past the remembered one (the first
///   one after a deploy with a genesis cursor of 0 is the whole chain height
///   away). A forward move is drift only when the contract could not have
///   accepted it. `applyBkSetUpdate` reverts `PrevSlotStillOwedForBundle` iff
///   the previous N is non-zero and the next bundle boundary after the cursor
///   is at or below it. The cursor only moves forward, so the first rotation
///   newer than the remembered N was accepted at a cursor at or below
///   `actual.last_seen`; it is contract-impossible only when even
///   `actual.last_seen` leaves the next boundary at or below the remembered N.
///   A burst of rotations ahead of the cursor is therefore accepted, as the
///   contract accepts it.
pub fn check_chain_monotonicity_with_stride(
    remembered: &BridgeOnChainState,
    actual: &BridgeOnChainState,
    max_forward_gap: u64,
    bundle_stride: u64,
) -> Result<(), HistoryDrift> {
    if actual.last_seen_block_seq_no < remembered.last_seen_block_seq_no {
        return Err(drift(
            "last_seen_block_seq_no_rewound",
            remembered.last_seen_block_seq_no.to_string(),
            actual.last_seen_block_seq_no.to_string(),
        ));
    }
    let gap = actual.last_seen_block_seq_no - remembered.last_seen_block_seq_no;
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
    if actual.last_bk_set_update_seq_no < remembered.last_bk_set_update_seq_no {
        return Err(drift(
            "last_bk_set_update_seq_no_rewound",
            remembered.last_bk_set_update_seq_no.to_string(),
            actual.last_bk_set_update_seq_no.to_string(),
        ));
    }
    if actual.last_bk_set_update_seq_no > remembered.last_bk_set_update_seq_no {
        let next = next_owed_boundary(actual.last_seen_block_seq_no, bundle_stride);
        if next <= remembered.last_bk_set_update_seq_no {
            return Err(drift(
                "last_bk_set_update_seq_no_uncovered",
                format!(
                    "previous N={} at or past the next owed boundary {next} (last_seen={}, \
                     bundle_stride={bundle_stride})",
                    remembered.last_bk_set_update_seq_no, actual.last_seen_block_seq_no
                ),
                actual.last_bk_set_update_seq_no.to_string(),
            ));
        }
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

    #[test]
    fn monotonicity_allows_first_rotation_ahead_of_cursor() {
        let remembered = BridgeOnChainState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: U256::from(1u64),
            prev_bk_set_commitment: U256::ZERO,
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: 0,
        };
        let actual = BridgeOnChainState {
            bk_set_commitment: U256::from(2u64),
            prev_bk_set_commitment: U256::from(1u64),
            last_bk_set_update_seq_no: 500,
            ..remembered.clone()
        };
        assert!(check_chain_monotonicity(&remembered, &actual, 2048).is_ok());
    }

    #[test]
    fn is_transient_rpc_lag_classifies_rewinds_only() {
        // Both rewind signals must be classified as transient: a backend
        // behind the load balancer served us a pre-write snapshot.
        let seen_rewound = drift("last_seen_block_seq_no_rewound", "10", "9");
        assert!(seen_rewound.is_transient_rpc_lag());
        let bk_rewound = drift("last_bk_set_update_seq_no_rewound", "500", "0");
        assert!(bk_rewound.is_transient_rpc_lag());

        // Jumps, orphan commitments and uncovered rotations are structural
        // and must remain fatal — stale RPC alone cannot produce them.
        for field in [
            "last_seen_block_seq_no_jumped",
            "bk_set_commitment_without_update",
            "last_bk_set_update_seq_no_jumped",
            "last_bk_set_update_seq_no_uncovered",
            "startup_on_chain_drift",
        ] {
            assert!(
                !drift(field, "a", "b").is_transient_rpc_lag(),
                "{field} must not be reclassified as transient"
            );
        }
    }

    #[test]
    fn monotonicity_rejects_second_rotation_while_previous_uncovered() {
        let remembered = BridgeOnChainState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: U256::from(2u64),
            prev_bk_set_commitment: U256::from(1u64),
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: 500,
        };
        let actual = BridgeOnChainState {
            bk_set_commitment: U256::from(3u64),
            prev_bk_set_commitment: U256::from(2u64),
            last_bk_set_update_seq_no: 2000,
            ..remembered.clone()
        };
        let err = check_chain_monotonicity(&remembered, &actual, 2048).unwrap_err();
        assert_eq!(err.field, "last_bk_set_update_seq_no_uncovered");
    }

    fn snapshot(last_seen: u64, last_bk: u64, bk: u64) -> BridgeOnChainState {
        BridgeOnChainState {
            last_seen_block_seq_no: last_seen,
            bk_set_commitment: U256::from(bk),
            prev_bk_set_commitment: U256::from(bk.saturating_sub(1)),
            prev_max_level_layer_hash: U256::ZERO,
            last_bk_set_update_seq_no: last_bk,
        }
    }

    #[test]
    fn strided_check_accepts_a_burst_ahead_of_the_cursor() {
        // The shellnet shape: rotations at 500 and 2000 with the cursor at 0
        // and an L1 stride. The contract applied 2000 because the next owed
        // boundary, 1024, is above 500 — the old check calls it uncovered.
        let remembered = snapshot(0, 500, 2);
        let actual = snapshot(0, 2000, 3);
        assert_eq!(
            check_chain_monotonicity(&remembered, &actual, 4096)
                .unwrap_err()
                .field,
            "last_bk_set_update_seq_no_uncovered"
        );
        assert!(check_chain_monotonicity_with_stride(&remembered, &actual, 4096, 1024).is_ok());
    }

    #[test]
    fn strided_check_rejects_what_the_contract_rejects() {
        // Previous N = 1500 is past the next owed boundary 1024, so the
        // contract reverts any further rotation until verifyBlock(1024).
        let remembered = snapshot(0, 1500, 2);
        let actual = snapshot(0, 2000, 3);
        let err =
            check_chain_monotonicity_with_stride(&remembered, &actual, 4096, 1024).unwrap_err();
        assert_eq!(err.field, "last_bk_set_update_seq_no_uncovered");
        // Once the cursor is at 1024 the same pair is legal.
        let actual = snapshot(1024, 2000, 3);
        assert!(check_chain_monotonicity_with_stride(&remembered, &actual, 4096, 1024).is_ok());
    }

    #[test]
    fn strided_check_keeps_every_other_drift() {
        let remembered = snapshot(16_384, 16_000, 2);
        // A verifyBlock we did not see: one L2 bundle forward is fine with an
        // L2-sized gap, five bundles is still a jump.
        let one_bundle = snapshot(32_768, 16_000, 2);
        assert!(
            check_chain_monotonicity_with_stride(&remembered, &one_bundle, 4 * 16_384, 16_384)
                .is_ok()
        );
        let five_bundles = snapshot(16_384 * 6, 16_000, 2);
        assert_eq!(
            check_chain_monotonicity_with_stride(&remembered, &five_bundles, 4 * 16_384, 16_384)
                .unwrap_err()
                .field,
            "last_seen_block_seq_no_jumped"
        );
        let rewound = snapshot(0, 16_000, 2);
        assert_eq!(
            check_chain_monotonicity_with_stride(&remembered, &rewound, 4 * 16_384, 16_384)
                .unwrap_err()
                .field,
            "last_seen_block_seq_no_rewound"
        );
        let orphan = snapshot(16_384, 16_000, 7);
        assert_eq!(
            check_chain_monotonicity_with_stride(&remembered, &orphan, 4 * 16_384, 16_384)
                .unwrap_err()
                .field,
            "bk_set_commitment_without_update"
        );
    }

    #[test]
    fn strided_check_accepts_a_rotation_far_past_the_remembered_one() {
        // A rotation that landed without its receipt is one rotation ahead of
        // the snapshot, and rotations follow AN epochs, not bundles: after a
        // deploy with a genesis cursor of 0 the first one is the chain height
        // away. The old check calls anything past four bundles a jump, and the
        // relayer then failed every tick and every restart.
        let remembered = snapshot(23_887_872, 0, 2);
        let actual = snapshot(23_887_872, 23_897_426, 3);
        assert_eq!(
            check_chain_monotonicity(&remembered, &actual, 4 * 16_384)
                .unwrap_err()
                .field,
            "last_bk_set_update_seq_no_jumped"
        );
        assert!(
            check_chain_monotonicity_with_stride(&remembered, &actual, 4 * 16_384, 16_384).is_ok()
        );

        // Two rotations a day apart, on a network that rotates daily.
        let remembered = snapshot(23_887_872, 23_886_566, 2);
        let next_day = snapshot(23_887_872, 23_886_566 + 259_200, 3);
        assert!(
            check_chain_monotonicity_with_stride(&remembered, &next_day, 4 * 16_384, 16_384)
                .is_ok()
        );

        // Backwards is still drift.
        let rewound = snapshot(23_887_872, 23_886_000, 3);
        assert_eq!(
            check_chain_monotonicity_with_stride(&remembered, &rewound, 4 * 16_384, 16_384)
                .unwrap_err()
                .field,
            "last_bk_set_update_seq_no_rewound"
        );
    }
}
