//! Pure-Rust reference of the on-chain `withdrawByProofBundle` gate.
//!
//! A "withdrawal bundle" is one `BridgeEventFinalProof` snark plus `n`
//! `BridgeMultiHopProof` snarks, where `n = ceil(L / H_HOPS_PER_PROOF)` for a
//! cross-thread claim, or `n = 0` for a same-thread claim (the anchor block
//! is the event block, no L7 walk needed).
//!
//! The on-chain contract in `contracts/ethereum/src/AckiNackiBridge.sol` will
//! accept a bundle iff all of the following hold (in this order):
//!
//!   1. Structural sanity — the FinalProof is at index 0, each snark has the
//!      declared PI length, and `n = bundle.len() - 1 <= N_BUNDLE_MAX`.
//!   2. If `n == 0`, `FinalProof.PI[PUB_X_BLOCK_ID] == PI[PUB_Y_BLOCK_ID]` (a
//!      same-thread claim: no hop chain, so X and Y must be the same block).
//!   3. If `n > 0`:
//!      * head linkage: `FinalProof.PI[PUB_X_BLOCK_ID] ==
//!        MultiHop[0].PI[HOP_START_BLOCK_ID]`;
//!      * hop adjacency: `MultiHop[i].PI[HOP_END_BLOCK_ID] ==
//!        MultiHop[i+1].PI[HOP_START_BLOCK_ID]` for every `i`;
//!      * tail linkage: `MultiHop[last].PI[HOP_END_BLOCK_ID] ==
//!        FinalProof.PI[PUB_Y_BLOCK_ID]`.
//!   4. `_isKnownLayerAnchor(final_root, anchor_layer)` accepts the pair
//!      `(FinalProof.PI[PUB_FINAL_ROOT], FinalProof.PI[PUB_ANCHOR_LAYER])`.
//!      Off-chain callers wire this through a closure; on-chain the check is
//!      `AckiNackiBridge._isKnownLayerAnchor`.
//!
//! This module is the *specification-executable* version of that gate — the
//! same relationship `dexdo-halo2-kit/dex-halo2-circuit/src/bundle_verifier.rs`
//! has to `RootPN.sol`. It performs **no** halo2 verification of the
//! underlying snarks; that is the aggregator's job. Only the public-input
//! gate is enforced here.
//!
//! There is no salt binding: the bridge withdrawal path never anonymises the
//! event-block linkage, so `salt_commitment` / `salted_*` slots are absent
//! from both the FinalProof and the MultiHopProof PI layouts. This is a
//! strict subset of the DEX bundle logic; see the migration plan §6 delta
//! table.

use gosh_dense_balanced_tree::fr_to_bytes;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

use crate::{
    bridge_event_final_proof::{
        PUB_ANCHOR_LAYER, PUB_FINAL_ROOT, PUB_X_BLOCK_ID, PUB_Y_BLOCK_ID, TOTAL_PUBLIC_INPUTS,
    },
    multi_hop_proof::MULTI_HOP_PUBLIC_LEN,
    multi_hop_witness::N_BUNDLE_MAX,
};

// ---------------------------------------------------------------------------
// PI-layout constants (mirror the two circuits' declared publics)
// ---------------------------------------------------------------------------

/// `BridgeEventFinalProof` PI vector length.
pub const FINAL_LEN: usize = TOTAL_PUBLIC_INPUTS;
/// `BridgeMultiHopProof` PI vector length.
pub const MULTI_HOP_LEN: usize = MULTI_HOP_PUBLIC_LEN;

const FINAL_LENGTHS: &[usize] = &[FINAL_LEN];
const MULTI_HOP_LENGTHS: &[usize] = &[MULTI_HOP_LEN];

/// FinalProof PI offsets. Re-exported alias for readability at call sites.
pub mod final_offset {
    pub use crate::bridge_event_final_proof::{
        PUB_ANCHOR_LAYER as ANCHOR_LAYER, PUB_FINAL_ROOT as FINAL_ROOT,
        PUB_X_BLOCK_ID as X_BLOCK_ID, PUB_Y_BLOCK_ID as Y_BLOCK_ID,
    };
}

