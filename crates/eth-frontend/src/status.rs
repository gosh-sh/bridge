//! Status tracking for bridge operations

use serde::{Deserialize, Serialize};

/// Bridge operation status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BridgeStatus {
    /// Operation pending
    Pending,
    /// Operation in progress
    InProgress,
    /// Operation completed
    Completed,
    /// Operation failed
    Failed(String),
}

/// Transaction tracker
pub struct TransactionTracker {}

impl TransactionTracker {
    /// Create a new transaction tracker
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for TransactionTracker {
    fn default() -> Self {
        Self::new()
    }
}
