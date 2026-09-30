//! The Acki Nacki side. Reads go through GraphQL and the SDK's local TVM;
//! the one write is `finalizeDeposit`, an unsigned external message the
//! SDK posts to the host's `/v2/messages`.
//!
//! Accounts are addressed as `<id>::<id>`: the node finds an account by
//! its own id in the dapp role, whichever dapp it lives in. GraphQL
//! resolves relations (a message's transaction, a transaction's messages)
//! only in single-object queries, never in lists.
//!
//! The schema calls a transaction's and a message's hash `id`; the queries
//! alias it to `hash`, which is what the parsers read.

use std::sync::Arc;

use async_trait::async_trait;
use bridge_gql_fetcher::gql_client::{create_client, GqlClient};
use serde_json::{json, Value};
use tvm_client::{
    abi::{
        decode_message_body, encode_message, Abi, CallSet, ParamsOfDecodeMessageBody,
        ParamsOfEncodeMessage, Signer,
    },
    account::{get_account, ParamsOfGetAccount},
    error::ClientError,
    processing::{
        send_message, wait_for_transaction, ErrorCode, ParamsOfSendMessage,
        ParamsOfWaitForTransaction,
    },
    tvm::{run_tvm, ParamsOfRunTvm, StdContractError},
    ClientContext,
};

use crate::{
    deposit::refusals::{exit_code_from_sdk, FinalizeSend},
    errors::{CliError, CliResult},
};

/// An account's state, as GraphQL names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccStatus {
    /// Deployed: it has code.
    Active,
    /// It holds a balance but no code yet.
    Uninit,
    /// Frozen for unpaid storage.
    Frozen,
    /// Not on chain.
    NonExist,
}

impl AccStatus {
    /// Reads `acc_type_name`, `orig_status_name` or `end_status_name`;
    /// anything unknown counts as not on chain.
    fn parse(s: &str) -> AccStatus {
        match s {
            "Active" => AccStatus::Active,
            "Uninit" => AccStatus::Uninit,
            "Frozen" => AccStatus::Frozen,
            _ => AccStatus::NonExist,
        }
    }
}

/// What the CLI reads about an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    /// Its state.
    pub status: AccStatus,
    /// The dapp it lives in; absent until it is deployed.
    pub dapp_id: Option<[u8; 32]>,
    /// Its ECC[3] (eccUSDC) balance, in micro-USDC.
    pub ecc3: u128,
}

/// A message's transaction, as a message query resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRef {
    /// The transaction hash.
    pub hash: String,
    /// The transaction aborted.
    pub aborted: bool,
    /// The account it ran on, `0:<id>`.
    pub account: String,
    /// Its time, in seconds.
    pub now: u64,
}

/// A message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgView {
    /// The message hash.
    pub hash: String,
    /// `Internal`, `ExtIn` or `ExtOut`.
    pub msg_type: String,
    /// The sender, `0:<id>`; empty for an inbound external message.
    pub src: String,
    /// The receiver; an outbound external message's is `:<event address>`.
    pub dst: String,
    /// The body, a base64 BOC.
    pub body: Option<String>,
    /// The currencies it carries: `(currency, amount)`.
    pub ecc: Vec<(u32, u128)>,
    /// The bounce flag, for an internal message.
    pub bounce: Option<bool>,
    /// When it was created, in seconds.
    pub created_at: u64,
    /// The transaction that received it; only a single-message query
    /// resolves it.
    pub dst_tx: Option<TxRef>,
    /// The hash of the transaction that sent it; only a single-message
    /// query resolves it.
    pub src_tx: Option<String>,
}

/// A transaction with what it sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxView {
    /// The transaction hash.
    pub hash: String,
    /// It aborted.
    pub aborted: bool,
    /// The compute phase's exit code, if it ran.
    pub exit_code: Option<i32>,
    /// The account it ran on, `0:<id>`.
    pub account: String,
    /// Its time, in seconds.
    pub now: u64,
    /// The messages it sent.
    pub out: Vec<MsgView>,
    /// How its account's currencies changed: `(currency, delta)`.
    pub ecc_delta: Vec<(u32, i128)>,
}

/// A transaction in an account's list, where relations are not resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxListItem {
    /// The transaction hash.
    pub hash: String,
    /// Its time, in seconds.
    pub now: u64,
    /// The account's state before it.
    pub orig_status: AccStatus,
    /// The account's state after it.
    pub end_status: AccStatus,
    /// It aborted.
    pub aborted: bool,
    /// The hashes of the messages it sent.
    pub out_msgs: Vec<String>,
}

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The items, in the order the call documents.
    pub items: Vec<T>,
    /// Where the next page starts; `None` when there are no more pages.
    pub cursor: Option<String>,
}

/// Which external messages of an account to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtDir {
    /// Inbound: calls from outside, such as `finalizeDeposit`.
    In,
    /// Outbound: the account's events.
    Out,
}

