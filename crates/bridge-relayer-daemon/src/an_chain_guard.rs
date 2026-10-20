//! Stop the daemon when the Acki Nacki chain is behind the bridge.
//!
//! The bridge's last verified block is one the chain has produced, so a chain
//! head below it means the network was restarted from a new zerostate, or
//! `BRIDGE_GQL_ENDPOINT` serves another network. This bridge cannot follow
//! either: the new chain's blocks do not extend the anchors it holds, and its
//! next boundary is months away. Without this check the daemon took the low
//! head for "the next bundle is not there yet" and waited without a word.
//!
//! A read below the bridge can also come from a GraphQL replica far behind,
//! so the daemon stops only once the head has stayed below for
//! [`BEHIND_FOR_TO_STOP`] by the clock, over at least
//! [`POLLS_BEHIND_TO_STOP`] polls in a row. Counting polls alone was not
//! enough: they follow the daemon's backoff, 4, 8, 16 and 32 s apart after
//! a start, so five of them took about a minute. A GraphQL answer with no
//! blocks, which the driver reads as head 0, is no head at all and does not
//! count either way.

use std::time::Duration;

use tokio::time::Instant;
use tracing::warn;

use crate::RelayerError;

/// How long the chain head must stay below the bridge's last verified block,
/// from the first poll that saw it there, before the daemon stops.
pub const BEHIND_FOR_TO_STOP: Duration = Duration::from_secs(5 * 60);

/// Polls in a row, at least, that must see the head below the bridge before
/// the daemon stops, however long ago the first of them was: two reads with
/// a long stall between them are not enough.
pub const POLLS_BEHIND_TO_STOP: u32 = 5;

/// Tracks the polls in a row that saw the chain head below the bridge.
#[derive(Debug, Default)]
pub struct ChainBehindGuard {
    /// When the first poll of the current run saw the head below.
    behind_since: Option<Instant>,
    polls_behind: u32,
}

impl ChainBehindGuard {
    /// A poll that found a bundle past the bridge: the chain is not behind
    /// it, so a run of polls behind ends here, as a head at or above the
    /// bridge ends it in [`Self::observe_at`].
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record one poll's chain head against the bridge's last verified
    /// block, now. See [`Self::observe_at`].
    pub fn observe(&mut self, chain_head: u64, last_verified: u64) -> Result<(), RelayerError> {
        self.observe_at(Instant::now(), chain_head, last_verified)
    }

    /// Record the chain head one poll read at `now`.
    /// [`RelayerError::AnChainBehindBridge`] once the head has stayed below
    /// the bridge's last verified block for [`BEHIND_FOR_TO_STOP`] and
    /// [`POLLS_BEHIND_TO_STOP`] polls in a row; a head at or above it starts
    /// both again.
    ///
    /// A head of 0 is the driver's reading of a GraphQL answer with no blocks
    /// (`query_latest_blocks` returns an empty list, and its maximum falls
    /// back to 0). No network the bridge follows has a head of 0, so the poll
    /// is skipped: it neither counts toward the stop nor clears a run of
    /// polls behind.
    pub fn observe_at(
        &mut self,
        now: Instant,
        chain_head: u64,
        last_verified: u64,
    ) -> Result<(), RelayerError> {
        if chain_head == 0 {
            return Ok(());
        }
        if chain_head >= last_verified {
            *self = Self::default();
            return Ok(());
        }
        let since = *self.behind_since.get_or_insert(now);
        self.polls_behind += 1;
        let behind_for = now.saturating_duration_since(since);
        if self.polls_behind >= POLLS_BEHIND_TO_STOP && behind_for >= BEHIND_FOR_TO_STOP {
            return Err(RelayerError::AnChainBehindBridge {
                chain_head,
                last_verified,
            });
        }
        warn!(
            chain_head,
            last_verified,
            polls_behind = self.polls_behind,
            behind_for_secs = behind_for.as_secs(),
            stop_after_secs = BEHIND_FOR_TO_STOP.as_secs(),
            "Acki Nacki chain head is below the bridge's last verified block; the daemon stops if \
             it stays there"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon-live backoff after a start (2 s, doubling, at most 60 s):
    /// the polls come 4, 8, 16, 32, 60, 60 … s apart.
    fn poll_times() -> impl Iterator<Item = Duration> {
        let mut at = Duration::ZERO;
        let mut delay = Duration::from_secs(2);
        std::iter::from_fn(move || {
            let this = at;
            delay = (delay * 2).min(Duration::from_secs(60));
            at += delay;
            Some(this)
        })
    }

    #[test]
    fn a_head_at_or_above_the_bridge_never_stops_the_daemon() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        for head in [100, 100, 16_484, u64::MAX] {
            for at in poll_times().take(20) {
                guard.observe_at(start + at, head, 100).unwrap();
            }
        }
    }

    /// The review's case: right after a start the polls come seconds
    /// apart, and five of them used to stop the daemon within a minute.
    #[test]
    fn a_head_below_the_bridge_stops_the_daemon_after_five_minutes() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        let mut stopped_at = None;
        for at in poll_times().take(20) {
            if let Err(err) = guard.observe_at(start + at, 42_630, 24_018_944) {
                assert!(
                    matches!(err, RelayerError::AnChainBehindBridge {
                        chain_head: 42_630,
                        last_verified: 24_018_944,
                    }),
                    "{err:?}"
                );
                stopped_at = Some(at);
                break;
            }
        }
        assert_eq!(stopped_at, Some(BEHIND_FOR_TO_STOP));
    }

