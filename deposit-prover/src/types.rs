//! Type definitions for deposit event proofs

use serde::{Deserialize, Serialize};

/// Number of public inputs the deposit circuit commits to.
///
/// Derived from [`crate::circuit_v2::DEPOSIT_PUBLIC_INPUT_LAYOUT`] rather than
/// written out again, so this and `num_instance()` cannot disagree — the layout
/// moved 7 → 10 → 11 → 12 in one quarter and every hand-maintained copy of the
/// count was a drift waiting to happen (BC-D08).
pub const NUM_PUBLIC_INPUTS: usize = crate::circuit_v2::DEPOSIT_NUM_PUBLIC_INPUTS;

/// Deposit event data from Ethereum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositEventData {
    /// Block number where deposit occurred
    pub block_number: u64,
    /// Transaction index in block
    pub transaction_index: u64,
    /// Log index in transaction receipt
    pub log_index: usize,
    /// Unique deposit ID (prevents double-spending)
    pub deposit_id: u64,
    /// Sender address (topics[2])
    pub sender: [u8; 20],
    /// Amount (from log data) - full uint256 (32 bytes big-endian)
    /// FIX BC-TYPES-001: Changed from u64 to [u8; 32] to support amounts >
    /// 18.44 ETH
    pub amount: [u8; 32],
    /// Acki Nacki destination workchain id (TVM `int8`, from log data).
    pub an_workchain: i8,
    /// Acki Nacki destination account (256-bit TVM address, from log data).
    /// The EVM `sender` is not a valid AN recipient, so the destination is
    /// supplied explicitly at deposit time and carried as ZK public inputs.
    pub an_account: [u8; 32],
    /// Timestamp (from log data)
    pub timestamp: u64,
    /// Contract address that emitted the event
    pub contract_address: [u8; 20],
    /// EIP-155 chain id of the source L1/L2 network. Committed as public
    /// input #4 (see [`NUM_PUBLIC_INPUTS`]); populated by
    /// [`ethereum_fetcher`](crate::ethereum_fetcher) from `eth_chainId`.
    /// `#[serde(default)]` keeps pre-12-PI fixture JSON loadable (falls back
    /// to `0`, which the AN-side allowlist rejects).
    #[serde(default)]
    pub chain_id: u64,
}

/// Receipt proof data for Merkle-Patricia Trie verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptProof {
    /// RLP-encoded receipt
    pub receipt_rlp: Vec<u8>,
    /// Merkle-Patricia Trie proof (list of trie nodes)
    pub proof_nodes: Vec<Vec<u8>>,
    /// Receipt root from block header
    pub receipt_root: [u8; 32],
    /// Block header RLP
    pub block_header_rlp: Vec<u8>,
}

/// Transaction-trie MPT proof for the enclosing deposit tx (chain-id binding).
///
/// Wire format is the typed-tx blob as stored in the transactions trie
/// (`0x02 || RLP(eip1559_fields)` for EIP-1559). Used by Track 2 of
/// `docs/archive/bridge_deposit_chain_binding_fix_proposal_2026_07_20.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransactionProof {
    /// Typed-tx wire bytes (MPT leaf value)
    pub tx_bytes: Vec<u8>,
    /// Merkle-Patricia Trie proof nodes for `rlp(tx_index)` under `transactionsRoot`
    pub proof_nodes: Vec<Vec<u8>>,
    /// `transactionsRoot` from the same block header as [`ReceiptProof`]
    pub transactions_root: [u8; 32],
}

