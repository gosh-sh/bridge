//! Ethereum JSON-RPC over an ordered list of endpoints.
//!
//! `RPC_URL` is the primary endpoint and `RPC_FAILOVER_URLS`
//! (comma-separated) the fallbacks, tried in that order. Every request starts
//! at the top of the list and moves down only when the endpoint, not the
//! request, is the problem:
//!
//! * the endpoint is unreachable or slow (a transport error, including the
//!   per-endpoint bound of [`crate::rpc_http`]), answers with an HTTP status
//!   that is about the endpoint — 401 or 403 for a rejected key, 404, 408, 429,
//!   5xx — whatever its body says, answers another HTTP error status without a
//!   JSON-RPC error in its body, or refuses with a rate-limit, quota, key or
//!   upstream error: the request moves on and the endpoint waits
//!   [`ENDPOINT_COOLDOWN`] at the back of the line;
//! * the endpoint cannot serve this one request — an `eth_getLogs` span over
//!   its cap, history it has pruned, a block it has not seen yet, a method it
//!   does not offer: the request moves on and the endpoint keeps its place;
//! * anything else — `execution reverted`, `nonce too low`, `insufficient
//!   funds`, any error this module does not recognise — is the answer, and goes
//!   back to the caller unchanged: another node would say the same.
//!
//! A JSON-RPC error that comes with any other HTTP status is read by these
//! rules as if it had come with 200: Alchemy sends an `eth_getLogs` span
//! over its cap with HTTP 400.
//!
//! An endpoint at the back of the line is still tried, after the others, so a
//! request fails only once every endpoint has failed it, and the primary is
//! first again as soon as its cooldown ends.
//!
//! `eth_sendRawTransaction` is the one request with a side effect. The signed
//! transaction goes to the first endpoint that accepts it, and copies go to
//! the others in the background: one transaction and one hash, however many
//! nodes hold it. An endpoint that answers `already known` holds it already,
//! which counts as accepted.
//!
//! The URLs carry API keys in their path. Nothing here logs one: an endpoint
//! is named by its scheme and host ([`endpoint_label`]), and URLs inside error
//! messages are cut down the same way ([`redact_urls`]). Requests go out
//! through [`Endpoint::post`], not alloy's `Http` transport, which opens a
//! debug span with the full URL on every request.

