//! Off-chain replica of `EthBeaconLightClient` control flow (no Halo2).
//!
//! Proof bytes are assumed valid: these tests pin the `require`s around head
//! movement, ancestry, sink re-push, and the weak-subjectivity hatch, and the
//! byte order of the words the sink is sent.

use std::collections::{HashMap, VecDeque};

use crate::header_rlp::{keccak256, rlp_parent_hash};

/// 365 * 24 * 3600 / 12. Matches `EthBeaconLightClient.SLOTS_PER_YEAR`.
pub const SLOTS_PER_YEAR: u64 = 2_628_000;
/// Sync-committee period: 256 epochs of 32 slots.
pub const SLOTS_PER_PERIOD: u64 = 8192;
/// Matches `EthBeaconLightClient.MAX_EVICT_PER_TX`.
pub const MAX_EVICT_PER_TX: usize = 128;

/// Matches `EthBeaconLightClient.MAX_FORGETS_PER_TX`: the separate cap on sink
/// messages, which is what has to fit under the balance floor.
pub const MAX_FORGETS_PER_TX: usize = 32;

/// `EthBeaconLightClient._piForm` over big-endian bytes: each 16-byte half
/// reversed. The step circuit reads each half of a hash little-endian, so the
/// contract keys its proven set by this form of the Ethereum-order hash, while
/// `USDCBridge` keys its anchor set by the hash itself. Its own inverse.
pub fn pi_form(h: &[u8; 32]) -> [u8; 32] {
    let mut k = *h;
    k[..16].reverse();
    k[16..].reverse();
    k
}

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
    SelfRotation,
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
            Self::SelfRotation => 253,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LightClient {
    pub finalized_slot: u64,
    pub exec_hash: [u8; 32],
    pub committee: [u8; 32],
    /// The commitment `committee` replaced; accepted for late registration
    /// only, so a checkpoint skipped near a period boundary is not lost when
    /// the rotation lands.
    pub prev_committee: [u8; 32],
    /// The finalized slot the head stood on when `prev_committee` left
    /// office. The window is measured from this, not from the period label,
    /// which the rotate circuit does not constrain.
    pub prev_committee_head_slot: u64,
    pub period: u64,
    pub owner_rotation_enabled: bool,
    pub re_anchors_applied: u64,
    /// Stored key (`pi_form` of the Ethereum-order hash) → Ethereum slot at
    /// admission. Absent / expired ⇒ not live.
    pub proven: HashMap<[u8; 32], u64>,
    pub queue: VecDeque<[u8; 32]>,
    /// The hash each `_notifySink` / `rePushAnchor` sends (including first
    /// insert), in Ethereum byte order.
    pub sink_notifies: Vec<[u8; 32]>,
    /// Header links the walk has checked. The contract pays gas for each one
    /// after `tvm.accept()`, so where the two-end check sits is visible only
    /// here: both placements refuse the same payloads, and only the work done
    /// tells them apart.
    pub walk_steps: u64,
    /// The hash each `_forgetSink` after eviction sends, in Ethereum byte
    /// order.
    pub sink_forgets: Vec<[u8; 32]>,
}

impl LightClient {
    pub fn bootstrap(committee: [u8; 32], period: u64) -> Self {
        Self {
            finalized_slot: 0,
            exec_hash: [0; 32],
            committee,
            prev_committee: [0; 32],
            prev_committee_head_slot: 0,
            period,
            owner_rotation_enabled: true,
            re_anchors_applied: 0,
            proven: HashMap::new(),
            queue: VecDeque::new(),
            sink_notifies: Vec::new(),
            walk_steps: 0,
            sink_forgets: Vec::new(),
        }
    }

    fn year_cutoff(&self) -> u64 {
        self.finalized_slot.saturating_sub(SLOTS_PER_YEAR)
    }

    fn within_year(&self, eth_slot: u64) -> bool {
        eth_slot >= self.year_cutoff()
    }

    pub fn is_live(&self, h: &[u8; 32]) -> bool {
        match self.proven.get(h) {
            Some(&s) if s != 0 && self.within_year(s) => true,
            _ => false,
        }
    }

