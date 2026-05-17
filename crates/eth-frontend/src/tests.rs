//! Tests for Ethereum frontend

use crate::DepositManager;

#[test]
fn test_deposit_manager_creation() {
    let manager = DepositManager::new();
    assert!(std::mem::size_of_val(&manager) == 0); // Empty struct
}

#[test]
fn test_deposit_manager_default() {
    let manager = DepositManager::default();
    assert!(std::mem::size_of_val(&manager) == 0); // Empty struct
}
