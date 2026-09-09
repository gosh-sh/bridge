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
    /// What the node objected to, if anything.
    ///
    /// The checks in [`answer`] are assertions, and they run inside a
    /// detached `tokio::spawn`: a panic there kills the TASK, not the
    /// test. The socket then closes, the SDK reports a network failure,
    /// and the run under test is refused with a sentence about the
    /// chain — which is exactly the "refusal about the fake, wearing the
    /// clothes of a refusal about the withdrawal" this node was hardened
    /// to stop producing. So the complaint is caught and kept, and
    /// [`FakeNode::complaint`] is what a test reads before believing its
    /// own refusal.
    complaint: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl FakeNode {
    /// Fail the test with what the node objected to, if it objected.
    ///
    /// Called after the run rather than during it, because the run's own
    /// refusal is usually the more interesting failure — unless the
    /// reason for it is that this node refused to answer.
    pub(crate) fn complaint(&self) {
        if let Some(what) = self
            .complaint
            .lock()
            .expect("the fake node's complaint")
            .take()
        {
            panic!(
                "the fake node refused to answer, so what the run reported is not the \
                 withdrawal's fault: {what}"
            );
        }
    }
}

/// Serve one withdrawal's worth of TVM node, on an ephemeral port.
///
/// A raw listener rather than a framework, like `preflight::tests::
/// mock_rpc`, and every request on a connection is served rather than
/// just the first: the SDK keeps the connection alive across endpoint
/// resolution, the account fetch and the send, and a handler that
/// answered once and returned closed the socket underneath it.
///
/// Requests are read to their `Content-Length` rather than to whatever
/// one `read` happened to return. That is not pedantry about a loopback
/// socket: a body split across two segments left the second half in the
/// buffer, and the fake then answered the wrong shape to the next
/// request on the same connection — a failure that would arrive as a
/// puzzling refusal in whichever test was unlucky.
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
    let complaint: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let fixture = std::sync::Arc::new(fixture);
    let seen_bg = seen.clone();
    let complaint_bg = complaint.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let fixture = fixture.clone();
            let seen = seen_bg.clone();
            let complaint = complaint_bg.clone();
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 65536];
                loop {
                    let head_end = loop {
                        if let Some(at) = end_of_headers(&buf) {
                            break at;
                        }
                        // An error is not an EOF, and `unwrap_or(0)`
                        // said it was: a socket that failed mid-request
                        // looked to this loop exactly like a client that
                        // had finished, and the test saw a clean close.
                        match sock.read(&mut chunk).await {
                            Ok(0) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(e) => {
                                object(&complaint, format!("reading a request: {e}"));
                                return;
                            },
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let length = content_length(&head);
                    while buf.len() < head_end + length {
                        match sock.read(&mut chunk).await {
                            Ok(0) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(e) => {
                                object(&complaint, format!("reading a body: {e}"));
                                return;
                            },
                        }
                    }
                    let body =
                        String::from_utf8_lossy(&buf[head_end..head_end + length]).to_string();
                    buf.drain(..head_end + length);

                    let line = head.lines().next().unwrap_or("").to_string();
                    seen.lock().await.push(line.clone());

                    // Caught, so an assertion in `answer` reaches the
                    // test as itself rather than as a closed socket.
                    let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        answer(&fixture, &line, &body)
                    }));
                    let payload = match answered {
                        Ok(json) => serde_json::to_string(&json).unwrap(),
                        Err(panic) => {
                            let what = panic
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                                .unwrap_or_else(|| "a panic with no message".to_string());
                            object(&complaint, what.clone());
                            serde_json::json!({ "errors": [{ "message": what }] }).to_string()
                        },
                    };
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
        complaint,
    }
}

/// Keep the FIRST objection. A closed connection produces more of them,
/// and the first one is the one that explains the rest.
fn object(complaint: &std::sync::Mutex<Option<String>>, what: String) {
    let mut slot = complaint.lock().expect("the fake node's complaint");
    if slot.is_none() {
        *slot = Some(what);
    }
}