/// A number GraphQL sends either as a JSON number or as a string, hex
/// (`"0xe4e1c0"`, `"-0x10"`) or decimal.
fn num(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) => n.as_i64().map(i128::from),
        Value::String(s) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("-0x")) {
            Some(h) => {
                i128::from_str_radix(h, 16)
                    .ok()
                    .map(|x| if s.starts_with('-') { -x } else { x })
            },
            None => s.parse().ok(),
        },
        _ => None,
    }
}

/// A currency id. The schema types it as a Float, so the node sends `3.0`.
fn currency(v: &Value) -> Option<u32> {
    let c = v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && *f >= 0.0)
            .map(|f| f as u64)
    })?;
    u32::try_from(c).ok()
}

/// A `{ currency value }` list as `(currency, value)` pairs; entries it
/// cannot read are left out.
fn ecc(v: &Value) -> Vec<(u32, i128)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| Some((currency(&c["currency"])?, num(&c["value"])?)))
                .collect()
        })
        .unwrap_or_default()
}

/// A 32-byte id from hex, with or without `0x`.
fn id32(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(s.trim_start_matches("0x"), &mut out).ok()?;
    Some(out)
}

/// An account from `blockchain.account { info { … } }`; `None` when the
/// node has no such account.
pub fn parse_account(v: &Value) -> Option<AccountInfo> {
    let info = v.get("info")?;
    if info.is_null() {
        return None;
    }
    Some(AccountInfo {
        status: AccStatus::parse(info["acc_type_name"].as_str().unwrap_or("NonExist")),
        dapp_id: info["dapp_id"].as_str().and_then(id32),
        ecc3: ecc(&info["balance_other"])
            .into_iter()
            .find(|(c, _)| *c == 3)
            .map(|(_, v)| v.max(0) as u128)
            .unwrap_or(0),
    })
}

/// A message, with its transactions when the query resolved them.
pub fn parse_msg(v: &Value) -> Option<MsgView> {
    Some(MsgView {
        hash: v["hash"].as_str()?.to_string(),
        msg_type: v["msg_type_name"].as_str().unwrap_or_default().to_string(),
        src: v["src"].as_str().unwrap_or_default().to_string(),
        dst: v["dst"].as_str().unwrap_or_default().to_string(),
        body: v["body"].as_str().map(String::from),
        ecc: ecc(&v["value_other"])
            .into_iter()
            .map(|(c, x)| (c, x.max(0) as u128))
            .collect(),
        bounce: v["bounce"].as_bool(),
        created_at: v["created_at"].as_u64().unwrap_or(0),
        dst_tx: v
            .get("dst_transaction")
            .filter(|t| !t.is_null())
            .map(|t| TxRef {
                hash: t["hash"].as_str().unwrap_or_default().to_string(),
                aborted: t["aborted"].as_bool().unwrap_or(true),
                account: t["account_addr"].as_str().unwrap_or_default().to_string(),
                now: t["now"].as_u64().unwrap_or(0),
            }),
        src_tx: v
            .pointer("/src_transaction/hash")
            .and_then(|h| h.as_str())
            .map(String::from),
    })
}

/// A transaction with its outbound messages.
pub fn parse_tx(v: &Value) -> Option<TxView> {
    Some(TxView {
        hash: v["hash"].as_str()?.to_string(),
        aborted: v["aborted"].as_bool().unwrap_or(true),
        exit_code: v
            .pointer("/compute/exit_code")
            .and_then(|c| c.as_i64())
            .map(|c| c as i32),
        account: v["account_addr"].as_str().unwrap_or_default().to_string(),
        now: v["now"].as_u64().unwrap_or(0),
        out: v["out_messages"]
            .as_array()
            .map(|a| a.iter().filter_map(parse_msg).collect())
            .unwrap_or_default(),
        ecc_delta: ecc(&v["balance_delta_other"]),
    })
}

