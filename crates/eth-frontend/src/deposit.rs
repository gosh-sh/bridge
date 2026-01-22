//! Deposit flow implementation

use crate::error::Result;
use crypto::{Hash, SecureRng};

/// Deposit manager
pub struct DepositManager {
    rng: SecureRng,
}

impl DepositManager {
    /// Create a new deposit manager
    pub fn new() -> Self {
        Self {
            rng: SecureRng::new(),
        }
    }
}

impl Default for DepositManager {
    fn default() -> Self {
        Self::new()
    }
}

