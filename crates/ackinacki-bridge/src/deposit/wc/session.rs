//! Sign API v2 as a dApp: propose on a pairing topic, settle on the
//! session topic, then JSON-RPC requests. Only what the deposit needs.
//!
//! A message is sealed once and published unchanged until the relay takes
//! it: a call that failed with a dropped connection may still have reached
//! the relay, and a byte-identical copy is one a relay client drops as a
//! duplicate. A wallet can still answer twice in differently sealed copies,
//! so an answer is matched to its request by JSON-RPC id. Ids never repeat
//! within a run and a request ends at its first answer, so a late copy
//! matches nothing.

use std::{future::Future, time::Duration};

use alloy_primitives::Address;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::deposit::{
    args::Network,
    retry::until_with,
    wallet::WalletError,
    wc::{
        crypto::{derive_sym_key, open, parse, random_bytes, seal_type0, topic_of, KeyPair},
        relay::Relay,
    },
};

/// The methods the session proposal asks the wallet for.
pub const METHODS: [&str; 6] = [
    "eth_sendTransaction",
    "eth_accounts",
    "personal_sign",
    "wallet_switchEthereumChain",
    "wallet_addEthereumChain",
    "wallet_getCapabilities",
];
/// The events the session proposal asks the wallet for.
pub const EVENTS: [&str; 2] = ["chainChanged", "accountsChanged"];
/// Relay tag of `wc_sessionPropose`.
pub const TAG_PROPOSE: u32 = 1100;
/// Relay tag of the answer to `wc_sessionPropose`.
/// The wallet sends it; the mock wallet peer does.
#[cfg(test)]
pub const TAG_PROPOSE_RESP: u32 = 1101;
/// Relay tag of `wc_sessionSettle`.
/// The wallet sends it; the mock wallet peer does.
#[cfg(test)]
pub const TAG_SETTLE: u32 = 1102;
/// Relay tag of the answer to `wc_sessionSettle`.
pub const TAG_SETTLE_RESP: u32 = 1103;
/// Relay tag of `wc_sessionUpdate`; its answer is one more.
pub const TAG_UPDATE: u32 = 1104;
/// Relay tag of `wc_sessionExtend`; its answer is one more.
pub const TAG_EXTEND: u32 = 1106;
/// Relay tag of `wc_sessionRequest`.
pub const TAG_REQUEST: u32 = 1108;
/// Relay tag of the answer to `wc_sessionRequest`.
/// The wallet sends it; the mock wallet peer does.
#[cfg(test)]
pub const TAG_REQUEST_RESP: u32 = 1109;
/// Relay tag of `wc_sessionEvent`; its answer is one more.
pub const TAG_EVENT: u32 = 1110;
/// Relay tag of `wc_sessionDelete`.
pub const TAG_DELETE: u32 = 1112;
/// Relay tag of `wc_sessionPing`; its answer is one more.
pub const TAG_PING: u32 = 1114;

/// How long the relay keeps a message: five minutes. Also the shortest
/// life of a pairing URI.
const TTL: u64 = 300;

/// How long the relay keeps `wc_sessionDelete`: a day, so a wallet that is
/// offline now still learns the session is over.
const DELETE_TTL: u64 = 86_400;

/// The longest closing a session waits for the relay.
const DELETE_WAIT: Duration = Duration::from_secs(5);

/// The longest the answer to something the wallet sent us may take to
/// publish; it only keeps the wallet's side of the exchange tidy.
const ACK_WAIT: Duration = Duration::from_secs(5);

/// The life of a pairing URI and of its proposal on the relay, in seconds:
/// the time the CLI waits for the wallet, rounded up, and never less than
/// [`TTL`]. A URI that expired while the CLI still waits could not be
/// paired.
fn pairing_ttl_s(pair_timeout: Duration) -> u64 {
    let whole = pair_timeout
        .as_secs()
        .saturating_add(u64::from(pair_timeout.subsec_nanos() > 0));
    whole.max(TTL)
}

/// What the QR code carries: the pairing topic, the key that seals the
/// proposal and the wallet's answer to it, and when the URI expires.
#[derive(Debug, Clone)]
pub struct PairingUri {
    /// Hex of 32 random bytes.
    pub topic: String,
    /// The symmetric key of the pairing topic.
    pub sym_key: [u8; 32],
    /// Unix seconds after which a wallet refuses the URI.
    pub expiry: u64,
}