use std::{
    fmt,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use alloy::{
    network::EthereumWallet,
    primitives::{hex, keccak256, U64},
    providers::{Provider, ProviderBuilder, RootProvider, WalletProvider},
    rpc::{
        client::RpcClient,
        json_rpc::{RequestPacket, Response, ResponsePacket, ResponsePayload},
    },
    signers::{local::PrivateKeySigner, Signer},
    transports::{
        http::reqwest::{self, Url},
        RpcError, TransportError, TransportErrorKind, TransportFut,
    },
};
use serde_json::value::RawValue;
use tower::Service;
use tracing::{debug, info, warn};

use crate::rpc_http::{rpc_http_client, rpc_request_timeout_from_env};

/// Comma-separated fallback endpoints behind `RPC_URL`, tried in order.
pub const ENV_RPC_FAILOVER_URLS: &str = "RPC_FAILOVER_URLS";

/// How long a failing endpoint waits at the back of the line before it is
/// tried in its own place again.
pub const ENDPOINT_COOLDOWN: Duration = Duration::from_secs(60);

const SEND_RAW_TRANSACTION: &str = "eth_sendRawTransaction";

/// Scheme and host (and port, if any) of `url`: how an endpoint is named in
/// logs and errors. Path, query and user info — where API keys live — are
/// left out.
pub fn endpoint_label(url: &Url) -> String {
    let host = url.host_str().unwrap_or("?");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// `text` with every `http://` and `https://` URL in it cut down to
/// [`endpoint_label`]. reqwest names the full URL in its errors, so any
/// transport error passes through here before it is logged or returned.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = find_url_start(rest) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '`'
                    )
            })
            .unwrap_or(tail.len());
        match Url::parse(&tail[..end]) {
            Ok(url) if url.host_str().is_some() => out.push_str(&endpoint_label(&url)),
            _ => out.push_str("<url>"),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn find_url_start(s: &str) -> Option<usize> {
    match (s.find("https://"), s.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `RPC_URL` followed by the `RPC_FAILOVER_URLS` entries, in order. Blank
/// entries and repeats are dropped; an entry that is not an `http(s)` URL is
/// an error, named by its position only — the value may hold a key.
pub fn endpoint_urls(primary: &str, failover: &[String]) -> anyhow::Result<Vec<Url>> {
    let mut urls: Vec<Url> = Vec::new();
    let entries = std::iter::once(("RPC_URL".to_string(), primary)).chain(
        failover.iter().enumerate().map(|(i, url)| {
            (
                format!("{ENV_RPC_FAILOVER_URLS} entry {}", i + 1),
                url.as_str(),
            )
        }),
    );
    for (name, raw) in entries {
        let raw = raw.trim();
        if raw.is_empty() {
            anyhow::ensure!(name != "RPC_URL", "RPC_URL is empty");
            continue;
        }
        let url = Url::parse(raw)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .ok_or_else(|| anyhow::anyhow!("{name} is not an http(s) URL"))?;
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    Ok(urls)
}

/// What a response tells the failover loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The answer: a result, or an error about the request itself.
    Answer,
    /// `eth_sendRawTransaction` answered `already known`: the endpoint holds
    /// the transaction.
    AlreadyKnown,
    /// This endpoint cannot serve this request; the next one may.
    NotHere,
    /// The endpoint is failing: move on, and put it at the back of the line.
    Failing,
}

/// How a JSON-RPC error from one endpoint is read. The order of the checks
/// matters: an error about the request wins over every endpoint pattern,
/// and `eth_getLogs` caps are recognised before rate limits, because Infura
/// reports both with `-32005`.
pub(crate) fn classify_error(method: &str, code: i64, message: &str) -> Verdict {
    let m = message.to_ascii_lowercase();
    let has = |patterns: &[&str]| patterns.iter().any(|p| m.contains(p));

    if method == SEND_RAW_TRANSACTION
        && has(&[
            "already known",
            "known transaction",
            "alreadyknown",
            "already imported",
        ])
    {
        return Verdict::AlreadyKnown;
    }
    if code == 3
        || has(&[
            "revert",
            "nonce too low",
            "nonce too high",
            "insufficient funds",
            "underpriced",
            "intrinsic gas",
            "gas required exceeds",
            "exceeds block gas limit",
            "invalid sender",
            "fee cap",
            "max fee per gas",
            "max priority fee per gas",
        ])
    {
        return Verdict::Answer;
    }
    if method == "eth_getLogs"
        && (matches!(code, 35 | -32012)
            || has(&[
                "range",
                "limited to",
                "exceeds limit",
                "results",
                "too large",
                "response size",
            ]))
    {
        return Verdict::NotHere;
    }
    if matches!(code, 4444 | -32601)
        || has(&[
            "pruned",
            "missing trie node",
            "historical state",
            "state not available",
            "header not found",
            "unknown block",
            "block not found",
            "method not found",
            "not supported",
            "unsupported",
        ])
    {
        return Verdict::NotHere;
    }
    if matches!(code, -32005 | -32029 | -32090 | -32603 | 429)
        || has(&[
            "rate limit",
            "too many requests",
            "request limit",
            "exceeded",
            "quota",
            "capacity",
            "throttl",
            "unauthorized",
            "api key",
            "apikey",
            "forbidden",
            "upstream",
            "timeout",
            "timed out",
            "unavailable",
            "internal error",
            "bad gateway",
            "try again",
        ])
    {
        return Verdict::Failing;
    }
    Verdict::Answer
}

fn classify_response(method: &str, response: &ResponsePacket) -> (Verdict, String) {
    match response {
        ResponsePacket::Single(Response {
            payload: ResponsePayload::Failure(error),
            ..
        }) => (
            classify_error(method, error.code, &error.message),
            short(&redact_urls(&format!("{}: {}", error.code, error.message))),
        ),
        _ => (Verdict::Answer, String::new()),
    }
}

fn request_method(request: &RequestPacket) -> String {
    match request {
        RequestPacket::Single(single) => single.method().to_string(),
        RequestPacket::Batch(_) => "batch".to_string(),
    }
}

fn short(text: &str) -> String {
    const MAX: usize = 200;
    match text.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// The success response an `already known` stands for: the hash of the raw
/// transaction in the request, which is what the endpoint would have
/// returned had it not seen the transaction before.
fn accepted_response(request: &RequestPacket) -> Option<ResponsePacket> {
    let RequestPacket::Single(single) = request else {
        return None;
    };
    let params: Vec<String> = serde_json::from_str(single.params()?.get()).ok()?;
    let raw = hex::decode(params.first()?).ok()?;
    let payload = RawValue::from_string(format!("\"{}\"", keccak256(&raw))).ok()?;
    Some(ResponsePacket::Single(Response {
        id: single.id().clone(),
        payload: ResponsePayload::Success(payload),
    }))
}

/// HTTP statuses about the endpoint, not the request: a rejected key, a
/// wrong URL, a timeout, a rate limit, the server itself. They fail the
/// endpoint whatever the body says. Alchemy's 401 for a rejected key
/// carries a JSON-RPC error no pattern of [`classify_error`] recognises
/// (`-32600 Must be authenticated!`, checked 2026-10-20).
fn endpoint_failed(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 408 | 429) || status >= 500
}

/// A transport error with every URL in it cut down to its label. The HTTP
/// status survives, so callers that tell 429 from 5xx still can.
fn redact_transport_error(error: TransportError) -> TransportError {
    match error {
        RpcError::Transport(TransportErrorKind::HttpError(http)) => {
            TransportErrorKind::http_error(http.status, redact_urls(&http.body))
        },
        RpcError::Transport(TransportErrorKind::HttpErrorWithRetryAfter {
            error,
            retry_after,
        }) => TransportErrorKind::http_error_with_retry_after(
            error.status,
            redact_urls(&error.body),
            Some(retry_after),
        ),
        other => {
            let text = other.to_string();
            let redacted = redact_urls(&text);
            if redacted == text {
                other
            } else {
                TransportErrorKind::custom_str(&redacted)
            }
        },
    }
}

struct Endpoint {
    label: String,
    url: Url,
    client: reqwest::Client,
    cooling_until: Mutex<Option<Instant>>,
}

impl Endpoint {
    /// One request to this endpoint, with the HTTP status of the reply.
    /// alloy's `Http` reads the body the same way, but hands back a JSON-RPC
    /// error as the answer and drops the status it came with, and it logs
    /// the full URL, API key included, in a debug span.
    async fn post(
        &self,
        request: &RequestPacket,
    ) -> (Option<u16>, Result<ResponsePacket, TransportError>) {
        let sent = self
            .client
            .post(self.url.clone())
            .json(request)
            .headers(request.headers())
            .send()
            .await;
        let reply = match sent {
            Ok(reply) => reply,
            Err(error) => return (None, Err(TransportErrorKind::custom(error))),
        };
        let status = reply.status().as_u16();
        let body = match reply.bytes().await {
            Ok(body) => body,
            Err(error) => return (Some(status), Err(TransportErrorKind::custom(error))),
        };
        let parsed = serde_json::from_slice::<ResponsePacket>(&body);
        let result = if (200..300).contains(&status) {
            parsed.map_err(|e| TransportError::deser_err(e, String::from_utf8_lossy(&body)))
        } else {
            match parsed {
                Ok(answer) if answer.is_error() => Ok(answer),
                _ => Err(TransportErrorKind::http_error(
                    status,
                    String::from_utf8_lossy(&body).into_owned(),
                )),
            }
        };
        (Some(status), result)
    }

    fn cooling(&self, now: Instant) -> bool {
        self.cooling_until
            .lock()
            .map(|until| until.is_some_and(|t| now < t))
            .unwrap_or(false)
    }

    fn cool_down(&self, cooldown: Duration) {
        if let Ok(mut until) = self.cooling_until.lock() {
            *until = Some(Instant::now() + cooldown);
        }
    }
}

/// The transport behind every Ethereum RPC call of the relayer; see the
/// module docs for the rules. Its `Debug` names hosts only.
#[derive(Clone)]
pub struct FailoverTransport {
    endpoints: Arc<[Endpoint]>,
    cooldown: Duration,
}

impl fmt::Debug for FailoverTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FailoverTransport")
            .field("endpoints", &self.labels())
            .field("cooldown", &self.cooldown)
            .finish()
    }
}

impl FailoverTransport {
    /// One endpoint per URL, in order, all sharing `client` (and its
    /// per-request bound).
    pub fn new(urls: &[Url], client: reqwest::Client, cooldown: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(!urls.is_empty(), "no Ethereum RPC endpoint configured");
        let endpoints: Vec<Endpoint> = urls
            .iter()
            .map(|url| Endpoint {
                label: endpoint_label(url),
                url: url.clone(),
                client: client.clone(),
                cooling_until: Mutex::new(None),
            })
            .collect();
        Ok(Self {
            endpoints: endpoints.into(),
            cooldown,
        })
    }

    /// The endpoints' labels, in configured order.
    pub fn labels(&self) -> Vec<String> {
        self.endpoints.iter().map(|e| e.label.clone()).collect()
    }

    /// Endpoints in the order this request tries them: those not cooling
    /// down in configured order, then those cooling down.
    fn order(&self) -> Vec<usize> {
        let now = Instant::now();
        let (mut ready, cooling): (Vec<usize>, Vec<usize>) =
            (0..self.endpoints.len()).partition(|&i| !self.endpoints[i].cooling(now));
        ready.extend(cooling);
        ready
    }

    async fn request(self, request: RequestPacket) -> Result<ResponsePacket, TransportError> {
        let method = request_method(&request);
        let order = self.order();
        let mut last = None;
        for (position, &i) in order.iter().enumerate() {
            let endpoint = &self.endpoints[i];
            let (status, result) = endpoint.post(&request).await;
            let (verdict, reason) = match (&result, status) {
                (Ok(response), Some(status)) if endpoint_failed(status) => {
                    let (_, reason) = classify_response(&method, response);
                    (Verdict::Failing, format!("HTTP {status}: {reason}"))
                },
                (Ok(response), _) => classify_response(&method, response),
                (Err(error), _) => (Verdict::Failing, short(&redact_urls(&error.to_string()))),
            };
            match verdict {
                Verdict::Answer => {
                    if method == SEND_RAW_TRANSACTION
                        && result.as_ref().is_ok_and(|r| r.is_success())
                    {
                        self.copy_transaction(i, &request);
                    }
                    return result.map_err(redact_transport_error);
                },
                Verdict::AlreadyKnown => {
                    if let Some(accepted) = accepted_response(&request) {
                        debug!(endpoint = %endpoint.label, "Ethereum RPC: transaction already known");
                        self.copy_transaction(i, &request);
                        return Ok(accepted);
                    }
                    return result.map_err(redact_transport_error);
                },
                Verdict::NotHere | Verdict::Failing => {
                    let failing = verdict == Verdict::Failing;
                    if failing {
                        endpoint.cool_down(self.cooldown);
                    }
                    let next = order
                        .get(position + 1)
                        .map(|&n| self.endpoints[n].label.as_str());
                    match (failing, next) {
                        (true, Some(to)) => warn!(
                            %method, from = %endpoint.label, %to, %reason,
                            "Ethereum RPC: endpoint failing, trying the next one"
                        ),
                        (true, None) => warn!(
                            %method, endpoint = %endpoint.label, %reason,
                            "Ethereum RPC: the last endpoint failed too"
                        ),
                        (false, Some(to)) => debug!(
                            %method, from = %endpoint.label, %to, %reason,
                            "Ethereum RPC: endpoint cannot serve this request, trying the next one"
                        ),
                        (false, None) => debug!(
                            %method, endpoint = %endpoint.label, %reason,
                            "Ethereum RPC: no endpoint can serve this request"
                        ),
                    }
                    last = Some(result);
                },
            }
        }
        match last {
            Some(result) => result.map_err(redact_transport_error),
            None => Err(TransportErrorKind::custom_str(
                "no Ethereum RPC endpoint configured",
            )),
        }
    }

    /// Sends the accepted transaction to every other endpoint in the
    /// background. A copy's fate is logged at debug and changes nothing.
    fn copy_transaction(&self, accepted: usize, request: &RequestPacket) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.endpoints.len() < 2 {
            return;
        }
        let endpoints = self.endpoints.clone();
        let request = request.clone();
        runtime.spawn(async move {
            for (i, endpoint) in endpoints.iter().enumerate() {
                if i == accepted {
                    continue;
                }
                match endpoint.post(&request).await.1 {
                    Ok(response) => debug!(
                        endpoint = %endpoint.label,
                        accepted = response.is_success(),
                        "Ethereum RPC: copy of the transaction sent"
                    ),
                    Err(error) => debug!(
                        endpoint = %endpoint.label,
                        error = %short(&redact_urls(&error.to_string())),
                        "Ethereum RPC: copy of the transaction not sent"
                    ),
                }
            }
        });
    }

    /// `eth_chainId` from every endpoint. All that answer must agree, and
    /// at least one must answer; one that does not answer stays in the list,
    /// since it may come back.
    pub async fn check_chain_ids(&self) -> anyhow::Result<u64> {
        let mut answers: Vec<(&str, u64)> = Vec::new();
        for (index, endpoint) in self.endpoints.iter().enumerate() {
            let client = RpcClient::new(
                OneEndpoint {
                    endpoints: self.endpoints.clone(),
                    index,
                },
                false,
            );
            match client.request_noparams::<U64>("eth_chainId").await {
                Ok(id) => answers.push((endpoint.label.as_str(), id.to::<u64>())),
                Err(error) => warn!(
                    endpoint = %endpoint.label,
                    error = %short(&redact_urls(&error.to_string())),
                    "Ethereum RPC: endpoint did not answer eth_chainId; keeping it in the list"
                ),
            }
        }
        let Some(&(first_label, chain_id)) = answers.first() else {
            anyhow::bail!(
                "no Ethereum RPC endpoint answered eth_chainId: {}",
                self.labels().join(", ")
            );
        };
        if let Some((label, other)) = answers.iter().find(|(_, id)| *id != chain_id) {
            anyhow::bail!(
                "Ethereum RPC endpoints disagree on the chain id: {first_label} answers \
                 {chain_id}, {label} answers {other}. Every RPC_URL and {ENV_RPC_FAILOVER_URLS} \
                 entry must serve the same chain"
            );
        }
        info!(
            endpoints = %self.labels().join(", "),
            chain_id,
            "Ethereum RPC endpoints, in order"
        );
        Ok(chain_id)
    }
}

