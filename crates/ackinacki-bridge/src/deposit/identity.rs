//! Who the bridge thinks a deposit is, and where its voucher lives.
//!
//! The bridge keys a deposit by `tvm.hash(abi.encode(depositId,
//! contractAddr, dappId, chainId))` with `dappId` pinned to zero, and
//! deploys a `DepositVoucher` whose only static data is that hash and a
//! zero `_pubkey`. The voucher's account id is the hash of that state
//! init, so it can be computed before the voucher exists.

use std::sync::Arc;

use alloy_primitives::{hex, Address, U256};
use serde_json::json;
use tvm_client::{
    abi::{
        encode_boc, encode_initial_data, AbiParam, ParamsOfAbiEncodeBoc, ParamsOfEncodeInitialData,
    },
    boc::{
        encode_state_init, get_boc_hash, get_code_from_tvc, ParamsOfEncodeStateInit,
        ParamsOfGetBocHash, ParamsOfGetCodeFromTvc,
    },
    ClientConfig, ClientContext,
};

use crate::errors::{CliError, CliResult};

/// The `DepositVoucher` image the bridge deploys, as compiled in this tree.
pub const VOUCHER_TVC: &[u8] =
    include_bytes!("../../../../contracts/an/0.81.0_compiled/exchange/DepositVoucher.tvc");
/// The `DepositVoucher` ABI; its `fields` lay out the voucher's data.
pub const VOUCHER_ABI: &str =
    include_str!("../../../../contracts/an/0.81.0_compiled/exchange/DepositVoucher.abi.json");
/// The `EthBeaconLightClient` ABI.
pub const LIGHT_CLIENT_ABI: &str =
    include_str!("../../../../contracts/an/0.81.1_compiled/exchange/EthBeaconLightClient.abi.json");
/// The `eccBridge` ABI, a copy of the deployed one.
pub const BRIDGE_ABI: &str = include_str!("../../abi/USDCBridge.abi.json");

/// `getDepositVoucherCodeHash()` of a bridge this build can compute
/// voucher addresses for.
pub const EXPECTED_VOUCHER_CODE_HASH: [u8; 32] =
    hex!("9ef59c91227d007737ff466c458b044592d62e1ef3cda4085d80ce19aca42b41");

/// What the bridge keys a deposit by. The dapp id is always zero and is
/// not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DepositIdentity {
    /// The deposit id from the `Deposit` event.
    pub deposit_id: U256,
    /// The EVM bridge contract that emitted the event.
    pub contract: Address,
    /// The EVM chain id.
    pub chain_id: u64,
}

/// A context for the SDK's local functions; it never connects.
pub fn offline_context() -> Arc<ClientContext> {
    Arc::new(
        ClientContext::new(ClientConfig::default()).expect("a default tvm_client config builds"),
    )
}

/// Maps an SDK error to a preflight refusal that names the step.
fn sdk<T>(what: &str, r: Result<T, tvm_client::error::ClientError>) -> CliResult<T> {
    r.map_err(|e| CliError::Preflight {
        reason: format!("{what}: tvm_client error {}", e.code()),
        source: Some(anyhow::anyhow!("{}", e.message())),
    })
}

/// The representation hash of a base64 BOC.
fn boc_hash(ctx: &Arc<ClientContext>, what: &str, boc: String) -> CliResult<[u8; 32]> {
    let h = sdk(
        what,
        get_boc_hash(ctx.clone(), ParamsOfGetBocHash {
            boc,
        }),
    )?
    .hash;
    let mut out = [0u8; 32];
    hex::decode_to_slice(&h, &mut out).map_err(|e| CliError::Preflight {
        reason: format!("{what}: hash {h} is not 32 bytes: {e}"),
        source: None,
    })?;
    Ok(out)
}

/// The voucher's code cell, as a base64 BOC.
fn voucher_code(ctx: &Arc<ClientContext>) -> CliResult<String> {
    let tvc = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, VOUCHER_TVC);
    Ok(sdk(
        "voucher code",
        get_code_from_tvc(ctx.clone(), ParamsOfGetCodeFromTvc {
            tvc,
        }),
    )?
    .code)
}

/// The hash of the bundled voucher code; a bridge whose
/// `getDepositVoucherCodeHash()` differs deploys vouchers elsewhere.
#[cfg(test)]
pub fn voucher_code_hash(ctx: &Arc<ClientContext>) -> CliResult<[u8; 32]> {
    boc_hash(ctx, "voucher code hash", voucher_code(ctx)?)
}

