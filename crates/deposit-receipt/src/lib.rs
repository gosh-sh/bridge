//! The arguments of `eccBridge.finalizeDeposit(chainId, header, txIndex,
//! receiptProof, logIndex)` for one EVM deposit.
//!
//! The bridge hashes `header`, requires the hash to be an accepted block of
//! `chainId`, proves the receipt of transaction `txIndex` against the header's
//! `receiptsRoot` and reads log `logIndex` of that receipt as the bridge's
//! `Deposit` event. [`fetch`] collects those arguments from an RPC node and
//! checks each one the way the bridge will: the header must hash to the block
//! hash, the receipts must rebuild the root, and the log must have the one
//! shape the bridge accepts. It reads receipts as plain JSON, so Arbitrum
//! receipt types (0x64–0x6a, 0x78) go through like Ethereum ones, and OP Stack
//! deposit receipts (0x7e) keep the fields their trie value carries.

use alloy::{
    consensus::{Eip658Value, Receipt, ReceiptWithBloom},
    eips::eip2718::Encodable2718,
    network::{AnyReceiptEnvelope, Network},
    primitives::{keccak256, Address, Bloom, Bytes, Log, LogData, B256, U64},
    providers::Provider,
    rpc::types::Header,
    trie::{proof::ProofRetainer, HashBuilder, Nibbles},
};
use serde::Deserialize;
use serde_json::json;

/// `keccak256("Deposit(uint256,address,uint256,bytes32,uint256,address)")`.
pub const DEPOSIT_TOPIC0: B256 =
    alloy::primitives::b256!("935bca73ba823197b8e5564da08c2c294de3811fc4a4cb482e6af4fd08623c0e");
/// topic0, `depositId`, `sender`.
pub const DEPOSIT_TOPICS: usize = 3;
/// `amount`, `anAccount`, `timestamp`, `token`.
pub const DEPOSIT_DATA_LEN: usize = 4 * 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{method}: {message}")]
    Rpc {
        method: &'static str,
        message: String,
    },
    #[error("{0} not found")]
    NotFound(String),
    #[error("transaction {0} failed: the bridge proves only a successful receipt")]
    Failed(B256),
    #[error("block {0} changed while it was read: {1}")]
    BlockChanged(B256, &'static str),
    #[error("the receipts of block {0} do not rebuild its receiptsRoot")]
    ReceiptsRoot(B256),
    #[error("the header of block {0} does not encode to its hash")]
    Header(B256),
    #[error("log {index} of transaction {tx}: {reason}")]
    NotDeposit {
        tx: B256,
        index: usize,
        reason: String,
    },
    #[error("{what} {value} does not fit the bridge's uint16")]
    TooLarge { what: &'static str, value: usize },
}

/// A receipt as `eth_getTransactionReceipt` and `eth_getBlockReceipts` return
/// it, with only the fields its trie value and its position need.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    #[serde(rename = "type", default)]
    pub ty: Option<U64>,
    #[serde(default)]
    pub status: Option<U64>,
    pub cumulative_gas_used: U64,
    pub logs_bloom: Bloom,
    pub logs: Vec<RpcLog>,
    pub transaction_hash: B256,
    pub transaction_index: U64,
    pub block_hash: B256,
    #[serde(default)]
    pub deposit_nonce: Option<U64>,
    #[serde(default)]
    pub deposit_receipt_version: Option<U64>,
}

/// The EIP-2718 type of an OP Stack deposit transaction, the first of every
/// OP Stack block.
pub const OP_DEPOSIT_TX_TYPE: u8 = 0x7e;

#[derive(Clone, Debug, Deserialize)]
pub struct RpcLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

impl RpcReceipt {
    fn succeeded(&self) -> bool {
        self.status.is_some_and(|s| s == U64::from(1))
    }

    /// The receipt-trie value: the EIP-2718 type byte unless legacy, then
    /// `rlp([status, cumulativeGasUsed, logsBloom, logs])`. An OP Stack deposit
    /// receipt that reports `depositReceiptVersion` (Canyon and later) also
    /// carries `depositNonce` and that version; Mantle reports neither.
    pub fn trie_value(&self) -> Vec<u8> {
        let value = self.base_trie_value();
        let ty = self.ty.map_or(0, |t| t.to::<u64>());
        let Some(version) = self
            .deposit_receipt_version
            .filter(|_| ty == OP_DEPOSIT_TX_TYPE as u64)
        else {
            return value;
        };
        let mut fields = &value[1..];
        let header = alloy::rlp::Header::decode(&mut fields).expect("an encoded receipt");
        let mut payload = fields[..header.payload_length].to_vec();
        alloy::rlp::Encodable::encode(
            &self.deposit_nonce.unwrap_or_default().to::<u64>(),
            &mut payload,
        );
        alloy::rlp::Encodable::encode(&version.to::<u64>(), &mut payload);
        let mut out = vec![OP_DEPOSIT_TX_TYPE];
        alloy::rlp::Header {
            list: true,
            payload_length: payload.len(),
        }
        .encode(&mut out);
        out.extend_from_slice(&payload);
        out
    }

