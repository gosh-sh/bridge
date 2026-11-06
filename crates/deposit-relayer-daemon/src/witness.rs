//! [`WitnessBuilder`] — turns a confirmed [`DepositEvent`] into the
//! arguments of `eccBridge.finalizeDeposit(chainId, header, txIndex,
//! receiptProof, logIndex)`.
//!
//! The bridge proves the deposit on chain: it requires the header to hash to
//! an accepted block, walks the receipt proof against the header's
//! `receiptsRoot` and reads the `Deposit` log of that receipt. All the relayer
//! has to collect is the header and the receipt proof:
//!
//! - [`ReceiptWitnessBuilder`] — production. [`deposit_receipt::fetch`] over
//!   the relayer's Ethereum RPC: `eth_getTransactionReceipt`,
//!   `eth_getBlockByHash` and `eth_getBlockReceipts` of the deposit's block,
//!   checked the way the bridge will check them.
//! - [`MockWitnessBuilder`] — deterministic, no RPC, for tests.

use alloy::{
    network::Ethereum,
    primitives::{Address, Bytes},
    providers::Provider,
};
use async_trait::async_trait;
pub use deposit_receipt::DepositWitness;

use crate::{error::RelayerError, types::DepositEvent};

/// Largest header plus receipt proof the relayer sends. The block manager
/// takes external messages of up to 64 KiB through `/v2/messages`; the rest
/// of the message (ABI header, signature, the other arguments, the cell tree
/// overhead) has to fit beside it.
pub const MAX_FINALIZE_PAYLOAD_BYTES: usize = 48 * 1024;

/// Produces the `finalizeDeposit` arguments for a confirmed [`DepositEvent`].
///
/// [`RelayerError::Unprovable`] means the deposit cannot be finalized from its
/// receipt, now or later; any other error is retried.
#[async_trait]
pub trait WitnessBuilder: Send + Sync {
    async fn build(&self, event: &DepositEvent) -> Result<DepositWitness, RelayerError>;
}

/// Refuses a witness whose message would be over the block manager's limit:
/// the bridge never sees it, so retrying cannot help.
pub fn check_payload_len(witness: &DepositWitness) -> Result<(), RelayerError> {
    let len = witness.payload_len();
    if len > MAX_FINALIZE_PAYLOAD_BYTES {
        return Err(RelayerError::Unprovable(format!(
            "the block header and receipt proof are {len} bytes, over the \
             {MAX_FINALIZE_PAYLOAD_BYTES}-byte limit of a finalizeDeposit message"
        )));
    }
    Ok(())
}

/// Refuses a witness of another deposit: another chain, another block (the
/// deposit was reorganised into a different block since it was found) or
/// another log.
pub fn check_witness_matches(
    event: &DepositEvent,
    witness: &DepositWitness,
) -> Result<(), RelayerError> {
    let mismatch = |what: &str, got: String, want: String| {
        Err(RelayerError::ProofGeneration(format!(
            "receipt proof {what} {got} != deposit {what} {want}"
        )))
    };
    if witness.chain_id != event.source_chain_id {
        return mismatch(
            "chainId",
            witness.chain_id.to_string(),
            event.source_chain_id.to_string(),
        );
    }
    if witness.block_hash != event.block_hash {
        return mismatch(
            "block hash",
            witness.block_hash.to_string(),
            event.block_hash.to_string(),
        );
    }
    if u64::from(witness.log_index) != event.log_index {
        return mismatch(
            "log index",
            witness.log_index.to_string(),
            event.log_index.to_string(),
        );
    }
    Ok(())
}

fn classify(e: deposit_receipt::Error) -> RelayerError {
    use deposit_receipt::Error;
    match e {
        Error::Failed(_)
        | Error::NotDeposit {
            ..
        }
        | Error::TooLarge {
            ..
        } => RelayerError::Unprovable(e.to_string()),
        Error::Rpc {
            ..
        }
        | Error::NotFound(_)
        | Error::BlockChanged(..)
        | Error::ReceiptsRoot(_)
        | Error::Header(_) => RelayerError::ProofGeneration(e.to_string()),
    }
}

/// Production builder: [`deposit_receipt::fetch`] over an Ethereum RPC that
/// serves `eth_getBlockReceipts` for the deposit's block.
pub struct ReceiptWitnessBuilder<P> {
    provider: P,
    bridge: Address,
}

impl<P> ReceiptWitnessBuilder<P> {
    pub fn new(provider: P, bridge: Address) -> Self {
        Self {
            provider,
            bridge,
        }
    }
}

#[async_trait]
impl<P> WitnessBuilder for ReceiptWitnessBuilder<P>
where
    P: Provider<Ethereum> + Send + Sync,
{
    async fn build(&self, event: &DepositEvent) -> Result<DepositWitness, RelayerError> {
        let log_index = usize::try_from(event.log_index).map_err(|_| {
            RelayerError::Unprovable(format!(
                "log index {} does not fit the bridge's uint16",
                event.log_index
            ))
        })?;
        let witness = deposit_receipt::fetch::<Ethereum, _>(
            &self.provider,
            event.source_chain_id,
            self.bridge,
            event.tx_hash,
            log_index,
        )
        .await
        .map_err(classify)?;
        check_witness_matches(event, &witness)?;
        Ok(witness)
    }
}

