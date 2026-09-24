pub mod paths;
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
/// `crates/bridge-circuits/docs/BRIDGE_PROVER_THINNING_SPEC.md`.
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
    ///
    /// **Status:** local / CI default (`#[default]`). Subcritical under
    /// shellnet 3 seq/s (prover slower than chain) — fine for short runs.
    #[default]
    L1,
    /// L2 anchoring: one bundle per `W²` seq_nos, `chain_steps = 1` L2 hop.
    ///
    /// **Status:** operational default on shellnet since 2026-08-18
    /// (Deploy #12). `BRIDGE_ANCHOR_LEVEL=2` in the shellnet-l2 runtime
    /// env and in `bridge_config.shellnet`. Circuit 2 is level-parametric
    /// (same VK across L1/L2). Local `bridge_config.local` stays L1.
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

    /// Cross-check a persisted `anchor_level` byte from a `BridgeState` /
    /// `RelayerState` file against the anchor level the daemon was configured
    /// to run at (`self`). Returns `Ok(())` when it is safe to proceed and
    /// [`AnchorLevelMismatch`] when the daemon must refuse to start.
    ///
    /// The rules mirror §3 of `docs/l2_anchoring_implementation_plan.md`
    /// and were previously duplicated between `bridge-prover-daemon` and
    /// `bridge-relayer-daemon`:
    ///
    /// | state_level | cfg (self) | result | rationale                        |
    /// |:-----------:|:----------:|:------:|:---------------------------------|
    /// | 0 (unknown) | L1         | ok     | legacy v4-schema state; assume L1|
    /// | 0 (unknown) | L2         | err    | can't safely infer prior level   |
    /// | 1           | L1         | ok     | steady state                     |
    /// | 1           | L2         | err    | live-bridge flip forbidden       |
    /// | 2           | L1         | err    | live-bridge flip forbidden       |
    /// | 2           | L2         | ok     | steady state                     |
    ///
    /// Callers should only invoke this on an `initialized` state; an
    /// uninitialized state carries `anchor_level=0` legitimately (the seed
    /// stamps the level on `apply`).
    ///
    /// This helper only returns the decision; each daemon formats its own
    /// diagnostic (state-file paths differ) around the resulting error.
    pub fn verify_state_level(self, state_level: u8) -> Result<(), AnchorLevelMismatch> {
        let cfg_level = self.level();
        let mismatch = match (state_level, cfg_level) {
            (0, 1) => false,     // legacy state file → L1 backward compat
            (0, _) => true,      // legacy state file → non-L1 requires explicit migration
            (s, c) => s != c,    // explicit mismatch either direction
        };
        if mismatch {
            Err(AnchorLevelMismatch { state_level, cfg_level })
        } else {
            Ok(())
        }
    }
}

/// Startup-drift error raised by [`AnchorMode::verify_state_level`] when the
/// persisted state's anchor level disagrees with the daemon's configured
/// mode. Carries the two levels so the caller can format a diagnostic that
/// includes the local state-file path (which lives outside this crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorLevelMismatch {
    /// `anchor_level` byte loaded from the on-disk state (0 = legacy/unknown,
    /// 1 = L1, 2 = L2).
    pub state_level: u8,
    /// The level the daemon was configured for at startup (1 or 2).
    pub cfg_level: u8,
}

impl std::fmt::Display for AnchorLevelMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "anchor-level drift: state anchor_level={} but daemon configured for L{}",
            self.state_level, self.cfg_level,
        )
    }
}

impl std::error::Error for AnchorLevelMismatch {}

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

    /// Full truth-table for [`AnchorMode::verify_state_level`]. The daemons
    /// must never diverge on this decision, so pin the entire matrix here
    /// (see §3 of `l2_anchoring_implementation_plan.md`).
    #[test]
    fn verify_state_level_truth_table() {
        // (cfg, state_level, expect_ok)
        let cases: &[(AnchorMode, u8, bool)] = &[
            // Legacy-state (v4 schema, anchor_level absent → 0).
            (AnchorMode::L1, 0, true),   // legacy → L1 backward compat
            (AnchorMode::L2, 0, false),  // legacy → L2 requires migration
            // Steady state.
            (AnchorMode::L1, 1, true),
            (AnchorMode::L2, 2, true),
            // Live-bridge flips (both directions forbidden).
            (AnchorMode::L1, 2, false),
            (AnchorMode::L2, 1, false),
            // Defensive: unknown future levels vs current cfg — refuse.
            (AnchorMode::L1, 3, false),
            (AnchorMode::L2, 3, false),
        ];
        for &(mode, state_level, expect_ok) in cases {
            let got = mode.verify_state_level(state_level);
            assert_eq!(
                got.is_ok(),
                expect_ok,
                "mode={mode:?} state_level={state_level}: expected ok={expect_ok}, got {got:?}",
            );
            if let Err(e) = got {
                assert_eq!(e.state_level, state_level);
                assert_eq!(e.cfg_level, mode.level());
            }
        }
    }
}
