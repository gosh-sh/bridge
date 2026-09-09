//! A real `UpdateCustodianMultisigWallet` account, built offline, so a
//! test can put one on the wire.
//!
//! `preflight::run` does not read fields off a JSON blob it could be
//! handed: it parses an account BOC with `tvm_block::Account`, checks
//! `status()` and `get_code_hash()`, reads ECC[3] out of the balance
//! collection, and then RUNS `getCustodians` on the account's own code
//! through the TVM. A hand-written fixture cannot satisfy the last of
//! those, and that check is the one that decides whether `--from-keys`
//! matches the on-chain owner.
//!
//! So the fixture is a genuine deploy, executed locally: encode the
//! constructor message the repo's own `deploy_msig_and_mint.py` sends,
//! run it against an emulated uninitialised account, and take the
//! resulting account state. No node is involved — `encode_message` and
//! `run_executor` are in-process — which is what makes this usable from
//! a unit test.
//!
//! The ECC[3] balance is added afterwards. On chain it arrives from the
//! giver's `sendCurrencyWithFlag`; here it is added to the account's
//! currency collection directly, because what the CLI reads is the
//! balance, not how it got there.
#![cfg(test)]

use std::sync::Arc;

use serde_json::json;
use tvm_block::{Account, CurrencyCollection, Deserializable, Serializable};
use tvm_client::{
    abi::{encode_message, Abi, CallSet, DeploySet, ParamsOfEncodeMessage, Signer},
    crypto::KeyPair,
    tvm::{run_executor, AccountForExecutor, ParamsOfRunExecutor},
    ClientConfig, ClientContext,
};

use crate::test_keys::{PAIR_PUBLIC, PAIR_SECRET};

/// The contract this CLI signs for, kept beside its ABI for the same
/// reason that copy is kept: the deploy artifact and the interface the
/// CLI encodes against have to move together.
const MULTISIG_TVC: &[u8] = include_bytes!("../abi/UpdateCustodianMultisigWallet.tvc");

const MULTISIG_ABI_JSON: &str = include_str!("../abi/UpdateCustodianMultisigWallet.abi.json");

/// A deployed single-custodian multisig owned by [`PAIR_PUBLIC`], with
/// `ecc3` in ECC[3].
///
/// Returns `(account_id_hex, account_boc_base64)`. The id is what a
/// `--from` of the form `dapp_id::account_id` carries in both halves —
/// the repo's deploy script sets the dapp id equal to the account id
/// (`msig.py`), and the CLI's own address parsing accepts that.
pub(crate) async fn deployed_multisig(ecc3: u128) -> (String, String) {
    // No endpoints: nothing here talks to a node, and giving it one
    // would let a mistake reach the network from a unit test.
    let ctx =
        Arc::new(ClientContext::new(ClientConfig::default()).expect("an offline client context"));
    let abi = Abi::Json(MULTISIG_ABI_JSON.to_string());

    // The same five constructor arguments `deploy_msig_and_mint.py`
    // sends, in the same shapes. `reqConfirms: 1` is what makes this a
    // single-custodian wallet, which is the only kind preflight accepts.
    let encoded = encode_message(ctx.clone(), ParamsOfEncodeMessage {
        abi: abi.clone(),
        deploy_set: Some(DeploySet {
            tvc: Some(tvm_types::base64_encode(MULTISIG_TVC)),
            // ABI 2.4: the initial public key is explicit rather than
            // taken from the signer.
            initial_data: Some(json!({ "_pubkey": format!("0x{PAIR_PUBLIC}") })),
            ..Default::default()
        }),
        call_set: Some(CallSet {
            function_name: "constructor".to_string(),
            input: Some(json!({
                "owners_pubkey": [format!("0x{PAIR_PUBLIC}")],
                "owners_address": [],
                "reqConfirms": 1,
                "reqConfirmsData": 1,
                "value": 100_000_000u64,
            })),
            ..Default::default()
        }),
        signer: Signer::Keys {
            keys: KeyPair {
                public: PAIR_PUBLIC.to_string(),
                secret: PAIR_SECRET.to_string(),
            },
        },
        ..Default::default()
    })
    .await
    .expect("the constructor message encodes offline");

    let deployed = run_executor(ctx, ParamsOfRunExecutor {
        message: encoded.message,
        // Emulates the uninitialised account the deploy message lands
        // on, with the balance the executor needs for gas.
        account: AccountForExecutor::Uninit,
        return_updated_account: Some(true),
        ..Default::default()
    })
    .await
    .expect("the deploy transaction executes against an uninit account");

    let mut account =
        Account::construct_from_base64(&deployed.account).expect("the executor returns an account");

    // ECC[3] is the withdrawable USDC. On chain the giver sends it; here
    // it is added to the collection the CLI reads.
    if ecc3 > 0 {
        let mut funds = CurrencyCollection::default();
        funds
            .set_other_ex(3, &ecc3.into())
            .expect("ECC[3] fits the collection");
        account
            .add_funds(&funds)
            .expect("adding ECC to a deployed account");
    }

    let boc = tvm_types::base64_encode(
        account
            .write_to_bytes()
            .expect("the account serialises back"),
    );
    let account_id = encoded
        .address
        .split_once(':')
        .expect("the encoder returns a `0:<hex>` address")
        .1
        .to_string();
    (account_id, boc)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything `preflight::run` asks of the `--from` account, asked
    /// here so a failure names the fixture rather than surfacing three
    /// layers away as a preflight refusal about somebody's test.
    #[tokio::test]
    async fn the_fixture_satisfies_what_preflight_reads_off_the_account() {
        let (account_id, boc) = deployed_multisig(5_000_000).await;
        assert_eq!(
            account_id.len(),
            64,
            "a bare 64-hex account id: {account_id}"
        );

        let account = Account::construct_from_base64(&boc).expect("the fixture BOC parses");
        assert_eq!(
            account.status(),
            tvm_block::AccountStatus::AccStateActive,
            "preflight refuses anything but Active",
        );
        assert!(
            account.get_code_hash().is_some(),
            "Active with no code is the corruption preflight names",
        );

        let ecc3 = account
            .balance()
            .and_then(|b| b.other.get(&3).ok().flatten())
            .map(|v| v.value().to_string())
            .expect("ECC[3] is present");
        assert_eq!(
            ecc3, "5000000",
            "the balance the withdrawal is checked against"
        );
    }
}
