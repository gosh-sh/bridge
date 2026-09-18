//! Thinned-key-block seqno arithmetic.
//!
//! The daemon advances in steps of `W * P` blocks: it proves only every
//! `P`-th master key block, with each Circuit 2 bundle internally chaining
//! `P` consecutive layer-1 windows via `verify_chain_of_dense_proofs`. The
//! bootstrap seed sits on a `W*P`-aligned key block; from there the first
//! thinned target is `seed + W*P`, then `seed + 2*W*P`, etc.
//!
//! Extracted verbatim from `bridge-prover-daemon/src/main.rs` so the same
//! computation is shared by our daemon and any external consumer (e.g.
//! Sergey's `bridge-relayer-daemon`) driving the library through
//! [`crate::live_driver::LiveProverDriver`].

/// Find the next bundle boundary strictly greater than `last_key_seqno`
/// whose height has already been produced (`<= latest_seqno`). Returns
/// `None` when the chain has not yet reached the next boundary.
///
/// The `stride` parameter is level-dependent:
/// * L1 mode: `stride = W * P` (e.g. 128 * 8 = 1024)
/// * L2 mode: `stride = W²`    (e.g. 128 * 128 = 16 384)
///
/// Works uniformly for bootstrap (`last_key_seqno == stride * 0 + …`, first
/// bundle at `stride`) and steady state (`last == k * stride`, next bundle
/// at `(k+1) * stride`). Requires `stride > 0`.
pub fn find_next_bundle_boundary(
    last_key_seqno: u64,
    latest_seqno: u64,
    stride: u64,
) -> Option<u64> {
    debug_assert!(stride > 0, "stride must be > 0");
    let next = ((last_key_seqno / stride) + 1) * stride;
    if next <= latest_seqno {
        Some(next)
    } else {
        None
    }
}

/// L1-mode shim over [`find_next_bundle_boundary`], preserved so external
/// call sites and pre-L2 tests keep compiling untouched. New code should
/// prefer the general form.
///
/// Semantics identical to the pre-L2 implementation: `stride = window_size *
/// thinning_factor`.
pub fn find_next_thinned_key_block(
    last_key_seqno: u64,
    latest_seqno: u64,
    window_size: u64,
    thinning_factor: u64,
) -> Option<u64> {
    find_next_bundle_boundary(last_key_seqno, latest_seqno, window_size * thinning_factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    // W = 128, P = 4 → step = 512. A smaller stride than production
    // (`THINNING_FACTOR_P` is 8) so the boundary arithmetic stays readable.
    const W: u64 = 128;
    const P: u64 = 4;
    const STEP: u64 = W * P;

    #[test]
    fn returns_none_when_chain_head_below_next_boundary() {
        // Steady state after last=512, chain head at 900 (below 1024).
        assert_eq!(find_next_thinned_key_block(STEP, STEP + 388, W, P), None);
    }

    #[test]
    fn returns_next_boundary_from_steady_state() {
        // last=512, head=1024 → next=1024.
        assert_eq!(find_next_thinned_key_block(STEP, 2 * STEP, W, P), Some(2 * STEP));
    }

    #[test]
    fn skips_boundaries_below_last_key_seqno() {
        // After a large gap: last=2048, head=4096 → next=2560 (not 512).
        assert_eq!(
            find_next_thinned_key_block(4 * STEP, 8 * STEP, W, P),
            Some(5 * STEP),
        );
    }

    #[test]
    fn bootstrap_last_equals_window_only_picks_first_step() {
        // Bootstrap edge case: last=W (not W*P) — represents the seed key
        // block. Next target must be at STEP, not at W.
        assert_eq!(find_next_thinned_key_block(W, STEP, W, P), Some(STEP));
        assert_eq!(find_next_thinned_key_block(W, STEP - 1, W, P), None);
    }

    #[test]
    fn returns_smallest_boundary_when_head_far_ahead() {
        // last=512, head=100000 → next=1024, NOT the biggest ≤head. Guarantees
        // in-order emission (matters for consumer state advancement).
        assert_eq!(find_next_thinned_key_block(STEP, 100_000, W, P), Some(2 * STEP));
    }

    // ---- L2-stride coverage over the general form -------------------------

    /// L2 stride at production W=128 → 16 384. All L2 tests use the production
    /// value to match the constants in `crate::BUNDLE_STRIDE_L2`.
    const L2_W: u64 = 128;
    const L2_STRIDE: u64 = L2_W * L2_W;

    #[test]
    fn l2_returns_next_boundary_from_steady_state() {
        // last=16384, head=32768 → next=32768.
        assert_eq!(
            find_next_bundle_boundary(L2_STRIDE, 2 * L2_STRIDE, L2_STRIDE),
            Some(2 * L2_STRIDE),
        );
    }

    #[test]
    fn l2_returns_none_when_below_next_boundary() {
        // last=16384, head=32767 (one below 32768) → None.
        assert_eq!(
            find_next_bundle_boundary(L2_STRIDE, 2 * L2_STRIDE - 1, L2_STRIDE),
            None,
        );
    }

    #[test]
    fn l2_skips_boundaries_far_below_last() {
        // last=65536 (4·stride), head=131072 → next=81920 (5·stride), NOT 16384.
        assert_eq!(
            find_next_bundle_boundary(4 * L2_STRIDE, 8 * L2_STRIDE, L2_STRIDE),
            Some(5 * L2_STRIDE),
        );
    }

    #[test]
    fn l2_bootstrap_from_seed_aligned_last() {
        // Under L2, the seed sits at a W²-aligned block. First target is
        // stride*2 relative to the "last = stride" bootstrap convention.
        // Semantically: last = k*stride ⇒ next = (k+1)*stride.
        assert_eq!(
            find_next_bundle_boundary(L2_STRIDE, 2 * L2_STRIDE, L2_STRIDE),
            Some(2 * L2_STRIDE),
        );
    }

    #[test]
    fn shim_matches_general_form() {
        // The L1 shim must be byte-identical to the general form at the
        // L1 stride.
        for &(last, head) in &[(0, 1024), (512, 2048), (1024, 100_000), (99, 1024)] {
            assert_eq!(
                find_next_thinned_key_block(last, head, W, P),
                find_next_bundle_boundary(last, head, W * P),
            );
        }
    }
}
