//! Withdrawal flow implementation

use crate::error::Result;

/// Withdrawal manager
pub struct WithdrawalManager {}

impl WithdrawalManager {
    /// Create a new withdrawal manager
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for WithdrawalManager {
    fn default() -> Self {
        Self::new()
    }
}

