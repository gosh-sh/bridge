//! In-process fake of the Ethereum JSON-RPC node [`EthLogSource`] talks to.
//!
//! It answers from an in-memory chain of bridge deposits: `eth_getLogs` is
//! filtered by address, topics and block range the way a node does it (and
//! rejects ranges over 10 blocks, like Alchemy's free tier), `depositCounter()`
//! is answered as of the requested block. Every `eth_getLogs` range and every
//! `depositCounter()` block is recorded, so tests can check what was scanned.
//!
//! [`EthLogSource`]: crate::source::EthLogSource

use std::{
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use alloy::{
    primitives::{Address, B256, U256},
    providers::RootProvider,
    rpc::{client::RpcClient, types::Log},
    sol_types::{SolCall, SolEvent},
    transports::{TransportError, TransportFut},
};
use alloy_json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use serde_json::{json, value::to_raw_value, Value};

use crate::source::AckiNackiBridge::{depositCounterCall, Deposit};

const MAX_LOG_RANGE: u64 = 10;
const SEPOLIA: u64 = 11_155_111;

#[derive(Default)]
struct Chain {
    head: u64,
    /// Block of each deposit, indexed by `depositId`.
    deposit_blocks: Vec<u64>,
    get_logs_ranges: Vec<(u64, u64)>,
    counter_blocks: Vec<u64>,
}

#[derive(Clone)]
pub(crate) struct FakeRpc {
    bridge: Address,
    chain: Arc<Mutex<Chain>>,
}

impl FakeRpc {
    pub(crate) fn new(bridge: Address, head: u64) -> Self {
        let chain = Chain {
            head,
            ..Chain::default()
        };
        Self {
            bridge,
            chain: Arc::new(Mutex::new(chain)),
        }
    }

    pub(crate) fn provider(&self) -> RootProvider {
        RootProvider::new(RpcClient::new(self.clone(), true))
    }

    pub(crate) fn set_head(&self, head: u64) {
        self.lock().head = head;
    }

    /// Mine the next deposit into `block`. Ids follow chain order, as
    /// `depositCounter++` makes them.
    pub(crate) fn deposit(&self, block: u64) -> u64 {
        let mut chain = self.lock();
        if let Some(&last) = chain.deposit_blocks.last() {
            assert!(block >= last, "deposits must follow chain order");
        }
        chain.deposit_blocks.push(block);
        chain.deposit_blocks.len() as u64 - 1
    }

    /// `eth_getLogs` ranges requested since the last call.
    pub(crate) fn take_get_logs_ranges(&self) -> Vec<(u64, u64)> {
        std::mem::take(&mut self.lock().get_logs_ranges)
    }

    /// Blocks `depositCounter()` was read at since the last call.
    pub(crate) fn take_counter_blocks(&self) -> Vec<u64> {
        std::mem::take(&mut self.lock().counter_blocks)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Chain> {
        self.chain.lock().expect("poisoned fake chain")
    }

    fn answer(&self, req: &SerializedRequest) -> Response {
        let params: Value = req
            .params()
            .map(|p| serde_json::from_str(p.get()).expect("params are JSON"))
            .unwrap_or(Value::Null);
        let payload = match self.dispatch(req.method(), &params) {
            Ok(value) => ResponsePayload::Success(to_raw_value(&value).expect("serializable")),
            Err(message) => ResponsePayload::Failure(ErrorPayload {
                code: -32000,
                message: message.into(),
                data: None,
            }),
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, String> {
        let mut chain = self.lock();
        match method {
            "eth_blockNumber" => Ok(json!(format!("{:#x}", chain.head))),
            "eth_chainId" => Ok(json!(format!("{SEPOLIA:#x}"))),
            "eth_call" => {
                let call = &params[0];
                assert_eq!(address(&call["to"]), self.bridge, "eth_call to the bridge");
                let input = call
                    .get("input")
                    .or_else(|| call.get("data"))
                    .and_then(Value::as_str)
                    .expect("eth_call input");
                assert!(
                    input.starts_with(&alloy::hex::encode_prefixed(depositCounterCall::SELECTOR)),
                    "only depositCounter() is expected, got {input}"
                );
                let block = quantity(&params[1]);
                if block > chain.head {
                    return Err(format!("header not found: {block}"));
                }
                chain.counter_blocks.push(block);
                let counter = chain.deposit_blocks.iter().filter(|&&b| b <= block).count();
                Ok(json!(B256::from(U256::from(counter))))
            },
            "eth_getLogs" => {
                let filter = &params[0];
                let (from, to) = (quantity(&filter["fromBlock"]), quantity(&filter["toBlock"]));
                if from > to || to - from + 1 > MAX_LOG_RANGE {
                    return Err(format!(
                        "block range {from}..={to} is over {MAX_LOG_RANGE} blocks"
                    ));
                }
                if to > chain.head {
                    return Err(format!(
                        "block range {from}..={to} is past head {}",
                        chain.head
                    ));
                }
                chain.get_logs_ranges.push((from, to));
                if !matches(&filter["address"], &self.bridge.to_string()) {
                    return Ok(json!([]));
                }
                let topics = &filter["topics"];
                let logs: Vec<Log> = (0..chain.deposit_blocks.len() as u64)
                    .filter(|&id| (from..=to).contains(&chain.deposit_blocks[id as usize]))
                    .map(|id| self.deposit_log(id, chain.deposit_blocks[id as usize]))
                    .filter(|log| {
                        log.topics()
                            .iter()
                            .enumerate()
                            .all(|(i, t)| matches(&topics[i], &t.to_string()))
                    })
                    .collect();
                Ok(json!(logs))
            },
            "eth_getTransactionReceipt" => {
                let tx = params[0].as_str().expect("tx hash");
                let found = (0..chain.deposit_blocks.len() as u64)
                    .find(|&id| tx_hash(id).to_string() == tx.to_ascii_lowercase());
                Ok(match found {
                    Some(id) => self.receipt(id, chain.deposit_blocks[id as usize]),
                    None => Value::Null,
                })
            },
            other => Err(format!("fake RPC does not serve {other}")),
        }
    }

    /// The deposit's log. Its block-global `logIndex` is not its position in
    /// the receipt: an unrelated log comes first in the same transaction.
    fn deposit_log(&self, id: u64, block: u64) -> Log {
        let event = Deposit {
            depositId: U256::from(id),
            sender: Address::repeat_byte(0x11),
            amount: U256::from(1_000 + id),
            anWorkchain: 0,
            anAccount: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
        };
        rpc_log(self.bridge, event.encode_log_data(), id, block, 2 * id + 1)
    }

    fn receipt(&self, id: u64, block: u64) -> Value {
        let other = rpc_log(
            Address::repeat_byte(0x55),
            alloy::primitives::LogData::new_unchecked(
                vec![B256::repeat_byte(0xee)],
                Default::default(),
            ),
            id,
            block,
            2 * id,
        );
        json!({
            "type": "0x2",
            "status": "0x1",
            "cumulativeGasUsed": "0x5208",
            "logs": [other, self.deposit_log(id, block)],
            "logsBloom": format!("0x{}", "0".repeat(512)),
            "transactionHash": tx_hash(id),
            "transactionIndex": "0x0",
            "blockHash": block_hash(block),
            "blockNumber": format!("{block:#x}"),
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x1",
            "from": Address::repeat_byte(0x11),
            "to": self.bridge,
            "contractAddress": null,
        })
    }
}

impl tower_service::Service<RequestPacket> for FakeRpc {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, packet: RequestPacket) -> Self::Future {
        let response = match packet {
            RequestPacket::Single(req) => ResponsePacket::Single(self.answer(&req)),
            RequestPacket::Batch(reqs) => {
                ResponsePacket::Batch(reqs.iter().map(|req| self.answer(req)).collect())
            },
        };
        Box::pin(async move { Ok(response) })
    }
}

pub(crate) fn tx_hash(id: u64) -> B256 {
    B256::from(U256::from(0xd000 + id))
}

pub(crate) fn block_hash(block: u64) -> B256 {
    B256::from(U256::from(0xb000_0000 + block))
}

fn rpc_log(
    address: Address,
    data: alloy::primitives::LogData,
    id: u64,
    block: u64,
    log_index: u64,
) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address,
            data,
        },
        block_hash: Some(block_hash(block)),
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: Some(tx_hash(id)),
        transaction_index: Some(0),
        log_index: Some(log_index),
        removed: false,
    }
}

fn quantity(v: &Value) -> u64 {
    let s = v.as_str().expect("hex quantity");
    u64::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex quantity")
}

fn address(v: &Value) -> Address {
    v.as_str().expect("address").parse().expect("address")
}

/// JSON-RPC filter matching: `null` matches anything, a string matches
/// itself, an array matches any of its members.
fn matches(filter: &Value, value: &str) -> bool {
    match filter {
        Value::Null => true,
        Value::String(s) => s.eq_ignore_ascii_case(value),
        Value::Array(items) => items.iter().any(|item| matches(item, value)),
        _ => false,
    }
}