impl PairingUri {
    /// A fresh URI at `now` (Unix seconds) that lives as long as the CLI
    /// waits for the wallet, `pair_timeout`, and at least five minutes.
    pub fn new(now: u64, pair_timeout: Duration) -> Self {
        PairingUri {
            topic: hex::encode(random_bytes::<32>()),
            sym_key: random_bytes(),
            expiry: now.saturating_add(pairing_ttl_s(pair_timeout)),
        }
    }

    /// The `wc:` URI a wallet scans.
    pub fn to_uri(&self) -> String {
        format!(
            "wc:{}@2?relay-protocol=irn&symKey={}&expiryTimestamp={}",
            self.topic,
            hex::encode(self.sym_key),
            self.expiry
        )
    }

    /// Reads a `wc:` URI back, the way a wallet does when it scans one.
    #[cfg(test)]
    pub fn parse(s: &str) -> Result<PairingUri, String> {
        let rest = s.strip_prefix("wc:").ok_or("not a wc: URI")?;
        let (topic, query) = rest.split_once("@2?").ok_or("not a WalletConnect v2 URI")?;
        let mut sym_key = None;
        let mut expiry = None;
        for kv in query.split('&') {
            match kv.split_once('=') {
                Some(("symKey", v)) => {
                    let mut k = [0u8; 32];
                    hex::decode_to_slice(v, &mut k).map_err(|e| format!("symKey: {e}"))?;
                    sym_key = Some(k);
                },
                Some(("expiryTimestamp", v)) => {
                    expiry = Some(v.parse().map_err(|e| format!("expiryTimestamp: {e}"))?)
                },
                _ => {},
            }
        }
        Ok(PairingUri {
            topic: topic.to_string(),
            sym_key: sym_key.ok_or("no symKey")?,
            expiry: expiry.ok_or("no expiryTimestamp")?,
        })
    }
}

/// A settled session.
#[derive(Clone)]
pub struct Session {
    /// The session topic: sha256 of `sym_key`.
    pub topic: String,
    /// The key both sides derived; it seals everything on the topic.
    pub sym_key: [u8; 32],
    /// The accounts the wallet shared, as CAIP-10 ids
    /// (`eip155:<chain id>:<address>`).
    pub accounts: Vec<String>,
    /// Unix seconds when the session ends; 0 when the wallet did not say.
    pub expiry: u64,
}

impl std::fmt::Debug for Session {
    /// Everything but the key, which would let a log reader into the
    /// session.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("topic", &self.topic)
            .field("sym_key", &"<redacted>")
            .field("accounts", &self.accounts)
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl Session {
    /// The shared accounts on the CAIP-2 chain `caip2`, such as
    /// `eip155:11155111`.
    pub fn accounts_on(&self, caip2: &str) -> Vec<Address> {
        self.accounts
            .iter()
            .filter_map(|a| a.strip_prefix(caip2)?.strip_prefix(':')?.parse().ok())
            .collect()
    }
}

/// Unix time in seconds.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A JSON-RPC id in the shape WalletConnect peers use — milliseconds since
/// the epoch times 1000 — and above every id handed out before in this
/// process, so that an answer can only ever match the request it answers.
fn rpc_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let floor = ms.saturating_mul(1000);
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let id = floor.max(last.saturating_add(1));
        match LAST.compare_exchange_weak(last, id, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(now) => last = now,
        }
    }
}

/// `timeout` from now; a timeout too long to add is thirty years.
fn deadline_after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(30 * 365 * 86_400))
}

/// `v` as a type 0 envelope under `sym`, with a fresh nonce.
fn seal_json(sym: &[u8; 32], v: &Value) -> String {
    seal_type0(sym, random_bytes(), v.to_string().as_bytes())
}

/// The JSON inside an envelope, or `None` when it does not open under
/// `sym` or is not JSON.
fn open_json(sym: &[u8; 32], message: &str) -> Option<Value> {
    let plain = open(sym, &parse(message).ok()?).ok()?;
    serde_json::from_slice(&plain).ok()
}

/// The answer to request `id`, if `v` is one. A JSON-RPC response carries
/// `result` or `error` and no `method`; anything with a `method` is a
/// request — including an echo of our own with the same id.
pub fn as_response(v: &Value, id: u64) -> Option<Result<Value, Value>> {
    if v.get("method").is_some() || v["id"].as_u64() != Some(id) {
        return None;
    }
    match (v.get("result"), v.get("error")) {
        (_, Some(e)) => Some(Err(e.clone())),
        (Some(r), None) => Some(Ok(r.clone())),
        (None, None) => None,
    }
}