    fn evict_expired(&mut self) {
        let mut n = 0usize;
        let mut dropped = 0usize;
        // Two caps, as the contract has: examinations, and separately the
        // forgets — each of those is a message the light client pays for in
        // the action phase, and the worst case one call can queue has to fit
        // under its balance floor.
        while !self.queue.is_empty() && n < MAX_EVICT_PER_TX && dropped < MAX_FORGETS_PER_TX {
            let h = self.queue.pop_front().expect("non-empty");
            n += 1;
            match self.proven.get(&h).copied() {
                Some(s) if self.within_year(s) => self.queue.push_back(h),
                Some(_) => {
                    self.proven.remove(&h);
                    self.sink_forgets.push(pi_form(&h));
                    dropped += 1;
                },
                None => {},
            }
        }
    }

    fn push_exec(&mut self, h: [u8; 32], eth_slot: u64) {
        if self.is_live(&h) {
            return;
        }
        if eth_slot == 0 {
            return;
        }
        self.proven.insert(h, eth_slot);
        self.queue.push_back(h);
        self.sink_notifies.push(pi_form(&h));
    }

    /// `submitUpdate` after a successful ZK verify (committee already checked).
    /// `exec` is the stored key, as the step public inputs carry it.
    /// As [`Self::submit_update_at`], attested inside the period in force.
    pub fn submit_update(
        &mut self,
        slot: u64,
        exec: [u8; 32],
        committee: [u8; 32],
    ) -> Result<UpdateKind, LcError> {
        let attested = self.period.saturating_mul(SLOTS_PER_PERIOD);
        self.submit_update_at(slot, attested, exec, committee)
    }

    /// `attested_slot` decides which committee may sign: the one in force for
    /// its own period, the one a rotation replaced for the period before.
    pub fn submit_update_at(
        &mut self,
        slot: u64,
        attested_slot: u64,
        exec: [u8; 32],
        committee: [u8; 32],
    ) -> Result<UpdateKind, LcError> {
        if self.committee == [0; 32] {
            return Err(LcError::CommitteeUnset);
        }
        let advancing = slot > self.finalized_slot;
        let late = slot < self.finalized_slot && !self.is_live(&exec);
        if !advancing && !late {
            return Err(LcError::StaleUpdate);
        }
        // The contract tests the year window before the committee, so a stale
        // update signed by the wrong committee reports StaleUpdate, not
        // WrongCommittee. The order is part of what this replica pins.
        if late && !self.within_year(slot) {
            return Err(LcError::StaleUpdate);
        }
        let prev_ok = late
            && self.prev_committee != [0; 32]
            && committee == self.prev_committee
            && self.prev_committee_head_slot != 0
            && attested_slot <= self.prev_committee_head_slot + SLOTS_PER_PERIOD
            && attested_slot + SLOTS_PER_PERIOD >= self.prev_committee_head_slot;
        let signer_ok = committee == self.committee || prev_ok;
        if !signer_ok {
            return Err(LcError::WrongCommittee);
        }
        if advancing {
            self.finalized_slot = slot;
            self.exec_hash = exec;
            self.evict_expired();
            self.push_exec(exec, slot);
            Ok(UpdateKind::Advanced)
        } else {
            self.evict_expired();
            self.push_exec(exec, slot);
            Ok(UpdateKind::Backfilled)
        }
    }

    pub fn submit_ancestry(&mut self, header_rlps: &[Vec<u8>]) -> Result<u64, LcError> {
        if header_rlps.len() < 3 {
            return Err(LcError::BadAncestry);
        }
        if header_rlps.len() > 33 {
            return Err(LcError::AncestryTooLong);
        }
        let last = header_rlps.len() - 1;
        let mut prev = keccak256(&header_rlps[0]);
        let anchor = pi_form(&prev);
        // Both ends before anything else, as the contract does before accept.
        if !self.is_live(&anchor) || !self.is_live(&pi_form(&keccak256(&header_rlps[last]))) {
            return Err(LcError::UnknownCheckpoint);
        }
        let anchor_slot = *self.proven.get(&anchor).expect("live");
        // The contract reverts, undoing every write, so nothing may be stored
        // before the whole payload has passed. Walk once to check, then write.
        let mut between = Vec::new();
        for (i, rlp) in header_rlps.iter().enumerate().skip(1) {
            self.walk_steps += 1;
            if rlp_parent_hash(rlp).map_err(|_| LcError::BadAncestry)? != prev {
                return Err(LcError::BadAncestry);
            }
            prev = keccak256(rlp);
            let key = pi_form(&prev);
            if i == last {
                if !self.is_live(&key) {
                    return Err(LcError::UnknownCheckpoint);
                }
            } else if !self.is_live(&key) {
                between.push(key);
            }
        }
        self.evict_expired();
        let mut added = 0u64;
        for key in between {
            if !self.is_live(&key) {
                self.push_exec(key, anchor_slot);
                added += 1;
            }
        }
        Ok(added)
    }

