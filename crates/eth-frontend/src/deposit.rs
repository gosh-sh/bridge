//! Deposit flow implementation

use crypto::SecureRng;

/// Deposit manager
pub struct DepositManager {
    _rng: SecureRng,
}

impl DepositManager {
    /// Create a new deposit manager
    pub fn new() -> Self {
        Self {
            _rng: SecureRng::new(),
        }
    }
}

impl Default for DepositManager {
    fn default() -> Self {
        Self::new()
    }
}