/// A wallet's JSON-RPC error. 4001 (EIP-1193) and 5000 (WalletConnect) are
/// the user saying no. Any other code is passed on with the wallet's
/// message, code first: `4902` is how a wallet says it does not know the
/// chain.
fn wallet_error(e: &Value) -> WalletError {
    match e["code"].as_i64() {
        Some(4001 | 5000) => WalletError::Rejected,
        Some(code) => WalletError::Other(format!(
            "{code}: {}",
            e["message"].as_str().unwrap_or("no message")
        )),
        None => WalletError::Other(e.to_string()),
    }
}

/// The CAIP-10 accounts in the `eip155` namespace of a settle or update.
fn eip155_accounts(params: &Value) -> Option<Vec<String>> {
    let a = params["namespaces"]["eip155"]["accounts"].as_array()?;
    Some(
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
    )
}

/// Runs the relay call `f` until it succeeds or `deadline` passes, pausing
/// 1 s doubling to 60 s, ±20 %, between attempts while the relay client
/// reconnects, and hands every failed attempt to `on_retry`. Out of time
/// after a failure, the last failure is the reason; out of time with none,
/// the wait ran out.
async fn with_retries<F, Fut, R>(deadline: Instant, f: F, on_retry: R) -> Result<(), WalletError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
    R: FnMut(u32, &anyhow::Error),
{
    until_with(Some(deadline), f, on_retry)
        .await
        .map_err(|last| match last {
            Some(e) => WalletError::Disconnected(format!("the WalletConnect relay: {e:#}")),
            None => WalletError::Timeout,
        })
}

/// Logs a failed relay call as an error: every attempt leaves a trace, the
/// ones a later attempt made good included.
fn log_failure(call: &str, topic: &str, attempt: u32, e: &anyhow::Error) {
    tracing::error!(
        call,
        topic,
        attempt,
        error = %format!("{e:#}"),
        "a WalletConnect relay call failed",
    );
}

/// Subscribes to `topic`, retrying until `deadline`.
async fn subscribe_by(relay: &Relay, topic: &str, deadline: Instant) -> Result<(), WalletError> {
    with_retries(
        deadline,
        move || relay.subscribe(topic),
        |attempt, e| log_failure("irn_subscribe", topic, attempt, e),
    )
    .await
}

/// Publishes the sealed `envelope` on `topic`, retrying the very same
/// envelope until `deadline`.
async fn publish_by(
    relay: &Relay,
    topic: &str,
    envelope: &str,
    ttl_s: u64,
    tag: u32,
    deadline: Instant,
) -> Result<(), WalletError> {
    with_retries(
        deadline,
        move || relay.publish(topic, envelope, ttl_s, tag),
        |attempt, e| log_failure(&format!("irn_publish (tag {tag})"), topic, attempt, e),
    )
    .await
}

/// The next message on `topic` that opens under `sym` as JSON; `Timeout`
/// once `deadline` passes. Messages on other topics, and ones that do not
/// open, are skipped.
async fn next_on(
    relay: &mut Relay,
    topic: &str,
    sym: &[u8; 32],
    deadline: Instant,
) -> Result<Value, WalletError> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Some(m) = relay.recv(left).await else {
            return Err(WalletError::Timeout);
        };
        if m.topic != topic {
            continue;
        }
        if let Some(v) = open_json(sym, &m.message) {
            return Ok(v);
        }
    }
}