/// Deterministic builder for tests: a witness that matches the event, with a
/// placeholder header and proof.
#[derive(Clone, Debug)]
pub struct MockWitnessBuilder {
    /// When set, `build` fails (retryably) for this `deposit_id`.
    pub fail_on: Option<u64>,
    /// When set, `build` answers [`RelayerError::Unprovable`] for this
    /// `deposit_id`.
    pub unprovable_on: Option<u64>,
    /// Length of the placeholder header.
    pub header_len: usize,
}

impl Default for MockWitnessBuilder {
    fn default() -> Self {
        Self {
            fail_on: None,
            unprovable_on: None,
            header_len: 600,
        }
    }
}

impl MockWitnessBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing_on(deposit_id: u64) -> Self {
        Self {
            fail_on: Some(deposit_id),
            ..Self::default()
        }
    }

    pub fn unprovable_on(deposit_id: u64) -> Self {
        Self {
            unprovable_on: Some(deposit_id),
            ..Self::default()
        }
    }

    pub fn with_header_len(header_len: usize) -> Self {
        Self {
            header_len,
            ..Self::default()
        }
    }

    /// The witness this builder returns for `event`.
    pub fn witness_for(&self, event: &DepositEvent) -> DepositWitness {
        DepositWitness {
            chain_id: event.source_chain_id,
            block_number: event.block_number,
            block_hash: event.block_hash,
            header: Bytes::from(vec![0xf9; self.header_len]),
            tx_index: 0,
            receipt_proof: vec![Bytes::from(vec![0xf8; 100])],
            log_index: u16::try_from(event.log_index).unwrap_or(u16::MAX),
        }
    }
}

#[async_trait]
impl WitnessBuilder for MockWitnessBuilder {
    async fn build(&self, event: &DepositEvent) -> Result<DepositWitness, RelayerError> {
        if self.unprovable_on == Some(event.deposit_id) {
            return Err(RelayerError::Unprovable(format!(
                "mock configured as unprovable on depositId={}",
                event.deposit_id
            )));
        }
        if self.fail_on == Some(event.deposit_id) {
            return Err(RelayerError::ProofGeneration(format!(
                "mock configured to fail on depositId={}",
                event.deposit_id
            )));
        }
        Ok(self.witness_for(event))
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{B256, U256};

    use super::*;

    fn event(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            token: Address::repeat_byte(0x7c),
            sender: Address::repeat_byte(0x11),
            amount: U256::from(1_000_000u64),
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 2,
            block_number: 500,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
        }
    }

    #[tokio::test]
    async fn mock_witness_matches_its_event() {
        let ev = event(9);
        let w = MockWitnessBuilder::new().build(&ev).await.unwrap();
        check_witness_matches(&ev, &w).unwrap();
        check_payload_len(&w).unwrap();
        assert!(MockWitnessBuilder::failing_on(9).build(&ev).await.is_err());
        assert!(matches!(
            MockWitnessBuilder::unprovable_on(9).build(&ev).await,
            Err(RelayerError::Unprovable(_))
        ));
    }

    #[test]
    fn a_witness_of_another_block_chain_or_log_is_refused() {
        let ev = event(1);
        let w = MockWitnessBuilder::new().witness_for(&ev);
        for other in [
            DepositEvent {
                block_hash: B256::repeat_byte(0x01),
                ..ev.clone()
            },
            DepositEvent {
                source_chain_id: 11_155_111,
                ..ev.clone()
            },
            DepositEvent {
                log_index: 3,
                ..ev.clone()
            },
        ] {
            assert!(matches!(
                check_witness_matches(&other, &w),
                Err(RelayerError::ProofGeneration(_))
            ));
        }
    }

    #[test]
    fn a_payload_over_the_message_limit_cannot_be_finalized() {
        let ev = event(1);
        let at_limit =
            MockWitnessBuilder::with_header_len(MAX_FINALIZE_PAYLOAD_BYTES - 100).witness_for(&ev);
        assert_eq!(at_limit.payload_len(), MAX_FINALIZE_PAYLOAD_BYTES);
        check_payload_len(&at_limit).unwrap();
        let over =
            MockWitnessBuilder::with_header_len(MAX_FINALIZE_PAYLOAD_BYTES - 99).witness_for(&ev);
        assert!(matches!(
            check_payload_len(&over),
            Err(RelayerError::Unprovable(_))
        ));
    }

    #[test]
    fn failed_transactions_and_foreign_logs_cannot_be_finalized_rpc_errors_retry() {
        use deposit_receipt::Error;
        for e in [
            Error::Failed(B256::ZERO),
            Error::NotDeposit {
                tx: B256::ZERO,
                index: 0,
                reason: "x".into(),
            },
            Error::TooLarge {
                what: "txIndex",
                value: 70_000,
            },
        ] {
            assert!(matches!(classify(e), RelayerError::Unprovable(_)));
        }
        for e in [
            Error::Rpc {
                method: "eth_getBlockReceipts",
                message: "method not found".into(),
            },
            Error::NotFound("eth_getTransactionReceipt".into()),
            Error::ReceiptsRoot(B256::ZERO),
            Error::Header(B256::ZERO),
            Error::BlockChanged(B256::ZERO, "x"),
        ] {
            assert!(matches!(classify(e), RelayerError::ProofGeneration(_)));
        }
    }
}
