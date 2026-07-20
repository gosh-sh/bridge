//! Mock implementations for testing

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::time::{sleep, Duration};

use crate::{
    error::{AckiNackiError, Result},
    traits::{IAckiNacki, TransactionSender},
    types::{
        AckiNackiTransaction, ContractCallRequest, TransactionReceipt, TransactionStatus, TxHash,
    },
};

/// Mock Acki Nacki blockchain for testing
#[derive(Debug, Clone)]
pub struct MockAckiNacki {
    /// Stored transactions
    transactions: Arc<Mutex<HashMap<TxHash, AckiNackiTransaction>>>,
    /// Transaction receipts
    receipts: Arc<Mutex<HashMap<TxHash, TransactionReceipt>>>,
    /// Current block number
    block_number: Arc<Mutex<u64>>,
    /// Whether to simulate failures
    fail_mode: Arc<Mutex<bool>>,
    /// Receipt status for `call_contract` / `send_transaction` (default
    /// Confirmed).
    finalize_receipt_status: Arc<Mutex<TransactionStatus>>,
}

impl MockAckiNacki {
    /// Create a new mock instance
    pub fn new() -> Self {
        Self {
            transactions: Arc::new(Mutex::new(HashMap::new())),
            receipts: Arc::new(Mutex::new(HashMap::new())),
            block_number: Arc::new(Mutex::new(1)),
            fail_mode: Arc::new(Mutex::new(false)),
            finalize_receipt_status: Arc::new(Mutex::new(TransactionStatus::Confirmed)),
        }
    }

    /// Force finalize-style contract calls to return a specific terminal
    /// status.
    pub fn set_finalize_receipt_status(&self, status: TransactionStatus) {
        *self.finalize_receipt_status.lock().unwrap() = status;
    }

    /// Enable failure mode (all transactions will fail)
    pub fn enable_fail_mode(&self) {
        *self.fail_mode.lock().unwrap() = true;
    }

    /// Disable failure mode
    pub fn disable_fail_mode(&self) {
        *self.fail_mode.lock().unwrap() = false;
    }

    /// Advance block number
    pub fn advance_block(&self) {
        let mut block = self.block_number.lock().unwrap();
        *block += 1;
    }

    /// Get number of transactions
    pub fn transaction_count(&self) -> usize {
        self.transactions.lock().unwrap().len()
    }
}

impl Default for MockAckiNacki {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IAckiNacki for MockAckiNacki {
    async fn send_transaction(&self, tx: AckiNackiTransaction) -> Result<TxHash> {
        let fail_mode = *self.fail_mode.lock().unwrap();
        if fail_mode {
            return Err(AckiNackiError::TransactionFailed(
                "Mock failure mode enabled".to_string(),
            ));
        }

        let tx_hash = *tx.hash();

        // Store transaction
        self.transactions.lock().unwrap().insert(tx_hash, tx);

        // Create receipt
        let block_number = *self.block_number.lock().unwrap();
        let status = *self.finalize_receipt_status.lock().unwrap();
        let receipt = TransactionReceipt::new(tx_hash, status, Some(block_number), 50000, vec![]);

        self.receipts.lock().unwrap().insert(tx_hash, receipt);

        Ok(tx_hash)
    }

    async fn call_contract(&self, call: ContractCallRequest) -> Result<TxHash> {
        let mut tx_hash = [0u8; 32];
        tx_hash[0] = 0xCC;
        let tx = AckiNackiTransaction::new(
            tx_hash,
            call.from.to_string(),
            call.to.to_string(),
            serde_json::to_vec(&call.params)
                .map_err(|e| AckiNackiError::SerializationError(e.to_string()))?,
            1_000_000,
            0,
        );
        self.send_transaction(tx).await
    }

    async fn get_transaction_status(&self, tx_hash: &TxHash) -> Result<TransactionStatus> {
        let receipts = self.receipts.lock().unwrap();
        receipts
            .get(tx_hash)
            .map(|r| r.status)
            .ok_or_else(|| AckiNackiError::TransactionNotFound(hex::encode(tx_hash)))
    }

