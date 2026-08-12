//! Startup routing for the daemon-live relayer.
//!
//! Replaces the ad-hoc 3-arm `seed_policy` match + late drift-guard in
//! `bin/relayer.rs` with an explicit routing table over
//! (local `BridgeState`, on-chain `ContractFullState`). The four legs are
//! [`StartupDecision::Cold`], [`StartupDecision::WarmResume`],
//! [`StartupDecision::Resurrect`], [`StartupDecision::Stop`].
//!
//! Design goals:
//! * "Local state absent" is **not** the only resurrect trigger — a local
//!   state that is present but lags the contract (typical in shared test
//!   bridges where a co-tester has advanced the contract) also resurrects.
//! * Any state that is *ahead* of the contract is a fatal inconsistency
//!   — we never silently rewind an on-chain contract.
//! * Cold-start policy selection (`Auto` vs `Explicit(n)`) is kept
//!   orthogonal to the resurrect vs resume decision — the operator's
//!   `--bootstrap-seqno` flag still drives it.
//! * Unit-testable without an alloy RPC provider: consumes plain
//!   [`ContractFullState`] shaped by the daemon's `read_full_state`.

use bridge_prover_lib::bridge_state::{
    BridgeState, ContractFullState as LibContractFullState, ContractLayerWindow as LibContractLayerWindow,
    MAX_LAYERS,
};
use bridge_prover_lib::live_driver::SeedPolicy;

use crate::bridge::{ContractFullState, ContractLayerWindow};

/// The four legs of startup routing.
#[derive(Debug)]
pub enum StartupDecision {
    /// Contract is at genesis (`last_seen_block_seq_no == 0`) and local
    /// state has not been through a bundle apply. Bootstrap normally
    /// via `SeedPolicy::Auto` or `SeedPolicy::Explicit(n)`.
    Cold { policy: SeedPolicy },

    /// Local state matches the contract exactly (same last_seen, same
    /// bk-set commitment, same bk-update seq_no, same layer windows
    /// byte-for-byte). Safe to resume: pass local state through as-is
    /// with `SeedPolicy::Resume`.
    WarmResume,

    /// Local state is either absent-but-chain-has-history, or present
    /// but behind the contract (typical of a shared bridge that another
    /// tester has advanced). Rebuild `BridgeState` from the on-chain
    /// snapshot and drive with `SeedPolicy::Resume`. The rebuilt state
    /// is byte-for-byte the contract mirror.
    Resurrect { fresh_state: Box<BridgeState> },

    /// Unrecoverable inconsistency between local and chain — the daemon
    /// must not paper over this. Operator must investigate before
    /// restart. Message is the human-readable reason.
    Stop { reason: String },
}

/// Inputs for [`decide`]. All fields are references so the caller keeps
/// ownership; the decision consumes `chain` only when it needs to
/// build the resurrect state.
pub struct DecideInputs<'a> {
    /// Local `BridgeState` as just loaded from `prover_state.json`
    /// (or freshly constructed via `BridgeState::new` if the file is
    /// absent — the caller must not distinguish the two cases here).
    pub local: &'a BridgeState,
    /// Full snapshot of the on-chain contract state.
    pub chain: &'a ContractFullState,
    /// The operator's `--bootstrap-seqno` flag, if any. Only consulted
    /// on the Cold path (bootstrap into a genesis-state contract).
    pub bootstrap_seqno: Option<u64>,
    /// Rolling window width the daemon was launched with. Must equal
    /// the width the contract was deployed with; a mismatch is a Stop
    /// (silently rebuilding a differently-shaped ring corrupts slot
    /// math).
    pub window_size: usize,
}

/// Convert the daemon-side (alloy-typed) `ContractLayerWindow` into the
/// alloy-neutral `bridge_prover_lib` shape consumed by
/// `BridgeState::from_contract`. Alloy `U256` → 32-byte big-endian, and
/// the width fields are copied through.
fn to_lib_layer_window(w: &ContractLayerWindow) -> LibContractLayerWindow {
    LibContractLayerWindow {
        data: w.data.clone(),
        heights: w.heights.clone(),
        data_len: w.data_len,
        write_cursor: w.write_cursor,
        last_height: w.last_height,
    }
}