/// `tvm.hash(abi.encode(depositId, contractAddr, 0, chainId))`, the
/// voucher's `_depositHash`.
pub fn deposit_hash(ctx: &Arc<ClientContext>, id: &DepositIdentity) -> CliResult<[u8; 32]> {
    let p = |name: &str| AbiParam {
        name: name.into(),
        param_type: "uint256".into(),
        ..Default::default()
    };
    let encoded = sdk(
        "deposit hash",
        encode_boc(ctx.clone(), ParamsOfAbiEncodeBoc {
            params: vec![p("a"), p("b"), p("c"), p("d")],
            data: json!({
                "a": id.deposit_id.to_string(),
                "b": U256::from_be_slice(id.contract.as_slice()).to_string(),
                "c": "0",
                "d": id.chain_id.to_string(),
            }),
            boc_cache: None,
        }),
    )?;
    boc_hash(ctx, "deposit hash", encoded.boc)
}

/// The account id of the voucher the bridge deploys for `id`: the hash
/// of its state init, known before the voucher exists.
pub fn voucher_account_id(ctx: &Arc<ClientContext>, id: &DepositIdentity) -> CliResult<[u8; 32]> {
    let dh = deposit_hash(ctx, id)?;
    let data = sdk(
        "voucher data",
        encode_initial_data(ctx.clone(), ParamsOfEncodeInitialData {
            abi: tvm_client::abi::Abi::Json(VOUCHER_ABI.to_string()),
            initial_data: Some(json!({
                "_pubkey": format!("0x{}", "0".repeat(64)),
                "_depositHash": format!("0x{}", hex::encode(dh)),
            })),
            initial_pubkey: None,
            boc_cache: None,
        }),
    )?
    .data;
    let state_init = sdk(
        "voucher state init",
        encode_state_init(ctx.clone(), ParamsOfEncodeStateInit {
            code: Some(voucher_code(ctx)?),
            data: Some(data),
            ..Default::default()
        }),
    )?
    .state_init;
    boc_hash(ctx, "voucher address", state_init)
}

/// The light client's key form: each 16-byte half of the hash reversed.
/// Its own inverse.
pub fn pi_form(h: [u8; 32]) -> [u8; 32] {
    let mut o = h;
    o[..16].reverse();
    o[16..].reverse();
    o
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, U256};

    use super::*;

    #[test]
    fn the_bundled_voucher_is_the_one_the_bridge_deploys() {
        let ctx = offline_context();
        assert_eq!(voucher_code_hash(&ctx).unwrap(), EXPECTED_VOUCHER_CODE_HASH);
    }

    #[test]
    fn pi_form_reverses_each_half_and_is_an_involution() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8;
        }
        let p = pi_form(h);
        assert_eq!(p[0], 15);
        assert_eq!(p[15], 0);
        assert_eq!(p[16], 31);
        assert_eq!(p[31], 16);
        assert_eq!(pi_form(p), h);
    }

    // Destination of the voucher deployment emitted by finalizeDeposit in
    // tvm-debugger with this identity and the tracked DepositVoucher code.
    #[test]
    fn computes_the_voucher_address_used_by_the_bridge() {
        let id = DepositIdentity {
            deposit_id: U256::from(7),
            contract: address!("000000000000000000000000000000000000002a"),
            chain_id: 1,
        };
        assert_eq!(
            voucher_account_id(&offline_context(), &id).unwrap(),
            hex!("3d8ed73a4b4f2dd1a6b6d016dfd23acab2d7ed298da6fe274575fcdf82fdcf3d")
        );
    }

    #[test]
    fn the_bundled_bridge_abi_matches_the_deployed_one() {
        let bundled: serde_json::Value = serde_json::from_str(BRIDGE_ABI).unwrap();
        let deployed: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../contracts/an/0.80.1_compiled/exchange/eccBridge.abi.json"
        ))
        .unwrap();
        let sig = |abi: &serde_json::Value, name: &str| {
            abi["functions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .cloned()
        };
        for f in [
            "finalizeDeposit",
            "retryDeposit",
            "getDepositStatus",
            "confirmDeposit",
            "isPaused",
            "getAnchorConfig",
            "isOwnerAnchorsEnabled",
            "isAcceptedBlockHash",
            "isTrustedL1Bridge",
            "getDepositVoucherCodeHash",
            "getVersion",
            "disableOwnerAnchors",
            "disableOwnerAnchorsOn",
            "getLightClientL1ChainId",
        ] {
            assert!(
                sig(&deployed, f).is_some(),
                "{f} missing from the deployed ABI"
            );
            assert_eq!(sig(&bundled, f), sig(&deployed, f), "{f}");
        }
        // The SDK builds a deploy's initial data from `fields`, so a stale
        // entry here does not fail a call — it makes the constructor read past
        // the end of its own data (exit 9). Compare the whole section.
        assert_eq!(
            bundled["fields"], deployed["fields"],
            "the bundled `fields` must match the deployed data layout"
        );
    }
}