    async fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Result<TransactionReceipt> {
        let receipts = self.receipts.lock().unwrap();
        receipts
            .get(tx_hash)
            .cloned()
            .ok_or_else(|| AckiNackiError::TransactionNotFound(hex::encode(tx_hash)))
    }

    async fn wait_for_confirmation(
        &self,
        tx_hash: &TxHash,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed().as_secs() > timeout_secs {
                return Err(AckiNackiError::Timeout);
            }

            match self.get_transaction_receipt(tx_hash).await {
                Ok(receipt) if receipt.status.is_finalized() => return Ok(receipt),
                Ok(_) => {
                    sleep(Duration::from_millis(100)).await;
                },
                Err(_) => {
                    sleep(Duration::from_millis(100)).await;
                },
            }
        }
    }

    async fn get_block_number(&self) -> Result<u64> {
        Ok(*self.block_number.lock().unwrap())
    }

    async fn get_balance(&self, _address: &str) -> Result<u64> {
        Ok(1_000_000) // Mock balance
    }
}

/// Mock transaction sender with retry logic
#[derive(Debug, Clone)]
pub struct MockTransactionSender {
    client: MockAckiNacki,
}

impl MockTransactionSender {
    /// Create a new mock transaction sender
    pub fn new(client: MockAckiNacki) -> Self {
        Self {
            client,
        }
    }
}

#[async_trait]
impl TransactionSender for MockTransactionSender {
    async fn send_with_retry(&self, tx: AckiNackiTransaction, max_retries: u32) -> Result<TxHash> {
        let mut last_error = None;

        for attempt in 0..=max_retries {
            match self.client.send_transaction(tx.clone()).await {
                Ok(hash) => return Ok(hash),
                Err(e) => {
                    last_error = Some(e);
                    if attempt < max_retries {
                        sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
                    }
                },
            }
        }

        Err(last_error.unwrap_or_else(|| {
            AckiNackiError::TransactionFailed("Max retries exceeded".to_string())
        }))
    }

    async fn send_and_wait(
        &self,
        tx: AckiNackiTransaction,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt> {
        let tx_hash = self.client.send_transaction(tx).await?;
        self.client
            .wait_for_confirmation(&tx_hash, timeout_secs)
            .await
    }

    async fn send_with_retry_and_wait(
        &self,
        tx: AckiNackiTransaction,
        max_retries: u32,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt> {
        let tx_hash = self.send_with_retry(tx, max_retries).await?;
        self.client
            .wait_for_confirmation(&tx_hash, timeout_secs)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_send_transaction() {
        let mock = MockAckiNacki::new();
        let tx = AckiNackiTransaction::new(
            [1u8; 32],
            "sender".to_string(),
            "recipient".to_string(),
            vec![],
            100000,
            1,
        );

        let result = mock.send_transaction(tx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_fail_mode() {
        let mock = MockAckiNacki::new();
        mock.enable_fail_mode();

        let tx = AckiNackiTransaction::new(
            [1u8; 32],
            "sender".to_string(),
            "recipient".to_string(),
            vec![],
            100000,
            1,
        );

        let result = mock.send_transaction(tx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_transaction_sender_retry() {
        let mock = MockAckiNacki::new();
        let sender = MockTransactionSender::new(mock.clone());

        // Enable fail mode temporarily
        mock.enable_fail_mode();

        let tx = AckiNackiTransaction::new(
            [1u8; 32],
            "sender".to_string(),
            "recipient".to_string(),
            vec![],
            100000,
            1,
        );

        // Should fail after retries
        let result = sender.send_with_retry(tx.clone(), 2).await;
        assert!(result.is_err());

        // Disable fail mode
        mock.disable_fail_mode();

        // Should succeed
        let result = sender.send_with_retry(tx, 2).await;
        assert!(result.is_ok());
    }
}
