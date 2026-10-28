//! Ethereum Data Fetcher
//!
//! This module provides utilities for fetching real Ethereum data including:
//! - Transaction receipts
//! - Block headers
//! - MPT proofs for receipts
//! - Parsing Deposit events from logs

use std::sync::Arc;

use anyhow::{anyhow, Result};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{TransactionReceipt, H160, H256},
};

use crate::{
    mpt::{generate_receipt_proof, generate_transaction_proof},
    require_supported_deposit_chain,
    types::{DepositEventData, DepositProofInput},
};

/// Ethereum data fetcher for deposit proofs
pub struct EthereumFetcher {
    provider: Provider<Http>,
}

impl EthereumFetcher {
    /// Create a new Ethereum fetcher with the given RPC URL
    pub fn new(rpc_url: &str) -> Result<Self> {
        let provider = Provider::<Http>::try_from(rpc_url)?;
        Ok(Self {
            provider,
        })
    }

    /// `eth_chainId` — must be in [`crate::SUPPORTED_DEPOSIT_CHAIN_IDS`].
    pub async fn chain_id(&self) -> Result<u64> {
        let id = self.provider.get_chainid().await?.as_u64();
        require_supported_deposit_chain(id)?;
        Ok(id)
    }

    /// Fetch a transaction receipt by hash
    pub async fn get_receipt(&self, tx_hash: H256) -> Result<TransactionReceipt> {
        self.provider
            .get_transaction_receipt(tx_hash)
            .await?
            .ok_or_else(|| anyhow!("Receipt not found for tx: {:?}", tx_hash))
    }

    /// Parse a Deposit event from a transaction receipt
    ///
    /// Expected event signature: Deposit(uint256 indexed depositId, address
    /// indexed sender, uint256 amount, bytes32 anAccount,
    /// uint256 timestamp)
    pub fn parse_deposit_event(
        &self,
        receipt: &TransactionReceipt,
        contract_address: H160,
        log_index: usize,
    ) -> Result<DepositEventData> {
        // Get the log at the specified index
        let log = receipt
            .logs
            .get(log_index)
            .ok_or_else(|| anyhow!("Log index {} not found in receipt", log_index))?;

        // Verify contract address
        if log.address != contract_address {
            return Err(anyhow!(
                "Log address {:?} doesn't match expected contract {:?}",
                log.address,
                contract_address
            ));
        }

        // Verify event signature
        let expected_signature = get_deposit_event_signature();
        if log.topics.is_empty() || log.topics[0].as_bytes() != &expected_signature {
            return Err(anyhow!(
                "Event signature doesn't match Deposit event. Expected: {:?}, Got: {:?}",
                hex::encode(expected_signature),
                log.topics.get(0).map(|t| hex::encode(t.as_bytes()))
            ));
        }

        // Parse indexed parameters
        if log.topics.len() < 3 {
            return Err(anyhow!(
                "Not enough topics. Expected 3 (signature + 2 indexed params), got {}",
                log.topics.len()
            ));
        }

        // depositId (indexed, topic[1])
        let deposit_id = log.topics[1].to_low_u64_be();

        // sender (indexed, topic[2])
        let sender = {
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&log.topics[2].as_bytes()[12..32]);
            addr
        };

        // Parse non-indexed parameters from data.
        // ABI layout (4 words):
        // [amount(32)][anAccount(32)][timestamp(32)][token(32)]
        if log.data.len() != 128 {
            return Err(anyhow!(
                "Expected 128 bytes of Deposit data (amount + anAccount + timestamp + token), got \
                 {}",
                log.data.len()
            ));
        }

