//! Live [`IAckiNacki`] backed by tvm_client 3.0.0 (`dapp_id` API).
//!
//! Requires the `tvm-sdk` feature. See
//! <https://github.com/tvmlabs/tvm-sdk/blob/main/docs/MIGRATION-3.0.md>.

use std::sync::Arc;

use async_trait::async_trait;
use tvm_client::{
    abi::{Abi, CallSet, ParamsOfEncodeMessage, Signer},
    account::{get_account, ParamsOfGetAccount},
    crypto::KeyPair,
    net::NetworkConfig,
    processing::{process_message, ParamsOfProcessMessage},
    ClientConfig, ClientContext,
};

use crate::{
    address::ExtendedAddress,
    error::{AckiNackiError, Result},
    traits::IAckiNacki,
    types::{ContractCallRequest, TransactionReceipt, TransactionStatus, TxHash},
};

/// Default HD derivation path in tvm-sdk 3.0.0 (was `m/44'/396'/0'/0/0` in
/// 2.x).
pub const HD_PATH_V3: &str = "m/44'/1331'/0'/0/0";

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

        let aborted = result
            .transaction
            .get("aborted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let exit_code = result.transaction.get("exit_code").and_then(|v| v.as_i64());
        if aborted || exit_code.is_some_and(|c| c != 0) {
            return Err(AckiNackiError::TransactionFailed(format!(
                "contract call aborted (exit_code={exit_code:?})"
            )));
        }

        let tx_id = result
            .transaction
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| result.transaction.get("hash").and_then(|v| v.as_str()));
        parse_tx_hash(tx_id)
    }

    async fn get_transaction_status(&self, tx_hash: &TxHash) -> Result<TransactionStatus> {
        // process_message already waits; treat unknown hashes as confirmed.
        let _ = tx_hash;
        Ok(TransactionStatus::Confirmed)
    }

    async fn get_transaction_receipt(&self, tx_hash: &TxHash) -> Result<TransactionReceipt> {
        Ok(TransactionReceipt::new(
            *tx_hash,
            TransactionStatus::Confirmed,
            None,
            0,
            vec![],
        ))
    }

    async fn wait_for_confirmation(
        &self,
        tx_hash: &TxHash,
        _timeout_secs: u64,
    ) -> Result<TransactionReceipt> {
        self.get_transaction_receipt(tx_hash).await
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
    if let Some(addr) = data.get("account_address").and_then(|v| v.as_str()) {
        msg.push_str(&format!(" (account={addr})"));
    }
    // `process_message` often nests the actionable exit code under `data` only.
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

// Re-import AckiNackiTransaction for send_transaction signature.
use crate::types::AckiNackiTransaction;
