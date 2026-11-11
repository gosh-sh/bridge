//! Plain-data types shared between the relayer loop, [`crate::source`],
//! [`crate::witness`] and [`crate::submitter`].
//!
//! A [`DepositEvent`] is the parsed on-chain `Deposit(depositId, sender,
//! amount, anAccount, timestamp, token)` log plus what locates it: the
//! transaction, the log's position inside that transaction's receipt, and the
//! block. [`crate::witness::WitnessBuilder`] turns it into the
//! `finalizeDeposit` arguments ([`crate::witness::DepositWitness`]).

use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

/// A parsed `Deposit` event from `AckiNackiBridge.sol`, augmented with what
/// locates its receipt.
///
/// The `deposit_id` is the bridge's monotonic `depositCounter` value; the
/// relayer processes deposits in increasing `deposit_id` order, treating it
/// as the loop cursor (the on-chain `uint256 depositId` topic is a small
/// counter that always fits in a `u64`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DepositEvent {
    /// Monotonic deposit counter (event topic `depositId`). Loop cursor.
    pub deposit_id: u64,
    /// Depositor address (event topic `sender`).
    pub sender: Address,
    /// Deposited amount (event data word 0).
    pub amount: U256,
    /// Acki Nacki destination account (256-bit TVM address, event data word 1).
    ///
    /// The EVM `sender` is not a valid AN recipient (different address system),
    /// so the destination is chosen by the depositor and carried here. The AN
    /// bridge reads it from the proven receipt, not from the relayer.
    pub an_account: B256,
    /// Block timestamp recorded in the event (event data word 2).
    pub timestamp: U256,
    /// The ERC-20 the depositor named (event data word 3).
    pub token: Address,
    /// Hash of the transaction that emitted the event; its receipt is what
    /// the bridge proves.
    pub tx_hash: B256,
    /// Index of the `Deposit` log within its transaction receipt (not the
    /// block-wide `logIndex`): `finalizeDeposit`'s `logIndex`.
    pub log_index: u64,
    /// Block number the event was mined in.
    pub block_number: u64,
    /// Block hash the event was mined in. The AN bridge must have accepted it
    /// before the deposit can be finalized.
    pub block_hash: B256,
    /// The bridge contract that emitted the event.
    pub source_contract: Address,
    /// EIP-155 `chain_id` of the RPC used to fetch this event (`eth_chainId`):
    /// `finalizeDeposit`'s `chainId`.
    pub source_chain_id: u64,
}
