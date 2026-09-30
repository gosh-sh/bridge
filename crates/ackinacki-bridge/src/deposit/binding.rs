//! Which transaction is this operation's.
//!
//! Equal `(sender, recipient, amount)` does not make two deposits the
//! same one: after `--abandon`, the user may have made the same deposit
//! again as another operation. So an operation is bound to a nonce slot,
//! and a transaction or a deposit another operation has claimed is never
//! a candidate.

use std::collections::HashSet;

use alloy_primitives::{Address, Bytes, B256, U256};

use crate::deposit::store::OpRecord;

/// A transaction seen on chain that might be an operation's deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The transaction hash.
    pub tx_hash: B256,
    /// The sender's nonce the transaction used.
    pub nonce: u64,
    /// The transaction's sender.
    pub from: Address,
    /// The transaction's calldata.
    pub input: Bytes,
    /// The receipt says the transaction succeeded.
    pub status_ok: bool,
    /// The receipt holds a `Deposit` log of the bridge.
    pub has_bridge_deposit: bool,
    /// The id of the deposit the log carries, when it has been read.
    pub deposit_id: Option<U256>,
}

/// What the other operations have already claimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    /// Transaction hashes known to belong to another operation.
    pub tx_hashes: HashSet<B256>,
    /// Deposit ids known to belong to another operation.
    pub deposit_ids: HashSet<U256>,
}

/// The claims of every operation except `me` on the same chain and bridge.
pub fn claims_of_others(recs: &[OpRecord], me: &str, chain_id: u64, bridge: Address) -> Claims {
    let mut c = Claims::default();
    for r in recs
        .iter()
        .filter(|r| r.op_id != me && r.params.chain_id == chain_id && r.params.bridge == bridge)
    {
        if let Some(t) = r.tx {
            c.tx_hashes.insert(t.tx_hash);
        }
        if let Some(h) = r.request.as_ref().and_then(|q| q.wallet_hash) {
            c.tx_hashes.insert(h);
        }
        if let Some(d) = &r.deposit {
            c.deposit_ids.insert(d.deposit_id);
        }
    }
    c
}

/// The outcome of looking for an operation's transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// This transaction is the operation's.
    Bind(B256),
    /// Several transactions, or none that can be trusted, fit; the user
    /// decides.
    Ambiguous {
        /// The transactions that fit.
        hashes: Vec<B256>,
        /// Why nothing was bound.
        why: String,
    },
    /// Nothing fits yet.
    NotYet,
}

/// A successful bridge deposit from this operation's sender that no other
/// operation has claimed, by transaction or by deposit id. Whether it
/// matches the request is NOT part of this: the nonce slot is the identity,
/// and a deposit in the slot with another amount or recipient is still this
/// operation's transaction — the later check reports it as a mismatch,
/// never as "nothing was deposited".
fn is_candidate(op: &OpRecord, c: &Candidate, others: &Claims) -> bool {
    Some(c.from) == op.from
        && c.status_ok
        && c.has_bridge_deposit
        && !others.tx_hashes.contains(&c.tx_hash)
        && !c
            .deposit_id
            .is_some_and(|d| others.deposit_ids.contains(&d))
}

/// Only for naming deposits outside the slot that look like this request.
fn matches_request(op: &OpRecord, c: &Candidate) -> bool {
    op.request.as_ref().is_some_and(|q| q.calldata == c.input)
}

/// The nonce slot the operation owns, when it is known.
fn slot_of(op: &OpRecord) -> Option<u64> {
    match (op.tx, &op.request) {
        (Some(t), _) => Some(t.tx_nonce),
        (None, Some(_)) if op.abandoned_ever => None,
        (None, Some(q)) => Some(q.nonce_before),
        (None, None) => None,
    }
}