/// MultiHopProof PI offsets. Slot 0 is the hop-chain start, slot 1 the end.
pub mod multi_hop_offset {
    pub const HOP_START_BLOCK_ID: usize = 0;
    pub const HOP_END_BLOCK_ID: usize = 1;
}

// ---------------------------------------------------------------------------
// Bundle proof types
// ---------------------------------------------------------------------------

/// Tag identifying which snark a `BundleProof` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    /// `BridgeEventFinalProof`. 13 instances.
    Final,
    /// `BridgeMultiHopProof`. 2 instances.
    MultiHop,
}

/// One snark contributing to a withdrawal bundle. `instances` is the exact
/// public-input vector produced by the snark, in declaration order.
#[derive(Debug, Clone)]
pub struct BundleProof {
    pub kind: ProofKind,
    pub instances: Vec<Fr>,
}

impl BundleProof {
    pub fn new_final(instances: Vec<Fr>) -> Self {
        Self {
            kind: ProofKind::Final,
            instances,
        }
    }
    pub fn new_multi_hop(instances: Vec<Fr>) -> Self {
        Self {
            kind: ProofKind::MultiHop,
            instances,
        }
    }
}

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Reasons the on-chain gate would reject a bundle.
#[derive(Debug, PartialEq, Eq)]
pub enum BundleError {
    /// The bundle is empty or the FinalProof is missing.
    MissingFinalProof,
    /// More than one FinalProof in the bundle.
    DuplicateFinalProof { count: usize },
    /// A FinalProof exists but is not at index 0.
    FinalProofNotFirst { found_at: usize },
    /// A proof's instance-vector length doesn't match its declared `kind`.
    BadInstanceLen {
        proof_index: usize,
        kind: ProofKind,
        got: usize,
        expected_one_of: &'static [usize],
    },
    /// The bundle carries more MultiHop snarks than the circuit family
    /// supports.
    HopBundleLengthOverflow { got: usize, max: usize },
    /// Same-thread claim (`n == 0`) but FinalProof declares `X != Y`.
    SameThreadEndpointsMismatch { x_block_id: Fr, y_block_id: Fr },
    /// `FinalProof.X_BLOCK_ID != MultiHop[0].HOP_START_BLOCK_ID`.
    HopChainHeadMismatch { x_block_id: Fr, first_hop_start: Fr },
    /// `MultiHop[last].HOP_END_BLOCK_ID != FinalProof.Y_BLOCK_ID`.
    HopChainTailMismatch { last_hop_end: Fr, y_block_id: Fr },
    /// `MultiHop[at].HOP_END_BLOCK_ID != MultiHop[at + 1].HOP_START_BLOCK_ID`.
    AdjacentHopBlockIdMismatch {
        at: usize,
        prev_end: Fr,
        next_start: Fr,
    },
    /// `_isKnownLayerAnchor(final_root, anchor_layer)` rejected the pair.
    /// `final_root` is the byte encoding of `FinalProof.PI[PUB_FINAL_ROOT]`
    /// as produced by `fr_to_bytes` (little-endian Fr repr).
    /// `anchor_layer` is the low byte of `FinalProof.PI[PUB_ANCHOR_LAYER]`.
    AnchorNotInLayerWindow {
        final_root: [u8; 32],
        anchor_layer: u8,
    },
    /// `FinalProof.PI[PUB_ANCHOR_LAYER]` is not representable in a `u8` —
    /// the upper 31 bytes of its LE Fr repr must be zero. Off-chain
    /// callers should have upstream-rejected such a proof; the check is
    /// here so the mock signals it explicitly rather than truncating.
    AnchorLayerOutOfRange { anchor_layer_fr: Fr },
}

// ---------------------------------------------------------------------------
// Field accessors (layout-aware, length-checked)
// ---------------------------------------------------------------------------

fn assert_final(p: &BundleProof, proof_index: usize) -> Result<&[Fr], BundleError> {
    if p.kind != ProofKind::Final || p.instances.len() != FINAL_LEN {
        return Err(BundleError::BadInstanceLen {
            proof_index,
            kind: p.kind,
            got: p.instances.len(),
            expected_one_of: FINAL_LENGTHS,
        });
    }
    Ok(&p.instances)
}

