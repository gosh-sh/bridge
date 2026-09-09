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

/// What the fake node knows about the withdrawal under test.
pub(crate) struct NodeFixture {
    /// The `--from` multisig, as [`deployed_multisig`] returned it.
    pub account_id: String,
    pub account_boc: String,
    /// The USDCBridge account id the run is pointed at. Answered
    /// `Active`, with a dapp id equal to itself — the shape
    /// `deploy_msig_and_mint.py` produces.
    pub usdc_bridge_account_id: String,
}

/// A running fake TVM node. Drop the handle and the listener dies with
/// the test.
pub(crate) struct FakeNode {
    /// Pass this as `--gql-endpoint`.
    pub url: String,
    /// Every request line the node served, in order. Tests assert on
    /// what the run actually asked for rather than on what it logged.
    pub seen: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
}

/// Serve one withdrawal's worth of TVM node, on an ephemeral port.
///
/// Modelled on `preflight::tests::mock_rpc` — a raw listener, one
/// connection at a time, every request on the connection served rather
/// than just the first. The SDK keeps the connection alive across
/// endpoint resolution, the account fetch and the send, and a handler
/// that answered once and returned closed the socket underneath it.
///
/// The version is answered `0.54.0` on purpose: below 1.0.0 the SDK
/// takes the v2 REST form for `get_account`, which is one `GET` with the
/// address in the query string. The v3 form exists to carry a dapp id
/// the fixture does not need.
pub(crate) async fn fake_node(fixture: NodeFixture) -> FakeNode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let fixture = std::sync::Arc::new(fixture);
    let seen_bg = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let fixture = fixture.clone();
            let seen = seen_bg.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let line = req.lines().next().unwrap_or("").to_string();
                    let body = req.rsplit("\r\n\r\n").next().unwrap_or("").to_string();
                    seen.lock().await.push(line.clone());

                    let json = answer(&fixture, &line, &body);
                    let payload = serde_json::to_string(&json).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: keep-alive\r\n\r\n{}",
                        payload.len(),
                        payload,
                    );
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });

    FakeNode {
        url: format!("{url}/graphql"),
        seen,
    }
}

/// The whole protocol, in one place.
///
/// Four shapes reach a node during stages 1-3, and they are worth naming
/// because none of them is guessed — each was read off the SDK or off
/// this crate:
///
/// * `GET /graphql?query={info…}` — endpoint resolution
///   (`tvm_client/src/net/endpoint.rs`, `QUERY_INFO`).
/// * `GET /v2/account?address=0:…` — `get_account`'s pre-1.0.0 form
///   (`tvm_client/src/account/mod.rs`).
/// * `POST /graphql` — everything `bridge_gql_fetcher::GqlClient` asks, which
///   for preflight is the USDCBridge account's `info`.
/// * `POST /v2/messages` — `send_message`
///   (`tvm_client/src/processing/send_message.rs`).
fn answer(fixture: &NodeFixture, request_line: &str, body: &str) -> serde_json::Value {
    let is_get = request_line.starts_with("GET ");
    if is_get && request_line.contains("/graphql") {
        // Endpoint resolution. A version below 1.0.0 selects the v2 REST
        // form for the account fetch.
        return json!({
            "data": { "info": {
                "version": "0.54.0",
                "time": 1_700_000_000i64,
                "latency": 1i64,
                "rempEnabled": false,
            } }
        });
    }
    if is_get && request_line.contains("/v2/account") {
        return json!({
            "boc": fixture.account_boc,
            "dapp_id": fixture.account_id,
            "state_timestamp": 1_700_000_000i64,
            "account_id": fixture.account_id,
        });
    }
    if request_line.contains("/v2/messages") {
        return json!({
            "result": {
                "message_hash": "00".repeat(32),
                "thread_id": null,
                "producers": [],
                "account_id": fixture.account_id,
                "dapp_id": fixture.account_id,
            },
            "error": null,
            "ext_message_token": null,
        });
    }

    // POST /graphql. Dispatch on what the query asks for, not on an
    // index: the run sends several and their order is the SDK's business.
    let query = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.to_string());

    if query.contains("acc_type_name") {
        return json!({ "data": { "blockchain": { "account": { "info": {
            "dapp_id": fixture.usdc_bridge_account_id,
            "acc_type_name": "Active",
        } } } } });
    }

    // Anything else: a null `data` node, which every caller in this
    // crate turns into a named refusal rather than a panic. A test that
    // needs one of these answered adds it here, so the set of shapes
    // this node knows stays readable in one screen.
    json!({ "data": null })
}