/// Picks the operation's transaction among the candidates.
pub fn decide(op: &OpRecord, candidates: &[Candidate], others: &Claims) -> Binding {
    let cands: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| is_candidate(op, c, others))
        .collect();
    let slot = slot_of(op);
    if let Some(n) = slot {
        let at_slot: Vec<B256> = cands
            .iter()
            .filter(|c| c.nonce == n)
            .map(|c| c.tx_hash)
            .collect();
        match at_slot.as_slice() {
            [one] => return Binding::Bind(*one),
            [] => {},
            many => {
                return Binding::Ambiguous {
                    hashes: many.to_vec(),
                    why: format!("several deposits use nonce {n}"),
                }
            },
        }
    }
    let alike: Vec<B256> = cands
        .iter()
        .filter(|c| matches_request(op, c))
        .map(|c| c.tx_hash)
        .collect();
    match (slot, alike.is_empty()) {
        (_, true) => Binding::NotYet,
        (None, false) => Binding::Ambiguous {
            hashes: alike,
            why: "this operation was released with --abandon before its transaction was known; \
                  name its transaction with --tx-hash"
                .into(),
        },
        (Some(n), false) => Binding::Ambiguous {
            hashes: alike,
            why: format!(
                "deposits like this one exist, but none uses nonce {n}, the one this operation \
                 requested"
            ),
        },
    }
}