    fn base_trie_value(&self) -> Vec<u8> {
        let logs = self
            .logs
            .iter()
            .map(|l| Log {
                address: l.address,
                data: LogData::new_unchecked(l.topics.clone(), l.data.clone()),
            })
            .collect();
        AnyReceiptEnvelope {
            inner: ReceiptWithBloom {
                receipt: Receipt {
                    status: Eip658Value::Eip658(self.succeeded()),
                    cumulative_gas_used: self.cumulative_gas_used.to(),
                    logs,
                },
                logs_bloom: self.logs_bloom,
            },
            r#type: self.ty.map_or(0, |t| t.to()),
        }
        .encoded_2718()
    }
}

/// The receipt trie over `values` (receipt-trie values in transaction order):
/// its root, and the nodes from the root down to the value at `index`.
pub fn receipt_proof(values: &[Vec<u8>], index: usize) -> (B256, Vec<Bytes>) {
    let mut leaves: Vec<(Nibbles, &[u8])> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (Nibbles::unpack(alloy::rlp::encode(i)), v.as_slice()))
        .collect();
    leaves.sort_by_key(|leaf| leaf.0);
    let target = Nibbles::unpack(alloy::rlp::encode(index));
    let mut builder = HashBuilder::default().with_proof_retainer(ProofRetainer::new(vec![target]));
    for (key, value) in &leaves {
        builder.add_leaf(*key, value);
    }
    let root = builder.root();
    let nodes = builder.take_proof_nodes().matching_nodes_sorted(&target);
    (root, nodes.into_iter().map(|(_, node)| node).collect())
}

/// Refuses a log the bridge would refuse for its shape or its emitter:
/// `bridge`'s `Deposit` with three topics and four data words.
pub fn check_deposit_log(log: &RpcLog, bridge: Address) -> Result<B256, String> {
    if log.address != bridge {
        return Err(format!(
            "emitted by {}, not the bridge {bridge}",
            log.address
        ));
    }
    if log.topics.first() != Some(&DEPOSIT_TOPIC0) {
        return Err("not the bridge's Deposit event".into());
    }
    if log.topics.len() != DEPOSIT_TOPICS || log.data.len() != DEPOSIT_DATA_LEN {
        return Err(format!(
            "{} topics and {} data bytes; the bridge reads {DEPOSIT_TOPICS} and {DEPOSIT_DATA_LEN}",
            log.topics.len(),
            log.data.len()
        ));
    }
    Ok(log.topics[1])
}

/// Everything `finalizeDeposit` takes, and the block it comes from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepositWitness {
    pub chain_id: u64,
    pub block_number: u64,
    pub block_hash: B256,
    pub header: Bytes,
    pub tx_index: u16,
    pub receipt_proof: Vec<Bytes>,
    pub log_index: u16,
}

impl DepositWitness {
    /// The `finalizeDeposit` call input, in the JSON form the TVM ABI encoder
    /// takes.
    pub fn finalize_params(&self) -> serde_json::Value {
        json!({
            "chainId": self.chain_id.to_string(),
            "header": alloy::primitives::hex::encode(&self.header),
            "txIndex": self.tx_index,
            "receiptProof": self
                .receipt_proof
                .iter()
                .map(alloy::primitives::hex::encode)
                .collect::<Vec<_>>(),
            "logIndex": self.log_index,
        })
    }

    /// Bytes of header and proof nodes: the bulk of the message.
    pub fn payload_len(&self) -> usize {
        self.header.len() + self.receipt_proof.iter().map(|p| p.len()).sum::<usize>()
    }

    /// An upper bound on the gas `finalizeDeposit` spends before it accepts
    /// the message: every proof node is hashed and parsed, and every log
    /// before `logIndex` is skipped. Fitted to eccBridge 1.17.0 in
    /// tvm-debugger, it exceeds the measured gas by 4.7–9.3% from 3 to 7 proof
    /// nodes and 0 to 150 logs of 0.2–1 KiB before the deposit.
    pub fn estimated_gas(&self) -> u64 {
        30_000
            + 22_000 * self.receipt_proof.len() as u64
            + 11 * self.payload_len() as u64
            + 2_600 * u64::from(self.log_index)
    }