    /// `rePushAnchor`: takes the stored key, unlike
    /// [`crate::submitter::AnSubmitter::re_push_anchor`], which takes the
    /// Ethereum-order hash and re-packs it.
    pub fn re_push_anchor(&mut self, block_hash: [u8; 32]) -> Result<(), LcError> {
        if !self.is_live(&block_hash) {
            return Err(LcError::NotProven);
        }
        self.sink_notifies.push(pi_form(&block_hash));
        Ok(())
    }

    /// `submitRotate`: a proven hand-off. The committee leaving office is kept
    /// as the previous one, which is the only path that grants it the
    /// late-registration window.
    pub fn rotate(&mut self, next: [u8; 32], period: u64) -> Result<(), LcError> {
        if self.committee == [0; 32] {
            return Err(LcError::CommitteeUnset);
        }
        // A hand-off that changes nothing exists for every period boundary and
        // would only evict the previous committee.
        if next == self.committee {
            return Err(LcError::SelfRotation);
        }
        // No test on `period`. The circuit binds it to the attested slot, but
        // the committee chain above is the replay guard and nothing gates on
        // the label.
        self.prev_committee = self.committee;
        self.prev_committee_head_slot = self.finalized_slot;
        self.committee = next;
        self.period = period;
        Ok(())
    }

    pub fn set_committee(&mut self, commitment: [u8; 32], period: u64) -> Result<(), LcError> {
        if !self.owner_rotation_enabled {
            return Err(LcError::OwnerRotationDisabled);
        }
        self.prev_committee = [0; 32];
        self.prev_committee_head_slot = 0;
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
        self.prev_committee = [0; 32];
        self.prev_committee_head_slot = 0;
        self.committee = commitment;
        self.period = period;
        self.re_anchors_applied += 1;
        Ok(())
    }

