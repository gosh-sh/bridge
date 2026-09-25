//! Startup routing for the daemon-live relayer.
//!
//! Replaces the ad-hoc 3-arm `seed_policy` match + late drift-guard in
//! `bin/relayer.rs` with an explicit routing table over
//! (local `BridgeState`, on-chain `EthBridgeContractState`). The four legs are
//! [`StartupDecision::Cold`], [`StartupDecision::WarmResume`],
//! [`StartupDecision::Resurrect`], [`StartupDecision::Stop`].
//!
//! Design goals:
//! * "Local state absent" is **not** the only resurrect trigger — a local state
//!   that is present but lags the contract (typical in shared test bridges
//!   where a co-tester has advanced the contract) also resurrects.
//! * Any state that is *ahead* of the contract is a fatal inconsistency — we
//!   never silently rewind an on-chain contract.
//! * Cold-start policy selection (`Auto` vs `Explicit(n)`) is kept orthogonal
//!   to the resurrect vs resume decision — the operator's `--bootstrap-seqno`
//!   flag still drives it.
//! * Unit-testable without an alloy RPC provider: consumes plain
//!   [`EthBridgeContractState`] shaped by the daemon's `read_full_state`.

#[cfg(test)]
use bridge_prover_lib::bridge_state::HistoryWindow;
use bridge_prover_lib::{
    bridge_state::{BridgeState, EthBridgeContractState, MAX_LAYERS},
    live_driver::SeedPolicy,
};

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
    pub chain: &'a EthBridgeContractState,
    /// The operator's `--bootstrap-seqno` flag, if any. Only consulted
    /// on the Cold path (bootstrap into a genesis-state contract).
    pub bootstrap_seqno: Option<u64>,
    /// Rolling window width the daemon was launched with. Must equal
    /// the width the contract was deployed with; a mismatch is a Stop
    /// (silently rebuilding a differently-shaped ring corrupts slot
    /// math).
    pub window_size: usize,
    /// Anchor level the daemon was launched with. Stamped into the
    /// rebuilt `BridgeState.anchor_level` on Resurrect; contract does
    /// not persist this field.
    pub anchor_level: u8,
}

/// Chain rotated at N and the local prover has not acked: it still
/// holds `storedPrevBkSetCommitment` and a smaller `last_bk`.
fn hold_window_unacked(local: &BridgeState, chain: &EthBridgeContractState) -> bool {
    chain.last_bk_set_update_seq_no > local.stored_last_bk_set_update_seq_no
        && chain.last_bk_set_update_seq_no != 0
        && local.stored_bk_set_commitment == chain.prev_bk_set_commitment
}

