//! Integration tests for Acki Nacki interface

use super::*;
use crypto::Hash;

#[tokio::test]
async fn test_mock_acki_nacki_basic_flow() {
    let mock = MockAckiNacki::new();

    let tx = AckiNackiTransaction::new(
        Hash::new([1u8; 32]),
        "sender".to_string(),
        "contract".to_string(),
        vec![1, 2, 3],
        100000,
        1,
    );

    // Send transaction
    let tx_hash = mock.send_transaction(tx).await.unwrap();

    // Get status
    let status = mock.get_transaction_status(&tx_hash).await.unwrap();
    assert_eq!(status, TransactionStatus::Confirmed);

    // Get receipt
    let receipt = mock.get_transaction_receipt(&tx_hash).await.unwrap();
    assert!(receipt.is_success());
}

#[tokio::test]
async fn test_transaction_sender_with_retry() {
    let mock = MockAckiNacki::new();
    let sender = MockTransactionSender::new(mock.clone());

    let tx = AckiNackiTransaction::new(
        Hash::new([2u8; 32]),
        "sender".to_string(),
        "contract".to_string(),
        vec![],
        100000,
        1,
    );

    // Send with retry
    let tx_hash = sender.send_with_retry(tx, 3).await.unwrap();

    // Verify transaction was sent
    let receipt = mock.get_transaction_receipt(&tx_hash).await.unwrap();
    assert!(receipt.is_success());
}

#[tokio::test]
async fn test_send_and_wait() {
    let mock = MockAckiNacki::new();
    let sender = MockTransactionSender::new(mock);

    let tx = AckiNackiTransaction::new(
        Hash::new([3u8; 32]),
        "sender".to_string(),
        "contract".to_string(),
        vec![],
        100000,
        1,
    );

    // Send and wait for confirmation
    let receipt = sender.send_and_wait(tx, 10).await.unwrap();
    assert!(receipt.is_success());
}