    /// `updateCode` / `onCodeUpgrade` keep committee, head, proven set, hatch
    /// counter — but not the previous committee: the migration cell is built
    /// by the code being replaced, so the field is absent and the upgraded
    /// client starts with no window.
    pub fn snapshot(&self) -> Self {
        let mut next = self.clone();
        next.prev_committee = [0; 32];
        next.prev_committee_head_slot = 0;
        next
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
    use crate::{
        header_rlp::{dummy_linked_chain, keccak256},
        submitter::anchor_key_hex,
    };

    fn lc() -> LightClient {
        LightClient::bootstrap([0xC0; 32], 0)
    }

    fn hex32(s: &str) -> [u8; 32] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    /// Step public inputs 6 and 7 of the fixture in
    /// `eth-light-client-prover/fixtures/step_vkblob`, recombined as
    /// `(hi << 128) | lo`: the key `submitUpdate` stores.
    const FIXTURE_STORED_KEY: &str =
        "89d2463dac3b23c587cf9d97a626f2cee2f7f00249d876f2e0794c53d2d1c5ee";
    /// `finalized_header.execution.block_hash` of the same update in
    /// `eth-light-client-prover/fixtures/mainnet/finality_update.json`: the
    /// word `finalizeDeposit` looks up for that block.
    const FIXTURE_BLOCK_HASH: &str =
        "cef226a6979dcf87c5233bac3d46d289eec5d1d2534c79e0f276d84902f0f7e2";

    #[test]
    fn sink_is_sent_the_ethereum_order_hash() {
        let key = hex32(FIXTURE_STORED_KEY);
        let block_hash = hex32(FIXTURE_BLOCK_HASH);
        assert_eq!(pi_form(&key), block_hash);
        assert_eq!(pi_form(&block_hash), key);
        // The relayer's re-packing for `rePushAnchor` is the same transform.
        assert_eq!(
            anchor_key_hex(&block_hash),
            format!("0x{FIXTURE_STORED_KEY}")
        );

        let mut c = lc();
        c.submit_update(100, key, [0xC0; 32]).unwrap();
        c.re_push_anchor(key).unwrap();
        assert_eq!(c.sink_notifies, vec![block_hash, block_hash]);
        assert_eq!(
            c.re_push_anchor(block_hash).unwrap_err(),
            LcError::NotProven
        );

        c.submit_update(100 + SLOTS_PER_YEAR + 1, [0x22; 32], [0xC0; 32])
            .unwrap();
        assert_eq!(c.sink_forgets, vec![block_hash]);
    }

    #[test]
    fn submit_update_advances_head_and_notifies_sink() {
        let mut c = lc();
        assert_eq!(
            c.submit_update(96, [0x11; 32], [0xC0; 32]).unwrap(),
            UpdateKind::Advanced
        );
        assert_eq!(c.finalized_slot, 96);
        assert!(c.is_live(&[0x11; 32]));
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
        assert!(!c.is_live(&[0x22; 32]));
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
        assert!(c.is_live(&[0xAA; 32]));
        assert!(c.is_live(&[0xBB; 32]));
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

    /// Both ends anchored: the blocks between them are admitted, the anchors
    /// themselves are not re-added.
    fn anchored_span(c: &mut LightClient, n: usize) -> Vec<Vec<u8>> {
        let chain = dummy_linked_chain(n);
        let older = pi_form(&keccak256(&chain[0]));
        let newer = pi_form(&keccak256(&chain[n - 1]));
        c.submit_update(32, older, [0xC0; 32]).unwrap();
        c.submit_update(64, newer, [0xC0; 32]).unwrap();
        chain
    }

    /// A checkpoint skipped near the end of a period is signed by the
    /// committee that the rotation then replaces. Without accepting that
    /// signer for late registration, those 32 blocks are unanchorable for
    /// good and only the owner can admit their deposits.
    #[test]
    fn a_checkpoint_skipped_before_a_rotation_can_still_be_registered() {
        let old_committee = [0xC0; 32];
        let new_committee = [0xC1; 32];
        let mut c = LightClient::bootstrap(old_committee, 1);
        c.submit_update(64, [0xAA; 32], old_committee).unwrap();

        // Slot 32 was never proven, and the committee rotates before it is.
        c.rotate(new_committee, 2).unwrap();
        assert_eq!(c.committee, new_committee);

        // Attested inside the period the old committee served.
        c.submit_update_at(32, 64, [0xBB; 32], old_committee)
            .expect("the skipped checkpoint must still be registrable");
        assert!(c.is_live(&[0xBB; 32]));
        assert_eq!(
            c.finalized_slot, 64,
            "late registration must not move the head"
        );
    }

    /// The rotate period is proven (the attested slot's period plus one) and
    /// stored as it is, but nothing gates on it: a hand-off goes through
    /// whatever its label, and the committee chain is the replay guard.
    #[test]
    fn the_rotate_label_is_stored_and_gates_nothing() {
        let mut c = LightClient::bootstrap([0xC0; 32], 1);
        c.submit_update_at(SLOTS_PER_PERIOD, SLOTS_PER_PERIOD, [0xAA; 32], [0xC0; 32])
            .unwrap();

        c.rotate([0xC1; 32], 2).unwrap();
        assert_eq!(c.period, 2, "the stored period is the proof's");
        assert_eq!(c.committee, [0xC1; 32]);

        c.submit_update_at(
            2 * SLOTS_PER_PERIOD,
            2 * SLOTS_PER_PERIOD,
            [0xAB; 32],
            [0xC1; 32],
        )
        .unwrap();
        c.rotate([0xC2; 32], 0)
            .expect("the label must not block a genuine hand-off");
        assert_eq!(c.committee, [0xC2; 32]);
        assert_eq!(c.period, 0);
    }

    /// Reachable for every period boundary: the last header of a period is
    /// signed in the next one and still carries the signer as its
    /// `next_sync_committee`. Accepting it would change nothing visible and
    /// evict the committee whose stragglers are still being registered.
    #[test]
    fn a_hand_off_that_does_not_change_the_committee_is_refused() {
        let mut c = LightClient::bootstrap([0xC0; 32], 1);
        c.submit_update_at(SLOTS_PER_PERIOD, SLOTS_PER_PERIOD, [0xAA; 32], [0xC0; 32])
            .unwrap();
        c.rotate([0xC1; 32], 2).unwrap();
        assert_eq!(c.prev_committee, [0xC0; 32]);

        assert_eq!(
            c.rotate([0xC1; 32], 3).unwrap_err(),
            LcError::SelfRotation,
            "a hand-off onto the committee already in force must be refused"
        );
        assert_eq!(
            c.prev_committee, [0xC0; 32],
            "the previous committee must survive the attempt"
        );
    }

    /// The replaced committee may only sign around the head it left behind.
    /// Without that bound it could sign any slot in the one-year window,
    /// including slots it never attested, and the hashes it wrote would back
    /// mints.
    #[test]
    fn the_replaced_committee_cannot_sign_past_the_head_it_left() {
        let old_committee = [0xC0; 32];
        let new_committee = [0xC1; 32];
        let mut c = LightClient::bootstrap(old_committee, 1);
        c.submit_update_at(
            2 * SLOTS_PER_PERIOD,
            SLOTS_PER_PERIOD,
            [0xAA; 32],
            old_committee,
        )
        .unwrap();
        c.rotate(new_committee, 2).unwrap();

        // The head it left was 2 periods in; more than one period past that
        // is outside the window.
        assert_eq!(
            c.submit_update_at(
                2 * SLOTS_PER_PERIOD - 1,
                3 * SLOTS_PER_PERIOD + 1,
                [0xDD; 32],
                old_committee
            )
            .unwrap_err(),
            LcError::WrongCommittee
        );
        assert!(!c.is_live(&[0xDD; 32]));
    }

    /// The skip the window exists for: a checkpoint the leaving committee
    /// never got to prove, attested *after* the last head it moved but still
    /// within its own period. The first slot-based window excluded exactly
    /// these, which is the case the feature is about.
    #[test]
    fn a_skip_after_the_last_moved_head_is_still_rescued() {
        let old_committee = [0xC0; 32];
        let new_committee = [0xC1; 32];
        let mut c = LightClient::bootstrap(old_committee, 1);
        c.submit_update_at(
            SLOTS_PER_PERIOD,
            SLOTS_PER_PERIOD,
            [0xAA; 32],
            old_committee,
        )
        .unwrap();
        c.rotate(new_committee, 2).unwrap();
        c.submit_update_at(
            2 * SLOTS_PER_PERIOD,
            2 * SLOTS_PER_PERIOD,
            [0xEE; 32],
            new_committee,
        )
        .unwrap();

        c.submit_update_at(
            SLOTS_PER_PERIOD + 32,
            SLOTS_PER_PERIOD + 40,
            [0xBB; 32],
            old_committee,
        )
        .expect("a skip just after the last moved head must be registrable");
        assert!(c.is_live(&[0xBB; 32]));
    }

    /// Nor anything more than one period before that head.
    #[test]
    fn the_replaced_committee_cannot_sign_long_before_its_window() {
        let old_committee = [0xC0; 32];
        let new_committee = [0xC1; 32];
        let mut c = LightClient::bootstrap(old_committee, 1);
        c.submit_update_at(
            3 * SLOTS_PER_PERIOD,
            3 * SLOTS_PER_PERIOD,
            [0xAA; 32],
            old_committee,
        )
        .unwrap();
        c.rotate(new_committee, 4).unwrap();

        assert_eq!(
            c.submit_update_at(100, 100, [0xDD; 32], old_committee)
                .unwrap_err(),
            LcError::WrongCommittee
        );
        assert!(!c.is_live(&[0xDD; 32]));
    }

    /// The owner hatches replace a committee the owner is rejecting, so it
    /// must not keep the late-registration window.
    #[test]
    fn an_owner_correction_leaves_no_window_for_the_committee_it_replaces() {
        let bad = [0xBA; 32];
        let good = [0x60; 32];
        let mut c = LightClient::bootstrap(bad, 1);
        c.submit_update(64, [0xAA; 32], bad).unwrap();
        c.set_committee(good, 1).unwrap();
        assert_eq!(
            c.prev_committee, [0; 32],
            "the rejected committee is not kept"
        );
        assert_eq!(
            c.submit_update_at(32, 64, [0xBB; 32], bad).unwrap_err(),
            LcError::WrongCommittee
        );
    }

    /// The old committee is for catching up only: it must never move the head.
    #[test]
    fn the_previous_committee_cannot_advance_the_head() {
        let old_committee = [0xC0; 32];
        let new_committee = [0xC1; 32];
        let mut c = LightClient::bootstrap(old_committee, 1);
        c.submit_update(64, [0xAA; 32], old_committee).unwrap();
        c.rotate(new_committee, 2).unwrap();

        assert_eq!(
            c.submit_update(96, [0xCC; 32], old_committee).unwrap_err(),
            LcError::WrongCommittee
        );
        assert_eq!(c.finalized_slot, 64);
        assert!(!c.is_live(&[0xCC; 32]));
    }

    #[test]
    fn ancestry_fills_between_two_anchors() {
        let mut c = lc();
        let chain = anchored_span(&mut c, 4);
        let before = c.sink_notifies.len();
        let added = c.submit_ancestry(&chain).unwrap();
        assert_eq!(added, 2);
        assert!(c.is_live(&pi_form(&keccak256(&chain[1]))));
        assert!(c.is_live(&pi_form(&keccak256(&chain[2]))));
        // The sink takes the Ethereum byte order, not the stored key — the
        // defect 1.4.1 fixed on the checkpoint path, which the span path has
        // to honour too.
        assert_eq!(
            &c.sink_notifies[before..],
            &[keccak256(&chain[1]), keccak256(&chain[2])],
            "ancestry must notify the sink in Ethereum byte order, oldest first"
        );
    }

    /// Forgets are capped per call separately from examinations, because each
    /// one is a message the light client pays for in the action phase and the
    /// worst case has to fit under its balance floor. A cluster of expiries
    /// therefore comes out over several calls instead of aborting one.
    #[test]
    fn a_cluster_of_expiries_is_forgotten_in_capped_batches() {
        let mut c = lc();
        let n = MAX_FORGETS_PER_TX + 8;
        for i in 1..=n {
            let mut h = [0u8; 32];
            h[0] = i as u8;
            h[1] = 0xEE;
            c.submit_update(32 * i as u64, h, [0xC0; 32]).unwrap();
        }
        assert!(c.sink_forgets.is_empty(), "nothing has expired yet");

        // Jump the head a year past all of them: every one is now outside the
        // window, and the next call compacts.
        let far = SLOTS_PER_YEAR + 32 * n as u64 + 32;
        c.submit_update(far, [0xFF; 32], [0xC0; 32]).unwrap();
        assert_eq!(
            c.sink_forgets.len(),
            MAX_FORGETS_PER_TX,
            "one call must forget at most the cap, not the whole cluster"
        );

        c.submit_update(far + 32, [0xFE; 32], [0xC0; 32]).unwrap();
        assert_eq!(
            c.sink_forgets.len(),
            n,
            "the rest must come out on the following calls, not be dropped"
        );
    }

    /// Where the two-end check sits is a gas property, not an outcome: a
    /// payload with an unproven far end is refused either way. On chain the
    /// difference was 1,917,462 gas charged to the light client per message
    /// versus nothing at all, so the walk must not start until both ends are
    /// known.
    #[test]
    fn an_unproven_far_end_is_refused_before_the_walk_does_any_work() {
        let mut c = lc();
        let chain = dummy_linked_chain(33);
        // Only the near end is an anchor — the shape a griefer sends.
        c.submit_update(32, pi_form(&keccak256(&chain[0])), [0xC0; 32])
            .unwrap();

        let before = c.walk_steps;
        assert_eq!(
            c.submit_ancestry(&chain).unwrap_err(),
            LcError::UnknownCheckpoint
        );
        assert_eq!(
            c.walk_steps, before,
            "the walk must not run a single link before both ends are proven"
        );

        // And an honest payload still walks: the check is a gate, not a wall.
        let mut honest = lc();
        let chain = anchored_span(&mut honest, 5);
        let before = honest.walk_steps;
        honest.submit_ancestry(&chain).unwrap();
        assert_eq!(
            honest.walk_steps,
            before + 4,
            "an accepted span walks every link exactly once"
        );
    }

    #[test]
    fn ancestry_newest_first_is_refused() {
        let mut c = lc();
        let mut chain = anchored_span(&mut c, 4);
        chain.reverse();
        assert_eq!(c.submit_ancestry(&chain).unwrap_err(), LcError::BadAncestry);
    }

    #[test]
    fn ancestry_far_end_must_be_an_anchor() {
        let mut c = lc();
        let chain = dummy_linked_chain(4);
        c.submit_update(32, pi_form(&keccak256(&chain[0])), [0xC0; 32])
            .unwrap();
        assert_eq!(
            c.submit_ancestry(&chain).unwrap_err(),
            LcError::UnknownCheckpoint
        );
        assert!(!c.is_live(&pi_form(&keccak256(&chain[1]))));
        assert!(!c.is_live(&pi_form(&keccak256(&chain[2]))));
    }

    #[test]
    fn ancestry_re_walk_adds_and_notifies_nothing() {
        let mut c = lc();
        let chain = anchored_span(&mut c, 4);
        assert_eq!(c.submit_ancestry(&chain).unwrap(), 2);
        let notified = c.sink_notifies.len();
        let queued = c.queue.len();
        assert_eq!(c.submit_ancestry(&chain).unwrap(), 0);
        assert_eq!(c.sink_notifies.len(), notified);
        assert_eq!(c.queue.len(), queued);
    }

    #[test]
    fn ancestry_unknown_checkpoint() {
        let mut c = lc();
        assert_eq!(
            c.submit_ancestry(&dummy_linked_chain(3)).unwrap_err(),
            LcError::UnknownCheckpoint
        );
    }

    #[test]
    fn ancestry_too_short_or_too_long() {
        let mut c = lc();
        assert_eq!(c.submit_ancestry(&[]).unwrap_err(), LcError::BadAncestry);
        assert_eq!(
            c.submit_ancestry(&dummy_linked_chain(2)).unwrap_err(),
            LcError::BadAncestry
        );
        let too_long = vec![vec![0u8; 3]; 34];
        assert_eq!(
            c.submit_ancestry(&too_long).unwrap_err(),
            LcError::AncestryTooLong
        );
    }

    /// The migration cell is built by the code being replaced, so it carries
    /// no previous committee: an upgraded client must start with no window
    /// rather than inherit one that is not in the cell.
    #[test]
    fn upgrade_clears_the_previous_committee_window() {
        let mut c = LightClient::bootstrap([0xC0; 32], 1);
        c.submit_update(64, [0xAA; 32], [0xC0; 32]).unwrap();
        c.rotate([0xC1; 32], 2).unwrap();
        assert_ne!(c.prev_committee, [0; 32], "the rotation armed the window");
        assert_ne!(c.prev_committee_head_slot, 0);

        let restored = c.snapshot();
        assert_eq!(restored.prev_committee, [0; 32]);
        assert_eq!(restored.prev_committee_head_slot, 0);
        assert_eq!(
            restored.committee, [0xC1; 32],
            "the committee in force survives"
        );
        assert_eq!(
            restored
                .clone()
                .submit_update_at(32, 64, [0xBB; 32], [0xC0; 32])
                .unwrap_err(),
            LcError::WrongCommittee,
            "no window after the upgrade"
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
        assert!(restored.is_live(&[0x11; 32]));
        assert!(!restored.owner_rotation_enabled);
        assert_eq!(restored.re_anchors_applied, 1);
        assert_eq!(restored.committee, [0xDD; 32]);
    }

    #[test]
    fn year_window_drops_hash_and_forgets_sink() {
        let mut c = lc();
        c.submit_update(100, [0x11; 32], [0xC0; 32]).unwrap();
        assert!(c.is_live(&[0x11; 32]));
        c.submit_update(100 + SLOTS_PER_YEAR + 1, [0x22; 32], [0xC0; 32])
            .unwrap();
        assert!(!c.is_live(&[0x11; 32]));
        assert!(c.is_live(&[0x22; 32]));
        assert_eq!(c.sink_forgets, vec![[0x11; 32]]);
        assert_eq!(
            c.re_push_anchor([0x11; 32]).unwrap_err(),
            LcError::NotProven
        );
    }

    #[test]
    fn late_register_older_than_year_is_stale() {
        let mut c = lc();
        c.submit_update(SLOTS_PER_YEAR + 50, [0xAA; 32], [0xC0; 32])
            .unwrap();
        assert_eq!(
            c.submit_update(40, [0xBB; 32], [0xC0; 32]).unwrap_err(),
            LcError::StaleUpdate
        );
        assert!(!c.is_live(&[0xBB; 32]));
    }

    #[test]
    fn ancestry_expired_checkpoint_unknown() {
        let mut c = lc();
        let chain = anchored_span(&mut c, 4);
        c.submit_update(64 + SLOTS_PER_YEAR + 1, [0xEE; 32], [0xC0; 32])
            .unwrap();
        assert_eq!(
            c.submit_ancestry(&chain).unwrap_err(),
            LcError::UnknownCheckpoint
        );
    }
}