/// Input for deposit proof generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProofInput {
    /// Deposit event data (public)
    pub event_data: DepositEventData,
    /// Receipt proof (private - proves event exists in Ethereum state)
    pub receipt_proof: ReceiptProof,
    /// Transaction proof (private — proves enclosing EIP-1559 tx + `chain_id`).
    /// Required for chain-binding; regenerate fixtures via
    /// `generate_transaction_proof` / `fetch_deposit_proof`.
    /// `#[serde(default)]` keeps pre-Track-2 JSON loadable; the circuit asserts
    /// non-empty `tx_bytes` before proving.
    #[serde(default)]
    pub tx_proof: TransactionProof,
    /// Acki Nacki destination dApp identifier (`UInt256`, 32 bytes big-endian).
    /// Config-supplied (not part of the Ethereum `Deposit` event): the bridge
    /// operator sets which AN dApp a deposit credits. Bound as the
    /// `dappIdHigh`/`dappIdLow` public inputs (replaced `anWorkchain` on
    /// 2026-06-02) and checked by `TokenBridge.finalizeDeposit` on the AN side.
    #[serde(default)]
    pub dapp_id: [u8; 32],
}

impl DepositProofInput {
    /// The `chain_id` the circuit will bind, decoded from the enclosing
    /// EIP-1559 transaction's RLP.
    pub fn witness_chain_id(&self) -> anyhow::Result<u64> {
        crate::rlp_utils::typed_tx_chain_id(&self.tx_proof.tx_bytes)
    }

    /// Fail if the witness proves a different chain than the caller selected.
    ///
    /// `--chain-id` picks the fetch network, but a witness loaded from disk
    /// carries its own chain; without this check the two silently disagree and
    /// the operator learns about it from an AN-side allowlist rejection.
    pub fn require_chain_id(&self, expected: u64) -> anyhow::Result<()> {
        if self.tx_proof.tx_bytes.is_empty() {
            // Pre-Track-2 witness; the circuit rejects it with its own message.
            return Ok(());
        }
        let actual = self.witness_chain_id()?;
        if actual != expected {
            anyhow::bail!(
                "witness proves chain_id {actual}, but --chain-id says {expected}; \
                 re-fetch the witness or pass --chain-id {actual}"
            );
        }
        Ok(())
    }

    /// Resolve which chain this run is about, and check it is one we accept.
    ///
    /// `selected` is the optional `--chain-id` flag. When it is absent the chain
    /// is taken from the witness, which is the only correct default: the flag
    /// used to default to Sepolia while `require_chain_id` ran unconditionally,
    /// so a perfectly valid Base or Arbitrum witness was rejected by a tool that
    /// had simply guessed the wrong network — and the relayer never passes the
    /// flag at all, which made that the production path for every chain except
    /// Sepolia.
    pub fn resolve_chain_id(&self, selected: Option<u64>) -> anyhow::Result<u64> {
        let chain_id = match selected {
            Some(expected) => {
                self.require_chain_id(expected)?;
                expected
            }
            None if self.tx_proof.tx_bytes.is_empty() => anyhow::bail!(
                "witness has no tx_proof.tx_bytes, so its chain_id cannot be read; \
                 re-fetch it with fetch_deposit_data (Track 2) or pass --chain-id"
            ),
            None => self.witness_chain_id()?,
        };
        crate::supported_chains::require_supported_deposit_chain(chain_id)
    }
}

/// Output of deposit proof generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositProofOutput {
    /// The ZK proof bytes (binary)
    pub proof: Vec<u8>,
    /// The ZK proof bytes (hex-encoded for easy use in scripts)
    pub proof_bytes: String,
    /// Public inputs (verified by the circuit)
    pub deposit_id: u64,
    pub sender: [u8; 20],
    /// Amount - full uint256 (32 bytes big-endian)
    /// FIX BC-TYPES-001: Changed from u64 to [u8; 32] to support amounts >
    /// 18.44 ETH
    pub amount: [u8; 32],
    /// Acki Nacki destination dApp identifier (`UInt256`, 32 bytes big-endian).
    /// Config-supplied tag bound as `dappIdHigh`/`dappIdLow` public inputs
    /// (replaced `anWorkchain` on 2026-06-02).
    pub dapp_id: [u8; 32],
    /// Acki Nacki destination account (256-bit TVM address).
    pub an_account: [u8; 32],
    pub contract_address: [u8; 20],
    /// Block hash (32 bytes) - proves the deposit is from a real Ethereum block
    pub block_hash: [u8; 32],
    /// EIP-155 chain id of the source L1/L2 (committed as public input #4).
    /// `#[serde(default)]` keeps pre-12-PI output JSON loadable.
    #[serde(default)]
    pub chain_id: u64,
}

