pub mod poseidon_dense;
pub mod keys;
pub mod transcript;
pub mod prover;
pub mod verifier;
pub mod ipc;
pub mod bridge_state;
pub mod prover_bk_set;
pub mod bootstrap;
pub mod bk_set_bootstrap;
pub mod block_id_tree;
pub mod chain_proof_builder;
pub mod real_chain_builder;
pub mod layer_prover;
pub mod live_driver;

// Re-export commonly used types.
pub use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

/// Prover-side thinning factor `P`: the prover only emits a (Circuit 1 + Circuit 2)
/// bundle every `P`-th master key block instead of every key block. See
/// `acki-nacki-to-eth-bridge-halo2-circuits/BRIDGE_PROVER_THINNING_SPEC.md`.
///
/// Hard constraints (checked at runtime by `chain_proof_builder::build_chain_proofs`):
///   * `P <= MAX_CHAIN_LEN = 11` (from `gosh-dense-balanced-tree`)
///   * `P` must divide `W = crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE`
///     so the on-chain `layerWindows[L≥2]` cadence is unchanged.
///
/// Current config: `W = 128`, `P = 8` (bundle stride `W·P = 1024`).
///
/// Re-applied 2026-08-17 as prep for Deploy #7 on Sepolia. Earlier same-day
/// attempt was reverted because live Deploy #6's on-chain
/// `expectedPrevAnchor(1)` was fossilized under P=4 arithmetic and a
/// cold-restart at P=8 would have reverted the first `verifyBlock` with
/// `PrevAnchorMismatch`. There's no in-place P transition on a live bridge —
/// a P bump requires a fresh deploy at the new P. Deploy #7 is a fresh
/// bridge at fresh W·P=1024-aligned anchors, so this constant is now
/// consistent with the on-chain genesis stamp again.
///
/// Cadence at P=8 / shellnet 3 b/s: 1024 seq_nos / bundle → 341 s chain-time
/// vs ~597 s prover-time (warm cache). Prover still ~1.75× slower than
/// chain; not real-time sustainable but survives long enough for a single
/// withdrawal E2E from a fresh-head seed. Spec §1.2 notes P=8 is the
/// recommended setting within the `MAX_CHAIN_LEN=11 ∧ W=128` ceiling;
/// steady-state throughput requires a producer-side W bump or faster HW.
pub const THINNING_FACTOR_P: u64 = 8;

/// Bundle stride under **L1 anchoring** (default mode): `W · P` seq_nos.
/// One (Circuit 1 + Circuit 2) bundle covers `chain_steps = P` horizontal
/// L1 hops of `W` blocks each. At W=128, P=8 → 1024 seq_nos.
pub const BUNDLE_STRIDE_L1: u64 =
    crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64 * THINNING_FACTOR_P;

/// Bundle stride under **L2 anchoring** (opt-in mode): `W²` seq_nos.
/// One bundle covers `chain_steps = 1` horizontal L2 hop between two
/// adjacent L2 key blocks. Circuit 2 runs with `num_layers = 2` and its
/// `layer_hash_frs[1]` slot populated with the L2 root. At W=128 →
/// 16 384 seq_nos → ~91 min chain-time on shellnet.
///
/// See `docs/l2_anchoring_proposal.md` for the rate model and
/// `docs/l2_anchoring_implementation_plan.md` for wiring details.
pub const BUNDLE_STRIDE_L2: u64 =
    (crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE as u64).pow(2);

/// Anchor level selector for the prover pipeline. L1 is the current default;
/// L2 is the opt-in supercritical mode (see [`BUNDLE_STRIDE_L2`]).
///
/// This lives at the top of `bridge-prover-lib` so all downstream crates
/// (`bridge-prover-daemon`, `bridge-relayer-daemon`) share one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AnchorMode {
    /// L1 anchoring: one bundle per `W·P` seq_nos, `chain_steps = P` L1 hops.
    #[default]
    L1,
    /// L2 anchoring: one bundle per `W²` seq_nos, `chain_steps = 1` L2 hop.
    L2,
}

impl AnchorMode {
    /// Bundle stride in seq_nos for this mode.
    pub const fn stride(self) -> u64 {
        match self {
            AnchorMode::L1 => BUNDLE_STRIDE_L1,
            AnchorMode::L2 => BUNDLE_STRIDE_L2,
        }
    }

    /// Numeric level (1 or 2) — used for env/CLI plumbing and Circuit 2's
    /// `num_layers` public instance.
    pub const fn level(self) -> u8 {
        match self {
            AnchorMode::L1 => 1,
            AnchorMode::L2 => 2,
        }
    }

    /// Parse from the `--anchor-level` CLI flag or `BRIDGE_ANCHOR_LEVEL` env.
    pub fn from_level(level: u8) -> Result<Self, String> {
        match level {
            1 => Ok(AnchorMode::L1),
            2 => Ok(AnchorMode::L2),
            other => Err(format!(
                "anchor level {other} not supported (expected 1 or 2)"
            )),
        }
    }
}

#[cfg(test)]
mod anchor_mode_tests {
    use super::*;

    #[test]
    fn strides_match_constants() {
        assert_eq!(AnchorMode::L1.stride(), 1024);
        assert_eq!(AnchorMode::L2.stride(), 16_384);
        assert_eq!(AnchorMode::L1.stride(), BUNDLE_STRIDE_L1);
        assert_eq!(AnchorMode::L2.stride(), BUNDLE_STRIDE_L2);
    }

    #[test]
    fn default_is_l1() {
        assert_eq!(AnchorMode::default(), AnchorMode::L1);
    }

    #[test]
    fn from_level_round_trip() {
        assert_eq!(AnchorMode::from_level(1).unwrap(), AnchorMode::L1);
        assert_eq!(AnchorMode::from_level(2).unwrap(), AnchorMode::L2);
        assert!(AnchorMode::from_level(0).is_err());
        assert!(AnchorMode::from_level(3).is_err());
        assert_eq!(AnchorMode::L1.level(), 1);
        assert_eq!(AnchorMode::L2.level(), 2);
    }
}