fn to_lib_full_state(cfs: &ContractFullState) -> LibContractFullState {
    // Split the fixed-size array elementwise. `std::array::from_fn`
    // gives us the layout invariance without needing `TryFrom<Vec<_>>`.
    let layer_windows: [LibContractLayerWindow; MAX_LAYERS] =
        std::array::from_fn(|i| to_lib_layer_window(&cfs.layer_windows[i]));
    LibContractFullState {
        last_seen_block_seq_no: cfs.last_seen_block_seq_no,
        bk_set_commitment: cfs.bk_set_commitment.to_be_bytes::<32>(),
        last_bk_set_update_seq_no: cfs.last_bk_set_update_seq_no,
        layer_windows,
    }
}

/// Byte-for-byte comparison of local vs chain layer windows.
/// Returns `true` iff every slot in every layer matches.
fn windows_match(local: &BridgeState, chain: &ContractFullState) -> bool {
    for i in 0..MAX_LAYERS {
        let lw = &local.layer_windows[i];
        let cw = &chain.layer_windows[i];
        if lw.data != cw.data
            || lw.heights != cw.heights
            || lw.data_len as u16 != cw.data_len
            || lw.write_cursor as u16 != cw.write_cursor
            || lw.last_height != cw.last_height
        {
            return false;
        }
    }
    true
}

