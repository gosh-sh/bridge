//! Live [`IAckiNacki`] backed by tvm_client 3.0.0 (`dapp_id` API).
//!
//! Requires the `tvm-sdk` feature. See
//! <https://github.com/tvmlabs/tvm-sdk/blob/main/docs/MIGRATION-3.0.md>.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde_json::Value;
use tvm_client::{
    abi::{Abi, CallSet, ParamsOfEncodeMessage, Signer},
    account::{get_account, ParamsOfGetAccount},
    crypto::KeyPair,
    net::{query, NetworkConfig, ParamsOfQuery},
    processing::{process_message, ParamsOfProcessMessage},
    ClientConfig, ClientContext,
};

use crate::{
    address::ExtendedAddress,
    error::{AckiNackiError, Result},
    traits::IAckiNacki,
    types::{
        AckiNackiTransaction, ContractCallRequest, TransactionReceipt, TransactionStatus, TxHash,
    },
};

/// Default HD derivation path in tvm-sdk 3.0.0 (was `m/44'/396'/0'/0/0` in
/// 2.x).
pub const HD_PATH_V3: &str = "m/44'/1331'/0'/0/0";

/// StdContractError: contract cannot be redeployed (DepositVoucher collision).
pub const EXIT_CONSTRUCTOR_ALREADY_CALLED: i32 = 51;
/// USDCBridge: `ERR_INVALID_ZKPROOF`.
pub const EXIT_INVALID_ZKPROOF: i32 = 220;

/// Configuration for a live GraphQL tvm_client connection.
#[derive(Clone, Debug)]
pub struct TvmClientConfig {
    /// GraphQL endpoints (e.g. `http://127.0.0.1:11000/graphql`).
    pub graphql_endpoints: Vec<String>,
    /// Relayer signer keys.
    pub keys: KeyPair,
    /// USDCBridge (or TokenBridge) contract ABI.
    pub bridge_abi: Abi,
}

/// Live Acki Nacki client using tvm_client 3.0.
pub struct TvmAckiNacki {
    context: Arc<ClientContext>,
    bridge_abi: Abi,
    keys: KeyPair,
    /// Receipts captured at `process_message` time, keyed by tx hash.
    ///
    /// `process_message` already produces and confirms the transaction and
    /// returns it in full, so this is the authoritative outcome. Caching it
    /// lets `wait_for_confirmation` answer without a second GraphQL round-trip
    /// — which matters because the legacy `query_collection` collection
    /// endpoint is disabled on current AN networks and the blockchain-API
    /// fallback can lag the transaction by longer than the confirm timeout.
    receipts: Arc<Mutex<HashMap<TxHash, TransactionReceipt>>>,
}