/// A page of an account's transactions, oldest first.
pub fn parse_tx_page(v: &Value) -> Option<Page<TxListItem>> {
    let items = v["edges"]
        .as_array()?
        .iter()
        .filter_map(|e| {
            let n = &e["node"];
            Some(TxListItem {
                hash: n["hash"].as_str()?.to_string(),
                now: n["now"].as_u64().unwrap_or(0),
                orig_status: AccStatus::parse(n["orig_status_name"].as_str().unwrap_or("")),
                end_status: AccStatus::parse(n["end_status_name"].as_str().unwrap_or("")),
                aborted: n["aborted"].as_bool().unwrap_or(true),
                out_msgs: n["out_msgs"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    let more = v
        .pointer("/pageInfo/hasNextPage")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let cursor = more
        .then(|| {
            v.pointer("/pageInfo/endCursor")
                .and_then(|c| c.as_str())
                .map(String::from)
        })
        .flatten();
    Some(Page {
        items,
        cursor,
    })
}

/// What the deposit reads from Acki Nacki.
#[async_trait]
pub trait AnRead: Send + Sync {
    /// The account over REST (`/v2/account`), the API the SDK sends
    /// through; an error when the host does not serve it.
    async fn rest_probe(&self, account: [u8; 32]) -> anyhow::Result<()>;
    /// The account over GraphQL; `None` when the node has no such account.
    async fn account(&self, id: [u8; 32]) -> anyhow::Result<Option<AccountInfo>>;
    /// A getter, run locally on the account's current state.
    async fn run_getter(
        &self,
        id: [u8; 32],
        abi: &str,
        function: &str,
        input: Value,
    ) -> anyhow::Result<Value>;
    /// The account's external messages, newest first; `before` is the
    /// cursor of the previous page.
    async fn ext_messages(
        &self,
        id: [u8; 32],
        dir: ExtDir,
        before: Option<String>,
    ) -> anyhow::Result<Page<MsgView>>;
    /// The account's transactions, oldest first; `after` is the cursor of
    /// the previous page.
    async fn transactions(
        &self,
        id: [u8; 32],
        after: Option<String>,
    ) -> anyhow::Result<Page<TxListItem>>;
    /// A message by hash, with its transactions.
    async fn message(&self, hash: &str) -> anyhow::Result<Option<MsgView>>;
    /// A transaction by hash, with its outbound messages.
    async fn transaction(&self, hash: &str) -> anyhow::Result<Option<TxView>>;
    /// A message body decoded by `abi`: `(function or event, values)`;
    /// `None` when it does not decode.
    fn decode(&self, abi: &str, body: &str, internal: bool) -> Option<(String, Value)>;
}

/// The deposit's one write to Acki Nacki.
#[async_trait]
pub trait AnSend: Send + Sync {
    /// Sends `finalizeDeposit(proof, publicInputs)` to the bridge once.
    async fn send_finalize(
        &self,
        bridge: [u8; 32],
        bridge_dapp: [u8; 32],
        proof: &[u8],
        public_inputs: &[u8],
    ) -> FinalizeSend;
}

/// Acki Nacki over the network: the SDK for REST, the local TVM and
/// sending, GraphQL for lists and relations.
pub struct LiveAn {
    /// The SDK context.
    ctx: Arc<ClientContext>,
    /// The GraphQL client.
    gql: GqlClient,
}

impl LiveAn {
    /// A client for `gql_endpoint`; nothing is contacted yet.
    pub fn connect(gql_endpoint: &str) -> CliResult<LiveAn> {
        // The crate's one context, on SDK defaults. `message_retries_count`
        // must stay at its default: it also bounds the loop inside
        // `send_message` that picks up the Block Manager token from the
        // server's answer, follows Block Keeper redirects and retries on
        // TOKEN_EXPIRED, and lowering it breaks sending. What must not
        // happen, an expired `finalizeDeposit` resent behind the caller's
        // checks, is kept out by not using the SDK's process-message call.
        let ctx = crate::preflight::build_client_context(gql_endpoint)?;
        let gql = create_client(gql_endpoint).map_err(|e| CliError::Preflight {
            reason: format!("--gql-endpoint {gql_endpoint}: {e}"),
            source: None,
        })?;
        Ok(LiveAn {
            ctx,
            gql,
        })
    }

    /// The account's BOC over REST.
    async fn boc(&self, id: [u8; 32]) -> anyhow::Result<String> {
        let h = hex::encode(id);
        let r = get_account(self.ctx.clone(), ParamsOfGetAccount {
            account_id: h.clone(),
            dapp_id: h,
        })
        .await
        .map_err(|e| anyhow::anyhow!("get_account: {} (code {})", e.message(), e.code()))?;
        anyhow::ensure!(
            !r.boc.is_empty(),
            "account {} is not on chain",
            hex::encode(id)
        );
        Ok(r.boc)
    }
}

#[async_trait]
impl AnRead for LiveAn {
    async fn rest_probe(&self, account: [u8; 32]) -> anyhow::Result<()> {
        self.boc(account).await.map(|_| ())
    }

    async fn account(&self, id: [u8; 32]) -> anyhow::Result<Option<AccountInfo>> {
        let h = hex::encode(id);
        let q = format!(
            r#"{{ blockchain {{ account(account_id: "{h}", dapp_id: "{h}") {{ info {{ dapp_id acc_type_name balance_other {{ currency value }} }} }} }} }}"#
        );
        let d = self.gql.query(&q).await?;
        Ok(parse_account(
            d.pointer("/blockchain/account").unwrap_or(&Value::Null),
        ))
    }

    async fn run_getter(
        &self,
        id: [u8; 32],
        abi: &str,
        function: &str,
        input: Value,
    ) -> anyhow::Result<Value> {
        let boc = self.boc(id).await?;
        let abi = Abi::Json(abi.to_string());
        // The encoder wants the legacy `0:<id>` address form.
        let msg = encode_message(self.ctx.clone(), ParamsOfEncodeMessage {
            abi: abi.clone(),
            address: Some(format!("0:{}", hex::encode(id))),
            call_set: Some(CallSet {
                function_name: function.into(),
                header: None,
                input: Some(input),
            }),
            signer: Signer::None,
            deploy_set: None,
            processing_try_index: None,
            signature_id: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("encode {function}: {}", e.message()))?;
        let run = run_tvm(self.ctx.clone(), ParamsOfRunTvm {
            message: msg.message,
            account: boc,
            abi: Some(abi),
            execution_options: None,
            boc_cache: None,
            return_updated_account: Some(false),
        })
        .await
        .map_err(|e| anyhow::anyhow!("run {function}: {} (code {})", e.message(), e.code()))?;
        run.decoded
            .and_then(|d| d.output)
            .ok_or_else(|| anyhow::anyhow!("{function} returned nothing"))
    }

    async fn ext_messages(
        &self,
        id: [u8; 32],
        dir: ExtDir,
        before: Option<String>,
    ) -> anyhow::Result<Page<MsgView>> {
        let h = hex::encode(id);
        let kind = match dir {
            ExtDir::In => "ExtIn",
            ExtDir::Out => "ExtOut",
        };
        let before = before
            .map(|c| format!(r#", before: "{c}""#))
            .unwrap_or_default();
        let q = format!(
            r#"{{ blockchain {{ account(account_id: "{h}", dapp_id: "{h}") {{ messages(msg_type: [{kind}], archive: true, last: 50{before}) {{ edges {{ node {{ hash: id msg_type_name src dst body created_at }} cursor }} pageInfo {{ hasPreviousPage startCursor }} }} }} }} }}"#
        );
        let d = self.gql.query(&q).await?;
        let m = d
            .pointer("/blockchain/account/messages")
            .cloned()
            .unwrap_or(Value::Null);
        let mut items: Vec<MsgView> = m["edges"]
            .as_array()
            .map(|a| a.iter().filter_map(|e| parse_msg(&e["node"])).collect())
            .unwrap_or_default();
        items.reverse();
        let more = m
            .pointer("/pageInfo/hasPreviousPage")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let cursor = more
            .then(|| {
                m.pointer("/pageInfo/startCursor")
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
            .flatten();
        Ok(Page {
            items,
            cursor,
        })
    }

    async fn transactions(
        &self,
        id: [u8; 32],
        after: Option<String>,
    ) -> anyhow::Result<Page<TxListItem>> {
        let h = hex::encode(id);
        let after = after
            .map(|c| format!(r#", after: "{c}""#))
            .unwrap_or_default();
        let q = format!(
            r#"{{ blockchain {{ account(account_id: "{h}", dapp_id: "{h}") {{ transactions(first: 50, archive: true{after}) {{ edges {{ node {{ hash: id now aborted orig_status_name end_status_name out_msgs }} cursor }} pageInfo {{ hasNextPage endCursor }} }} }} }} }}"#
        );
        let d = self.gql.query(&q).await?;
        parse_tx_page(
            d.pointer("/blockchain/account/transactions")
                .unwrap_or(&Value::Null),
        )
        .ok_or_else(|| anyhow::anyhow!("transactions of {h}: unexpected answer"))
    }

    async fn message(&self, hash: &str) -> anyhow::Result<Option<MsgView>> {
        let q = format!(
            r#"{{ blockchain {{ message(hash: "{hash}") {{ hash: id msg_type_name src dst body bounce created_at value_other {{ currency value }} src_transaction {{ hash: id }} dst_transaction {{ hash: id aborted account_addr now }} }} }} }}"#
        );
        let d = self.gql.query(&q).await?;
        Ok(d.pointer("/blockchain/message")
            .filter(|m| !m.is_null())
            .and_then(parse_msg))
    }

    async fn transaction(&self, hash: &str) -> anyhow::Result<Option<TxView>> {
        let q = format!(
            r#"{{ blockchain {{ transaction(hash: "{hash}") {{ hash: id aborted account_addr now compute {{ exit_code }} balance_delta_other {{ currency value }} out_messages {{ hash: id msg_type_name src dst body bounce created_at value_other {{ currency value }} }} }} }} }}"#
        );
        let d = self.gql.query(&q).await?;
        Ok(d.pointer("/blockchain/transaction")
            .filter(|t| !t.is_null())
            .and_then(parse_tx))
    }

    fn decode(&self, abi: &str, body: &str, internal: bool) -> Option<(String, Value)> {
        // Local and synchronous in the SDK: no network is touched.
        let r = decode_message_body(self.ctx.clone(), ParamsOfDecodeMessageBody {
            abi: Abi::Json(abi.to_string()),
            body: body.to_string(),
            is_internal: internal,
            allow_partial: false,
            function_name: None,
            data_layout: None,
        })
        .ok()?;
        Some((r.name, r.value.unwrap_or(Value::Null)))
    }
}

/// The bridge's exit code in a failed send, from structured fields only.
///
/// A node that refuses an external message answers `/v2/messages` with
/// `{"error": {"code", "message", "data": {"exit_code", ...}}}`, and the
/// SDK files that `data` under `node_error.extensions.details`. Zero is the
/// node's "no contract code" for refusals of its own (a full queue, a
/// duplicate or expired message).
fn bridge_exit_code(e: &ClientError) -> Option<i32> {
    exit_code_from_sdk(e.data())
        .or_else(|| {
            e.data()
                .pointer("/node_error/extensions/details")
                .and_then(exit_code_from_sdk)
        })
        .filter(|c| *c != 0)
}

/// Exit codes of the contract's own expiry and replay checks. Neither is
/// the bridge refusing the deposit: the message expired before it ran, or
/// an identical one ran already.
const EXPIRY_EXIT_CODES: [i32; 2] = [
    StdContractError::ReplayProtection as i32,
    StdContractError::ExtMessageExpired as i32,
];

/// The node's own codes for a message it may still run, or already ran.
const NODE_IN_DOUBT: [&str; 2] = ["MESSAGE_EXPIRED", "DUPLICATE_MESSAGE"];

/// The verdict in a failed send or wait.
fn classify_send_error(e: &ClientError) -> FinalizeSend {
    let message = e.message().to_string();
    let node_code = e
        .data()
        .pointer("/node_error/extensions/code")
        .and_then(Value::as_str);
    // The SDK waited and saw nothing: the message may have landed, and the
    // caller re-checks before sending again. Such a wait also re-runs the
    // message locally against today's state and files that code under
    // `local_error`; it is a guess, not a verdict.
    if e.code() == ErrorCode::MessageExpired as u32
        || e.code() == ErrorCode::TransactionWaitTimeout as u32
        || node_code.is_some_and(|c| NODE_IN_DOUBT.contains(&c))
    {
        return FinalizeSend::Unknown {
            message,
        };
    }
    match bridge_exit_code(e) {
        Some(code) if EXPIRY_EXIT_CODES.contains(&code) => FinalizeSend::Unknown {
            message,
        },
        Some(code) => FinalizeSend::Rejected {
            exit_code: Some(code),
            message,
        },
        None if e.message().to_ascii_lowercase().contains("timeout") => FinalizeSend::Unknown {
            message,
        },
        None => FinalizeSend::Rejected {
            exit_code: None,
            message,
        },
    }
}

/// `send_message` failures raised by its own checks, before it posts the
/// message: an already expired message, a missing dapp id, a bad thread id.
const FAILED_BEFORE_POSTING: [u32; 3] = [
    ErrorCode::MessageAlreadyExpired as u32,
    ErrorCode::DappIdRequired as u32,
    ErrorCode::InvalidThread as u32,
];

/// The verdict in a failed `send_message`. Its own checks fail before
/// anything leaves the process, so the send is not in doubt.
fn send_error(e: &ClientError) -> FinalizeSend {
    if FAILED_BEFORE_POSTING.contains(&e.code()) {
        return FinalizeSend::NotSent {
            message: format!("send finalizeDeposit: {}", e.message()),
        };
    }
    classify_send_error(e)
}

/// The `finalizeDeposit` external message to `bridge`, as a base64 BOC. A
/// failure here is local: nothing was sent, and sending again will not
/// help.
async fn encode_finalize(
    ctx: Arc<ClientContext>,
    abi: &str,
    bridge: [u8; 32],
    proof: &[u8],
    public_inputs: &[u8],
) -> Result<String, FinalizeSend> {
    encode_message(ctx, ParamsOfEncodeMessage {
        abi: Abi::Json(abi.to_string()),
        address: Some(format!("0:{}", hex::encode(bridge))),
        call_set: Some(CallSet {
            function_name: "finalizeDeposit".into(),
            header: None,
            input: Some(json!({
                "proof": hex::encode(proof),
                "publicInputs": hex::encode(public_inputs),
            })),
        }),
        signer: Signer::None,
        deploy_set: None,
        processing_try_index: None,
        signature_id: None,
    })
    .await
    .map(|m| m.message)
    .map_err(|e| FinalizeSend::NotSent {
        message: format!("encode finalizeDeposit: {}", e.message()),
    })
}

#[async_trait]
impl AnSend for LiveAn {
    /// One iteration of what the SDK's process-message call does — encode,
    /// send, wait — without its resend of an expired message: every resend
    /// goes through the checks the caller makes before each send.
    /// `send_message` keeps its own inner loop for the Block Manager token,
    /// Block Keeper redirects and TOKEN_EXPIRED; that loop resends the same
    /// message, which executes at most once.
    async fn send_finalize(
        &self,
        bridge: [u8; 32],
        bridge_dapp: [u8; 32],
        proof: &[u8],
        public_inputs: &[u8],
    ) -> FinalizeSend {
        let abi = crate::deposit::identity::BRIDGE_ABI;
        let message =
            match encode_finalize(self.ctx.clone(), abi, bridge, proof, public_inputs).await {
                Ok(m) => m,
                Err(not_sent) => return not_sent,
            };
        let abi = Abi::Json(abi.to_string());
        let sent = match send_message(
            self.ctx.clone(),
            ParamsOfSendMessage {
                message: message.clone(),
                abi: Some(abi.clone()),
                thread_id: None,
                send_events: false,
                dapp_id: hex::encode(bridge_dapp),
            },
            |_| async {},
        )
        .await
        {
            Ok(r) => r,
            Err(e) => return send_error(&e),
        };
        let waited = wait_for_transaction(
            self.ctx.clone(),
            ParamsOfWaitForTransaction {
                abi: Some(abi),
                message,
                shard_block_id: String::new(),
                send_events: false,
                sending_endpoints: Some(vec![]),
                tx_hash: sent.tx_hash.clone(),
            },
            |_| async {},
        )
        .await;
        match waited {
            Ok(r) => FinalizeSend::Executed {
                tx_id: r.transaction["id"]
                    .as_str()
                    .or(r.transaction["hash"].as_str())
                    .or(sent.tx_hash.as_deref())
                    .unwrap_or_default()
                    .to_string(),
                aborted: r.transaction["aborted"]
                    .as_bool()
                    .or(sent.aborted)
                    .unwrap_or(true),
                exit_code: r
                    .transaction
                    .pointer("/compute/exit_code")
                    .and_then(|c| c.as_i64())
                    .map(|c| c as i32)
                    .or(sent.exit_code),
            },
            Err(e) => classify_send_error(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tvm_client::processing::ErrorCode;

    use super::*;

    #[test]
    fn an_active_account_in_a_foreign_dapp() {
        let v = json!({ "info": { "dapp_id": "0".repeat(63) + "1", "acc_type_name": "Active",
            "balance_other": [{ "currency": 3, "value": "0xe4e1c0" }] } });
        let a = parse_account(&v).unwrap();
        assert_eq!(a.status, AccStatus::Active);
        assert_eq!(a.dapp_id.unwrap()[31], 1);
        assert_eq!(a.ecc3, 15_000_000);
    }

    #[test]
    fn an_uninit_account_has_no_dapp_yet() {
        let v =
            json!({ "info": { "dapp_id": null, "acc_type_name": "Uninit", "balance_other": [] } });
        let a = parse_account(&v).unwrap();
        assert_eq!((a.status, a.dapp_id, a.ecc3), (AccStatus::Uninit, None, 0));
        assert!(parse_account(&json!(null)).is_none());
    }

    #[test]
    fn a_transaction_page_and_its_cursor() {
        let v = json!({ "edges": [{ "node": { "hash": "aa", "now": 5, "aborted": false, "orig_status_name": "Uninit",
            "end_status_name": "Active", "out_msgs": ["m1"] }, "cursor": "c1" }],
            "pageInfo": { "hasNextPage": true, "endCursor": "c1" } });
        let p = parse_tx_page(&v).unwrap();
        assert_eq!(p.items[0].orig_status, AccStatus::Uninit);
        assert_eq!(p.items[0].out_msgs, vec!["m1".to_string()]);
        assert_eq!(p.cursor.as_deref(), Some("c1"));
    }

    #[test]
    fn a_message_with_its_destination_transaction() {
        let v = json!({ "hash": "m1", "msg_type_name": "Internal", "src": "0:aa", "dst": "0:bb", "body": "te6", "bounce": false,
            "created_at": 7, "value_other": [{ "currency": 3, "value": "1000000" }],
            "src_transaction": { "hash": "t0" }, "dst_transaction": { "hash": "t1", "aborted": false, "account_addr": "0:bb", "now": 9 } });
        let m = parse_msg(&v).unwrap();
        assert_eq!(m.ecc, vec![(3, 1_000_000)]);
        assert_eq!(m.dst_tx.unwrap().hash, "t1");
        assert_eq!(m.src_tx.as_deref(), Some("t0"));
    }

    #[test]
    fn a_transaction_with_its_outputs_and_delta() {
        let v = json!({ "hash": "t1", "aborted": true, "account_addr": "0:bb", "now": 9, "compute": { "exit_code": 207 },
            "balance_delta_other": [{ "currency": 3, "value": "0x10" }], "out_messages": [] });
        let t = parse_tx(&v).unwrap();
        assert!(t.aborted);
        assert_eq!(t.exit_code, Some(207));
        assert_eq!(t.ecc_delta, vec![(3, 16)]);
    }

    /// Shellnet (GraphQL 1.4.0) types `currency` as a Float and sends
    /// `3.0`; these are its answers for a bridge transaction that paid a
    /// deposit out, and for the recipient's account.
    #[test]
    fn a_currency_the_node_sends_as_a_float() {
        let t = parse_tx(&json!({ "hash": "35e9", "aborted": false, "account_addr": "0:1a", "now": 1790238399,
            "compute": { "exit_code": 0 }, "balance_delta_other": [{ "currency": 3.0, "value": "-0x17d7840" }],
            "out_messages": [{ "hash": "75ad", "msg_type_name": "Internal", "src": "0:1a", "dst": "0:35",
                "body": "te6", "bounce": false, "created_at": 1790238399,
                "value_other": [{ "currency": 3.0, "value": "0x17d7840" }] }] }))
        .unwrap();
        assert_eq!(t.ecc_delta, vec![(3, -25_000_000)]);
        assert_eq!(t.out[0].ecc, vec![(3, 25_000_000)]);
        let a = parse_account(&json!({ "info": { "dapp_id": "0".repeat(63) + "1", "acc_type_name": "Active",
            "balance_other": [{ "currency": 2.0, "value": "0xe8d4a51000" }, { "currency": 3.0, "value": "0x1c7ad7140" }] } }))
        .unwrap();
        assert_eq!(a.ecc3, 7_645_000_000);
    }

    #[test]
    fn the_deposit_client_keeps_the_sdk_send_loop_and_drops_its_resend() {
        // The token/redirect loop inside send_message is bounded by this count.
        let an = LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        assert_eq!(
            tvm_client::client::config(an.ctx.clone())
                .unwrap()
                .network
                .message_retries_count,
            tvm_client::ClientConfig::default()
                .network
                .message_retries_count
        );
        // process_message would resend an expired finalizeDeposit behind step 8's back.
        let production = crate::source_guard::production_source("an.rs", include_str!("an.rs"));
        assert!(
            !production.contains(concat!("process_", "message(")),
            "send through send_message + wait_for_transaction"
        );
        assert!(
            !production.contains(concat!("message_retries_", "count:")),
            "leave the SDK's retry count alone"
        );
        assert!(
            !production.contains(concat!("ClientContext::", "new(")),
            "build the context in one place"
        );
    }

    #[test]
    fn an_expired_send_is_unknown_and_a_coded_rejection_is_not() {
        let expired = tvm_client::error::ClientError::with_code_message(
            ErrorCode::MessageExpired as u32,
            "Message expired".into(),
        );
        assert!(matches!(
            classify_send_error(&expired),
            FinalizeSend::Unknown { .. }
        ));
        let rejected =
            tvm_client::error::ClientError::new(1, "rejected", json!({ "exit_code": 231 }));
        assert!(matches!(
            classify_send_error(&rejected),
            FinalizeSend::Rejected {
                exit_code: Some(231),
                ..
            }
        ));
    }

    /// What `send_message` returns when the node answers `/v2/messages`
    /// with an error: the SDK's own conversion of the node's body.
    fn node_refusal(code: &str, exit_code: i32) -> tvm_client::error::ClientError {
        tvm_client::net::Error::try_extract_send_messages_error(&json!({
            "result": null,
            "error": { "code": code, "message": "refused", "data": { "producers": [], "message_hash": "ab",
                "exit_code": exit_code, "current_time": "1", "thread_id": null, "account_id": "", "dapp_id": "" } },
            "ext_message_token": null,
        }))
        .unwrap()
    }

    #[test]
    fn a_refusal_from_the_node_carries_the_bridge_code() {
        for code in [231, 222, 220, 224] {
            assert!(
                matches!(classify_send_error(&node_refusal("TVM_ERROR", code)), FinalizeSend::Rejected { exit_code: Some(c), .. } if c == code),
                "{code}"
            );
        }
        // Zero is the node's "no contract code" for its own refusals.
        for code in ["QUEUE_OVERFLOW", "MESSAGE_EXPIRED", "DUPLICATE_MESSAGE"] {
            assert!(
                !matches!(
                    classify_send_error(&node_refusal(code, 0)),
                    FinalizeSend::Rejected {
                        exit_code: Some(_),
                        ..
                    }
                ),
                "{code}"
            );
        }
    }

    #[test]
    fn an_expiry_is_never_the_bridge_refusing() {
        // The contract's own expiry and replay checks: the message may run
        // again after a fresh encode, or ran already.
        for code in [52, 57] {
            assert!(
                matches!(
                    classify_send_error(&node_refusal("TVM_ERROR", code)),
                    FinalizeSend::Unknown { .. }
                ),
                "{code}"
            );
        }
        // A wait that ran out re-runs the message locally against today's
        // state; that code is a guess, not what happened on chain.
        let mut waited = tvm_client::error::ClientError::with_code_message(
            ErrorCode::MessageExpired as u32,
            "Message expired".into(),
        );
        waited.data_mut()["local_error"] =
            json!({ "code": 414, "message": "exit code 231", "data": { "exit_code": 231 } });
        assert!(matches!(
            classify_send_error(&waited),
            FinalizeSend::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn a_message_that_cannot_be_encoded_is_not_sent() {
        let ctx = crate::deposit::identity::offline_context();
        // An ABI without `finalizeDeposit`: nothing to send, nothing sent.
        let broken = encode_finalize(
            ctx.clone(),
            crate::deposit::identity::VOUCHER_ABI,
            [1; 32],
            b"p",
            b"i",
        )
        .await;
        assert!(
            matches!(broken, Err(FinalizeSend::NotSent { .. })),
            "{broken:?}"
        );
        let ok = encode_finalize(
            ctx,
            crate::deposit::identity::BRIDGE_ABI,
            [1; 32],
            b"p",
            b"i",
        )
        .await;
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn a_send_that_fails_its_own_checks_is_not_sent() {
        for code in [
            ErrorCode::MessageAlreadyExpired,
            ErrorCode::DappIdRequired,
            ErrorCode::InvalidThread,
        ] {
            let e = tvm_client::error::ClientError::with_code_message(code as u32, "local".into());
            assert!(matches!(send_error(&e), FinalizeSend::NotSent { .. }));
        }
        // Posted and refused: not a local failure.
        assert!(matches!(
            send_error(&node_refusal("TVM_ERROR", 231)),
            FinalizeSend::Rejected {
                exit_code: Some(231),
                ..
            }
        ));
        assert!(!matches!(
            send_error(&node_refusal("QUEUE_OVERFLOW", 0)),
            FinalizeSend::NotSent { .. }
        ));
    }

    #[tokio::test]
    #[ignore = "reads shellnet; run by hand"]
    async fn live_shellnet_bridge_answers_its_getters() {
        let an = LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        let bridge = [0x1a; 32];
        an.rest_probe(bridge).await.unwrap();
        let v = an
            .run_getter(
                bridge,
                crate::deposit::identity::BRIDGE_ABI,
                "getVersion",
                json!({}),
            )
            .await
            .unwrap();
        assert_eq!(v["value1"], "eccUSDCBridge");
    }

    /// Every GraphQL read against the live schema: a query naming a field
    /// the server does not have fails as a whole.
    #[tokio::test]
    #[ignore = "reads shellnet; run by hand"]
    async fn live_shellnet_answers_every_read() {
        let an = LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        let bridge = [0x1a; 32];
        let info = an
            .account(bridge)
            .await
            .unwrap()
            .expect("the bridge account");
        assert_eq!(info.status, AccStatus::Active);
        println!("bridge: {info:?}");
        let txs = an.transactions(bridge, None).await.unwrap();
        assert!(!txs.items.is_empty() && txs.cursor.is_some(), "{txs:?}");
        let next = an.transactions(bridge, txs.cursor.clone()).await.unwrap();
        assert!(
            next.items.first().map(|t| t.now) >= txs.items.last().map(|t| t.now),
            "oldest first"
        );
        let out = an.ext_messages(bridge, ExtDir::Out, None).await.unwrap();
        let newest = out.items.first().expect("the bridge's events");
        assert!(
            out.items
                .windows(2)
                .all(|w| w[0].created_at >= w[1].created_at),
            "newest first"
        );
        assert!(an
            .ext_messages(bridge, ExtDir::In, None)
            .await
            .unwrap()
            .items
            .iter()
            .all(|m| m.msg_type == "ExtIn"));
        let m = an
            .message(&newest.hash)
            .await
            .unwrap()
            .expect("the event by hash");
        let src = m.src_tx.clone().expect("the event's transaction");
        let t = an
            .transaction(&src)
            .await
            .unwrap()
            .expect("the transaction by hash");
        assert!(t.out.iter().any(|o| o.hash == newest.hash), "{t:?}");
        println!("{t:?}");
        let decoded = an.decode(
            crate::deposit::identity::BRIDGE_ABI,
            newest.body.as_deref().unwrap(),
            false,
        );
        println!("{decoded:?}");
        assert!(an.message(&"0".repeat(64)).await.unwrap().is_none());
        let nobody =
            hex::decode("7c3e9a1f5b2d8e4a6c0f9b3d7e1a5c8f2b6d0e4a8c3f7b1d5e9a2c6f0b4d8e3a")
                .unwrap();
        assert!(an
            .account(nobody.try_into().unwrap())
            .await
            .unwrap()
            .is_none());
    }
}