/// The routing table.
///
/// | local state          | chain state                        | decision                     |
/// |----------------------|------------------------------------|------------------------------|
/// | uninitialized        | genesis (last_seen == 0)           | Cold                         |
/// | uninitialized        | advanced (last_seen  > 0)          | Resurrect                    |
/// | initialized, matches | matches local exactly              | WarmResume                   |
/// | initialized, behind  | ahead of local (last_seen greater) | Resurrect                    |
/// | initialized, ahead   | genesis or older than local        | Stop                         |
/// | initialized, mismatch on same last_seen                     | Stop (bk-commit / windows)   |
///
/// Cold policy pick: `Explicit(n)` if the operator passed
/// `--bootstrap-seqno`, else `Auto`.
///
/// A `window_size` mismatch between local and any layer of chain is
/// treated as an unconditional Stop before any of the above arms fire.
pub fn decide(inputs: DecideInputs<'_>) -> StartupDecision {
    let DecideInputs { local, chain, bootstrap_seqno, window_size } = inputs;

    // Guard: every chain window must be sized to the launched W. If not,
    // no arm below is meaningful — reject before doing anything else.
    for (i, cw) in chain.layer_windows.iter().enumerate() {
        if cw.data.len() != window_size {
            return StartupDecision::Stop {
                reason: format!(
                    "chain layer {} window width {} != daemon window_size {} — \
                     contract deployed with a different W or corrupt read",
                    i + 1,
                    cw.data.len(),
                    window_size,
                ),
            };
        }
    }
    if local.window_size != window_size {
        return StartupDecision::Stop {
            reason: format!(
                "local BridgeState.window_size={} != daemon window_size={} — \
                 delete prover_state.json and rebootstrap",
                local.window_size, window_size,
            ),
        };
    }

    let chain_empty = chain.last_seen_block_seq_no == 0;

    if !local.initialized {
        // Local state has never applied a bundle. Either the daemon
        // is truly cold, or the state file is fresh but the contract
        // has already been driven by someone else.
        return if chain_empty {
            StartupDecision::Cold {
                policy: match bootstrap_seqno {
                    Some(n) => SeedPolicy::Explicit(n),
                    None => SeedPolicy::Auto,
                },
            }
        } else {
            // Chain-resurrect: rebuild BridgeState from the on-chain
            // snapshot. `from_contract` performs its own shape checks.
            match BridgeState::from_contract(to_lib_full_state(chain), window_size) {
                Ok(rebuilt) => StartupDecision::Resurrect {
                    fresh_state: Box::new(rebuilt),
                },
                Err(e) => StartupDecision::Stop {
                    reason: format!("resurrect from_contract failed: {e}"),
                },
            }
        };
    }

    // Local is initialized.
    if chain_empty {
        // Local has state but chain does not. We cannot silently rewind
        // a contract to genesis; this is either a wrong-contract config
        // or a redeployment that the operator has not acknowledged.
        return StartupDecision::Stop {
            reason: format!(
                "local BridgeState is initialized (last_seen={}) but contract is at genesis \
                 (last_seen=0) — wrong --bridge address or contract redeployed. Nuke local \
                 state (rm prover_state.json) or point the daemon at the intended contract.",
                local.stored_last_seen_block_seq_no,
            ),
        };
    }

    // Both sides have history. Compare cursors.
    let local_seq = local.stored_last_seen_block_seq_no;
    let chain_seq = chain.last_seen_block_seq_no;

    if local_seq > chain_seq {
        return StartupDecision::Stop {
            reason: format!(
                "local last_seen={} is ahead of chain last_seen={} — chain rewound (reorg or \
                 redeployment) or local state file was copied from another environment",
                local_seq, chain_seq,
            ),
        };
    }

    if local_seq < chain_seq {
        // Someone else advanced the contract while our local state was
        // idle. Resurrect from the chain snapshot.
        return match BridgeState::from_contract(to_lib_full_state(chain), window_size) {
            Ok(rebuilt) => StartupDecision::Resurrect {
                fresh_state: Box::new(rebuilt),
            },
            Err(e) => StartupDecision::Stop {
                reason: format!("resurrect from_contract failed: {e}"),
            },
        };
    }

    // local_seq == chain_seq && chain_seq > 0. This must be an exact
    // match on every mirror field — otherwise the state is subtly
    // inconsistent (e.g. same last_seen but different bk commitment)
    // and we must not silently proceed.
    let local_commit = local.stored_bk_set_commitment;
    let chain_commit = chain.bk_set_commitment.to_be_bytes::<32>();
    if local_commit != chain_commit {
        return StartupDecision::Stop {
            reason: format!(
                "cursor match (last_seen={}) but bk-set commitment diverges: local={} chain={}",
                local_seq,
                hex::encode(local_commit),
                hex::encode(chain_commit),
            ),
        };
    }
    if local.stored_last_bk_set_update_seq_no != chain.last_bk_set_update_seq_no {
        return StartupDecision::Stop {
            reason: format!(
                "cursor match (last_seen={}) but bk-update seq_no diverges: local={} chain={}",
                local_seq,
                local.stored_last_bk_set_update_seq_no,
                chain.last_bk_set_update_seq_no,
            ),
        };
    }
    if !windows_match(local, chain) {
        return StartupDecision::Stop {
            reason: format!(
                "cursor match (last_seen={}) but per-layer windows diverge byte-for-byte — \
                 local was written by a different verifier or drifted",
                local_seq,
            ),
        };
    }

    StartupDecision::WarmResume
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    const W: usize = 4;

    fn empty_lw() -> ContractLayerWindow {
        ContractLayerWindow {
            data: vec![[0u8; 32]; W],
            heights: vec![0u64; W],
            data_len: 0,
            write_cursor: 0,
            last_height: 0,
        }
    }

    fn empty_chain() -> ContractFullState {
        let layer_windows: [ContractLayerWindow; MAX_LAYERS] =
            std::array::from_fn(|_| empty_lw());
        ContractFullState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: U256::ZERO,
            last_bk_set_update_seq_no: 0,
            prev_max_level_layer_hash: U256::ZERO,
            layer_windows,
        }
    }

    /// Snapshot a `BridgeState` back into a `ContractFullState` shape.
    /// Only sensible when the state was produced by `append_bundle`
    /// with seq_no == height (so heights[] == the on-chain seq_no
    /// mirror). See `bridge_state.rs::from_contract` docstring.
    fn snapshot_as_chain(s: &BridgeState) -> ContractFullState {
        let mut arr: Vec<ContractLayerWindow> = Vec::with_capacity(MAX_LAYERS);
        for w in &s.layer_windows {
            arr.push(ContractLayerWindow {
                data: w.data.clone(),
                heights: w.heights.clone(),
                data_len: w.data_len as u16,
                write_cursor: w.write_cursor as u16,
                last_height: w.last_height,
            });
        }
        let layer_windows: [ContractLayerWindow; MAX_LAYERS] = arr.try_into().unwrap();
        let commit = U256::from_be_bytes::<32>(s.stored_bk_set_commitment);
        ContractFullState {
            last_seen_block_seq_no: s.stored_last_seen_block_seq_no,
            bk_set_commitment: commit,
            last_bk_set_update_seq_no: s.stored_last_bk_set_update_seq_no,
            prev_max_level_layer_hash: U256::ZERO,
            layer_windows,
        }
    }

    fn advanced_state(seq_no: u64) -> BridgeState {
        let mut s = BridgeState::new(W);
        s.initialize_bk_set_commitment([7u8; 32]).unwrap();
        s.append_bundle(&[([0x11; 32], 1)], seq_no, seq_no).unwrap();
        s
    }

    #[test]
    fn cold_no_bootstrap_flag_picks_auto() {
        let local = BridgeState::new(W);
        let chain = empty_chain();
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Cold { policy } => assert_eq!(policy, SeedPolicy::Auto),
            d => panic!("expected Cold, got {d:?}"),
        }
    }

    #[test]
    fn cold_with_bootstrap_flag_picks_explicit() {
        let local = BridgeState::new(W);
        let chain = empty_chain();
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: Some(512),
            window_size: W,
        }) {
            StartupDecision::Cold { policy } => assert_eq!(policy, SeedPolicy::Explicit(512)),
            d => panic!("expected Cold, got {d:?}"),
        }
    }

    #[test]
    fn warm_resume_on_exact_match() {
        let src = advanced_state(10);
        let chain = snapshot_as_chain(&src);
        match decide(DecideInputs {
            local: &src,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::WarmResume => {}
            d => panic!("expected WarmResume, got {d:?}"),
        }
    }

    #[test]
    fn resurrect_when_local_absent_chain_advanced() {
        let advanced = advanced_state(10);
        let chain = snapshot_as_chain(&advanced);
        let local = BridgeState::new(W);
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Resurrect { fresh_state } => {
                assert!(fresh_state.initialized);
                assert_eq!(fresh_state.stored_last_seen_block_seq_no, 10);
                assert_eq!(fresh_state.stored_bk_set_commitment, [7u8; 32]);
            }
            d => panic!("expected Resurrect, got {d:?}"),
        }
    }

    #[test]
    fn resurrect_when_local_lags_behind_chain() {
        // Local knows about seq 10, chain has advanced to seq 20 —
        // co-tester scenario. Must resurrect from chain (not resume,
        // otherwise the daemon would re-submit already-verified blocks).
        let mut local = advanced_state(10);
        // Build a chain that also has the same layer 1 entry PLUS a later one.
        let mut chain_src = advanced_state(10);
        chain_src.append_bundle(&[([0x22; 32], 1)], 20, 20).unwrap();
        let chain = snapshot_as_chain(&chain_src);
        // Sanity: keep local at seq 10 to prove the lag.
        assert_eq!(local.stored_last_seen_block_seq_no, 10);
        // Just to appease the borrow checker (we take &local below).
        let _ = &mut local;
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Resurrect { fresh_state } => {
                assert_eq!(fresh_state.stored_last_seen_block_seq_no, 20);
            }
            d => panic!("expected Resurrect, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_local_ahead_of_chain() {
        let local = advanced_state(20);
        let chain = empty_chain();
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Stop { reason } => {
                assert!(reason.contains("last_seen=20"), "reason: {reason}");
                assert!(reason.contains("genesis"), "reason: {reason}");
            }
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_cursor_matches_but_bk_commit_diverges() {
        let src = advanced_state(10);
        let mut chain = snapshot_as_chain(&src);
        // Perturb only the bk-set commitment.
        chain.bk_set_commitment = U256::from_be_bytes::<32>([0xAA; 32]);
        match decide(DecideInputs {
            local: &src,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Stop { reason } => {
                assert!(reason.contains("bk-set commitment diverges"), "reason: {reason}");
            }
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_cursor_matches_but_windows_diverge() {
        let src = advanced_state(10);
        let mut chain = snapshot_as_chain(&src);
        // Perturb layer 1's most recent hash slot.
        chain.layer_windows[0].data[0] = [0xFF; 32];
        match decide(DecideInputs {
            local: &src,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
        }) {
            StartupDecision::Stop { reason } => {
                assert!(reason.contains("per-layer windows diverge"), "reason: {reason}");
            }
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_on_window_size_mismatch() {
        // Local built with W=4, chain read with W=8 (contract deployed with
        // a different W). Must be rejected before any of the semantic arms.
        let local = BridgeState::new(4);
        let wide_lw = || ContractLayerWindow {
            data: vec![[0u8; 32]; 8],
            heights: vec![0u64; 8],
            data_len: 0,
            write_cursor: 0,
            last_height: 0,
        };
        let layer_windows: [ContractLayerWindow; MAX_LAYERS] =
            std::array::from_fn(|_| wide_lw());
        let chain = ContractFullState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: U256::ZERO,
            last_bk_set_update_seq_no: 0,
            prev_max_level_layer_hash: U256::ZERO,
            layer_windows,
        };
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: 4,
        }) {
            StartupDecision::Stop { reason } => {
                assert!(reason.contains("window width"), "reason: {reason}");
            }
            d => panic!("expected Stop, got {d:?}"),
        }
    }
}