impl TvmAckiNacki {
    /// Connect to the AN GraphQL endpoint(s) with the supplied config.
    pub fn connect(config: TvmClientConfig) -> Result<Self> {
        if config.graphql_endpoints.is_empty() {
            return Err(AckiNackiError::InvalidTransaction(
                "at least one GraphQL endpoint is required".to_string(),
            ));
        }
        let client_config = ClientConfig {
            network: NetworkConfig {
                endpoints: Some(config.graphql_endpoints),
                sending_endpoint_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let context = ClientContext::new(client_config)
            .map_err(|e| AckiNackiError::NetworkError(e.to_string()))?;
        Ok(Self {
            context: Arc::new(context),
            bridge_abi: config.bridge_abi,
            keys: config.keys,
            receipts: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Load a contract ABI from a JSON file path.
    pub fn load_abi(path: &str) -> Result<Abi> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| AckiNackiError::SerializationError(e.to_string()))?;
        Ok(Abi::Json(json))
    }

    /// Whether the connected node speaks the v3 `dapp_id` wire format
    /// (GraphQL `info.version >= 1.0.0`).
    pub async fn supports_dapp_id(&self) -> Result<bool> {
        self.context
            .supports_dapp_id()
            .await
            .map_err(|e| AckiNackiError::NetworkError(e.to_string()))
    }

    /// Probe account state via SDK 3.0 `ParamsOfGetAccount { account_id,
    /// dapp_id }`.
    pub async fn fetch_account_boc(&self, addr: &ExtendedAddress) -> Result<String> {
        let params = ParamsOfGetAccount {
            account_id: addr.account_id().to_owned(),
            dapp_id: addr.dapp_id().to_owned(),
        };
        let res = get_account(self.context.clone(), params)
            .await
            .map_err(|e| AckiNackiError::NetworkError(e.to_string()))?;
        Ok(res.boc)
    }
}

#[async_trait]
impl IAckiNacki for TvmAckiNacki {
    async fn send_transaction(&self, tx: AckiNackiTransaction) -> Result<TxHash> {
        Err(AckiNackiError::InvalidTransaction(format!(
            "raw send_transaction is not supported on the tvm-sdk 3.0 path; use call_contract \
             (from={}, to={})",
            tx.from, tx.to
        )))
    }

    async fn call_contract(&self, call: ContractCallRequest) -> Result<TxHash> {
        let call_set = CallSet::some_with_function_and_input(&call.function, call.params);
        let encode_params = ParamsOfEncodeMessage {
            abi: self.bridge_abi.clone(),
            address: Some(call.to.workchain_address()),
            call_set,
            signer: Signer::Keys {
                keys: self.keys.clone(),
            },
            ..Default::default()
        };
        let process_params = ParamsOfProcessMessage {
            message_encode_params: encode_params,
            send_events: false,
            dapp_id: call.to.dapp_id().to_owned(),
        };
        let result = process_message(self.context.clone(), process_params, |_| async {})
            .await
            .map_err(format_tvm_client_error)?;

        let (status, exit_code, aborted) = classify_tx_json(&result.transaction);
        if !matches!(
            status,
            TransactionStatus::Confirmed | TransactionStatus::Included
        ) {
            return Err(AckiNackiError::TransactionFailed(format!(
                "contract call aborted (exit_code={exit_code:?}, aborted={aborted})"
            )));
        }

        let tx_id = result
            .transaction
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| result.transaction.get("hash").and_then(|v| v.as_str()));
        let hash = parse_tx_hash(tx_id)?;

        // Cache the authoritative receipt from `process_message` so
        // `wait_for_confirmation` resolves without a second (deprecated / laggy)
        // GraphQL query.
        let gas_used = result
            .transaction
            .get("compute")
            .and_then(|c| c.get("gas_used"))
            .and_then(|g| g.as_u64())
            .unwrap_or(0);
        let block_number = result.transaction.get("now").and_then(|n| n.as_u64());
        if let Ok(mut cache) = self.receipts.lock() {
            cache.insert(
                hash,
                TransactionReceipt::with_compute(
                    hash,
                    status,
                    block_number,
                    gas_used,
                    exit_code,
                    aborted,
                ),
            );
        }
        Ok(hash)
    }

    async fn get_transaction_status(&self, tx_hash: &TxHash) -> Result<TransactionStatus> {
        Ok(self.get_transaction_receipt(tx_hash).await?.status)
    }

    async fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Result<TransactionReceipt> {
        // Prefer the receipt captured at `process_message` time — it is the
        // authoritative outcome and needs no network round-trip.
        if let Ok(cache) = self.receipts.lock() {
            if let Some(receipt) = cache.get(tx_hash) {
                return Ok(receipt.clone());
            }
        }
        let id = hex::encode(tx_hash);
        // Query via the modern `blockchain { transaction(hash:) }` API. The
        // legacy `query_collection("transactions", …)` collection endpoint is
        // disabled on current AN networks (shellnet returns "Deprecated API is
        // disabled") — even though `process_message`, which already produced and
        // confirmed this tx, succeeds. Re-querying the deprecated collection
        // here made a fully-successful `finalizeDeposit` report as a network
        // error. The blockchain API is what `tvm-cli` 3.0 uses.
        let gql = format!(
            "{{ blockchain {{ transaction(hash:\"{id}\") {{ aborted now compute {{ exit_code \
             success }} }} }} }}"
        );
        let res = query(self.context.clone(), ParamsOfQuery {
            query: gql,
            variables: None,
        })
        .await
        .map_err(|e| AckiNackiError::NetworkError(e.to_string()))?;
        let tx = res
            .result
            .get("blockchain")
            .and_then(|b| b.get("transaction"))
            .filter(|t| !t.is_null());
        let Some(tx) = tx else {
            // Not yet indexed by the blockchain API — treat as pending so the
            // caller's `wait_for_confirmation` loop keeps polling.
            return Ok(TransactionReceipt::with_compute(
                *tx_hash,
                TransactionStatus::Pending,
                None,
                0,
                None,
                false,
            ));
        };
        let (status, exit_code, aborted) = classify_tx_json(tx);
        let block_number = tx.get("now").and_then(|n| n.as_u64());
        Ok(TransactionReceipt::with_compute(
            *tx_hash,
            status,
            block_number,
            0, // gas_used not exposed by the blockchain transaction query
            exit_code,
            aborted,
        ))
    }

    async fn wait_for_confirmation(
        &self,
        tx_hash: &TxHash,
        timeout_secs: u64,
    ) -> Result<TransactionReceipt> {
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
        loop {
            let receipt = self.get_transaction_receipt(tx_hash).await?;
            if receipt.status.is_finalized() {
                return Ok(receipt);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(receipt); // still Pending
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn get_block_number(&self) -> Result<u64> {
        Err(AckiNackiError::NotImplemented)
    }

    async fn get_balance(&self, address: &str) -> Result<u64> {
        let addr = ExtendedAddress::parse(address)?;
        let boc = self.fetch_account_boc(&addr).await?;
        if boc.is_empty() {
            return Ok(0);
        }
        // Balance decoding is deployment-specific; return non-zero when account exists.
        Ok(1)
    }
}

/// Classify a tvm-sdk transaction JSON into status + compute exit code.
///
/// Prefers `compute.exit_code` (canonical process_message / GraphQL shape);
/// falls back to a top-level `exit_code` for older fixtures.
pub fn classify_tx_json(tx: &Value) -> (TransactionStatus, Option<i32>, bool) {
    let aborted = tx.get("aborted").and_then(|v| v.as_bool()).unwrap_or(false);
    let exit_code = tx
        .get("compute")
        .and_then(|c| c.get("exit_code"))
        .and_then(|v| v.as_i64())
        .or_else(|| tx.get("exit_code").and_then(|v| v.as_i64()))
        .map(|c| c as i32);
    let bad = aborted || exit_code.is_some_and(|c| c != 0);
    let status = if bad {
        TransactionStatus::Reverted
    } else {
        TransactionStatus::Confirmed
    };
    (status, exit_code, aborted)
}

/// Parse `exit_code=N` / `exit_code=Some(N)` from an error string.
pub fn parse_exit_code_from_message(msg: &str) -> Option<i32> {
    for marker in ["exit_code=Some(", "exit_code=", "local_exit_code="] {
        if let Some(rest) = msg.split(marker).nth(1) {
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(code) = digits.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

fn format_tvm_client_error(e: tvm_client::error::ClientError) -> AckiNackiError {
    let mut msg = e.message().to_string();
    let data = e.data();
    if let Some(code) = data.get("exit_code").and_then(|v| v.as_i64()) {
        msg.push_str(&format!(" (exit_code={code})"));
    }
    if let Some(local) = data.get("local_error") {
        if let Some(local_code) = local
            .get("data")
            .and_then(|d| d.get("exit_code"))
            .and_then(|v| v.as_i64())
        {
            msg.push_str(&format!(" (local_exit_code={local_code})"));
        }
        if let Some(local_msg) = local.get("message").and_then(|v| v.as_str()) {
            msg.push_str(&format!(" [{local_msg}]"));
        }
    }
    // Nested compute.exit_code inside error data.
    if let Some(code) = data
        .pointer("/transaction/compute/exit_code")
        .and_then(|v| v.as_i64())
        .or_else(|| data.pointer("/compute/exit_code").and_then(|v| v.as_i64()))
    {
        if !msg.contains("exit_code=") {
            msg.push_str(&format!(" (exit_code={code})"));
        }
    }
    if let Some(addr) = data.get("account_address").and_then(|v| v.as_str()) {
        msg.push_str(&format!(" (account={addr})"));
    }
    if !data.as_object().is_none_or(|o| o.len() <= 1) {
        if let Ok(extra) = serde_json::to_string(data) {
            if extra.len() < 512 {
                msg.push_str(&format!(" data={extra}"));
            }
        }
    }
    AckiNackiError::TransactionFailed(msg)
}

fn parse_tx_hash(id: Option<&str>) -> Result<TxHash> {
    let id = id.ok_or_else(|| {
        AckiNackiError::TransactionFailed("process_message returned no transaction id".to_string())
    })?;
    let bytes = hex::decode(id.trim_start_matches("0x"))
        .map_err(|e| AckiNackiError::SerializationError(e.to_string()))?;
    if bytes.len() != 32 {
        // tvm may return shorter ids on some networks — left-pad.
        let mut hash = [0u8; 32];
        let start = 32usize.saturating_sub(bytes.len());
        hash[start..].copy_from_slice(&bytes);
        return Ok(hash);
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn classify_success() {
        let tx = json!({
            "aborted": false,
            "compute": { "exit_code": 0, "gas_used": 100 }
        });
        let (status, code, aborted) = classify_tx_json(&tx);
        assert_eq!(status, TransactionStatus::Confirmed);
        assert_eq!(code, Some(0));
        assert!(!aborted);
    }

    #[test]
    fn classify_zk_reject_220() {
        let tx = json!({
            "aborted": true,
            "compute": { "exit_code": 220 }
        });
        let (status, code, aborted) = classify_tx_json(&tx);
        assert_eq!(status, TransactionStatus::Reverted);
        assert_eq!(code, Some(220));
        assert!(aborted);
    }

    #[test]
    fn classify_redeploy_51() {
        let tx = json!({
            "aborted": true,
            "compute": { "exit_code": 51 }
        });
        let (status, code, _) = classify_tx_json(&tx);
        assert_eq!(status, TransactionStatus::Reverted);
        assert_eq!(code, Some(EXIT_CONSTRUCTOR_ALREADY_CALLED));
    }

    #[test]
    fn parse_exit_code_variants() {
        assert_eq!(
            parse_exit_code_from_message("contract call aborted (exit_code=Some(51))"),
            Some(51)
        );
        assert_eq!(
            parse_exit_code_from_message("oops (exit_code=220) more"),
            Some(220)
        );
        assert_eq!(
            parse_exit_code_from_message("x (local_exit_code=51) y"),
            Some(51)
        );
        assert_eq!(parse_exit_code_from_message("no code here"), None);
    }
}