/// One endpoint of a [`FailoverTransport`] as a transport of its own, for
/// the chain id check: requests go to it alone, through [`Endpoint::post`].
#[derive(Clone)]
struct OneEndpoint {
    endpoints: Arc<[Endpoint]>,
    index: usize,
}

impl Service<RequestPacket> for OneEndpoint {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            let (_, result) = this.endpoints[this.index].post(&request).await;
            result.map_err(redact_transport_error)
        })
    }
}

impl Service<RequestPacket> for FailoverTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        Box::pin(self.clone().request(request))
    }
}

/// An RPC client over the endpoints, and the chain id they agree on.
pub struct EthRpc {
    pub client: RpcClient,
    pub chain_id: u64,
}

/// [`FailoverTransport`] over `urls`, each request bounded by `timeout`,
/// after [`FailoverTransport::check_chain_ids`].
pub async fn connect(urls: &[Url], timeout: Duration) -> anyhow::Result<EthRpc> {
    let transport = FailoverTransport::new(urls, rpc_http_client(timeout)?, ENDPOINT_COOLDOWN)?;
    let chain_id = transport.check_chain_ids().await?;
    Ok(EthRpc {
        client: RpcClient::new(transport, false),
        chain_id,
    })
}

/// A read-only provider over `urls` (per-request bound from
/// `BRIDGE_RPC_REQUEST_TIMEOUT_SECS`), and the chain id.
pub async fn read_provider(urls: &[Url]) -> anyhow::Result<(RootProvider, u64)> {
    let rpc = connect(urls, rpc_request_timeout_from_env()?).await?;
    Ok((RootProvider::new(rpc.client), rpc.chain_id))
}

