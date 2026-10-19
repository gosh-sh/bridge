//! Stop the daemon when the Acki Nacki chain is behind the bridge.
//!
//! The bridge's last verified block is one the chain has produced, so a chain
//! head below it means the network was restarted from a new zerostate, or
//! `BRIDGE_GQL_ENDPOINT` serves another network. This bridge cannot follow
//! either: the new chain's blocks do not extend the anchors it holds, and its
//! next boundary is months away. Without this check the daemon took the low
//! head for "the next bundle is not there yet" and waited without a word.
//!
//! One read below the bridge can come from a GraphQL replica far behind, so
//! the daemon stops only after [`POLLS_BEHIND_TO_STOP`] polls in a row.

use tracing::warn;

use crate::RelayerError;

/// Polls in a row with the chain head below the bridge's last verified block
/// before the daemon stops: about five minutes at the usual poll interval.
pub const POLLS_BEHIND_TO_STOP: u32 = 5;

/// Counts the polls in a row that saw the chain head below the bridge.
#[derive(Debug, Default)]
pub struct ChainBehindGuard {
    polls_behind: u32,
}

impl ChainBehindGuard {
    /// Record one poll's chain head against the bridge's last verified
    /// block. [`RelayerError::AnChainBehindBridge`] once the head has stayed
    /// below it for [`POLLS_BEHIND_TO_STOP`] polls in a row; a head at or
    /// above it starts the count again.
    pub fn observe(&mut self, chain_head: u64, last_verified: u64) -> Result<(), RelayerError> {
        if chain_head >= last_verified {
            self.polls_behind = 0;
            return Ok(());
        }
        self.polls_behind += 1;
        if self.polls_behind >= POLLS_BEHIND_TO_STOP {
            return Err(RelayerError::AnChainBehindBridge {
                chain_head,
                last_verified,
            });
        }
        warn!(
            chain_head,
            last_verified,
            polls_behind = self.polls_behind,
            stop_after = POLLS_BEHIND_TO_STOP,
            "Acki Nacki chain head is below the bridge's last verified block; the daemon stops if \
             it stays there"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_at_or_above_the_bridge_never_stops_the_daemon() {
        let mut guard = ChainBehindGuard::default();
        for head in [100, 100, 16_484, u64::MAX] {
            for _ in 0..(2 * POLLS_BEHIND_TO_STOP) {
                guard.observe(head, 100).unwrap();
            }
        }
    }

    #[test]
    fn a_head_below_the_bridge_stops_the_daemon_after_the_last_poll_in_a_row() {
        let mut guard = ChainBehindGuard::default();
        for _ in 1..POLLS_BEHIND_TO_STOP {
            guard.observe(42_630, 24_018_944).unwrap();
        }
        let err = guard.observe(42_631, 24_018_944).unwrap_err();
        assert!(
            matches!(err, RelayerError::AnChainBehindBridge {
                chain_head: 42_631,
                last_verified: 24_018_944,
            }),
            "{err:?}"
        );
    }

    /// A replica that lags for a few polls and then catches up must not
    /// stop the daemon later.
    #[test]
    fn a_head_that_catches_up_starts_the_count_again() {
        let mut guard = ChainBehindGuard::default();
        for _ in 1..POLLS_BEHIND_TO_STOP {
            guard.observe(90, 100).unwrap();
        }
        guard.observe(100, 100).unwrap();
        for _ in 1..POLLS_BEHIND_TO_STOP {
            guard.observe(90, 100).unwrap();
        }
        assert!(guard.observe(90, 100).is_err());
    }
}