/// Chronological equivalence check for local vs chain layer windows.
///
/// **Endianness.** Both sides are LE (`Fr::to_repr()`). `read_full_state`
/// reverses each `uint256` slot at construction time, so this comparator
/// does not need to reverse anything.
///
/// **Genesis prepend.** Cold-start bootstrap
/// (`bootstrap::BootstrapSeed::apply`, documented at `bootstrap.rs:17-18` as a
/// Phase 1 fix ensuring verifier daemon does not lag prover) calls
/// `append_bundle` with every seed `history_proofs` entry. This prepends one
/// genesis entry to each active local layer window. The contract does **not**
/// copy those entries into `_layerWindows`; it exposes only the configured
/// anchor layer's seed root in immutable `storedPrevMaxLevelLayerHash`. So
/// chronologically:
///
/// - **Pre-wrap** (local data_len ≤ W): each bootstrapped local layer window is
///   `[genesis_layer_root, vb_1, vb_2, …, vb_N]`; chain is `[vb_1, …, vb_N]`.
///   Local has exactly one extra leading entry. On the configured anchor layer
///   it must equal `chain.genesis_prev_max_level_layer_hash` (both LE — see
///   comparator endianness note at the top of this rustdoc). Other active
///   layers have no immutable on-chain genesis surface, so only their
///   post-genesis suffix can be compared.
/// - **Post-wrap** (both data_len == W, after the (N=W)-th verifyBlock
///   overwrites local's genesis slot): local and chain chronological sequences
///   are identical.
fn windows_match(local: &BridgeState, chain: &EthBridgeContractState, anchor_level: u8) -> bool {
    let w = local.window_size;
    let Some(anchor_index) = anchor_level.checked_sub(1).map(usize::from) else {
        return false;
    };
    if anchor_index >= MAX_LAYERS {
        return false;
    }
    for i in 0..MAX_LAYERS {
        let lw = &local.layer_windows[i];
        let cw = &chain.layer_windows[i];

        if lw.data.len() != cw.data.len() || lw.data.len() != w {
            return false;
        }
        // Do NOT compare `last_height` directly — on a layer whose only
        // populated entry is a genesis prepend (chain layer empty,
        // local layer has one entry at seed height), local.last_height
        // is the seed height and chain.last_height is 0. The
        // chronological tuple comparison below still covers height
        // equality on every populated slot.

        // Walk each ring oldest → newest. Both sides are already LE
        // (see comparator rustdoc: `read_full_state` normalises chain
        // slots at construction, so no reversal here).
        let local_chrono: Vec<([u8; 32], u64)> = {
            let len = lw.data_len;
            let start = if len < w { 0 } else { lw.write_cursor };
            (0..len)
                .map(|k| {
                    let p = (start + k) % w;
                    (lw.data[p], lw.heights[p])
                })
                .collect()
        };
        let chain_chrono: Vec<([u8; 32], u64)> = {
            let len = cw.data_len;
            let start = if len < w { 0 } else { cw.write_cursor };
            (0..len)
                .map(|k| {
                    let p = (start + k) % w;
                    (cw.data[p], cw.heights[p])
                })
                .collect()
        };

        // Common case: chronologies match directly (post-wrap on any
        // layer, or layers whose seed history_proofs did not touch them).
        if local_chrono == chain_chrono {
            continue;
        }

        // Genesis prepend case: local has one extra leading slot from
        // `BootstrapSeed::apply` calling `append_bundle` with the seed's
        // per-layer `history_proofs`. Chain does not copy those seed
        // entries into `_layerWindows`.
        //
        // The configured anchor layer's seed root is exposed on chain as
        // `storedPrevMaxLevelLayerHash`, so equality-check the prepend at
        // index `anchor_level - 1`. This matters for L2: the immutable is the
        // layer-2 seed root, not the layer-1 root. For other layers, chain has
        // no per-layer genesis anchor surface; accept the presence of a
        // genesis prepend but do not equality-check that first slot. Runtime
        // ack-time consistency (via `EthBridgeClient::submit_block`'s
        // `expectedPrevAnchor(numLayers)` cross-check, per
        // `history_consistency.rs:38-47`) is the authoritative gate for
        // deeper layers — startup routing only ensures we do not
        // silently resurrect on a mismatched cursor.
        if local_chrono.len() == chain_chrono.len() + 1 {
            if i == anchor_index && local_chrono[0].0 != chain.genesis_prev_max_level_layer_hash {
                return false;
            }
            if local_chrono[1..] != chain_chrono[..] {
                return false;
            }
            continue;
        }

        return false;
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
    let DecideInputs {
        local,
        chain,
        bootstrap_seqno,
        window_size,
        anchor_level,
    } = inputs;

    // Guard: every chain window must be sized to the launched W. If not,
    // no arm below is meaningful — reject before doing anything else.
    for (i, cw) in chain.layer_windows.iter().enumerate() {
        if cw.data.len() != window_size {
            return StartupDecision::Stop {
                reason: format!(
                    "chain layer {} window width {} != daemon window_size {} — contract deployed \
                     with a different W or corrupt read",
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
                "local BridgeState.window_size={} != daemon window_size={} — delete \
                 prover_state.json and rebootstrap",
                local.window_size, window_size,
            ),
        };
    }

    // Anchor-level vs chain-shape mismatch: refuse before the Resurrect arm
    // persists a state file with the daemon's `anchor_level` stamped over
    // windows produced under a different level. Both directions are
    // reachable without an on-disk state at all (fresh install), so this
    // check has to sit above the initialized/uninitialized fork below.
    //
    // The `last_seen % stride` alignment check in the caller does not catch
    // either direction — every W² boundary is simultaneously W·P-aligned
    // (16384 % 1024 == 0), so an L2-advanced cursor slips through as an
    // L1-daemon target, and an L1-driven cursor that happens to land on a
    // W² multiple slips through as an L2-daemon target.
    //
    // Failure mode without this gate: Resurrect copies chain windows into
    // local state, stamps `anchor_level = <daemon cfg>` on disk, then the
    // driver drives at the wrong cadence and the first verifyBlock reverts
    // on `PrevAnchorMismatch`. Worse, the poisoned state file blocks a
    // restart with the corrected `--anchor-level` (the state-vs-cfg drift
    // guard trips first), so recovery requires manually moving the file.
    if anchor_level == 1 {
        if let Some(l) = chain.highest_populated_layer() {
            return StartupDecision::Stop {
                reason: format!(
                    "refuse: daemon configured for L1 but on-chain contract has \
                     layer_windows[{l}].data_len > 0 (contract is L{l}-advanced). Restart with \
                     --anchor-level {l} / BRIDGE_ANCHOR_LEVEL={l}."
                ),
            };
        }
    }
    if anchor_level >= 2
        && chain.layer_windows[0].data_len > 0
        && chain
            .highest_populated_layer()
            .is_none_or(|l| l < anchor_level)
    {
        return StartupDecision::Stop {
            reason: format!(
                "refuse: daemon configured for L{anchor_level} but on-chain contract's deepest \
                 populated layer is 1 (contract has only ever been driven by an L1 daemon). \
                 Restart with --anchor-level 1 / BRIDGE_ANCHOR_LEVEL=1."
            ),
        };
    }

    let chain_empty = chain.last_seen_block_seq_no == 0;
    // Post-deploy transitional state: the constructor writes
    // `storedLastSeenBlockSeqNo = genesisLastSeenBlockSeqNo` (from
    // `compute_bridge_anchors`' emitted GENESIS_SEED_SEQNO) but does
    // NOT populate `_layerWindows` — those are only appended by
    // `verifyBlock` at runtime. So a just-deployed contract has a
    // non-zero cursor with every window empty. We must NOT route
    // this to Resurrect (that would copy empty windows into local
    // state and skip GQL bootstrap, causing PrevAnchorMismatch on
    // the first submit because the prover would compute
    // prev_max_level=0 while the contract expects the immutable
    // `storedPrevMaxLevelLayerHash`). Instead, route to Cold with
    // an Explicit policy pinned to the chain's cursor — this
    // forces the driver to fetch the seed key block via GQL, which
    // will populate layer windows and produce the same anchor the
    // contract's genesis constant carries.
    let chain_has_no_windows = chain.layer_windows.iter().all(|w| w.data_len == 0);

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
        } else if chain_has_no_windows {
            // Post-deploy, pre-first-verifyBlock. Bootstrap using the
            // chain's cursor as the seed seqno regardless of what the
            // operator passed via `--bootstrap-seqno` — otherwise the
            // proof's baked-in `last_seen` would diverge from
            // `storedLastSeenBlockSeqNo` and attestation verification
            // would fail.
            StartupDecision::Cold {
                policy: SeedPolicy::Explicit(chain.last_seen_block_seq_no),
            }
        } else {
            // Chain-resurrect: rebuild BridgeState from the on-chain
            // snapshot. `from_contract` performs its own shape checks.
            match BridgeState::from_contract(chain.clone(), window_size, anchor_level) {
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
                 (last_seen=0) — wrong --bridge address or contract redeployed. Nuke local state \
                 (rm prover_state.json) or point the daemon at the intended contract.",
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
        if hold_window_unacked(local, chain) {
            // Chain applied N; local still holds the outgoing set and
            // has not acked. Catch up with that set — do not resurrect
            // into the new commitment (that would desync prover_bk_set).
            return StartupDecision::WarmResume;
        }
        // Someone else advanced the contract while our local state was
        // idle. Resurrect from the chain snapshot.
        return match BridgeState::from_contract(chain.clone(), window_size, anchor_level) {
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
    // Both sides LE (`Fr::to_repr()`) — `read_full_state` reverses on read
    // per the convention in `history_consistency.rs:59` and memory
    // `bridge_genesis_anchor_endianness.md`.
    let chain_commit = chain.bk_set_commitment;
    if hold_window_unacked(local, chain) {
        return StartupDecision::WarmResume;
    }
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
                local_seq, local.stored_last_bk_set_update_seq_no, chain.last_bk_set_update_seq_no,
            ),
        };
    }
    if !windows_match(local, chain, anchor_level) {
        return StartupDecision::Stop {
            reason: format!(
                "cursor match (last_seen={}) but per-layer windows diverge byte-for-byte — local \
                 was written by a different verifier or drifted",
                local_seq,
            ),
        };
    }

    StartupDecision::WarmResume
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 4;

    fn empty_lw() -> HistoryWindow {
        HistoryWindow {
            data: vec![[0u8; 32]; W],
            heights: vec![0u64; W],
            data_len: 0,
            write_cursor: 0,
            last_height: 0,
        }
    }

    fn empty_chain() -> EthBridgeContractState {
        let layer_windows: [HistoryWindow; MAX_LAYERS] = std::array::from_fn(|_| empty_lw());
        EthBridgeContractState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: [0u8; 32],
            last_bk_set_update_seq_no: 0,
            prev_bk_set_commitment: [0u8; 32],
            genesis_prev_max_level_layer_hash: [0u8; 32],
            layer_windows,
        }
    }

    /// Snapshot a `BridgeState` back into a `EthBridgeContractState` shape.
    /// Only sensible when the state was produced by `append_bundle`
    /// with seq_no == height (so heights[] == the on-chain seq_no
    /// mirror). See `bridge_state.rs::from_contract` docstring.
    fn snapshot_as_chain(s: &BridgeState) -> EthBridgeContractState {
        let layer_windows: [HistoryWindow; MAX_LAYERS] =
            std::array::from_fn(|i| s.layer_windows[i].clone());
        EthBridgeContractState {
            last_seen_block_seq_no: s.stored_last_seen_block_seq_no,
            bk_set_commitment: s.stored_bk_set_commitment,
            last_bk_set_update_seq_no: s.stored_last_bk_set_update_seq_no,
            prev_bk_set_commitment: [0u8; 32],
            genesis_prev_max_level_layer_hash: [0u8; 32],
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
    fn warm_resume_when_chain_rotated_and_local_still_on_prev() {
        let mut local = advanced_state(8);
        local.stored_last_bk_set_update_seq_no = 0;
        let old = local.stored_bk_set_commitment;
        let mut chain = snapshot_as_chain(&local);
        chain.bk_set_commitment = [0xABu8; 32];
        chain.prev_bk_set_commitment = old;
        chain.last_bk_set_update_seq_no = 12;
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 1,
        }) {
            StartupDecision::WarmResume => {},
            d => panic!("expected WarmResume in hold window, got {d:?}"),
        }
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
            anchor_level: 1,
        }) {
            StartupDecision::Cold {
                policy,
            } => assert_eq!(policy, SeedPolicy::Auto),
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
            anchor_level: 1,
        }) {
            StartupDecision::Cold {
                policy,
            } => assert_eq!(policy, SeedPolicy::Explicit(512)),
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
            anchor_level: 1,
        }) {
            StartupDecision::WarmResume => {},
            d => panic!("expected WarmResume, got {d:?}"),
        }
    }

    fn l2_prepend_state() -> (BridgeState, EthBridgeContractState) {
        let seed_seqno = 100;
        let verified_seqno = 200;
        let seed_l1 = [0x11; 32];
        let seed_l2 = [0x22; 32];
        let verified_l1 = [0x33; 32];
        let verified_l2 = [0x44; 32];

        let mut local = BridgeState::new(W);
        local.initialize_bk_set_commitment([7u8; 32]).unwrap();
        local
            .append_bundle(&[(seed_l1, 1), (seed_l2, 2)], seed_seqno, seed_seqno)
            .unwrap();
        local.anchor_level = 2;
        local
            .append_bundle(
                &[(verified_l1, 1), (verified_l2, 2)],
                verified_seqno,
                verified_seqno,
            )
            .unwrap();

        // The contract stores only verifyBlock results in per-layer windows;
        // bootstrap seed entries exist only in local state. Its one immutable
        // genesis anchor is the configured layer-2 seed root.
        let mut chain = snapshot_as_chain(&local);
        chain.genesis_prev_max_level_layer_hash = seed_l2;
        chain.layer_windows[0] = empty_lw();
        chain.layer_windows[0].append(verified_l1, verified_seqno);
        chain.layer_windows[1] = empty_lw();
        chain.layer_windows[1].append(verified_l2, verified_seqno);
        (local, chain)
    }

    #[test]
    fn warm_resume_l2_checks_genesis_prepend_at_anchor_layer() {
        let (local, chain) = l2_prepend_state();
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 2,
        }) {
            StartupDecision::WarmResume => {},
            d => panic!("expected L2 WarmResume, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_l2_genesis_prepend_diverges_at_anchor_layer() {
        let (local, mut chain) = l2_prepend_state();
        // Matching the layer-1 seed is insufficient for an L2 daemon: the
        // immutable must bind the configured layer-2 seed root.
        chain.genesis_prev_max_level_layer_hash = [0x11; 32];
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 2,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(
                    reason.contains("per-layer windows diverge"),
                    "reason: {reason}"
                );
            },
            d => panic!("expected L2 Stop, got {d:?}"),
        }
    }

    #[test]
    fn cold_when_local_absent_chain_post_deploy() {
        // Just-deployed contract: constructor set last_seen to the
        // seed seqno emitted by compute_bridge_anchors, but no
        // verifyBlock has run yet so every layer window is empty.
        // Must route to Cold with Explicit(chain.last_seen), NOT
        // Resurrect (which would copy the empty windows and skip
        // the GQL bootstrap).
        let seed_seqno: u64 = 5_495_808;
        let mut chain = empty_chain();
        chain.last_seen_block_seq_no = seed_seqno;
        chain.bk_set_commitment = [7u8; 32];
        chain.last_bk_set_update_seq_no = 0;
        // layer_windows already all empty (data_len == 0) via empty_chain().
        let local = BridgeState::new(W);
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            // Operator's flag should be ignored on this path — chain
            // cursor is authoritative for the seed seqno.
            bootstrap_seqno: Some(999_999),
            window_size: W,
            anchor_level: 1,
        }) {
            StartupDecision::Cold {
                policy,
            } => {
                assert_eq!(policy, SeedPolicy::Explicit(seed_seqno));
            },
            d => panic!("expected Cold(Explicit({seed_seqno})), got {d:?}"),
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
            anchor_level: 1,
        }) {
            StartupDecision::Resurrect {
                fresh_state,
            } => {
                assert!(fresh_state.initialized);
                assert_eq!(fresh_state.stored_last_seen_block_seq_no, 10);
                assert_eq!(fresh_state.stored_bk_set_commitment, [7u8; 32]);
            },
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
            anchor_level: 1,
        }) {
            StartupDecision::Resurrect {
                fresh_state,
            } => {
                assert_eq!(fresh_state.stored_last_seen_block_seq_no, 20);
            },
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
            anchor_level: 1,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(reason.contains("last_seen=20"), "reason: {reason}");
                assert!(reason.contains("genesis"), "reason: {reason}");
            },
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_cursor_matches_but_bk_commit_diverges() {
        let src = advanced_state(10);
        let mut chain = snapshot_as_chain(&src);
        // Perturb only the bk-set commitment.
        chain.bk_set_commitment = [0xAA; 32];
        match decide(DecideInputs {
            local: &src,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 1,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(
                    reason.contains("bk-set commitment diverges"),
                    "reason: {reason}"
                );
            },
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
            anchor_level: 1,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(
                    reason.contains("per-layer windows diverge"),
                    "reason: {reason}"
                );
            },
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_on_window_size_mismatch() {
        // Local built with W=4, chain read with W=8 (contract deployed with
        // a different W). Must be rejected before any of the semantic arms.
        let local = BridgeState::new(4);
        let wide_lw = || HistoryWindow {
            data: vec![[0u8; 32]; 8],
            heights: vec![0u64; 8],
            data_len: 0,
            write_cursor: 0,
            last_height: 0,
        };
        let layer_windows: [HistoryWindow; MAX_LAYERS] = std::array::from_fn(|_| wide_lw());
        let chain = EthBridgeContractState {
            last_seen_block_seq_no: 0,
            bk_set_commitment: [0u8; 32],
            last_bk_set_update_seq_no: 0,
            prev_bk_set_commitment: [0u8; 32],
            genesis_prev_max_level_layer_hash: [0u8; 32],
            layer_windows,
        };
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: 4,
            anchor_level: 1,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(reason.contains("window width"), "reason: {reason}");
            },
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    // Chain-shape helper for anchor-level-vs-chain-shape tests below. Builds
    // an `EthBridgeContractState` whose layer 1 window has a single populated
    // entry, and optionally a single populated entry in the layer 2 window
    // (`with_l2 = true`). Cursor / bk fields set to non-genesis values so
    // routing does not treat the state as an empty chain.
    fn chain_with_layer1_and_optional_l2(seq: u64, with_l2: bool) -> EthBridgeContractState {
        let mut chain = empty_chain();
        chain.last_seen_block_seq_no = seq;
        chain.bk_set_commitment = [7u8; 32];
        chain.layer_windows[0].data_len = 1;
        chain.layer_windows[0].data[0] = [0x11; 32];
        chain.layer_windows[0].heights[0] = seq;
        chain.layer_windows[0].last_height = seq;
        if with_l2 {
            chain.layer_windows[1].data_len = 1;
            chain.layer_windows[1].data[0] = [0x22; 32];
            chain.layer_windows[1].heights[0] = seq;
            chain.layer_windows[1].last_height = seq;
        }
        chain
    }

    #[test]
    fn stop_when_cfg_l1_but_chain_l2_advanced_before_resurrect_save() {
        // Fresh install (local uninitialized) pointed at an L2-advanced
        // contract while the operator forgot `--anchor-level 2`. Must
        // refuse before `decide()` would otherwise choose `Resurrect`
        // and persist a `state.anchor_level=1` mirroring L2 windows.
        let chain = chain_with_layer1_and_optional_l2(10, true);
        let local = BridgeState::new(W);
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 1,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(reason.contains("layer_windows[2]"), "reason: {reason}");
                assert!(reason.contains("--anchor-level 2"), "reason: {reason}");
            },
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn stop_when_cfg_l2_but_chain_l1_only_at_w2_boundary() {
        // W=4 → W² = 16. Chain has last_seen=16 (W²-aligned, so any upstream
        // stride-modulus guard passes) but only layer 1 was ever populated —
        // classic "L1-driven contract that happens to sit on a W² multiple".
        // An L2-configured daemon must refuse rather than adopt a chain
        // snapshot missing the layer it was launched to prove.
        let chain = chain_with_layer1_and_optional_l2(16, false);
        assert_eq!(chain.last_seen_block_seq_no % (W as u64 * W as u64), 0);
        let local = BridgeState::new(W);
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 2,
        }) {
            StartupDecision::Stop {
                reason,
            } => {
                assert!(
                    reason.contains("only ever been driven by an L1"),
                    "reason: {reason}",
                );
                assert!(reason.contains("--anchor-level 1"), "reason: {reason}");
            },
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn cfg_l2_does_not_refuse_post_deploy_empty_windows() {
        // Regression guard for the L2 branch: a just-deployed L2 contract
        // has a non-zero cursor (constructor stamps genesis seed_seqno) but
        // every layer window is empty. Must route to Cold, not Stop.
        let seed_seqno: u64 = 16;
        let mut chain = empty_chain();
        chain.last_seen_block_seq_no = seed_seqno;
        chain.bk_set_commitment = [7u8; 32];
        let local = BridgeState::new(W);
        match decide(DecideInputs {
            local: &local,
            chain: &chain,
            bootstrap_seqno: None,
            window_size: W,
            anchor_level: 2,
        }) {
            StartupDecision::Cold {
                policy,
            } => {
                assert_eq!(policy, SeedPolicy::Explicit(seed_seqno));
            },
            d => panic!("expected Cold, got {d:?}"),
        }
    }
}