/// Where the request head ends, body included from there on.
fn end_of_headers(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// `Content-Length`, or zero for the GETs that carry no body.
fn content_length(head: &str) -> usize {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split_once(':'))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
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
/// * `POST /v2/messages` — `send_message` (`tvm_client/src/net/server_link.rs`,
///   an array of `ExtMessageV2`).
///
/// Anything else PANICS, naming what was asked. The previous default was
/// `{"data": null}`, which every caller in this crate turns into a named
/// refusal — so a test that reached an unimplemented shape would fail
/// with a refusal about the chain rather than about the fake, and one
/// that reached `wait_for_transaction` would poll a null answer until
/// its timeout. A shape this node has not been taught is a defect in the
/// test, and it should read like one.
fn answer(fixture: &NodeFixture, request_line: &str, body: &str) -> serde_json::Value {
    let is_get = request_line.starts_with("GET ");
    if is_get && request_line.contains("/graphql") {
        assert!(
            request_line.contains("version"),
            "the fake node answers `{{info{{version…}}}}` to a GET, and this is not it: \
             {request_line}",
        );
        // A version below 1.0.0 selects the v2 REST form for the account
        // fetch.
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
        // The ADDRESS is checked. Answering with the one account it has,
        // whatever it is asked for, is a fake that cannot tell a run
        // reading the right account from a run reading any other:
        // measured, with a `--from` pointing at an account the node had
        // never heard of and the suite green.
        let asked = request_line
            .split_once("address=")
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split([' ', '&']).next())
            .unwrap_or_default();
        let asked = asked
            .strip_prefix("0:")
            .or_else(|| asked.strip_prefix("0%3A"))
            .or_else(|| asked.strip_prefix("0%3a"))
            .unwrap_or(asked);
        assert_eq!(
            asked, fixture.account_id,
            "this node knows one account and was asked for another. A fake that answers anyway \
             makes every check downstream of it vacuous",
        );
        return json!({
            "boc": fixture.account_boc,
            "dapp_id": fixture.account_id,
            "state_timestamp": 1_700_000_000i64,
            "account_id": fixture.account_id,
        });
    }
    if request_line.contains("/v2/messages") {
        // The hash comes from the BYTES that arrived, and the id the
        // client claims is checked against them. One constant hash for
        // every message is a fake that cannot tell a run that composed
        // the right message from a run that composed any other, and the
        // hash is what the whole resume path is keyed on.
        let sent: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|e| {
            panic!("`/v2/messages` takes a JSON array of messages: {e}: {body}")
        });
        let message = sent
            .get(0)
            .unwrap_or_else(|| panic!("`/v2/messages` takes a non-empty array: {body}"));
        let boc = message["body"]
            .as_str()
            .unwrap_or_else(|| panic!("a message carries its BOC in `body`: {message}"));
        let cell = tvm_types::read_single_root_boc(
            tvm_types::base64_decode(boc).expect("the SDK sends base64"),
        )
        .expect("the SDK sends a message BOC");
        let hash = cell.repr_hash().as_hex_string();
        assert_eq!(
            message["id"].as_str().unwrap_or_default(),
            hash,
            "the id the client claims is not the hash of the message it sent",
        );
        return json!({
            "result": {
                "message_hash": hash,
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

    panic!("the fake node has no answer for `{request_line}` / `{query}`. Teach it one here");
}

/// Both chains, faked, plus the arguments that point a run at them.
///
/// The tempdirs are fields rather than locals because dropping either
/// takes the state directory or the key file out from under a run that
/// is still going.
pub(crate) struct FakeWorld {
    args: Option<crate::args::WithdrawArgs>,
    pub node: FakeNode,
    pub state_dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
    _plumbing: Option<tempfile::TempDir>,
}

impl FakeWorld {
    /// Put a run through this world.
    ///
    /// `--dry-run` lives in TWO places — the field `main` parses out of
    /// the command line, and the parameter `run` actually branches on —
    /// and the fixture used to set the field to `false` while the only
    /// test calling it passed `true`. Nothing read the field, so nothing
    /// noticed; a fixture that contradicts its own caller documents
    /// whatever the reader guesses. One argument sets both here.
    ///
    /// `skip_prompt` is always true: there is no TTY under `cargo test`,
    /// and `confirm_before_burn` refuses without one before anything
    /// else in the branch runs.
    /// The identity these arguments name, parsed the way `run` parses it.
    fn identity(
        &self,
    ) -> (
        crate::args::FromAddress,
        crate::args::ToAddress,
        crate::args::UsdcAmount,
    ) {
        let args = self.args.as_ref().expect("the arguments are still here");
        (
            crate::args::parse_from(&args.from).expect("the fixture's --from parses"),
            crate::args::parse_to(&args.to, args.to_chain).expect("the fixture's --to parses"),
            crate::args::parse_amount(&args.amount).expect("the fixture's --amount parses"),
        )
    }

    /// Leave behind what a first run leaves when the send returns
    /// nothing: a `Reserved` record with no `an_tx_hash`.
    ///
    /// Published by `reserve` rather than written by hand, so it is the
    /// record the code produces — including the cross-field invariants
    /// `read_record` will hold it to.
    ///
    /// This is the population that cannot say whether a burn is on the
    /// wire, and it is the one every exit-code claim in this pipeline is
    /// measured against.
    pub(crate) fn leave_a_hash_less_reservation(&self) -> std::path::PathBuf {
        let (from, to, amount) = self.identity();
        let (record, _) =
            crate::idempotency::reserve(self.state_dir.path(), &from, &to, &amount, false)
                .expect("the first reservation is uncontested");
        assert!(
            record.an_tx_hash.is_none(),
            "a fresh reservation has no hash"
        );
        crate::idempotency::record_path(self.state_dir.path(), &record.key)
    }

    /// Fill in the five submit-only values a real run demands before it
    /// will look at the burn branch.
    ///
    /// Directories only — empty ones. What they are pointed AT is the
    /// wall this fixture cannot climb: `check_prover_artifacts` loads
    /// the Hermez ceremony at k=20 and k=21 and identifies it by its
    /// `s_g2` head, so an empty `--params-dir` is refused there and a
    /// fabricated one would mean defeating the check that stops a
    /// locally generated SRS — under which every proof is forgeable.
    ///
    /// Which makes this the honest boundary of the harness, and it is
    /// worth a test of its own rather than a sentence: a real run gets
    /// through argument parsing, the state directory, the record, the
    /// plumbing, both halves of preflight against two fake chains and
    /// the burner key, and stops on the ceremony.
    /// Point the run at an anchor layer, valid or not.
    ///
    /// `--anchor-layer` is fed by `BRIDGE_ANCHOR_LAYER`, so a re-run
    /// under a different profile can arrive with a value the last one
    /// did not have. That is why its refusal has to know whether a
    /// record exists.
    pub(crate) fn with_anchor_layer(&mut self, layer: &str) {
        self.args
            .as_mut()
            .expect("the arguments are still here")
            .anchor_layer = layer.to_string();
    }

    pub(crate) fn with_submit_plumbing(&mut self) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        // The verifier bytecode has to be there, because supplying
        // `--verifiers-dir` makes stage 1 do MORE: `check_bridge_deploy`
        // reads it and compares it against the deployed Yul runtime.
        // Without it a "real run" stops in the EVM preflight, which is
        // neither the wall this fixture is about nor the one the last
        // round named.
        //
        // The bytes are the pairing `preflight::tests::
        // a_matching_deployed_verifier_passes` uses: a 32-byte CREATE
        // prelude followed by the four bytes `mock_rpc` answers
        // `eth_getCode` with.
        let mut verifier = vec![0u8; 32];
        verifier.extend_from_slice(&[0x60, 0x80, 0x60, 0x40]);
        std::fs::write(
            path.join("BridgeWithdrawalAggregatorVerifier.bin"),
            &verifier,
        )
        .unwrap();
        let args = self.args.as_mut().expect("the arguments are still here");
        args.eth_private_key = Some("1a".repeat(32));
        args.aggregator_dir = Some(path.clone());
        args.verifiers_dir = Some(path.clone());
        args.params_dir = Some(path.clone());
        args.work_dir = Some(path);
        self._plumbing = Some(dir);
    }

    pub(crate) async fn run(
        &mut self,
        dry_run: bool,
    ) -> crate::errors::CliResult<crate::orchestrator::WithdrawSuccess> {
        let mut args = self.args.take().expect("a world puts one run through it");
        args.dry_run = dry_run;
        let outcome = crate::orchestrator::run(args, dry_run, true).await;
        // Before the caller reads the outcome: a refusal caused by this
        // node refusing to answer is not a fact about the withdrawal.
        self.node.complaint();
        outcome
    }
}

/// A world where a DRY withdrawal of `amount` from a multisig holding
/// `ecc3` passes every check it makes.
///
/// Dry, and the qualifier is the honest half of this sentence. A real
/// run is refused before the burn branch twice over, and the first of
/// the two is this fixture's own doing: it leaves the five submit-only
/// values unset, so `require_submit_plumbing` refuses. That is a
/// deliberate default — it is the state the exit-code test needs — and
/// [`FakeWorld::with_submit_plumbing`] fills them in.
///
/// Past it lies the wall proper: `check_prover_artifacts` loads the
/// ceremony at k=20 and k=21 before the burn branch, and it is a ~256 MB
/// Hermez file that this fixture cannot fabricate.
/// Not "has not fabricated yet": `assert_hermez_srs` identifies the
/// Perpetual Powers of Tau ceremony by its `s_g2` head precisely so that
/// a locally generated SRS cannot pass, because one that did would make
/// every proof under it forgeable. A fixture that got past that check
/// would be a fixture that had defeated it.
///
/// So the money path below stage 1 is reachable from a test only where
/// those artifacts are really provisioned, and the refusals that live
/// between the reservation and the send are tested where they are
/// raised — `reserve_and_decide_holding`, `idempotency::reserve` — which
/// is a test of the same code and not of a stub.
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
        args: Some(args),
        node,
        state_dir,
        _keys: keys_dir,
        _plumbing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture for the two checks the node makes on what it is asked.
    /// Any BOC serves as the message: the node hashes the bytes that
    /// arrive, which is the property under test.
    async fn one_account_node() -> (NodeFixture, String) {
        let (account_id, boc) = deployed_multisig(0).await;
        (
            NodeFixture {
                account_id: account_id.clone(),
                account_boc: boc.clone(),
                usdc_bridge_account_id: "2b".repeat(32),
            },
            boc,
        )
    }

    #[tokio::test]
    async fn the_node_answers_with_the_hash_of_the_message_it_was_sent() {
        let (fixture, boc) = one_account_node().await;
        let hash = tvm_types::read_single_root_boc(tvm_types::base64_decode(&boc).unwrap())
            .unwrap()
            .repr_hash()
            .as_hex_string();

        let body = json!([{ "id": hash, "body": boc }]).to_string();
        let answered = answer(&fixture, "POST /v2/messages HTTP/1.1", &body);
        assert_eq!(
            answered["result"]["message_hash"], hash,
            "one constant hash for every message cannot tell one burn from another, and the \
             resume path is keyed on the hash",
        );
    }

    #[tokio::test]
    #[should_panic(expected = "is not the hash of the message it sent")]
    async fn the_node_checks_the_id_against_the_bytes() {
        let (fixture, boc) = one_account_node().await;
        let body = json!([{ "id": "00".repeat(32), "body": boc }]).to_string();
        answer(&fixture, "POST /v2/messages HTTP/1.1", &body);
    }

    #[tokio::test]
    #[should_panic(expected = "knows one account and was asked for another")]
    async fn the_node_refuses_an_account_it_has_never_heard_of() {
        // Measured before this check existed: a production `--from`
        // pointing at an account the node knew nothing about walked the
        // whole of preflight green, because the fake answered with the
        // only account it had.
        let (fixture, _) = one_account_node().await;
        let line = format!("GET /v2/account?address=0:{} HTTP/1.1", "ab".repeat(32));
        answer(&fixture, &line, "");
    }

    #[tokio::test]
    #[should_panic(expected = "has no answer for")]
    async fn a_shape_the_node_has_not_been_taught_is_a_failure_and_not_an_empty_answer() {
        // `{"data": null}` was the default, and it is the worst of both:
        // a caller in this crate turns it into a refusal about the chain,
        // and `wait_for_transaction` polls it until the timeout.
        let (fixture, _) = one_account_node().await;
        answer(
            &fixture,
            "POST /graphql HTTP/1.1",
            &json!({ "query": "query{blockchain{transaction(hash:\"x\"){boc}}}" }).to_string(),
        );
    }

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
