//! Self-consistent deposit witnesses for circuit synthesis tests.
//!
//! Mutating a saved fixture cannot test the type-0 gate: any change to
//! `tx_bytes` breaks the MPT root. These builders make a single-leaf tx trie
//! and a single-leaf receipt trie at index 0, then re-encode a header that
//! commits to those roots.

use alloy_rlp::{Encodable, Header};
use ethers::types::{Address, Block, Bloom, Bytes, Log, TransactionReceipt, H256, H64, U256, U64};
use ethers_core::utils::keccak256;

use crate::{
    circuit_v2::{get_deposit_event_signature, SAFEL2_MULTISIG_LOG_DATA, SAFE_EXECTX_MIN_CALLDATA},
    mpt::proof_for_single_leaf,
    rlp_utils::{encode_block_header, encode_receipt, encode_tx_index},
    supported_chains::CHAIN_ID_SEPOLIA,
    types::{DepositEventData, DepositProofInput, ReceiptProof, TransactionProof},
};

#[derive(Clone, Copy, Debug)]
pub enum EnclosingTx {
    /// EIP-1559, empty access list, `data_len` calldata bytes.
    Type2 { data_len: usize },
    /// EIP-2930 with one address + one storage key in the access list.
    Type1WithAccessList { data_len: usize },
    /// Legacy type 0. The circuit must reject this.
    Type0 { data_len: usize },
}

#[derive(Clone, Copy, Debug)]
pub struct SyntheticSpec {
    pub tx: EnclosingTx,
    /// Extra unindexed log of this many data bytes (SafeL2
    /// `SafeMultiSigTransaction`).
    pub extra_log_data: usize,
}

impl SyntheticSpec {
    pub fn type2_direct() -> Self {
        Self {
            tx: EnclosingTx::Type2 {
                data_len: 100,
            },
            extra_log_data: 0,
        }
    }

    pub fn type1_with_access_list() -> Self {
        Self {
            tx: EnclosingTx::Type1WithAccessList {
                data_len: 100,
            },
            extra_log_data: 0,
        }
    }

    pub fn safe_l2_exec() -> Self {
        Self {
            tx: EnclosingTx::Type2 {
                data_len: SAFE_EXECTX_MIN_CALLDATA,
            },
            extra_log_data: SAFEL2_MULTISIG_LOG_DATA,
        }
    }

    pub fn erc4337_handle_ops() -> Self {
        Self {
            tx: EnclosingTx::Type2 {
                data_len: 900,
            },
            extra_log_data: 0,
        }
    }

    pub fn type0_legacy() -> Self {
        Self {
            tx: EnclosingTx::Type0 {
                data_len: 32,
            },
            extra_log_data: 0,
        }
    }
}

fn rlp_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    data.encode(&mut out);
    out
}