/// Proposes a session on the pairing topic of `uri` and waits, no longer
/// than `timeout`, for the wallet to answer and settle it. The proposal
/// stays on the relay as long as the URI is valid.
pub async fn propose_and_settle(
    relay: &mut Relay,
    uri: &PairingUri,
    net: Network,
    timeout: Duration,
) -> Result<Session, WalletError> {
    let deadline = deadline_after(timeout);
    subscribe_by(relay, &uri.topic, deadline).await?;
    let me = KeyPair::generate();
    let id = rpc_id();
    let propose = json!({
        "id": id, "jsonrpc": "2.0", "method": "wc_sessionPropose",
        "params": {
            "requiredNamespaces": {},
            "optionalNamespaces": {
                "eip155": { "chains": [net.caip2()], "methods": METHODS, "events": EVENTS }
            },
            "relays": [{ "protocol": "irn" }],
            "proposer": {
                "publicKey": hex::encode(me.public),
                "metadata": {
                    "name": "ackinacki-bridge",
                    "description": "Acki Nacki bridge deposit",
                    "url": "https://github.com/gosh-sh/bridge",
                    "icons": []
                }
            },
            "expiryTimestamp": uri.expiry,
            "pairingTopic": uri.topic,
        }
    });
    let sealed = seal_json(&uri.sym_key, &propose);
    let ttl = pairing_ttl_s(timeout);
    publish_by(relay, &uri.topic, &sealed, ttl, TAG_PROPOSE, deadline).await?;

    let responder = loop {
        let v = next_on(relay, &uri.topic, &uri.sym_key, deadline).await?;
        let result = match as_response(&v, id) {
            None => continue,
            Some(Err(e)) => return Err(wallet_error(&e)),
            Some(Ok(r)) => r,
        };
        let key = result["responderPublicKey"]
            .as_str()
            .and_then(|s| hex::decode(s).ok())
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
        match key {
            Some(k) => break k,
            None => {
                return Err(WalletError::Other(
                    "the wallet answered the proposal without a key".into(),
                ))
            },
        }
    };
    let sym = derive_sym_key(&me.secret, &responder);
    let topic = topic_of(&sym);
    // The wallet may settle before it answers the proposal; the relay keeps
    // the settlement and hands it over on this subscription.
    subscribe_by(relay, &topic, deadline).await?;
    loop {
        let v = next_on(relay, &topic, &sym, deadline).await?;
        if v["method"] != "wc_sessionSettle" {
            continue;
        }
        let ack = json!({ "id": v["id"], "jsonrpc": "2.0", "result": true });
        let sealed = seal_json(&sym, &ack);
        publish_by(relay, &topic, &sealed, TTL, TAG_SETTLE_RESP, deadline).await?;
        return Ok(Session {
            topic,
            sym_key: sym,
            accounts: eip155_accounts(&v["params"]).unwrap_or_default(),
            expiry: v["params"]["expiry"].as_u64().unwrap_or(0),
        });
    }
}

/// Handles a request the wallet sent us while we wait for an answer.
/// `wc_sessionDelete` ends the wait with `Disconnected`. An update, an
/// extension, a ping or an event is applied and acknowledged, the
/// acknowledgement on a best-effort basis. Anything else — a second copy of
/// the settlement, an echo of our own request — is ignored.
async fn from_wallet(
    relay: &Relay,
    s: &mut Session,
    method: &str,
    v: &Value,
) -> Result<(), WalletError> {
    let tag = match method {
        "wc_sessionDelete" => {
            return Err(WalletError::Disconnected(
                "the wallet ended the session".into(),
            ))
        },
        "wc_sessionUpdate" => {
            if let Some(a) = eip155_accounts(&v["params"]) {
                s.accounts = a;
            }
            TAG_UPDATE + 1
        },
        "wc_sessionExtend" => {
            if let Some(e) = v["params"]["expiry"].as_u64() {
                s.expiry = e;
            }
            TAG_EXTEND + 1
        },
        "wc_sessionPing" => TAG_PING + 1,
        "wc_sessionEvent" => TAG_EVENT + 1,
        _ => return Ok(()),
    };
    let ack = json!({ "id": v["id"], "jsonrpc": "2.0", "result": true });
    let sealed = seal_json(&s.sym_key, &ack);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a lost acknowledgement costs the wallet a retry, not us the answer we wait for"
    )]
    let _ = tokio::time::timeout(ACK_WAIT, relay.publish(&s.topic, &sealed, TTL, tag)).await;
    Ok(())
}