/// Checks a transaction the user named with `--tx-hash`: the nonce and
/// uniqueness rules are skipped, the rest is not.
pub fn check_explicit(
    op: &OpRecord,
    cand: &Candidate,
    others: &Claims,
    unresolved_others: &[&OpRecord],
) -> Result<(), String> {
    if !is_candidate(op, cand, others) {
        return Err(format!(
            "{} is not a successful bridge deposit from {:?}, or another operation has already \
             claimed it",
            cand.tx_hash, op.from
        ));
    }
    for o in unresolved_others {
        // By the other operation's own rules its slot binds any deposit.
        if o.from == op.from && slot_of(o) == Some(cand.nonce) {
            return Err(format!(
                "{} may belong to operation {}, whose outcome is unknown; resume that operation \
                 first",
                cand.tx_hash, o.op_id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::*;
    use crate::deposit::store::{DepositInfo, OpParams, OpRecord, OpStage, RequestInfo, TxClaim};

    const FROM: Address = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");

    fn calldata() -> Bytes {
        Bytes::from(vec![0xa4, 0x1d, 0x02, 0x29, 1, 2, 3])
    }

    fn op(id: &str, nonce_before: u64) -> OpRecord {
        let mut r = OpRecord::new(
            id.into(),
            OpParams {
                chain_id: 11_155_111,
                bridge: BRIDGE,
                to: "x".into(),
                amount_units: 1,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            "bd44".into(),
        );
        r.from = Some(FROM);
        r.stage = OpStage::Requested;
        r.request = Some(RequestInfo {
            nonce_before,
            from_block: 1,
            calldata: calldata(),
            wallet_hash: None,
        });
        r
    }

    fn cand(h: u8, nonce: u64) -> Candidate {
        Candidate {
            tx_hash: B256::repeat_byte(h),
            nonce,
            from: FROM,
            input: calldata(),
            status_ok: true,
            has_bridge_deposit: true,
            deposit_id: None,
        }
    }

    #[test]
    fn a_single_candidate_at_nonce_before_binds_a_never_abandoned_operation() {
        assert_eq!(
            decide(&op("A", 7), &[cand(1, 7)], &Claims::default()),
            Binding::Bind(B256::repeat_byte(1))
        );
    }

    #[test]
    fn a_replacement_with_the_same_nonce_is_found() {
        let mut a = op("A", 7);
        a.stage = OpStage::Signed;
        a.tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        // The wallet sped it up: new hash, same nonce.
        assert_eq!(
            decide(&a, &[cand(2, 7)], &Claims::default()),
            Binding::Bind(B256::repeat_byte(2))
        );
    }

    #[test]
    fn a_candidate_at_another_nonce_is_ambiguous() {
        let got = decide(&op("A", 7), &[cand(1, 9)], &Claims::default());
        assert!(matches!(got, Binding::Ambiguous { .. }), "{got:?}");
    }

    #[test]
    fn a_deposit_claimed_by_another_operation_is_never_taken() {
        // A lost the wallet's answer, was abandoned; B deposited the same
        // amount to the same recipient and claimed its transaction.
        let mut a = op("A", 7);
        a.stage = OpStage::Abandoned;
        a.abandoned_ever = true;
        let mut b = op("B", 7);
        b.stage = OpStage::Signed;
        b.tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(5),
            tx_nonce: 7,
        });
        let others = claims_of_others(&[a.clone(), b], "A", 11_155_111, BRIDGE);
        assert_ne!(
            decide(&a, &[cand(5, 7)], &others),
            Binding::Bind(B256::repeat_byte(5))
        );
    }

    #[test]
    fn an_abandoned_operation_without_identity_is_never_bound_automatically() {
        let mut a = op("A", 7);
        a.stage = OpStage::Abandoned;
        a.abandoned_ever = true;
        let b = op("B", 7); // B requested with the same nonce_before and died before the hash
        let t = cand(9, 7);
        assert!(matches!(
            decide(&a, &[t.clone()], &Claims::default()),
            Binding::Ambiguous { .. }
        ));
        // --resume A --tx-hash T: T is B's candidate by B's own rules.
        let e = check_explicit(&a, &t, &Claims::default(), &[&b]).unwrap_err();
        assert!(e.contains("B"), "{e}");
        // --resume B binds it.
        assert_eq!(
            decide(&b, &[t], &Claims::default()),
            Binding::Bind(B256::repeat_byte(9))
        );
    }

    #[test]
    fn a_candidate_must_be_a_successful_bridge_deposit_from_the_sender() {
        let mut reverted = cand(1, 7);
        reverted.status_ok = false;
        let mut no_log = cand(3, 7);
        no_log.has_bridge_deposit = false;
        let mut other_sender = cand(4, 7);
        other_sender.from = Address::ZERO;
        assert_eq!(
            decide(
                &op("A", 7),
                &[reverted, no_log, other_sender],
                &Claims::default()
            ),
            Binding::NotYet
        );
    }

    #[test]
    fn a_deposit_in_the_slot_that_differs_from_the_request_is_still_bound() {
        // An EIP-681 wallet mangled the arguments, or a speed-up changed the amount:
        // step 5 must see it and report a mismatch, not "nothing was deposited".
        let mut other_data = cand(2, 7);
        other_data.input = Bytes::from(vec![0u8; 4]);
        assert_eq!(
            decide(&op("A", 7), &[other_data.clone()], &Claims::default()),
            Binding::Bind(B256::repeat_byte(2))
        );
        let mut signed = op("A", 7);
        signed.stage = OpStage::Signed;
        signed.tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(1),
            tx_nonce: 7,
        });
        assert_eq!(
            decide(&signed, &[other_data], &Claims::default()),
            Binding::Bind(B256::repeat_byte(2))
        );
    }

    #[test]
    fn an_unrelated_deposit_at_another_nonce_is_not_even_named() {
        let mut other = cand(2, 9);
        other.input = Bytes::from(vec![0u8; 4]);
        assert_eq!(
            decide(&op("A", 7), &[other], &Claims::default()),
            Binding::NotYet
        );
    }

    #[test]
    fn an_explicit_hash_skips_nonce_and_uniqueness_but_not_the_rest() {
        let a = op("A", 7);
        check_explicit(&a, &cand(1, 42), &Claims::default(), &[]).unwrap();
        let mut claimed = Claims::default();
        claimed.tx_hashes.insert(B256::repeat_byte(1));
        assert!(check_explicit(&a, &cand(1, 42), &claimed, &[]).is_err());
    }

    #[test]
    fn a_candidate_whose_deposit_id_another_operation_claimed_is_excluded() {
        let mut b = op("B", 7);
        b.stage = OpStage::Credited;
        b.deposit = Some(DepositInfo {
            deposit_id: U256::from(77u64),
            block_number: 5,
            block_hash: B256::repeat_byte(8),
            block_log_index: 0,
            receipt_log_index: 0,
            access_list_rlp_len: 0,
            voucher_account: "v".into(),
        });
        let others = claims_of_others(&[op("A", 7), b], "A", 11_155_111, BRIDGE);
        assert!(others.deposit_ids.contains(&U256::from(77u64)));
        let mut t = cand(6, 7);
        t.deposit_id = Some(U256::from(77u64));
        assert_eq!(decide(&op("A", 7), &[t.clone()], &others), Binding::NotYet);
        assert!(check_explicit(&op("A", 7), &t, &others, &[]).is_err());
        // Another deposit id in the same slot is still bound.
        let mut free = cand(6, 7);
        free.deposit_id = Some(U256::from(78u64));
        assert_eq!(
            decide(&op("A", 7), &[free], &others),
            Binding::Bind(B256::repeat_byte(6))
        );
    }
}