fn assert_multi_hop(p: &BundleProof, proof_index: usize) -> Result<&[Fr], BundleError> {
    if p.kind != ProofKind::MultiHop || p.instances.len() != MULTI_HOP_LEN {
        return Err(BundleError::BadInstanceLen {
            proof_index,
            kind: p.kind,
            got: p.instances.len(),
            expected_one_of: MULTI_HOP_LENGTHS,
        });
    }
    Ok(&p.instances)
}

pub fn final_x_block_id(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_final(p, proof_index)?[PUB_X_BLOCK_ID])
}

pub fn final_y_block_id(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_final(p, proof_index)?[PUB_Y_BLOCK_ID])
}

pub fn final_final_root(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_final(p, proof_index)?[PUB_FINAL_ROOT])
}

pub fn final_anchor_layer_fr(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_final(p, proof_index)?[PUB_ANCHOR_LAYER])
}

pub fn multi_hop_start(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_multi_hop(p, proof_index)?[multi_hop_offset::HOP_START_BLOCK_ID])
}

pub fn multi_hop_end(p: &BundleProof, proof_index: usize) -> Result<Fr, BundleError> {
    Ok(assert_multi_hop(p, proof_index)?[multi_hop_offset::HOP_END_BLOCK_ID])
}

// ---------------------------------------------------------------------------
// Bundle verifier
// ---------------------------------------------------------------------------