        // amount (word 0)
        // FIX BC-TYPES-001: Changed from u64 to [u8; 32] to support amounts > 18.44 ETH
        let amount = {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&log.data[0..32]);
            bytes
        };

        // anAccount (word 1): 256-bit TVM account address.
        let an_account = {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&log.data[32..64]);
            bytes
        };

        // timestamp (word 2, low 8 bytes)
        let timestamp = {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&log.data[88..96]);
            u64::from_be_bytes(bytes)
        };

        // token (word 3): an ABI-encoded address, left-padded to 32 bytes. The
        // circuit constrains those 12 leading bytes to zero, so reject a word
        // that sets them here rather than letting the proof fail later.
        let token = {
            if log.data[96..108].iter().any(|&b| b != 0) {
                return Err(anyhow!(
                    "Deposit `token` word has non-zero padding: 0x{}",
                    hex::encode(&log.data[96..128])
                ));
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&log.data[108..128]);
            addr
        };

        Ok(DepositEventData {
            block_number: receipt
                .block_number
                .ok_or_else(|| anyhow!("Receipt missing block number"))?
                .as_u64(),
            transaction_index: receipt.transaction_index.as_u64(),
            log_index,
            deposit_id,
            sender,
            amount,
            an_account,
            timestamp,
            token,
            contract_address: contract_address.as_bytes().try_into().unwrap(),
            // `chain_id` is not part of the log — the caller
            // (`fetch_deposit_proof`) stamps the RPC `eth_chainId` on the
            // returned `DepositEventData` after this parse.
            chain_id: 0,
        })
    }

    /// Fetch a complete deposit proof input for a transaction
    ///
    /// This fetches:
    /// - The transaction receipt
    /// - The block header
    /// - The MPT proof for the receipt (by building the receipt trie)
    /// - Parses the Deposit event
    pub async fn fetch_deposit_proof(
        &self,
        tx_hash: H256,
        contract_address: H160,
        log_index: usize,
    ) -> Result<DepositProofInput> {
        println!("Fetching deposit proof for tx: {:?}", tx_hash);

        // Reject unsupported networks early (clear error for operators).
        let chain_id = self.chain_id().await?;
        println!(
            "  - RPC chain_id={} ({})",
            chain_id,
            crate::supported_deposit_chain_name(chain_id).unwrap_or("?")
        );

        // 1. Fetch the receipt
        println!("  - Fetching receipt...");
        let receipt = self.get_receipt(tx_hash).await?;
        println!("    ✓ Receipt found (block: {:?})", receipt.block_number);

        // 2. Parse the Deposit event
        println!("  - Parsing Deposit event...");
        let mut event_data = self.parse_deposit_event(&receipt, contract_address, log_index)?;
        // Stamp RPC `eth_chainId` — required for PI #4 (chain-binding).
        event_data.chain_id = chain_id;
        println!("    ✓ Event parsed (depositId: {})", event_data.deposit_id);

        // 3. Generate MPT proof using the existing implementation
        println!("  - Generating MPT proof...");
        println!("    (This will fetch all receipts in the block and build the trie)");
        let provider_arc = Arc::new(self.provider.clone());
        let receipt_proof = generate_receipt_proof(provider_arc.clone(), tx_hash).await?;
        println!(
            "    ✓ MPT proof generated ({} proof nodes)",
            receipt_proof.proof_nodes.len()
        );

        // 4. Transaction-trie MPT proof (chain_id binding)
        println!("  - Generating transaction MPT proof...");
        let tx_proof = generate_transaction_proof(
            provider_arc,
            event_data.block_number,
            event_data.transaction_index,
        )
        .await?;
        println!(
            "    ✓ Tx MPT proof generated ({} proof nodes, {} wire bytes)",
            tx_proof.proof_nodes.len(),
            tx_proof.tx_bytes.len()
        );

        Ok(DepositProofInput {
            event_data,
            receipt_proof,
            tx_proof,
            // dappId is a config tag, not part of the event. Defaults to zero
            // here; callers (CLI / prover config) overwrite `input.dapp_id`
            // with the operator-configured Acki Nacki dApp identifier.
            dapp_id: [0u8; 32],
        })
    }
}

/// The `Deposit` event signature the fetcher matches `topics[0]` against.
///
/// Deliberately the circuit's own constant rather than a second spelling of
/// the string: the fetcher and the circuit have to agree on the signature, and
/// a local copy is a drift waiting to happen the next time a field is added.
pub fn get_deposit_event_signature() -> [u8; 32] {
    crate::circuit_v2::get_deposit_event_signature()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deposit_event_uses_four_data_words() {
        use ethers::types::{Bytes, Log, U256, U64};

        let bridge = H160::repeat_byte(0x33);
        let account = [0xa3; 32];
        let mut amount = [0u8; 32];
        U256::from(12_500_000u64).to_big_endian(&mut amount);
        let mut timestamp = [0u8; 32];
        U256::from(1_700_000_000u64).to_big_endian(&mut timestamp);
        let token_addr = [0x7cu8; 20];
        let mut token = [0u8; 32];
        token[12..].copy_from_slice(&token_addr);
        let mut receipt = TransactionReceipt {
            block_number: Some(U64::from(100)),
            logs: vec![Log {
                address: bridge,
                topics: vec![
                    H256::from(get_deposit_event_signature()),
                    H256::from_low_u64_be(7),
                    H256::from_low_u64_be(0x1234),
                ],
                data: Bytes::from([amount, account, timestamp, token].concat()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fetcher = EthereumFetcher::new("http://localhost:8545").unwrap();
        let event = fetcher.parse_deposit_event(&receipt, bridge, 0).unwrap();
        assert_eq!(event.deposit_id, 7);
        assert_eq!(event.amount, amount);
        assert_eq!(event.an_account, account);
        assert_eq!(event.timestamp, 1_700_000_000);
        assert_eq!(event.token, token_addr);

        // A shifted payload must not decode: the fields are read at fixed
        // offsets, so an extra leading word would silently re-point every one.
        receipt.logs[0].data = Bytes::from([amount, [0u8; 32], account, timestamp, token].concat());
        assert!(fetcher.parse_deposit_event(&receipt, bridge, 0).is_err());

        // The token word is an ABI-encoded address: set padding is a malformed
        // event, not an address to truncate. The circuit constrains those
        // twelve bytes to zero, so the fetcher refuses it here rather than
        // handing the prover an input that cannot be satisfied.
        let mut padded = token;
        padded[0] = 1;
        receipt.logs[0].data = Bytes::from([amount, account, timestamp, padded].concat());
        assert!(fetcher.parse_deposit_event(&receipt, bridge, 0).is_err());
    }

    #[test]
    fn test_event_signature() {
        let sig = get_deposit_event_signature();
        println!("Deposit event signature: 0x{}", hex::encode(sig));

        // Verify it's a valid keccak256 hash (32 bytes)
        assert_eq!(sig.len(), 32);
    }

    #[test]
    fn test_parse_deposit_event_signature() {
        // The signature should be deterministic
        let sig1 = get_deposit_event_signature();
        let sig2 = get_deposit_event_signature();
        assert_eq!(sig1, sig2);
    }
}