fn rlp_u64(n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    n.encode(&mut out);
    out
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload_length = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::new();
    Header {
        list: true,
        payload_length,
    }
    .encode(&mut out);
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

fn access_list_one_slot() -> Vec<u8> {
    let keys = rlp_list(&[rlp_bytes(&[0xab; 32])]);
    let entry = rlp_list(&[rlp_bytes(&[0x11; 20]), keys]);
    rlp_list(&[entry])
}

fn encode_typed_tx(tx_type: u8, fields: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![tx_type];
    out.extend_from_slice(&rlp_list(fields));
    out
}

fn enclosing_tx_bytes(spec: EnclosingTx, to: [u8; 20], chain_id: u64) -> Vec<u8> {
    let v = rlp_u64(0);
    let r = rlp_bytes(&[0x11; 32]);
    let s = rlp_bytes(&[0x22; 32]);
    match spec {
        EnclosingTx::Type2 {
            data_len,
        } => encode_typed_tx(0x02, &[
            rlp_u64(chain_id),
            rlp_u64(0),
            rlp_u64(1),
            rlp_u64(1),
            rlp_u64(21_000),
            rlp_bytes(&to),
            rlp_u64(0),
            rlp_bytes(&vec![0xab; data_len]),
            rlp_list(&[]),
            v,
            r,
            s,
        ]),
        EnclosingTx::Type1WithAccessList {
            data_len,
        } => encode_typed_tx(0x01, &[
            rlp_u64(chain_id),
            rlp_u64(0),
            rlp_u64(1),
            rlp_u64(21_000),
            rlp_bytes(&to),
            rlp_u64(0),
            rlp_bytes(&vec![0xab; data_len]),
            access_list_one_slot(),
            v,
            r,
            s,
        ]),
        EnclosingTx::Type0 {
            data_len,
        } => rlp_list(&[
            rlp_u64(0),
            rlp_u64(1),
            rlp_u64(21_000),
            rlp_bytes(&to),
            rlp_u64(0),
            rlp_bytes(&vec![0xab; data_len]),
            rlp_u64(27),
            r,
            s,
        ]),
    }
}

fn deposit_log(event: &DepositEventData) -> Log {
    let mut deposit_id = [0u8; 32];
    deposit_id[24..].copy_from_slice(&event.deposit_id.to_be_bytes());
    let mut sender = [0u8; 32];
    sender[12..].copy_from_slice(&event.sender);
    let mut data = Vec::with_capacity(128);
    data.extend_from_slice(&event.amount);
    let mut workchain = [0u8; 32];
    workchain[31] = event.an_workchain as u8;
    data.extend_from_slice(&workchain);
    data.extend_from_slice(&event.an_account);
    let mut timestamp = [0u8; 32];
    timestamp[24..].copy_from_slice(&event.timestamp.to_be_bytes());
    data.extend_from_slice(&timestamp);
    Log {
        address: Address::from(event.contract_address),
        topics: vec![
            H256::from(get_deposit_event_signature()),
            H256::from(deposit_id),
            H256::from(sender),
        ],
        data: Bytes::from(data),
        block_hash: None,
        block_number: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        transaction_log_index: None,
        log_type: None,
        removed: None,
    }
}

fn extra_log(data_len: usize) -> Log {
    Log {
        address: Address::from([0x55; 20]),
        topics: vec![],
        data: Bytes::from(vec![0xcd; data_len]),
        block_hash: None,
        block_number: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        transaction_log_index: None,
        log_type: None,
        removed: None,
    }
}

fn header_for_roots(tx_root: [u8; 32], receipt_root: [u8; 32]) -> Vec<u8> {
    let block = Block::<H256> {
        hash: Some(H256::zero()),
        parent_hash: H256::from_low_u64_be(1),
        uncles_hash: H256::from_low_u64_be(2),
        author: Some(Address::zero()),
        state_root: H256::from_low_u64_be(3),
        transactions_root: H256::from(tx_root),
        receipts_root: H256::from(receipt_root),
        number: Some(U64::from(100)),
        gas_used: U256::from(21_000u64),
        gas_limit: U256::from(30_000_000u64),
        extra_data: Bytes::default(),
        logs_bloom: Some(Bloom::default()),
        timestamp: U256::from(1_700_000_000u64),
        difficulty: U256::zero(),
        total_difficulty: None,
        seal_fields: vec![],
        uncles: vec![],
        transactions: vec![],
        size: None,
        mix_hash: Some(H256::from_low_u64_be(6)),
        nonce: Some(H64::zero()),
        base_fee_per_gas: Some(U256::from(7u64)),
        withdrawals_root: None,
        withdrawals: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
        other: Default::default(),
    };
    encode_block_header(&block).expect("synthetic header")
}

fn sample_event() -> DepositEventData {
    let mut amount = [0u8; 32];
    amount[31] = 42;
    DepositEventData {
        block_number: 100,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 7,
        sender: [0x11; 20],
        amount,
        an_workchain: 0,
        an_account: [0x22; 32],
        timestamp: 1_700_000_000,
        contract_address: [0x33; 20],
        chain_id: CHAIN_ID_SEPOLIA,
    }
}

/// Build a self-consistent [`DepositProofInput`] for `spec`.
pub fn build(spec: SyntheticSpec) -> DepositProofInput {
    let event = sample_event();
    let tx_bytes = enclosing_tx_bytes(spec.tx, event.contract_address, event.chain_id);
    let key = encode_tx_index(0);
    let (transactions_root, tx_proof_nodes) =
        proof_for_single_leaf(key.clone(), tx_bytes.clone()).expect("tx trie");

    let mut logs = vec![deposit_log(&event)];
    if spec.extra_log_data > 0 {
        logs.push(extra_log(spec.extra_log_data));
    }
    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::from(0),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(event.block_number)),
        from: Address::from(event.sender),
        to: Some(Address::from(event.contract_address)),
        cumulative_gas_used: U256::from(21_000u64),
        gas_used: Some(U256::from(21_000u64)),
        contract_address: None,
        logs,
        status: Some(U64::from(1)),
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: match spec.tx {
            EnclosingTx::Type2 {
                ..
            } => Some(U64::from(2u64)),
            EnclosingTx::Type1WithAccessList {
                ..
            } => Some(U64::from(1u64)),
            EnclosingTx::Type0 {
                ..
            } => Some(U64::from(0u64)),
        },
        effective_gas_price: None,
        other: Default::default(),
    };
    let receipt_rlp = encode_receipt(&receipt).expect("receipt rlp");
    let (receipt_root, receipt_proof_nodes) =
        proof_for_single_leaf(key, receipt_rlp.clone()).expect("receipt trie");
    let block_header_rlp = header_for_roots(transactions_root, receipt_root);
    // The circuit commits keccak(header) as the blockHash public inputs.
    // We do not need it to match a real chain; keep the event fields as-is.
    let _ = keccak256(&block_header_rlp);

    DepositProofInput {
        event_data: event,
        receipt_proof: ReceiptProof {
            receipt_rlp,
            proof_nodes: receipt_proof_nodes,
            receipt_root,
            block_header_rlp,
        },
        tx_proof: TransactionProof {
            tx_bytes,
            proof_nodes: tx_proof_nodes,
            transactions_root,
        },
        dapp_id: [0u8; 32],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mpt::reject_unprovable_enclosing_tx, rlp_utils::typed_tx_chain_id};

    #[test]
    fn type2_witness_decodes_chain_id() {
        let input = build(SyntheticSpec::type2_direct());
        assert_eq!(
            typed_tx_chain_id(&input.tx_proof.tx_bytes).unwrap(),
            CHAIN_ID_SEPOLIA
        );
        reject_unprovable_enclosing_tx(&input.tx_proof.tx_bytes).unwrap();
    }

    #[test]
    fn type1_witness_has_access_list_and_decodes_chain_id() {
        let input = build(SyntheticSpec::type1_with_access_list());
        assert_eq!(input.tx_proof.tx_bytes.first(), Some(&0x01));
        assert_eq!(
            typed_tx_chain_id(&input.tx_proof.tx_bytes).unwrap(),
            CHAIN_ID_SEPOLIA
        );
        // one address + one key is well above an empty list (`0xc0`)
        assert!(input.tx_proof.tx_bytes.len() > 80);
    }

    #[test]
    fn safe_l2_witness_meets_size_floors() {
        let input = build(SyntheticSpec::safe_l2_exec());
        assert!(input.tx_proof.tx_bytes.len() > SAFE_EXECTX_MIN_CALLDATA);
        assert!(input.receipt_proof.receipt_rlp.len() > SAFEL2_MULTISIG_LOG_DATA);
        reject_unprovable_enclosing_tx(&input.tx_proof.tx_bytes).unwrap();
    }

    #[test]
    fn type2_oversize_calldata_is_rejected_even_if_leaf_fits() {
        let input = build(SyntheticSpec {
            tx: EnclosingTx::Type2 {
                data_len: 2300,
            },
            extra_log_data: 0,
        });
        let err = reject_unprovable_enclosing_tx(&input.tx_proof.tx_bytes)
            .unwrap_err()
            .to_string();
        assert!(err.contains("calldata"), "{err}");
    }

    #[test]
    fn type0_witness_is_legacy_and_rejected_early() {
        let input = build(SyntheticSpec::type0_legacy());
        let first = input.tx_proof.tx_bytes[0];
        assert!(first >= 0xc0, "legacy list header, got {first:#x}");
        let err = reject_unprovable_enclosing_tx(&input.tx_proof.tx_bytes)
            .unwrap_err()
            .to_string();
        assert!(err.contains("neither provable nor refundable"), "{err}");
        assert!(typed_tx_chain_id(&input.tx_proof.tx_bytes).is_err());
    }
}