/// Run every `withdrawByProofBundle` acceptance check against an in-Rust
/// bundle. Returns `Ok(())` iff the on-chain gate would accept.
///
/// `anchor_ok(final_root, anchor_layer)` mirrors
/// `AckiNackiBridge._isKnownLayerAnchor`. In tests, pass a closure that
/// consults a fixed set of known layer anchors.
///
/// This function does **not** verify the underlying halo2 snarks. That is
/// the aggregator's job (SHPLONK verify per snark). The bundle-acceptance
/// gate runs *on top of* successful snark verification.
pub fn verify_bundle(
    bundle: &[BundleProof],
    anchor_ok: impl Fn(&[u8; 32], u8) -> bool,
) -> Result<(), BundleError> {
    // ---- (1) structural sanity -----------------------------------------
    let final_count = bundle.iter().filter(|p| p.kind == ProofKind::Final).count();
    if final_count == 0 {
        return Err(BundleError::MissingFinalProof);
    }
    if final_count > 1 {
        return Err(BundleError::DuplicateFinalProof {
            count: final_count,
        });
    }
    // `bundle` is non-empty (final_count == 1).
    if bundle[0].kind != ProofKind::Final {
        let found_at = bundle
            .iter()
            .position(|p| p.kind == ProofKind::Final)
            .unwrap();
        return Err(BundleError::FinalProofNotFirst {
            found_at,
        });
    }
    // Length-check every slot up front so downstream accessors can assume the
    // shape is well-formed.
    let _ = assert_final(&bundle[0], 0)?;
    for (i, p) in bundle.iter().enumerate().skip(1) {
        let _ = assert_multi_hop(p, i)?;
    }

    let hop_count = bundle.len() - 1;
    if hop_count > N_BUNDLE_MAX {
        return Err(BundleError::HopBundleLengthOverflow {
            got: hop_count,
            max: N_BUNDLE_MAX,
        });
    }

    let x_block_id = final_x_block_id(&bundle[0], 0)?;
    let y_block_id = final_y_block_id(&bundle[0], 0)?;

    // ---- (2) same-thread claim: no hops ⇒ X == Y -----------------------
    if hop_count == 0 {
        if x_block_id != y_block_id {
            return Err(BundleError::SameThreadEndpointsMismatch {
                x_block_id,
                y_block_id,
            });
        }
    } else {
        // ---- (3) cross-thread claim: head, adjacency, tail --------------
        let first_hop_start = multi_hop_start(&bundle[1], 1)?;
        if x_block_id != first_hop_start {
            return Err(BundleError::HopChainHeadMismatch {
                x_block_id,
                first_hop_start,
            });
        }

        for i in 1..bundle.len() - 1 {
            let prev_end = multi_hop_end(&bundle[i], i)?;
            let next_start = multi_hop_start(&bundle[i + 1], i + 1)?;
            if prev_end != next_start {
                return Err(BundleError::AdjacentHopBlockIdMismatch {
                    at: i,
                    prev_end,
                    next_start,
                });
            }
        }

        let last_idx = bundle.len() - 1;
        let last_hop_end = multi_hop_end(&bundle[last_idx], last_idx)?;
        if last_hop_end != y_block_id {
            return Err(BundleError::HopChainTailMismatch {
                last_hop_end,
                y_block_id,
            });
        }
    }

    // ---- (4) anchor pair is in the known-layer window ------------------
    let final_root_fr = final_final_root(&bundle[0], 0)?;
    let anchor_layer_fr = final_anchor_layer_fr(&bundle[0], 0)?;
    let final_root_bytes = fr_to_bytes(final_root_fr);
    let anchor_layer_bytes = fr_to_bytes(anchor_layer_fr);
    // Anchor layer is a single byte (spec: MAX_LAYER_HASHES = 10). Reject
    // anything wider than a byte so the on-chain-mirror truncation is
    // explicit.
    if anchor_layer_bytes[1..].iter().any(|&b| b != 0) {
        return Err(BundleError::AnchorLayerOutOfRange {
            anchor_layer_fr,
        });
    }
    let anchor_layer = anchor_layer_bytes[0];
    if !anchor_ok(&final_root_bytes, anchor_layer) {
        return Err(BundleError::AnchorNotInLayerWindow {
            final_root: final_root_bytes,
            anchor_layer,
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- helpers ------------------------------------------------------------

    /// Build a FinalProof PI vector with the given `x_block_id`,
    /// `y_block_id`, `final_root`, and `anchor_layer`. Other slots are
    /// filled with distinguishable sentinels so tests can spot accidental
    /// cross-talk with the acceptance gate.
    fn make_final(x: Fr, y: Fr, final_root: Fr, anchor_layer: u8) -> BundleProof {
        let mut pi = vec![Fr::from(0u64); FINAL_LEN];
        pi[0] = Fr::from(201u64); // token_id sentinel
        pi[1] = Fr::from(202u64); // amount sentinel
        pi[2] = Fr::from(203u64); // recipient_hi sentinel
        pi[3] = Fr::from(204u64); // recipient_lo sentinel
        pi[4] = Fr::from(205u64); // dst_chain_id sentinel
        pi[5] = Fr::from(206u64); // sender_acc_fr sentinel
        pi[6] = Fr::from(207u64); // dapp_fr sentinel
        pi[7] = Fr::from(208u64); // acc_fr sentinel
        pi[8] = Fr::from(209u64); // nullifier sentinel
        pi[PUB_FINAL_ROOT] = final_root;
        pi[PUB_ANCHOR_LAYER] = Fr::from(anchor_layer as u64);
        pi[PUB_X_BLOCK_ID] = x;
        pi[PUB_Y_BLOCK_ID] = y;
        BundleProof::new_final(pi)
    }

    fn make_hop(start: Fr, end: Fr) -> BundleProof {
        BundleProof::new_multi_hop(vec![start, end])
    }

    /// An accept-all anchor callback for cases where the anchor gate is not
    /// under test.
    fn accept_any_anchor(_root: &[u8; 32], _layer: u8) -> bool {
        true
    }

    // -- happy paths --------------------------------------------------------

    #[test]
    fn same_thread_bundle_verifies() {
        // n == 0: no hop chain. X == Y, anchor pair accepted.
        let block_id = Fr::from(0xB10CD1Du64);
        let final_root = Fr::from(0x123456u64);
        let anchor_layer = 3u8;
        let bundle = vec![make_final(block_id, block_id, final_root, anchor_layer)];

        // Anchor closure asserts exactly the pair we expect.
        let expected_root = fr_to_bytes(final_root);
        assert_eq!(
            verify_bundle(&bundle, |root, layer| {
                *root == expected_root && layer == anchor_layer
            }),
            Ok(())
        );
    }

    #[test]
    fn cross_thread_bundle_verifies() {
        // n == 4: full chain of the maximum canonical length.
        let final_root = Fr::from(0xDEADBEEFu64);
        let anchor_layer = 7u8;
        let p0 = Fr::from(1000u64);
        let p1 = Fr::from(1001u64);
        let p2 = Fr::from(1002u64);
        let p3 = Fr::from(1003u64);
        let p4 = Fr::from(1004u64);
        let bundle = vec![
            make_final(p0, p4, final_root, anchor_layer),
            make_hop(p0, p1),
            make_hop(p1, p2),
            make_hop(p2, p3),
            make_hop(p3, p4),
        ];
        assert_eq!(verify_bundle(&bundle, accept_any_anchor), Ok(()));
    }

    #[test]
    fn single_hop_bundle_verifies() {
        // n == 1: minimum cross-thread chain. Adjacency loop is empty.
        let final_root = Fr::from(1u64);
        let anchor_layer = 0u8;
        let x = Fr::from(100u64);
        let y = Fr::from(200u64);
        let bundle = vec![make_final(x, y, final_root, anchor_layer), make_hop(x, y)];
        assert_eq!(verify_bundle(&bundle, accept_any_anchor), Ok(()));
    }

    // -- (1) structural sanity ---------------------------------------------

    #[test]
    fn empty_bundle_rejected() {
        assert_eq!(
            verify_bundle(&[], accept_any_anchor),
            Err(BundleError::MissingFinalProof)
        );
    }

    #[test]
    fn missing_final_proof_rejected() {
        let bundle = vec![make_hop(Fr::from(1u64), Fr::from(2u64))];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::MissingFinalProof)
        );
    }

    #[test]
    fn duplicate_final_proof_rejected() {
        let bundle = vec![
            make_final(Fr::from(1u64), Fr::from(1u64), Fr::from(1u64), 0),
            make_final(Fr::from(1u64), Fr::from(1u64), Fr::from(1u64), 0),
        ];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::DuplicateFinalProof {
                count: 2
            })
        );
    }

    #[test]
    fn final_proof_not_first_rejected() {
        let bundle = vec![
            make_hop(Fr::from(1u64), Fr::from(2u64)),
            make_final(Fr::from(1u64), Fr::from(2u64), Fr::from(1u64), 0),
        ];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::FinalProofNotFirst {
                found_at: 1
            })
        );
    }

    #[test]
    fn final_bad_instance_len_rejected() {
        let bad = BundleProof::new_final(vec![Fr::from(1u64); 5]); // not FINAL_LEN
        match verify_bundle(&[bad], accept_any_anchor) {
            Err(BundleError::BadInstanceLen {
                proof_index,
                kind,
                got,
                ..
            }) => {
                assert_eq!(proof_index, 0);
                assert_eq!(kind, ProofKind::Final);
                assert_eq!(got, 5);
            },
            other => panic!("expected BadInstanceLen, got {:?}", other),
        }
    }

    #[test]
    fn multi_hop_bad_instance_len_rejected() {
        let bundle = vec![
            make_final(Fr::from(1u64), Fr::from(1u64), Fr::from(0u64), 0),
            BundleProof::new_multi_hop(vec![Fr::from(1u64); 3]), // not 2
        ];
        match verify_bundle(&bundle, accept_any_anchor) {
            Err(BundleError::BadInstanceLen {
                proof_index,
                kind,
                got,
                ..
            }) => {
                assert_eq!(proof_index, 1);
                assert_eq!(kind, ProofKind::MultiHop);
                assert_eq!(got, 3);
            },
            other => panic!("expected BadInstanceLen, got {:?}", other),
        }
    }

    #[test]
    fn hop_bundle_length_overflow_rejected() {
        // N_BUNDLE_MAX + 1 hops → overflow.
        let p = Fr::from(1u64);
        let mut bundle = vec![make_final(p, p, Fr::from(0u64), 0)];
        for _ in 0..(N_BUNDLE_MAX + 1) {
            bundle.push(make_hop(p, p));
        }
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::HopBundleLengthOverflow {
                got: N_BUNDLE_MAX + 1,
                max: N_BUNDLE_MAX,
            })
        );
    }

    // -- (2) same-thread endpoint mismatch ---------------------------------

    #[test]
    fn same_thread_endpoints_mismatch_rejected() {
        // n == 0 requires X == Y.
        let x = Fr::from(1u64);
        let y = Fr::from(2u64);
        let bundle = vec![make_final(x, y, Fr::from(0u64), 0)];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::SameThreadEndpointsMismatch {
                x_block_id: x,
                y_block_id: y
            })
        );
    }

    // -- (3) head, adjacency, tail -----------------------------------------

    #[test]
    fn hop_chain_head_mismatch_rejected() {
        let x = Fr::from(100u64);
        let wrong_start = Fr::from(999u64);
        let bundle = vec![
            make_final(x, Fr::from(200u64), Fr::from(0u64), 0),
            make_hop(wrong_start, Fr::from(200u64)),
        ];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::HopChainHeadMismatch {
                x_block_id: x,
                first_hop_start: wrong_start,
            })
        );
    }

    #[test]
    fn adjacent_hop_block_id_mismatch_rejected() {
        // Break adjacency between hops 1 → 2.
        let p0 = Fr::from(100u64);
        let p1 = Fr::from(101u64);
        let broken = Fr::from(999u64);
        let p3 = Fr::from(103u64);
        let bundle = vec![
            make_final(p0, p3, Fr::from(0u64), 0),
            make_hop(p0, p1),
            make_hop(broken, p3), // start != previous end
        ];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::AdjacentHopBlockIdMismatch {
                at: 1,
                prev_end: p1,
                next_start: broken,
            })
        );
    }

    #[test]
    fn hop_chain_tail_mismatch_rejected() {
        let p0 = Fr::from(100u64);
        let p1 = Fr::from(101u64);
        let hop_end = Fr::from(200u64);
        let wrong_y = Fr::from(999u64);
        let bundle = vec![
            make_final(p0, wrong_y, Fr::from(0u64), 0),
            make_hop(p0, p1),
            make_hop(p1, hop_end),
        ];
        assert_eq!(
            verify_bundle(&bundle, accept_any_anchor),
            Err(BundleError::HopChainTailMismatch {
                last_hop_end: hop_end,
                y_block_id: wrong_y,
            })
        );
    }

    // -- (4) anchor callback ------------------------------------------------

    #[test]
    fn anchor_not_in_layer_window_rejected() {
        // Same-thread bundle so the endpoint checks pass; anchor callback
        // rejects. Verifies the error carries the exact byte encoding.
        let block_id = Fr::from(0xB10CD1Du64);
        let final_root = Fr::from(0xC0FFEEu64);
        let anchor_layer = 5u8;
        let bundle = vec![make_final(block_id, block_id, final_root, anchor_layer)];
        let expected_root = fr_to_bytes(final_root);

        match verify_bundle(&bundle, |_root, _layer| false) {
            Err(BundleError::AnchorNotInLayerWindow {
                final_root,
                anchor_layer: al,
            }) => {
                assert_eq!(final_root, expected_root);
                assert_eq!(al, anchor_layer);
            },
            other => panic!("expected AnchorNotInLayerWindow, got {:?}", other),
        }
    }

    #[test]
    fn anchor_layer_out_of_range_rejected() {
        // Force an anchor_layer PI slot that doesn't fit in a byte. We build
        // the PI vector by hand to bypass `make_final`'s u8 cast.
        let mut pi = vec![Fr::from(0u64); FINAL_LEN];
        pi[PUB_X_BLOCK_ID] = Fr::from(1u64);
        pi[PUB_Y_BLOCK_ID] = Fr::from(1u64);
        pi[PUB_FINAL_ROOT] = Fr::from(0u64);
        pi[PUB_ANCHOR_LAYER] = Fr::from(0x100u64); // 256 — one bit past u8
        let bundle = vec![BundleProof::new_final(pi)];
        match verify_bundle(&bundle, accept_any_anchor) {
            Err(BundleError::AnchorLayerOutOfRange {
                anchor_layer_fr,
            }) => {
                assert_eq!(anchor_layer_fr, Fr::from(0x100u64));
            },
            other => panic!("expected AnchorLayerOutOfRange, got {:?}", other),
        }
    }
}