/// Sends `method(params)` to the wallet for `chain` (CAIP-2) and waits, no
/// longer than `timeout`, for its answer. `4001` is `Rejected`, `4902` an
/// `Other` that starts with the code, and `wc_sessionDelete` from the
/// wallet `Disconnected`. A session update that arrives meanwhile updates
/// `s`.
pub async fn request(
    relay: &mut Relay,
    s: &mut Session,
    chain: &str,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, WalletError> {
    let deadline = deadline_after(timeout);
    if s.expiry != 0 && now() >= s.expiry {
        return Err(WalletError::Disconnected(
            "the wallet session expired".into(),
        ));
    }
    let id = rpc_id();
    let req = json!({
        "id": id, "jsonrpc": "2.0", "method": "wc_sessionRequest",
        "params": { "request": { "method": method, "params": params }, "chainId": chain }
    });
    let sealed = seal_json(&s.sym_key, &req);
    publish_by(relay, &s.topic, &sealed, TTL, TAG_REQUEST, deadline).await?;
    loop {
        let v = next_on(relay, &s.topic, &s.sym_key, deadline).await?;
        if let Some(m) = v["method"].as_str() {
            from_wallet(relay, s, m, &v).await?;
            continue;
        }
        match as_response(&v, id) {
            None => continue,
            Some(Ok(r)) => return Ok(r),
            Some(Err(e)) => return Err(wallet_error(&e)),
        }
    }
}

/// Waits, sending nothing, until the session shares an account on the
/// CAIP-2 chain `caip2` — a `wc_sessionUpdate` from the wallet — and
/// returns `Timeout` if that does not happen within `timeout`. Updates,
/// pings and events are applied and acknowledged as in [`request`].
pub async fn wait_for_accounts_on(
    relay: &mut Relay,
    s: &mut Session,
    caip2: &str,
    timeout: Duration,
) -> Result<(), WalletError> {
    let deadline = deadline_after(timeout);
    while s.accounts_on(caip2).is_empty() {
        let v = next_on(relay, &s.topic, &s.sym_key, deadline).await?;
        if let Some(m) = v["method"].as_str() {
            from_wallet(relay, s, m, &v).await?;
        }
    }
    Ok(())
}

/// Ends the session for the wallet too: `wc_sessionDelete`, kept by the
/// relay for a day. Best effort, and no longer than [`DELETE_WAIT`]: a
/// session the relay cannot carry the delete for expires on its own.
pub async fn disconnect(relay: &Relay, s: &Session) {
    let del = json!({
        "id": rpc_id(), "jsonrpc": "2.0", "method": "wc_sessionDelete",
        "params": { "code": 6000, "message": "User disconnected." }
    });
    let sealed = seal_json(&s.sym_key, &del);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a session that is already gone needs no delete"
    )]
    let _ = tokio::time::timeout(
        DELETE_WAIT,
        relay.publish(&s.topic, &sealed, DELETE_TTL, TAG_DELETE),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy_primitives::B256;
    use serde_json::json;

    use super::*;
    use crate::deposit::{args::Network, testkit::*, wc::relay::Relay};

    fn peer(signer: &alloy::signers::local::PrivateKeySigner) -> PeerBehaviour {
        PeerBehaviour {
            add_update: AddUpdate::Before,
            accounts: vec![format!("eip155:11155111:{:#x}", signer.address())],
            signer: signer.clone(),
            send_result: Ok(B256::repeat_byte(0x42)),
            missing_chain_then_add: false,
        }
    }

    const SEPOLIA: &str = "eip155:11155111";
    const WAIT: Duration = Duration::from_secs(10);

    /// A wallet the test scripts by hand: it pairs, then hands the relay,
    /// the session topic and its key to `script`.
    fn scripted_wallet<F, Fut>(
        url: String,
        uri: PairingUri,
        script: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: FnOnce(Relay, String, [u8; 32]) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        tokio::spawn(async move {
            let mut w = Relay::connect(url).await.unwrap();
            let signer = alloy::signers::local::PrivateKeySigner::random();
            let accounts = vec![format!("{SEPOLIA}:{:#x}", signer.address())];
            let (topic, sym) = MockWalletPeer::pair(&mut w, &uri, &accounts).await;
            script(w, topic, sym).await;
        })
    }

    #[test]
    fn the_pairing_uri_has_the_documented_shape() {
        let u = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let s = u.to_uri();
        assert!(s.starts_with(&format!("wc:{}@2?relay-protocol=irn&symKey=", u.topic)));
        assert!(s.contains(&hex::encode(u.sym_key)));
        assert!(s.contains("expiryTimestamp=1700000300"));
        assert_eq!(u.topic.len(), 64);
        let back = PairingUri::parse(&s).unwrap();
        assert_eq!(
            (back.topic, back.sym_key, back.expiry),
            (u.topic, u.sym_key, u.expiry)
        );
        assert!(PairingUri::parse("wc:abc@1?symKey=00").is_err());
    }

    #[test]
    fn a_pairing_uri_lives_as_long_as_the_wait_for_the_wallet() {
        let life = |t: Duration| PairingUri::new(1_700_000_000, t).expiry - 1_700_000_000;
        assert_eq!(life(Duration::from_millis(300)), 300);
        assert_eq!(life(Duration::from_secs(300)), 300);
        assert_eq!(life(Duration::from_secs(600)), 600);
        assert_eq!(life(Duration::from_millis(600_001)), 601);
        let long = PairingUri::new(1_700_000_000, Duration::from_secs(600));
        assert!(long.to_uri().contains("expiryTimestamp=1700000600"));
        let absurd = PairingUri::new(1_700_000_000, Duration::from_secs(u64::MAX));
        assert_eq!(absurd.expiry, u64::MAX);
    }

    #[test]
    fn accounts_are_picked_by_their_exact_chain() {
        let a = Address::repeat_byte(0x11);
        let s = Session {
            topic: String::new(),
            sym_key: [0; 32],
            accounts: vec![
                format!("eip155:1:{a:#x}"),
                format!("eip155:11155111:{a:#x}"),
                "eip155:11155111:not-an-address".into(),
            ],
            expiry: 0,
        };
        assert_eq!(s.accounts_on(SEPOLIA), vec![a]);
        assert_eq!(s.accounts_on("eip155:1"), vec![a]);
        assert!(s.accounts_on("eip155:5").is_empty());
    }

    #[test]
    fn the_session_key_stays_out_of_debug_output() {
        let s = Session {
            topic: "t".into(),
            sym_key: [0xab; 32],
            accounts: vec![],
            expiry: 0,
        };
        let shown = format!("{s:?}");
        // 171 is 0xab, the way a derived `Debug` prints key bytes.
        assert!(!shown.contains("171"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn an_echo_of_our_own_request_is_not_an_answer() {
        let req = json!({ "id": 7, "jsonrpc": "2.0", "method": "wc_sessionRequest", "params": {} });
        assert_eq!(as_response(&req, 7), None);
        let propose =
            json!({ "id": 7, "jsonrpc": "2.0", "method": "wc_sessionPropose", "params": {} });
        assert_eq!(as_response(&propose, 7), None);
        assert_eq!(
            as_response(&json!({ "id": 7, "jsonrpc": "2.0", "result": null }), 7),
            Some(Ok(Value::Null))
        );
        assert!(matches!(
            as_response(
                &json!({ "id": 7, "jsonrpc": "2.0", "error": { "code": 4001 } }),
                7
            ),
            Some(Err(_))
        ));
        assert_eq!(
            as_response(&json!({ "id": 8, "jsonrpc": "2.0", "result": 1 }), 7),
            None
        );
    }

    #[test]
    fn a_message_with_a_method_is_never_an_answer() {
        let odd = json!({ "id": 7, "jsonrpc": "2.0", "method": "wc_sessionRequest", "result": 1 });
        assert_eq!(as_response(&odd, 7), None);
    }

    #[test]
    fn request_ids_never_repeat() {
        let ids: Vec<u64> = (0..10_000).map(|_| rpc_id()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        assert!(
            ids[9_999] < 1 << 53,
            "a JavaScript wallet reads the id as a number"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn every_failed_relay_call_is_reported_even_when_a_later_one_succeeds() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let mut reported = vec![];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        let done = with_retries(
            deadline,
            || async {
                match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 | 1 => anyhow::bail!("relay connection dropped"),
                    _ => Ok(()),
                }
            },
            |attempt, e| reported.push((attempt, e.to_string())),
        )
        .await;
        assert_eq!(done, Ok(()));
        let dropped = "relay connection dropped".to_string();
        assert_eq!(reported, vec![(1, dropped.clone()), (2, dropped)]);
    }

    #[tokio::test(start_paused = true)]
    async fn out_of_time_the_relay_error_is_the_last_one() {
        // Attempts at 0, ~1, ~3 and ~7 s; the pause after the fourth would
        // end past the deadline, so the fourth error is the answer.
        let calls = std::sync::atomic::AtomicU32::new(0);
        let mut reported = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let e = with_retries(
            deadline,
            || async {
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                anyhow::bail!("failure {n}")
            },
            |_, _| reported += 1,
        )
        .await
        .unwrap_err();
        assert_eq!(reported, 4);
        assert_eq!(
            e,
            crate::deposit::wallet::WalletError::Disconnected(
                "the WalletConnect relay: failure 4".into()
            )
        );

        // An attempt the deadline cuts short leaves the failure before it.
        let calls = std::sync::atomic::AtomicU32::new(0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let e = with_retries(
            deadline,
            || async {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    anyhow::bail!("failure 1")
                }
                std::future::pending().await
            },
            |_, _| {},
        )
        .await
        .unwrap_err();
        assert_eq!(
            e,
            crate::deposit::wallet::WalletError::Disconnected(
                "the WalletConnect relay: failure 1".into()
            )
        );

        // With no failure at all, the wait just ran out.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let e = with_retries(deadline, std::future::pending, |_, _| {})
            .await
            .unwrap_err();
        assert_eq!(e, crate::deposit::wallet::WalletError::Timeout);
    }

    #[tokio::test]
    async fn pairs_signs_and_sends_through_a_mock_wallet() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), peer(&signer));
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        assert_eq!(s.accounts_on(SEPOLIA), vec![signer.address()]);
        let sig = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "personal_sign",
            json!([
                format!("0x{}", hex::encode("hi")),
                format!("{:#x}", signer.address())
            ]),
            WAIT,
        )
        .await
        .unwrap();
        assert_eq!(sig.as_str().unwrap().len(), 2 + 130);
        // The mock answers every request twice; the second copy of the
        // signature must not pass for the answer to this one.
        let h = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "eth_sendTransaction",
            json!([{}]),
            WAIT,
        )
        .await
        .unwrap();
        assert_eq!(h, json!(format!("{:#x}", B256::repeat_byte(0x42))));
    }

    #[tokio::test]
    async fn a_rejection_in_the_wallet_is_rejected() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let mut b = peer(&signer);
        b.send_result = Err(4001);
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), b);
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        let e = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "eth_sendTransaction",
            json!([{}]),
            WAIT,
        )
        .await
        .unwrap_err();
        assert_eq!(e, crate::deposit::wallet::WalletError::Rejected);
    }

    #[tokio::test]
    async fn nobody_scanning_times_out() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let e = propose_and_settle(&mut r, &uri, Network::Sepolia, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(e, crate::deposit::wallet::WalletError::Timeout);
    }

    #[tokio::test]
    async fn a_longer_pair_timeout_keeps_the_proposal_on_the_relay_as_long() {
        let relay = MockRelay::start().await;
        let pair_timeout = Duration::from_secs(600);
        let uri = PairingUri::new(1_700_000_000, pair_timeout);
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), peer(&signer));
        let mut r = Relay::connect(relay.url()).await.unwrap();
        propose_and_settle(&mut r, &uri, Network::Sepolia, pair_timeout)
            .await
            .unwrap();
        let proposals: Vec<u64> = relay
            .published()
            .into_iter()
            .filter(|(topic, tag, _)| *topic == uri.topic && *tag == u64::from(TAG_PROPOSE))
            .map(|(_, _, ttl)| ttl)
            .collect();
        assert_eq!(proposals, vec![600]);
        assert_eq!(uri.expiry, 1_700_000_600);
    }

    #[tokio::test]
    async fn a_proposal_declined_in_the_wallet_is_rejected() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let (url, u) = (relay.url(), uri.clone());
        tokio::spawn(async move {
            let mut w = Relay::connect(url).await.unwrap();
            w.subscribe(&u.topic).await.unwrap();
            let (propose, _) = MockWalletPeer::next(&mut w, &u.topic, &u.sym_key, WAIT)
                .await
                .unwrap();
            let no = json!({ "id": propose["id"], "jsonrpc": "2.0", "error": { "code": 5000, "message": "User rejected." } });
            MockWalletPeer::send(&w, &u.topic, &u.sym_key, no, TAG_PROPOSE_RESP).await;
        });
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let e = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap_err();
        assert_eq!(e, crate::deposit::wallet::WalletError::Rejected);
    }

    #[tokio::test]
    async fn a_wallet_without_the_chain_answers_4902_and_adds_it() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let mut b = peer(&signer);
        b.missing_chain_then_add = true;
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), b);
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        assert!(s.accounts_on(SEPOLIA).is_empty());
        let switch = json!([{ "chainId": "0xaa36a7" }]);
        let e = request(
            &mut r,
            &mut s,
            "eip155:1",
            "wallet_switchEthereumChain",
            switch,
            WAIT,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&e, crate::deposit::wallet::WalletError::Other(m) if m.starts_with("4902")),
            "{e:?}"
        );
        let add = json!([Network::Sepolia.eip3085()]);
        request(
            &mut r,
            &mut s,
            "eip155:1",
            "wallet_addEthereumChain",
            add,
            WAIT,
        )
        .await
        .unwrap();
        // The session update came before the answer and was applied.
        assert_eq!(s.accounts_on(SEPOLIA), vec![signer.address()]);
    }

    #[tokio::test]
    async fn a_ping_is_answered_and_the_wallet_ending_the_session_is_a_disconnect() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let wallet = scripted_wallet(relay.url(), uri.clone(), |mut w, topic, sym| async move {
            let ping =
                json!({ "id": 5, "jsonrpc": "2.0", "method": "wc_sessionPing", "params": {} });
            MockWalletPeer::send(&w, &topic, &sym, ping, TAG_PING).await;
            let (mut pong, mut asked) = (None, false);
            while pong.is_none() || !asked {
                let (v, tag) = MockWalletPeer::next(&mut w, &topic, &sym, WAIT)
                    .await
                    .expect("the dApp's messages");
                if v["method"] == "wc_sessionRequest" {
                    asked = true;
                } else if v["id"] == 5 {
                    pong = Some((v["result"].clone(), tag));
                }
            }
            assert_eq!(pong, Some((json!(true), TAG_PING + 1)));
            let delete = json!({ "id": 6, "jsonrpc": "2.0", "method": "wc_sessionDelete",
                "params": { "code": 6000, "message": "User disconnected." } });
            MockWalletPeer::send(&w, &topic, &sym, delete, TAG_DELETE).await;
        });
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        let e = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "eth_sendTransaction",
            json!([{}]),
            WAIT,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(e, crate::deposit::wallet::WalletError::Disconnected(_)),
            "{e:?}"
        );
        wallet.await.unwrap();
    }

    #[tokio::test]
    async fn closing_the_session_tells_the_wallet() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let wallet = scripted_wallet(relay.url(), uri.clone(), |mut w, topic, sym| async move {
            loop {
                let (v, tag) = MockWalletPeer::next(&mut w, &topic, &sym, WAIT)
                    .await
                    .expect("a session delete");
                if v["method"] == "wc_sessionDelete" {
                    assert_eq!(tag, TAG_DELETE);
                    return;
                }
            }
        });
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        disconnect(&r, &s).await;
        wallet.await.unwrap();
    }

    #[tokio::test]
    async fn closing_does_not_wait_on_a_silent_relay() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), peer(&signer));
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        tokio::time::pause();
        relay.stall_all();
        let started = tokio::time::Instant::now();
        disconnect(&r, &s).await;
        // Left to itself, the relay client fails the call only when it gives
        // up on the socket, fifty seconds in.
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn an_expired_session_is_not_asked() {
        let relay = MockRelay::start().await;
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = Session {
            topic: "t".into(),
            sym_key: [7; 32],
            accounts: vec![],
            expiry: 1,
        };
        let e = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "eth_sendTransaction",
            json!([{}]),
            WAIT,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(e, crate::deposit::wallet::WalletError::Disconnected(_)),
            "{e:?}"
        );
        assert!(relay.published().is_empty());
    }

    #[tokio::test]
    async fn a_request_caught_in_a_silent_socket_is_published_again() {
        let relay = MockRelay::start().await;
        let uri = PairingUri::new(1_700_000_000, Duration::from_secs(300));
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let _peer = MockWalletPeer::spawn(relay.url(), uri.clone(), peer(&signer));
        let mut r = Relay::connect(relay.url()).await.unwrap();
        let mut s = propose_and_settle(&mut r, &uri, Network::Sepolia, WAIT)
            .await
            .unwrap();
        tokio::time::pause();
        // Half-open: the publish is written and never answered, until the
        // relay client gives up on the socket and fails the call.
        relay.stall_all();
        let sig = request(
            &mut r,
            &mut s,
            SEPOLIA,
            "personal_sign",
            json!([
                format!("0x{}", hex::encode("hi")),
                format!("{:#x}", signer.address())
            ]),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        assert_eq!(sig.as_str().unwrap().len(), 2 + 130);
    }
}