    /// Two reads below the bridge with a long stall between them are not
    /// five polls in a row.
    #[test]
    fn the_clock_alone_does_not_stop_the_daemon() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        for i in 1..POLLS_BEHIND_TO_STOP {
            let at = BEHIND_FOR_TO_STOP * i;
            guard.observe_at(start + at, 90, 100).unwrap();
        }
        let last = BEHIND_FOR_TO_STOP * POLLS_BEHIND_TO_STOP;
        assert!(guard.observe_at(start + last, 90, 100).is_err());
    }

    /// The review's case: GraphQL answering with no blocks for a long time
    /// is not a restarted network, and must not tell the operator to deploy
    /// a new bridge.
    #[test]
    fn a_graphql_answer_with_no_blocks_never_stops_the_daemon() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        for at in poll_times().take(40) {
            guard.observe_at(start + at, 0, 24_018_944).unwrap();
        }
    }

    /// Empty answers between polls behind are not polls behind, and do not
    /// start the run again either: with every third poll reading a head,
    /// the fifth of those comes 540 s after the first, and the stop with it.
    #[test]
    fn empty_answers_neither_count_nor_clear_a_run_behind() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        let mut stopped_at = None;
        for (i, at) in poll_times().take(20).enumerate() {
            let head = if i % 3 == 0 { 42_630 } else { 0 };
            if guard.observe_at(start + at, head, 24_018_944).is_err() {
                stopped_at = Some(at);
                break;
            }
        }
        assert_eq!(stopped_at, Some(Duration::from_secs(540)));
    }

    /// The review's case: a poll that yields a bundle shows the chain past
    /// the bridge, so polls behind on either side of it are not in a row.
    #[test]
    fn a_bundle_between_polls_behind_starts_the_count_again() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        let behind = |guard: &mut ChainBehindGuard, secs: u64| {
            guard.observe_at(start + Duration::from_secs(secs), 42_630, 24_018_944)
        };
        for secs in [0, 60, 120, 180] {
            behind(&mut guard, secs).unwrap();
        }
        guard.reset();
        // Without the reset the poll at 300 s would stop the daemon: five
        // minutes after the first poll behind, and the sixth in a row.
        for secs in [240, 300, 360, 420] {
            behind(&mut guard, secs).unwrap();
        }
        assert!(behind(&mut guard, 540).is_err());
    }

    /// A replica that lags for two minutes after a start and then catches
    /// up must not stop the daemon, then or later.
    #[test]
    fn a_head_that_catches_up_starts_the_count_again() {
        let start = Instant::now();
        let mut guard = ChainBehindGuard::default();
        let mut polls = poll_times().map(|at| start + at);
        for at in polls
            .by_ref()
            .take_while(|at| *at < start + Duration::from_secs(120))
        {
            guard.observe_at(at, 90, 100).unwrap();
        }
        let caught_up = polls.next().unwrap();
        guard.observe_at(caught_up, 100, 100).unwrap();
        let mut behind_again = None;
        for at in polls.take(20) {
            let since = *behind_again.get_or_insert(at);
            if guard.observe_at(at, 90, 100).is_err() {
                assert!(at - since >= BEHIND_FOR_TO_STOP);
                return;
            }
        }
        panic!("the guard never stopped the daemon");
    }
}
