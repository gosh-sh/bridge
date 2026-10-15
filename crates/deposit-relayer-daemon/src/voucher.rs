//! Off-chain derivation of a deposit's `DepositVoucher` address.
//!
//! `eccUSDCBridge.finalizeDeposit` deploys one voucher per deposit at the
//! address of
//! `StateInit { code: _depositVoucherCode, data: { _depositHash } }`, where
//! `_depositHash = tvm.hash(abi.encode(depositId, contractAddr, dappId,
//! chainId))` and `dappId` is pinned to 0. An active account at that address
//! means the voucher constructor ran, i.e. the deposit was finalized.

use std::sync::Arc;

use alloy::primitives::U256;
use serde_json::json;
use tvm_client::{
    abi::{
        decode_account_data, encode_boc as abi_encode_boc, encode_initial_data, Abi, AbiParam,
        ParamsOfAbiEncodeBoc, ParamsOfDecodeAccountData, ParamsOfEncodeInitialData,
    },
    boc::{encode_state_init, get_boc_hash, ParamsOfEncodeStateInit, ParamsOfGetBocHash},
    ClientConfig, ClientContext,
};

use crate::error::RelayerError;

/// ABI of the `DepositVoucher` the bridge deploys; only its storage layout is
/// used.
const VOUCHER_ABI: &str =
    include_str!("../../../contracts/an/0.81.0_compiled/exchange/DepositVoucher.abi.json");

/// Proof-bound deposit identity, as `finalizeDeposit` hashes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositIdentity {
    pub deposit_id: U256,
    pub contract_addr: U256,
    pub chain_id: U256,
}

fn err(what: &str, e: impl std::fmt::Display) -> RelayerError {
    RelayerError::other(format!("voucher address: {what}: {e}"))
}

fn uint(v: U256) -> String {
    format!("0x{v:064x}")
}

/// Offline tvm_client context for the ABI/BOC helpers; no network is used.
pub fn offline_context() -> Result<Arc<ClientContext>, RelayerError> {
    ClientContext::new(ClientConfig::default())
        .map(Arc::new)
        .map_err(|e| err("client context", e))
}

/// `tvm.hash(abi.encode(depositId, contractAddr, 0, chainId))`.
pub fn deposit_hash(ctx: &Arc<ClientContext>, id: &DepositIdentity) -> Result<U256, RelayerError> {
    let param = |name: &str| AbiParam {
        name: name.to_string(),
        param_type: "uint256".to_string(),
        ..Default::default()
    };
    let encoded = abi_encode_boc(ctx.clone(), ParamsOfAbiEncodeBoc {
        params: vec![param("a"), param("b"), param("c"), param("d")],
        data: json!({
            "a": uint(id.deposit_id),
            "b": uint(id.contract_addr),
            "c": uint(U256::ZERO),
            "d": uint(id.chain_id),
        }),
        boc_cache: None,
    })
    .map_err(|e| err("abi.encode", e))?;
    let hash = get_boc_hash(ctx.clone(), ParamsOfGetBocHash {
        boc: encoded.boc,
    })
    .map_err(|e| err("hash", e))?
    .hash;
    hash.parse::<U256>()
        .or_else(|_| U256::from_str_radix(&hash, 16))
        .map_err(|e| err("hash hex", e))
}

/// Account id (64 hex chars, no workchain) of the voucher `voucher_code`
/// deploys for `id`.
pub fn voucher_account_id(
    ctx: &Arc<ClientContext>,
    voucher_code: &str,
    id: &DepositIdentity,
) -> Result<String, RelayerError> {
    let data = encode_initial_data(ctx.clone(), ParamsOfEncodeInitialData {
        abi: Abi::Json(VOUCHER_ABI.to_string()),
        // The bridge deploys with no pubkey.
        initial_data: Some(json!({
            "_pubkey": uint(U256::ZERO),
            "_depositHash": uint(deposit_hash(ctx, id)?),
        })),
        initial_pubkey: None,
        boc_cache: None,
    })
    .map_err(|e| err("initial data", e))?
    .data;
    let state_init = encode_state_init(ctx.clone(), ParamsOfEncodeStateInit {
        code: Some(voucher_code.to_string()),
        data: Some(data),
        ..Default::default()
    })
    .map_err(|e| err("state init", e))?
    .state_init;
    Ok(get_boc_hash(ctx.clone(), ParamsOfGetBocHash {
        boc: state_init,
    })
    .map_err(|e| err("state init hash", e))?
    .hash)
}

/// `_depositVoucherCode` out of the bridge's account data (base64 BOC).
pub fn voucher_code_from_bridge_data(
    ctx: &Arc<ClientContext>,
    bridge_abi: &Abi,
    bridge_data: &str,
) -> Result<String, RelayerError> {
    let decoded = decode_account_data(ctx.clone(), ParamsOfDecodeAccountData {
        abi: bridge_abi.clone(),
        data: bridge_data.to_string(),
        allow_partial: true,
    })
    .map_err(|e| err("decode bridge data", e))?
    .data;
    decoded
        .get("_depositVoucherCode")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| err("bridge data", "no _depositVoucherCode field"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shellnet, 2026-10-12: the voucher `finalizeDeposit` tx `4b722bbe…`
    /// deployed. Code is the voucher account's own (hash `bd44b82a…`); the
    /// identity is decoded from its constructor message `9aa9755e…`.
    const SHELLNET_VOUCHER_CODE: &str = include_str!("../tests/fixtures/deposit_voucher_code.b64");
    const SHELLNET_VOUCHER: &str =
        "ffd5c4e7770b73e67ad8a44c828c0d2ee2e642cbbff53096ffd0c68ed0f9a80e";

    fn shellnet_identity() -> DepositIdentity {
        DepositIdentity {
            deposit_id: U256::from(DEPOSIT_ID),
            contract_addr: U256::from_str_radix(CONTRACT_ADDR, 16).unwrap(),
            chain_id: U256::from(CHAIN_ID),
        }
    }

    const DEPOSIT_ID: u64 = 1;
    const CONTRACT_ADDR: &str = "32b9e87acaa1ad7d61a81f93dd9d525f64ff4f38";
    const CHAIN_ID: u64 = 11_155_111; // Sepolia

    #[test]
    fn reproduces_a_shellnet_voucher_address() {
        let ctx = offline_context().unwrap();
        let got =
            voucher_account_id(&ctx, SHELLNET_VOUCHER_CODE.trim(), &shellnet_identity()).unwrap();
        assert_eq!(got, SHELLNET_VOUCHER);
    }

    #[test]
    fn identity_changes_the_address() {
        let ctx = offline_context().unwrap();
        let mut other = shellnet_identity();
        other.chain_id += U256::from(1u8);
        let a = voucher_account_id(&ctx, SHELLNET_VOUCHER_CODE.trim(), &shellnet_identity());
        let b = voucher_account_id(&ctx, SHELLNET_VOUCHER_CODE.trim(), &other);
        assert_ne!(a.unwrap(), b.unwrap());
    }
}
