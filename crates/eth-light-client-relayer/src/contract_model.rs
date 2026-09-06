//! Off-chain replica of `EthBeaconLightClient` control flow (no Halo2).
//!
//! Proof bytes are assumed valid: these tests pin the `require`s around head
//! movement, ancestry, sink re-push, and the weak-subjectivity hatch.

use std::collections::HashSet;

use crate::header_rlp::{keccak256, rlp_parent_hash};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LcError {
    StaleUpdate,
    NotProven,
    ReanchorNotArmed,
    ReanchorNoop,
    StalePeriod,
    CommitteeUnset,
    OwnerRotationDisabled,
    UnknownCheckpoint,
    BadAncestry,
    AncestryTooLong,
    WrongCommittee,
}

impl LcError {
    pub fn code(self) -> u16 {
        match self {
            Self::WrongCommittee => 242,
            Self::CommitteeUnset => 243,
            Self::StaleUpdate => 244,
            Self::StalePeriod => 245,
            Self::OwnerRotationDisabled => 246,
            Self::ReanchorNotArmed => 247,
            Self::ReanchorNoop => 248,
            Self::UnknownCheckpoint => 249,
            Self::BadAncestry => 250,
            Self::AncestryTooLong => 251,
            Self::NotProven => 252,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LightClient {
    pub finalized_slot: u64,
    pub exec_hash: [u8; 32],
    pub committee: [u8; 32],
    pub period: u64,
    pub owner_rotation_enabled: bool,
    pub re_anchors_applied: u64,
    pub proven: HashSet<[u8; 32]>,
    /// Each `_notifySink` / `rePushAnchor` (including first insert).
    pub sink_notifies: Vec<[u8; 32]>,
}

impl LightClient {
    pub fn bootstrap(committee: [u8; 32], period: u64) -> Self {
        Self {
            finalized_slot: 0,
            exec_hash: [0; 32],
            committee,
            period,
            owner_rotation_enabled: true,
            re_anchors_applied: 0,
            proven: HashSet::new(),
            sink_notifies: Vec::new(),
        }
    }

    fn push_exec(&mut self, h: [u8; 32]) {
        if self.proven.contains(&h) {
            return;
        }
        self.proven.insert(h);
        self.sink_notifies.push(h);
    }

    /// `submitUpdate` after a successful ZK verify (committee already checked).
    pub fn submit_update(
        &mut self,
        slot: u64,
        exec: [u8; 32],
        committee: [u8; 32],
    ) -> Result<UpdateKind, LcError> {
        if self.committee == [0; 32] {
            return Err(LcError::CommitteeUnset);
        }
        if committee != self.committee {
            return Err(LcError::WrongCommittee);
        }
        let advancing = slot > self.finalized_slot;
        let late = slot < self.finalized_slot && !self.proven.contains(&exec);
        if !advancing && !late {
            return Err(LcError::StaleUpdate);
        }
        if advancing {
            self.finalized_slot = slot;
            self.exec_hash = exec;
            self.push_exec(exec);
            Ok(UpdateKind::Advanced)
        } else {
            self.push_exec(exec);
            Ok(UpdateKind::Backfilled)
        }
    }

    pub fn submit_ancestry(&mut self, header_rlps: &[Vec<u8>]) -> Result<u64, LcError> {
        if header_rlps.len() < 2 {
            return Err(LcError::BadAncestry);
        }
        if header_rlps.len() > 32 {
            return Err(LcError::AncestryTooLong);
        }
        let checkpoint = keccak256(&header_rlps[0]);
        if !self.proven.contains(&checkpoint) {
            return Err(LcError::UnknownCheckpoint);
        }
        let mut want = rlp_parent_hash(&header_rlps[0]).map_err(|_| LcError::BadAncestry)?;
        let mut added = 0u64;
        for rlp in &header_rlps[1..] {
            let h = keccak256(rlp);
            if h != want {
                return Err(LcError::BadAncestry);
            }
            if !self.proven.contains(&h) {
                self.push_exec(h);
                added += 1;
            }
            want = rlp_parent_hash(rlp).map_err(|_| LcError::BadAncestry)?;
        }
        Ok(added)
    }

    pub fn re_push_anchor(&mut self, block_hash: [u8; 32]) -> Result<(), LcError> {
        if !self.proven.contains(&block_hash) {
            return Err(LcError::NotProven);
        }
        self.sink_notifies.push(block_hash);
        Ok(())
    }

    pub fn set_committee(&mut self, commitment: [u8; 32], period: u64) -> Result<(), LcError> {
        if !self.owner_rotation_enabled {
            return Err(LcError::OwnerRotationDisabled);
        }
        self.committee = commitment;
        self.period = period;
        Ok(())
    }

    pub fn disable_owner_rotation(&mut self) -> Result<(), LcError> {
        if self.committee == [0; 32] {
            return Err(LcError::CommitteeUnset);
        }
        self.owner_rotation_enabled = false;
        Ok(())
    }

    pub fn re_anchor_committee(
        &mut self,
        commitment: [u8; 32],
        period: u64,
    ) -> Result<(), LcError> {
        if self.owner_rotation_enabled {
            return Err(LcError::ReanchorNotArmed);
        }
        if self.committee == [0; 32] || commitment == [0; 32] {
            return Err(LcError::CommitteeUnset);
        }
        if period < self.period {
            return Err(LcError::StalePeriod);
        }
        if commitment == self.committee && period == self.period {
            return Err(LcError::ReanchorNoop);
        }
        self.committee = commitment;
        self.period = period;
        self.re_anchors_applied += 1;
        Ok(())
    }

    /// `updateCode` / `onCodeUpgrade` keep committee, head, proven set, hatch
    /// counter.
    pub fn snapshot(&self) -> Self {
        self.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    Advanced,
    Backfilled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header_rlp::{dummy_linked_headers, keccak256};

    fn lc() -> LightClient {
        LightClient::bootstrap([0xC0; 32], 0)
    }

    #[test]
    fn submit_update_advances_head_and_notifies_sink() {
        let mut c = lc();
        assert_eq!(
            c.submit_update(96, [0x11; 32], [0xC0; 32]).unwrap(),
            UpdateKind::Advanced
        );
        assert_eq!(c.finalized_slot, 96);
        assert!(c.proven.contains(&[0x11; 32]));
        assert_eq!(c.sink_notifies, vec![[0x11; 32]]);
    }

    #[test]
    fn same_slot_is_stale() {
        let mut c = lc();
        c.submit_update(96, [0x11; 32], [0xC0; 32]).unwrap();
        assert_eq!(
            c.submit_update(96, [0x22; 32], [0xC0; 32]).unwrap_err(),
            LcError::StaleUpdate
        );
        assert_eq!(c.finalized_slot, 96);
        assert!(!c.proven.contains(&[0x22; 32]));
    }

    #[test]
    fn late_register_does_not_rewind_head() {
        let mut c = lc();
        c.submit_update(192, [0xAA; 32], [0xC0; 32]).unwrap();
        assert_eq!(
            c.submit_update(96, [0xBB; 32], [0xC0; 32]).unwrap(),
            UpdateKind::Backfilled
        );
        assert_eq!(c.finalized_slot, 192);
        assert!(c.proven.contains(&[0xAA; 32]));
        assert!(c.proven.contains(&[0xBB; 32]));
    }

    #[test]
    fn already_proven_behind_head_is_stale() {
        let mut c = lc();
        c.submit_update(96, [0xBB; 32], [0xC0; 32]).unwrap();
        c.submit_update(192, [0xAA; 32], [0xC0; 32]).unwrap();
        assert_eq!(
            c.submit_update(96, [0xBB; 32], [0xC0; 32]).unwrap_err(),
            LcError::StaleUpdate
        );
    }

    #[test]
    fn re_push_requires_proven() {
        let mut c = lc();
        assert_eq!(
            c.re_push_anchor([0x11; 32]).unwrap_err(),
            LcError::NotProven
        );
        c.submit_update(8, [0x11; 32], [0xC0; 32]).unwrap();
        c.re_push_anchor([0x11; 32]).unwrap();
        assert_eq!(c.sink_notifies.len(), 2);
        assert_eq!(c.sink_notifies[1], [0x11; 32]);
    }

    #[test]
    fn re_anchor_not_armed_while_owner_path_on() {
        let mut c = lc();
        assert_eq!(
            c.re_anchor_committee([0xDD; 32], 1).unwrap_err(),
            LcError::ReanchorNotArmed
        );
        assert_eq!(LcError::ReanchorNotArmed.code(), 247);
    }

    #[test]
    fn re_anchor_after_disable() {
        let mut c = lc();
        c.disable_owner_rotation().unwrap();
        c.re_anchor_committee([0xDD; 32], 3).unwrap();
        assert_eq!(c.committee, [0xDD; 32]);
        assert_eq!(c.period, 3);
        assert_eq!(c.re_anchors_applied, 1);
        assert!(c.proven.is_empty());
    }

    #[test]
    fn re_anchor_noop_and_stale_period() {
        let mut c = lc();
        c.disable_owner_rotation().unwrap();
        assert_eq!(
            c.re_anchor_committee([0xC0; 32], 0).unwrap_err(),
            LcError::ReanchorNoop
        );
        assert_eq!(LcError::ReanchorNoop.code(), 248);
        c.re_anchor_committee([0xDD; 32], 5).unwrap();
        assert_eq!(
            c.re_anchor_committee([0xEE; 32], 4).unwrap_err(),
            LcError::StalePeriod
        );
    }

    #[test]
    fn set_committee_blocked_after_disable() {
        let mut c = lc();
        c.disable_owner_rotation().unwrap();
        assert_eq!(
            c.set_committee([0x11; 32], 1).unwrap_err(),
            LcError::OwnerRotationDisabled
        );
    }

    #[test]
    fn ancestry_walks_parents_into_proven_set() {
        let mut c = lc();
        let (child, parent) = dummy_linked_headers();
        let ckpt = keccak256(&child);
        c.submit_update(32, ckpt, [0xC0; 32]).unwrap();
        let added = c.submit_ancestry(&[child, parent.clone()]).unwrap();
        assert_eq!(added, 1);
        assert!(c.proven.contains(&keccak256(&parent)));
    }

    #[test]
    fn ancestry_unknown_checkpoint() {
        let mut c = lc();
        let (child, parent) = dummy_linked_headers();
        assert_eq!(
            c.submit_ancestry(&[child, parent]).unwrap_err(),
            LcError::UnknownCheckpoint
        );
    }

    #[test]
    fn ancestry_too_short_or_too_long() {
        let mut c = lc();
        assert_eq!(c.submit_ancestry(&[]).unwrap_err(), LcError::BadAncestry);
        let too_long = vec![vec![0u8; 3]; 33];
        assert_eq!(
            c.submit_ancestry(&too_long).unwrap_err(),
            LcError::AncestryTooLong
        );
    }

    #[test]
    fn upgrade_snapshot_keeps_hatch_and_proven() {
        let mut c = lc();
        c.submit_update(10, [0x11; 32], [0xC0; 32]).unwrap();
        c.disable_owner_rotation().unwrap();
        c.re_anchor_committee([0xDD; 32], 2).unwrap();
        let restored = c.snapshot();
        assert_eq!(restored.finalized_slot, 10);
        assert!(restored.proven.contains(&[0x11; 32]));
        assert!(!restored.owner_rotation_enabled);
        assert_eq!(restored.re_anchors_applied, 1);
        assert_eq!(restored.committee, [0xDD; 32]);
    }
}
