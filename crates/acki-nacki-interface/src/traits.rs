//! Traits for Acki Nacki blockchain interaction

use async_trait::async_trait;

use crate::{
    error::Result,
    types::{AckiNackiTransaction, TransactionReceipt, TransactionStatus, TxHash},
};

/// Main interface for interacting with Acki Nacki blockchain
#[async_trait]
pub trait IAckiNacki: Send + Sync {
    /// Send a transaction to the blockchain
    async fn send_transaction(&self, tx: AckiNackiTransaction) -> Result<TxHash>;

    /// Get transaction status
    async fn get_transaction_status(&self, tx_hash: &TxHash) -> Result<TransactionStatus>;

    /// Get transaction receipt
    async fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Result<TransactionReceipt>;

    /// Wait for transaction confirmation
    async fn wait_for_confirmation(
        &self,
        tx_hash: &TxHash,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt>;

    /// Get current block number
    async fn get_block_number(&self) -> Result<u64>;

    /// Get account balance
    async fn get_balance(&self, address: &str) -> Result<u64>;
}

/// Trait for sending transactions with retry logic
#[async_trait]
pub trait TransactionSender: Send + Sync {
    /// Send transaction with automatic retry
    async fn send_with_retry(&self, tx: AckiNackiTransaction, max_retries: u32) -> Result<TxHash>;

    /// Send transaction and wait for confirmation
    async fn send_and_wait(
        &self,
        tx: AckiNackiTransaction,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt>;

    /// Send transaction with retry and wait for confirmation
    async fn send_with_retry_and_wait(
        &self,
        tx: AckiNackiTransaction,
        max_retries: u32,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt>;
}