    /// Refuses a deposit the bridge could not accept: its check would run
    /// out of [`AN_GAS_CREDIT`] first. That also keeps the message far below
    /// the 64 KiB a block manager takes.
    pub fn check_gas(&self) -> Result<(), String> {
        let gas = self.estimated_gas();
        if gas > AN_GAS_CREDIT {
            return Err(format!(
                "checking its receipt proof ({} bytes, {} nodes, log {} of its receipt) takes up \
                 to {gas} gas, over the {AN_GAS_CREDIT} an external message may spend before the \
                 bridge accepts it",
                self.payload_len(),
                self.receipt_proof.len(),
                self.log_index
            ));
        }
        Ok(())
    }
}

/// The gas an external message may spend before the contract accepts it
/// (`gas_credit` in the Acki Nacki blockchain config). `finalizeDeposit`
/// checks the whole deposit within it.
pub const AN_GAS_CREDIT: u64 = 500_000;

async fn rpc<N, P, R>(
    provider: &P,
    method: &'static str,
    params: serde_json::Value,
) -> Result<R, Error>
where
    N: Network,
    P: Provider<N>,
    R: serde::de::DeserializeOwned,
{
    let out: serde_json::Value =
        provider
            .raw_request(method.into(), params)
            .await
            .map_err(|e| Error::Rpc {
                method,
                message: e.to_string(),
            })?;
    if out.is_null() {
        return Err(Error::NotFound(method.to_string()));
    }
    serde_json::from_value(out).map_err(|e| Error::Rpc {
        method,
        message: e.to_string(),
    })
}

/// The `finalizeDeposit` arguments for log `log_index` (counted within the
/// receipt, not the block) of transaction `tx_hash`, emitted by `bridge` on
/// chain `chain_id`. The node must serve `eth_getBlockReceipts` for the block.
pub async fn fetch<N: Network, P: Provider<N>>(
    provider: &P,
    chain_id: u64,
    bridge: Address,
    tx_hash: B256,
    log_index: usize,
) -> Result<DepositWitness, Error> {
    let receipt: RpcReceipt = rpc(provider, "eth_getTransactionReceipt", json!([tx_hash])).await?;
    if !receipt.succeeded() {
        return Err(Error::Failed(tx_hash));
    }
    let log = receipt
        .logs
        .get(log_index)
        .ok_or_else(|| Error::NotDeposit {
            tx: tx_hash,
            index: log_index,
            reason: format!("the receipt has {} logs", receipt.logs.len()),
        })?;
    check_deposit_log(log, bridge).map_err(|reason| Error::NotDeposit {
        tx: tx_hash,
        index: log_index,
        reason,
    })?;

    let block_hash = receipt.block_hash;
    let header: Header = rpc(provider, "eth_getBlockByHash", json!([block_hash, false])).await?;
    if header.hash != block_hash {
        return Err(Error::BlockChanged(
            block_hash,
            "eth_getBlockByHash returned another block",
        ));
    }
    let header_rlp = alloy::rlp::encode(&header.inner);
    if keccak256(&header_rlp) != block_hash {
        return Err(Error::Header(block_hash));
    }

    let mut receipts: Vec<RpcReceipt> =
        rpc(provider, "eth_getBlockReceipts", json!([block_hash])).await?;
    receipts.sort_by_key(|r| r.transaction_index);
    let in_order = receipts
        .iter()
        .enumerate()
        .all(|(i, r)| r.transaction_index == U64::from(i) && r.block_hash == block_hash);
    if !in_order {
        return Err(Error::BlockChanged(
            block_hash,
            "eth_getBlockReceipts is not this block's",
        ));
    }
    let tx_index: usize = receipt.transaction_index.to();
    if receipts.get(tx_index).map(|r| r.transaction_hash) != Some(tx_hash) {
        return Err(Error::BlockChanged(
            block_hash,
            "the transaction is not at its index",
        ));
    }
    let values: Vec<Vec<u8>> = receipts.iter().map(RpcReceipt::trie_value).collect();
    let (root, proof) = receipt_proof(&values, tx_index);
    if root != header.inner.receipts_root {
        return Err(Error::ReceiptsRoot(block_hash));
    }

    Ok(DepositWitness {
        chain_id,
        block_number: header.inner.number,
        block_hash,
        header: header_rlp.into(),
        tx_index: u16::try_from(tx_index).map_err(|_| Error::TooLarge {
            what: "txIndex",
            value: tx_index,
        })?,
        receipt_proof: proof,
        log_index: u16::try_from(log_index).map_err(|_| Error::TooLarge {
            what: "logIndex",
            value: log_index,
        })?,
    })
}

#[cfg(test)]
mod tests;