/// Both chains, faked, plus the arguments that point a run at them.
///
/// The tempdirs are fields rather than locals because dropping either
/// takes the state directory or the key file out from under a run that
/// is still going.
pub(crate) struct FakeWorld {
    pub args: crate::args::WithdrawArgs,
    pub node: FakeNode,
    pub state_dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
}

/// A world where a withdrawal of `amount` from a multisig holding `ecc3`
/// passes every check stage 1 makes.
///
/// The EVM half is `preflight`'s own `full_walk`, which is the only
/// answer set in this crate that gets `check_bridge_deploy` to `Ok` —
/// with the two identity words overridden, because the pair the bridge
/// is pinned to has to equal the one THIS withdrawal will prove, and
/// that is computed from the USDCBridge ids the fake node serves.
pub(crate) async fn fake_world(ecc3: u128, amount: &str) -> FakeWorld {
    use crate::preflight::tests::{full_walk, full_walk_code, mock_rpc_code_for};

    let (account_id, account_boc) = deployed_multisig(ecc3).await;
    let usdc_bridge_account_id = "2b".repeat(32);
    let node = fake_node(NodeFixture {
        account_id: account_id.clone(),
        account_boc,
        usdc_bridge_account_id: usdc_bridge_account_id.clone(),
    })
    .await;

    // The fake serves a USDCBridge whose dapp id equals its account id,
    // which is the shape `deploy_msig_and_mint.py` produces.
    let mut id = [0u8; 32];
    hex::decode_to_slice(&usdc_bridge_account_id, &mut id).unwrap();
    let (dapp_fr, acc_fr) = crate::preflight::withdrawal_identity_frs(&id, &id);
    let rpc = mock_rpc_code_for(
        &full_walk_code(),
        full_walk(&[
            (
                crate::preflight::tests::SEL_DAPP_FR,
                format!("0x{dapp_fr:064x}"),
            ),
            (
                crate::preflight::tests::SEL_ACC_FR,
                format!("0x{acc_fr:064x}"),
            ),
        ]),
    )
    .await;

    let keys_dir = tempfile::TempDir::new().unwrap();
    let keys = keys_dir.path().join("keys.json");
    std::fs::write(
        &keys,
        format!(r#"{{"public":"{PAIR_PUBLIC}","secret":"{PAIR_SECRET}"}}"#),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o400)).unwrap();
    }

    let state_dir = tempfile::TempDir::new().unwrap();
    let args = crate::args::WithdrawArgs {
        from: format!("{account_id}::{account_id}"),
        from_keys: keys,
        to: "0x742d35Cc6634C0532925a3b844Bc454e4438f44e".to_string(),
        to_chain: Some(11_155_111),
        amount: amount.to_string(),
        dry_run: false,
        allow_retry: false,
        allow_verifier_drift: false,
        gql_endpoint: node.url.clone(),
        usdc_bridge_account: usdc_bridge_account_id,
        anchor_layer: "1".to_string(),
        i_know_the_wait: false,
        rpc_url: rpc,
        bridge_address: alloy::primitives::Address::repeat_byte(1),
        eth_private_key: None,
        aggregator_dir: None,
        verifiers_dir: None,
        params_dir: None,
        snark_dir: state_dir.path().join("snark"),
        pk_cache_dir: None,
        prover_out_dir: None,
        prover_timeout_s: 60,
        work_dir: None,
        state_dir: Some(state_dir.path().to_path_buf()),
    };

    FakeWorld {
        args,
        node,
        state_dir,
        _keys: keys_dir,
    }
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