/// A provider over `urls` that signs with `signer` for the chain the
/// endpoints serve. Its nonce filler reads the `pending` count before every
/// transaction instead of counting locally (alloy's default, which never
/// recovers from a lost send); the bridge client sets the nonce itself when
/// it knows its sender ([`crate::nonce`]).
pub async fn wallet_provider(
    urls: &[Url],
    signer: PrivateKeySigner,
) -> anyhow::Result<impl Provider + WalletProvider<Wallet = EthereumWallet> + Clone + 'static> {
    wallet_provider_with_timeout(urls, signer, rpc_request_timeout_from_env()?).await
}

pub(crate) async fn wallet_provider_with_timeout(
    urls: &[Url],
    signer: PrivateKeySigner,
    timeout: Duration,
) -> anyhow::Result<impl Provider + WalletProvider<Wallet = EthereumWallet> + Clone + 'static> {
    let rpc = connect(urls, timeout).await?;
    let wallet = EthereumWallet::from(signer.with_chain_id(Some(rpc.chain_id)));
    Ok(ProviderBuilder::new()
        .disable_recommended_fillers()
        .with_gas_estimation()
        .with_simple_nonce_management()
        .with_chain_id(rpc.chain_id)
        .wallet(wallet)
        .connect_client(rpc.client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_keep_scheme_host_and_port_only() {
        let url = Url::parse("https://sepolia.infura.io/v3/KEY?x=1").unwrap();
        assert_eq!(endpoint_label(&url), "https://sepolia.infura.io");
        let url = Url::parse("http://user:secret@127.0.0.1:8545/key").unwrap();
        assert_eq!(endpoint_label(&url), "http://127.0.0.1:8545");
    }

    #[test]
    fn urls_in_error_text_lose_their_keys() {
        let text = "error sending request for url (https://eth-sepolia.g.alchemy.com/v2/alch_KEY) \
                    then http://10.0.0.1:8545/v3/abc, and https://RPC.example.org/v3/InfuraKey-0c9b/";
        let redacted = redact_urls(text);
        assert_eq!(
            redacted,
            "error sending request for url (https://eth-sepolia.g.alchemy.com) then \
             http://10.0.0.1:8545, and https://rpc.example.org"
        );
        for key in ["alch_KEY", "abc", "InfuraKey"] {
            assert!(!redacted.contains(key), "{redacted}");
        }
        assert_eq!(redact_urls("no url here"), "no url here");
    }

    #[test]
    fn endpoint_urls_keep_order_drop_blanks_and_repeats() {
        let failover = vec![
            "https://b.example/k2".to_string(),
            " ".to_string(),
            "https://a.example/k1".to_string(),
            "https://c.example".to_string(),
        ];
        let urls = endpoint_urls("https://a.example/k1", &failover).unwrap();
        let labels: Vec<String> = urls.iter().map(endpoint_label).collect();
        assert_eq!(labels, [
            "https://a.example",
            "https://b.example",
            "https://c.example"
        ]);
    }

    #[test]
    fn a_bad_endpoint_is_named_by_position_not_value() {
        let err = endpoint_urls("https://a.example", &["ftp://x/secret".to_string()])
            .unwrap_err()
            .to_string();
        assert_eq!(err, "RPC_FAILOVER_URLS entry 1 is not an http(s) URL");
        assert!(endpoint_urls("  ", &[]).is_err());
        let err = endpoint_urls("not a url with a key", &[])
            .unwrap_err()
            .to_string();
        assert!(!err.contains("key"), "{err}");
    }

    /// The error texts the probes of 2026-10-19 got back from the Sepolia
    /// endpoints the shellnet profile lists, and a few of geth's own.
    #[test]
    fn errors_are_read_the_way_the_endpoints_mean_them() {
        use Verdict::*;
        let cases: &[(&str, i64, &str, Verdict)] = &[
            // Request errors: the answer, whatever the endpoint.
            (
                "eth_estimateGas",
                3,
                "execution reverted: UnknownAnchor",
                Answer,
            ),
            (
                "eth_sendRawTransaction",
                -32000,
                "nonce too low: next nonce 355, tx nonce 354",
                Answer,
            ),
            (
                "eth_sendRawTransaction",
                -32000,
                "replacement transaction underpriced",
                Answer,
            ),
            (
                "eth_sendRawTransaction",
                -32000,
                "insufficient funds for gas * price + value",
                Answer,
            ),
            (
                "eth_estimateGas",
                -32000,
                "gas required exceeds allowance (30000000)",
                Answer,
            ),
            (
                "eth_call",
                -32602,
                "invalid argument 0: hex string has length 3",
                Answer,
            ),
            (
                "eth_call",
                -32000,
                "something nobody has seen before",
                Answer,
            ),
            // A transaction the endpoint holds already.
            (
                "eth_sendRawTransaction",
                -32000,
                "already known",
                AlreadyKnown,
            ),
            (
                "eth_sendRawTransaction",
                -32010,
                "Known transaction: 0xabc",
                AlreadyKnown,
            ),
            // eth_getLogs caps: Alchemy free, Infura, ethPandaOps, dRPC, 1RPC.
            (
                "eth_getLogs",
                -32600,
                "Under the Free tier plan, you can make eth_getLogs requests with up to a 10 \
                 block range.",
                NotHere,
            ),
            (
                "eth_getLogs",
                -32602,
                "range 49999 exceeds limit of 10000",
                NotHere,
            ),
            (
                "eth_getLogs",
                -32005,
                "query returned more than 10000 results",
                NotHere,
            ),
            (
                "eth_getLogs",
                -32012,
                "getLogs request exceeded max allowed range",
                NotHere,
            ),
            (
                "eth_getLogs",
                35,
                "ranges over 10000 blocks are not supported on free plan",
                NotHere,
            ),
            (
                "eth_getLogs",
                -32602,
                "eth_getLogs is limited to 0 - 50 blocks range",
                NotHere,
            ),
            // History or blocks this endpoint does not have.
            ("eth_getLogs", 4444, "pruned history unavailable", NotHere),
            ("eth_call", -32000, "header not found", NotHere),
            (
                "eth_call",
                -32000,
                "missing trie node 1a2b (path )",
                NotHere,
            ),
            (
                "eth_feeHistory",
                -32601,
                "the method eth_feeHistory does not exist/is not available",
                NotHere,
            ),
            // The endpoint is failing.
            (
                "eth_call",
                -32005,
                "daily request count exceeded, request rate limited",
                Failing,
            ),
            (
                "eth_getLogs",
                -32029,
                "Too Many Requests, Please apply an OnFinality API key",
                Failing,
            ),
            (
                "eth_call",
                429,
                "Your app has exceeded its compute units per second capacity",
                Failing,
            ),
            (
                "eth_sendRawTransaction",
                -32005,
                "rate limit exceeded",
                Failing,
            ),
            ("eth_blockNumber", -32603, "Internal error", Failing),
            (
                "eth_chainId",
                -32000,
                "Unauthorized: You must authenticate your request with an API key",
                Failing,
            ),
            ("eth_call", -32000, "upstream request timeout", Failing),
        ];
        for &(method, code, message, expected) in cases {
            assert_eq!(
                classify_error(method, code, message),
                expected,
                "{method} {code} {message}"
            );
        }
    }

    #[test]
    fn already_known_stands_for_the_raw_transaction_hash() {
        let raw = "0x02f86c83aa36a7";
        let request: RequestPacket = alloy::rpc::json_rpc::Request::new(
            SEND_RAW_TRANSACTION,
            alloy::rpc::json_rpc::Id::Number(7),
            [raw],
        )
        .serialize()
        .unwrap()
        .into();
        let ResponsePacket::Single(response) = accepted_response(&request).unwrap() else {
            panic!("expected a single response");
        };
        assert_eq!(response.id, alloy::rpc::json_rpc::Id::Number(7));
        let ResponsePayload::Success(payload) = response.payload else {
            panic!("expected a success payload");
        };
        let expected = keccak256(hex::decode(raw).unwrap());
        assert_eq!(payload.get(), format!("\"{expected}\""));
    }

    /// Read-only check of the endpoints an operator configured: the chain id
    /// check, then the bridge's full state with the startup
    /// `LayerAnchorAppended` scan. Reads `RPC_URL`, `RPC_FAILOVER_URLS`,
    /// `BRIDGE_ADDRESS`, `BRIDGE_DEPLOY_BLOCK` and the scan's
    /// `BRIDGE_GET_LOGS_CHUNK_BLOCKS` / `BRIDGE_GET_LOGS_PAUSE_MS`, as
    /// `daemon-live` does:
    ///
    /// ```text
    /// cd crates/bridge-prover-libraries
    /// set -a && source L2_config/env && set +a
    /// cargo test -p bridge-relayer-daemon --lib live_endpoints -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "reads a live chain; needs RPC_URL and BRIDGE_ADDRESS"]
    async fn live_endpoints_read_the_bridge() {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
        let primary = std::env::var("RPC_URL").expect("RPC_URL");
        let failover: Vec<String> = std::env::var(ENV_RPC_FAILOVER_URLS)
            .unwrap_or_default()
            .split(',')
            .map(str::to_string)
            .collect();
        let urls = endpoint_urls(&primary, &failover).unwrap();
        let (provider, chain_id) = read_provider(&urls).await.unwrap();
        let address = std::env::var("BRIDGE_ADDRESS").expect("BRIDGE_ADDRESS");
        let number = |name: &str| -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)
        };
        let bridge = crate::EthBridgeClient::with_scan_config(
            address.parse().unwrap(),
            provider,
            crate::LogScanConfig {
                deploy_block: number("BRIDGE_DEPLOY_BLOCK"),
                chunk_blocks: number("BRIDGE_GET_LOGS_CHUNK_BLOCKS"),
                pause: Duration::from_millis(number("BRIDGE_GET_LOGS_PAUSE_MS")),
            },
        );
        let state = bridge.read_full_state().await.unwrap();
        println!(
            "chain {chain_id}: bridge last seen {}, anchors held per layer {:?}",
            state.last_seen_block_seq_no,
            state
                .layer_windows
                .iter()
                .map(|w| w.data_len)
                .collect::<Vec<_>>()
        );
    }

    // ── The transport against local endpoints ──────────────────────────────

    use std::sync::atomic::Ordering;

    use alloy::primitives::B256;
    use serde_json::{json, Value};

    use super::test_support::{by_method, endpoint, http_reply, result, rpc_error, silent};

    fn client(urls: &[Url]) -> RpcClient {
        let http = rpc_http_client(Duration::from_millis(300)).unwrap();
        RpcClient::new(
            FailoverTransport::new(urls, http, ENDPOINT_COOLDOWN).unwrap(),
            true,
        )
    }

    async fn block_number(client: &RpcClient) -> Result<u64, TransportError> {
        client
            .request_noparams::<U64>("eth_blockNumber")
            .await
            .map(|n| n.to::<u64>())
    }

    #[tokio::test]
    async fn a_silent_primary_hands_over_and_waits_at_the_back() {
        let (primary, primary_hits) = endpoint(silent()).await;
        let (fallback, fallback_hits) = endpoint(result(json!("0x2a"))).await;
        let client = client(&[primary, fallback]);

        assert_eq!(block_number(&client).await.unwrap(), 42);
        assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
        // On cooldown: the next request goes straight to the fallback.
        assert_eq!(block_number(&client).await.unwrap(), 42);
        assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rate_limits_and_http_errors_move_on() {
        let (limited, _) = endpoint(rpc_error(-32005, "daily request count exceeded")).await;
        let (http_429, _) = endpoint(Arc::new(|_| Some((429, json!("Too Many Requests"))))).await;
        let (http_400, _) = endpoint(Arc::new(|_| Some((400, json!("Bad Request"))))).await;
        let (http_503, _) = endpoint(http_reply(
            503,
            json!({"jsonrpc": "2.0", "id": 0,
                   "error": {"code": -32000, "message": "no backend is currently healthy"}}),
        ))
        .await;
        let (working, hits) = endpoint(result(json!("0x7"))).await;
        let client = client(&[limited, http_429, http_400, http_503, working]);
        assert_eq!(block_number(&client).await.unwrap(), 7);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// Alchemy's 401 for a rejected key carries a JSON-RPC error that no
    /// pattern recognises (`-32600 Must be authenticated!`). It is the
    /// endpoint failing all the same.
    #[tokio::test]
    async fn a_rejected_key_moves_on_whatever_the_body_says() {
        let (bad_key, bad_key_hits) = endpoint(http_reply(
            401,
            json!({"jsonrpc": "2.0", "id": 0,
                   "error": {"code": -32600, "message": "Must be authenticated!"}}),
        ))
        .await;
        let (working, _) = endpoint(result(json!("0x7"))).await;
        let client = client(&[bad_key, working]);
        assert_eq!(block_number(&client).await.unwrap(), 7);
        // On cooldown: the next request goes straight to the working one.
        assert_eq!(block_number(&client).await.unwrap(), 7);
        assert_eq!(bad_key_hits.load(Ordering::SeqCst), 1);
    }

    /// The review's case: Alchemy's free tier answers an `eth_getLogs` span
    /// over 10 blocks with HTTP 400 (seen 2026-10-20). Read as a span cap,
    /// it passes the request on and leaves the endpoint in its place.
    #[tokio::test]
    async fn a_span_cap_that_comes_with_http_400_does_not_bench_the_endpoint() {
        let (alchemy, alchemy_hits) = endpoint(by_method(vec![
            (
                "eth_getLogs",
                http_reply(
                    400,
                    json!({"jsonrpc": "2.0", "id": 0, "error": {"code": -32600, "message":
                        "Under the Free tier plan, you can make eth_getLogs requests with up \
                         to a 10 block range."}}),
                ),
            ),
            ("eth_blockNumber", result(json!("0x1"))),
        ]))
        .await;
        let (wide, _) = endpoint(result(json!([]))).await;
        let client = client(&[alchemy, wide]);

        let logs: Value = client
            .request(
                "eth_getLogs",
                (json!({"fromBlock": "0x1", "toBlock": "0x7d0"}),),
            )
            .await
            .unwrap();
        assert_eq!(logs, json!([]));
        assert_eq!(block_number(&client).await.unwrap(), 1);
        assert_eq!(alchemy_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_revert_that_comes_with_http_400_is_the_answer() {
        let (primary, _) = endpoint(http_reply(
            400,
            json!({"jsonrpc": "2.0", "id": 0,
                   "error": {"code": 3, "message": "execution reverted"}}),
        ))
        .await;
        let (fallback, fallback_hits) = endpoint(result(json!("0x"))).await;
        let client = client(&[primary, fallback]);
        let err = client
            .request::<_, Value>(
                "eth_call",
                (
                    json!({"to": "0x0000000000000000000000000000000000000001"}),
                    "latest",
                ),
            )
            .await
            .unwrap_err();
        let payload = err.as_error_resp().expect("the endpoint's JSON-RPC error");
        assert_eq!(payload.code, 3);
        assert_eq!(fallback_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_capped_get_logs_moves_on_without_benching_the_endpoint() {
        let (alchemy_like, alchemy_hits) = endpoint(by_method(vec![
            (
                "eth_getLogs",
                rpc_error(
                    -32600,
                    "you can make eth_getLogs requests with up to a 10 block range",
                ),
            ),
            ("eth_blockNumber", result(json!("0x1"))),
        ]))
        .await;
        let (wide, _) = endpoint(result(json!([]))).await;
        let client = client(&[alchemy_like, wide]);

        let logs: Value = client
            .request(
                "eth_getLogs",
                (json!({"fromBlock": "0x1", "toBlock": "0x7d0"}),),
            )
            .await
            .unwrap();
        assert_eq!(logs, json!([]));
        // Not on cooldown: it is still first for the next request.
        assert_eq!(block_number(&client).await.unwrap(), 1);
        assert_eq!(alchemy_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_revert_is_the_answer_and_goes_nowhere_else() {
        let (primary, _) = endpoint(rpc_error(3, "execution reverted: UnknownAnchor")).await;
        let (fallback, fallback_hits) = endpoint(result(json!("0x"))).await;
        let client = client(&[primary, fallback]);
        let err = client
            .request::<_, Value>(
                "eth_call",
                (
                    json!({"to": "0x0000000000000000000000000000000000000001"}),
                    "latest",
                ),
            )
            .await
            .unwrap_err();
        let payload = err.as_error_resp().expect("the endpoint's JSON-RPC error");
        assert_eq!(payload.code, 3);
        assert_eq!(fallback_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn when_every_endpoint_fails_the_error_names_hosts_not_keys() {
        // Nothing listens on these ports: connection refused, twice.
        let urls = [
            Url::parse("http://127.0.0.1:9/v3/SECRET-KEY").unwrap(),
            Url::parse("http://127.0.0.1:7/v2/OTHER-SECRET").unwrap(),
        ];
        let err = block_number(&client(&urls)).await.unwrap_err().to_string();
        assert!(!err.contains("SECRET"), "{err}");
        assert!(err.contains("http://127.0.0.1:7"), "{err}");
    }

    #[tokio::test]
    async fn a_transaction_already_known_counts_as_sent_and_copies_go_out() {
        let raw = "0x02f86c83aa36a780";
        let (primary, _) = endpoint(rpc_error(-32000, "already known")).await;
        let (other, other_hits) = endpoint(result(json!("0x00"))).await;
        let client = client(&[primary, other]);
        let hash: B256 = client
            .request("eth_sendRawTransaction", (raw,))
            .await
            .unwrap();
        assert_eq!(hash, keccak256(hex::decode(raw).unwrap()));
        // The copy to the other endpoint is sent in the background.
        for _ in 0..50 {
            if other_hits.load(Ordering::SeqCst) == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the copy of the transaction never reached the other endpoint");
    }

    /// The log at any level, debug and trace included, while the transport
    /// checks chain ids, fails over, sends a transaction and copies it.
    /// alloy's `Http` opened a debug span with the full URL on every
    /// request, so `RUST_LOG=debug` put the API key into the log.
    #[tokio::test]
    async fn no_log_line_holds_a_key_at_any_level() {
        #[derive(Clone)]
        struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Captured(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let (silent_one, _) = endpoint(silent()).await;
        let (working, working_hits) = endpoint(by_method(vec![
            ("eth_chainId", result(json!("0xaa36a7"))),
            ("eth_blockNumber", result(json!("0x2a"))),
            ("eth_sendRawTransaction", rpc_error(-32000, "already known")),
        ]))
        .await;
        let (copy, copy_hits) = endpoint(by_method(vec![
            ("eth_chainId", result(json!("0xaa36a7"))),
            ("eth_sendRawTransaction", result(json!("0x00"))),
        ]))
        .await;
        let http = rpc_http_client(Duration::from_millis(300)).unwrap();
        let transport =
            FailoverTransport::new(&[silent_one, working, copy], http, ENDPOINT_COOLDOWN).unwrap();
        transport.check_chain_ids().await.unwrap();
        let client = RpcClient::new(transport, true);
        assert_eq!(block_number(&client).await.unwrap(), 42);
        let _: B256 = client
            .request("eth_sendRawTransaction", ("0x02f86c83aa36a782",))
            .await
            .unwrap();
        for _ in 0..50 {
            if copy_hits.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(working_hits.load(Ordering::SeqCst) >= 3);

        let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("127.0.0.1"),
            "endpoints are named in the log:\n{text}"
        );
        assert!(!text.contains("SECRET-KEY"), "{text}");
    }

    #[tokio::test]
    async fn a_lost_send_goes_through_the_next_endpoint() {
        let raw = "0x02f86c83aa36a781";
        let expected = keccak256(hex::decode(raw).unwrap());
        let (lost, _) = endpoint(silent()).await;
        let (working, _) = endpoint(result(json!(expected))).await;
        let client = client(&[lost, working]);
        let hash: B256 = client
            .request("eth_sendRawTransaction", (raw,))
            .await
            .unwrap();
        assert_eq!(hash, expected);
    }

    #[tokio::test]
    async fn endpoints_must_agree_on_the_chain() {
        let http = || rpc_http_client(Duration::from_millis(300)).unwrap();
        let (sepolia, _) = endpoint(result(json!("0xaa36a7"))).await;
        let (also_sepolia, _) = endpoint(result(json!("0xaa36a7"))).await;
        let (mainnet, _) = endpoint(result(json!("0x1"))).await;
        let (down, _) = endpoint(silent()).await;

        let ok = FailoverTransport::new(
            &[down.clone(), sepolia.clone(), also_sepolia],
            http(),
            ENDPOINT_COOLDOWN,
        )
        .unwrap();
        assert_eq!(ok.check_chain_ids().await.unwrap(), 11_155_111);

        let mixed = FailoverTransport::new(&[sepolia, mainnet], http(), ENDPOINT_COOLDOWN).unwrap();
        let err = mixed.check_chain_ids().await.unwrap_err().to_string();
        assert!(err.contains("disagree on the chain id"), "{err}");
        assert!(!err.contains("SECRET"), "{err}");

        let none = FailoverTransport::new(&[down], http(), ENDPOINT_COOLDOWN).unwrap();
        let err = none.check_chain_ids().await.unwrap_err().to_string();
        assert!(err.contains("no Ethereum RPC endpoint answered"), "{err}");
        assert!(!err.contains("SECRET"), "{err}");
    }
}

/// A local JSON-RPC endpoint for tests of the transport and of the code
/// that sends through it.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use alloy::transports::http::reqwest::Url;
    use serde_json::{json, Value};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    pub(crate) type Responder = Arc<dyn Fn(&Value) -> Option<(u16, Value)> + Send + Sync>;

    /// A local JSON-RPC endpoint answering each request with `respond(request)`
    /// (`None`: never answer), at a URL whose path stands for an API key.
    /// Returns the URL and the number of requests it has received.
    pub(crate) async fn endpoint(respond: Responder) -> (Url, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/v3/SECRET-KEY",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let respond = respond.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let body = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break buf[end + 4..end + 4 + len].to_vec();
                        }
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    let Some((status, mut reply)) = respond(&request) else {
                        std::future::pending::<()>().await;
                        return;
                    };
                    if reply.is_object() && reply.get("jsonrpc").is_some() {
                        reply["id"] = request["id"].clone();
                    }
                    let text = reply.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (url, hits)
    }

    pub(crate) fn result(value: Value) -> Responder {
        Arc::new(move |_| Some((200, json!({"jsonrpc": "2.0", "id": 0, "result": value}))))
    }

    pub(crate) fn rpc_error(code: i64, message: &'static str) -> Responder {
        Arc::new(move |_| {
            Some((
                200,
                json!({"jsonrpc": "2.0", "id": 0, "error": {"code": code, "message": message}}),
            ))
        })
    }

    /// `body` with HTTP `status`, to every request.
    pub(crate) fn http_reply(status: u16, body: Value) -> Responder {
        Arc::new(move |_| Some((status, body.clone())))
    }

    pub(crate) fn by_method(routes: Vec<(&'static str, Responder)>) -> Responder {
        Arc::new(move |request| {
            let method = request["method"].as_str().unwrap_or_default();
            routes
                .iter()
                .find(|(m, _)| *m == method)
                .and_then(|(_, r)| r(request))
        })
    }

    pub(crate) fn silent() -> Responder {
        Arc::new(|_| None)
    }
}
