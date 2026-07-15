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

/// Find the next thinned key block strictly greater than `last_key_seqno`
/// whose height has already been produced (`<= latest_seqno`). Returns
/// `None` when the chain has not yet reached the next W·P boundary.
///
/// Works uniformly for bootstrap (`last == W`, first bundle at `W*P`) and
/// steady state (`last == k * W * P`, next bundle at `(k+1) * W * P`).
pub fn find_next_thinned_key_block(
    last_key_seqno: u64,
    latest_seqno: u64,
    window_size: u64,
    thinning_factor: u64,
) -> Option<u64> {
    let step = window_size * thinning_factor;
    let next = ((last_key_seqno / step) + 1) * step;
    if next <= latest_seqno {
        Some(next)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // W = 128, P = 4 → step = 512 (mirrors the production configuration).
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
}