impl DepositProofOutput {
    /// Create a new proof output with hex-encoded proof bytes
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proof: Vec<u8>,
        deposit_id: u64,
        sender: [u8; 20],
        amount: [u8; 32],
        dapp_id: [u8; 32],
        an_account: [u8; 32],
        contract_address: [u8; 20],
        block_hash: [u8; 32],
        chain_id: u64,
    ) -> Self {
        let proof_bytes = format!("0x{}", hex::encode(&proof));
        Self {
            proof,
            proof_bytes,
            deposit_id,
            sender,
            amount,
            dapp_id,
            an_account,
            contract_address,
            block_hash,
            chain_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supported_chains::{CHAIN_ID_BASE, CHAIN_ID_SEPOLIA};

    /// Minimal witness carrying only what the chain-id logic reads: an
    /// EIP-1559 typed-tx prefix whose RLP field 0 is `chain_id`.
    fn input_for_chain(chain_id: u64) -> DepositProofInput {
        let mut rlp = vec![0x02u8, 0xc0 + 5, 0x84];
        rlp.extend_from_slice(&(chain_id as u32).to_be_bytes());
        DepositProofInput {
            event_data: DepositEventData {
                block_number: 1,
                transaction_index: 0,
                log_index: 0,
                deposit_id: 0,
                sender: [0u8; 20],
                amount: [0u8; 32],
                an_workchain: 0,
                an_account: [0u8; 32],
                timestamp: 0,
                contract_address: [0u8; 20],
                chain_id,
            },
            receipt_proof: ReceiptProof {
                receipt_rlp: vec![],
                proof_nodes: vec![],
                receipt_root: [0u8; 32],
                block_header_rlp: vec![],
            },
            tx_proof: TransactionProof {
                tx_bytes: rlp,
                proof_nodes: vec![],
                transactions_root: [0u8; 32],
            },
            dapp_id: [0u8; 32],
        }
    }

    #[test]
    fn resolve_chain_id_reads_the_witness_when_no_flag_is_given() {
        // The regression this guards: `--chain-id` used to default to Sepolia
        // and be enforced unconditionally, so a valid Base witness was rejected
        // by a tool that had merely guessed. The relayer passes no flag at all.
        let input = input_for_chain(CHAIN_ID_BASE);
        assert_eq!(input.resolve_chain_id(None).unwrap(), CHAIN_ID_BASE);
    }

    #[test]
    fn resolve_chain_id_cross_checks_an_explicit_flag() {
        let input = input_for_chain(CHAIN_ID_SEPOLIA);
        assert_eq!(
            input.resolve_chain_id(Some(CHAIN_ID_SEPOLIA)).unwrap(),
            CHAIN_ID_SEPOLIA
        );
        let err = input
            .resolve_chain_id(Some(CHAIN_ID_BASE))
            .unwrap_err()
            .to_string();
        assert!(err.contains("witness proves chain_id"), "got: {err}");
    }

    #[test]
    fn resolve_chain_id_rejects_a_chain_outside_the_allowlist() {
        let input = input_for_chain(31337);
        assert!(input.resolve_chain_id(None).is_err());
        assert!(input.resolve_chain_id(Some(31337)).is_err());
    }

    #[test]
    fn resolve_chain_id_explains_a_pre_track2_witness() {
        let mut input = input_for_chain(CHAIN_ID_SEPOLIA);
        input.tx_proof.tx_bytes.clear();
        let err = input.resolve_chain_id(None).unwrap_err().to_string();
        assert!(err.contains("fetch_deposit_data"), "got: {err}");
    }
}
