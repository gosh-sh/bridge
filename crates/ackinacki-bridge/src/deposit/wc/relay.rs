//! A WalletConnect relay client: one websocket, JSON-RPC 2.0, the three
//! `irn_*` calls this CLI needs. The socket lives in a background task
//! that reconnects with backoff and resubscribes every topic; the relay
//! keeps messages for their TTL, so a reconnect loses nothing. A socket
//! that goes silent without closing — a network switch, a sleep, a
//! middlebox that dropped the flow — is caught by pinging it and giving up
//! on it when nothing at all comes back. A message is handed out at most
//! once: a redelivery after a reconnect and an echo of this client's own
//! publication are both dropped.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures::{SinkExt as _, StreamExt as _};
use rand::Rng as _;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{tungstenite::Message, Connector};

use crate::deposit::retry::{base_delay, jittered};

/// A message the relay delivered on a subscribed topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The topic it was published on.
    pub topic: String,
    /// The envelope, exactly as published.
    pub message: String,
    /// The publisher's tag: which WalletConnect request or response it is.
    pub tag: u32,
}

/// A request for the socket task.
enum Cmd {
    /// Make the JSON-RPC call `method(params)` and send its result to `reply`.
    Call {
        method: &'static str,
        params: Value,
        reply: oneshot::Sender<anyhow::Result<Value>>,
    },
}

/// A connection to a WalletConnect relay. Dropping it stops the socket task.
pub struct Relay {
    /// Calls for the socket task.
    cmd: mpsc::UnboundedSender<Cmd>,
    /// Messages from subscribed topics, each one once.
    inbox: mpsc::UnboundedReceiver<Incoming>,
    /// The socket task.
    task: tokio::task::JoinHandle<()>,
}

/// How long one attempt to open the websocket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the client pings the relay.
const PING_INTERVAL: Duration = Duration::from_secs(25);

/// How long the socket may stay silent — not even a pong — before it is
/// taken for dead and replaced: two missed pings.
const READ_DEADLINE: Duration = Duration::from_secs(2 * PING_INTERVAL.as_secs());

/// A fresh JSON-RPC id: milliseconds since the epoch times 1000 plus a
/// counter, the shape WalletConnect peers use.
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ms * 1000 + N.fetch_add(1, Ordering::Relaxed) % 1000
}

/// TLS for a `wss://` relay: rustls with the `ring` provider named here and
/// the Mozilla roots from `webpki-roots`. This build links both rustls
/// providers, so rustls has no process default to fall back on and
/// `ClientConfig::builder()` — what tokio-tungstenite calls when it is not
/// handed a connector — panics. Naming the provider avoids that without
/// installing a process-wide default for other code to inherit.
fn tls_connector() -> anyhow::Result<Connector> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

/// Opens the websocket, giving up after [`CONNECT_TIMEOUT`]. A `ws://` URL
/// does not use `tls`.
async fn open(url: &str, tls: &Connector) -> anyhow::Result<Ws> {
    let connecting =
        tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(tls.clone()));
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| {
            anyhow::anyhow!("the relay did not answer in {}s", CONNECT_TIMEOUT.as_secs())
        })??;
    Ok(ws)
}

/// Where the socket task opens the websocket: asked for every opening, so
/// that a URL whose token runs out is given a fresh one.
type UrlSource = Box<dyn Fn() -> String + Send + Sync>;

impl Relay {
    /// Connects to `url`, the same URL for every reconnect.
    pub async fn connect(url: String) -> anyhow::Result<Relay> {
        Self::connect_with(move || url.clone()).await
    }

    /// Connects to the URL `url` returns, which carries the JWT and the
    /// project id, and asks it again for each reconnect. A bad URL or a
    /// refused authorization fails here; a connection that drops later is
    /// reopened by the socket task.
    pub async fn connect_with(
        url: impl Fn() -> String + Send + Sync + 'static,
    ) -> anyhow::Result<Relay> {
        let tls = tls_connector()?;
        let ws = open(&url(), &tls).await?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(Box::new(url), tls, ws, cmd_rx, in_tx));
        Ok(Relay {
            cmd: cmd_tx,
            inbox: in_rx,
            task,
        })
    }

    /// Makes one call and waits for its result. A connection that drops
    /// before the result arrives is an error, though the call may have
    /// reached the relay.
    async fn call(&self, method: &'static str, params: Value) -> anyhow::Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Call {
                method,
                params,
                reply: tx,
            })
            .map_err(|_| anyhow::anyhow!("relay task ended"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("relay connection dropped"))?
    }

    /// Subscribes to `topic`; the subscription is renewed after every
    /// reconnect.
    pub async fn subscribe(&self, topic: &str) -> anyhow::Result<()> {
        self.call("irn_subscribe", json!({ "topic": topic }))
            .await
            .map(|_| ())
    }

    /// Publishes `message` on `topic`; the relay keeps it for `ttl_s`
    /// seconds. Tags 1100 (session proposal) and 1108 (session request)
    /// ask the relay to notify the wallet.
    pub async fn publish(
        &self,
        topic: &str,
        message: &str,
        ttl_s: u64,
        tag: u32,
    ) -> anyhow::Result<()> {
        let prompt = matches!(tag, 1100 | 1108);
        self.call(
            "irn_publish",
            json!({ "topic": topic, "message": message, "ttl": ttl_s, "tag": tag, "prompt": prompt }),
        )
        .await
        .map(|_| ())
    }

    /// The next message from a subscribed topic, or `None` when `timeout`
    /// passes first.
    pub async fn recv(&mut self, timeout: Duration) -> Option<Incoming> {
        tokio::time::timeout(timeout, self.inbox.recv())
            .await
            .ok()
            .flatten()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        // Without this a task stuck reconnecting would outlive the session.
        self.task.abort();
    }
}

/// The websocket the relay speaks over.
type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The error every call waiting on a dropped connection gets.
fn dropped() -> anyhow::Error {
    anyhow::anyhow!("relay connection dropped")
}

/// Hands `result` to the caller waiting for it.
fn answer(reply: oneshot::Sender<anyhow::Result<Value>>, result: anyhow::Result<Value>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a caller that stopped waiting has nobody left to tell"
    )]
    let _ = reply.send(result);
}

/// A JSON-RPC frame as a websocket text message.
fn text(frame: Value) -> Message {
    Message::Text(frame.to_string().into())
}

/// Writes a frame nobody waits on. A failed write needs no handling here:
/// a broken socket ends the next read, and a silent one misses the read
/// deadline; either way it is replaced.
async fn send_unanswered(ws: &mut Ws, frame: Message) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a broken socket shows up on the next read or at the read deadline"
    )]
    let _ = ws.send(frame).await;
}

/// Opens the websocket again, pausing before each attempt: 1 s doubling to
/// 60 s, ±20 %, each attempt on the URL `url` returns then. It never gives
/// up; callers bound their own waits.
async fn reconnect(url: &UrlSource, tls: &Connector) -> Ws {
    let mut attempt = 0u32;
    loop {
        let pause = jittered(
            base_delay(attempt),
            rand::thread_rng().gen_range(-1.0..=1.0),
        );
        tokio::time::sleep(pause).await;
        attempt = attempt.saturating_add(1);
        if let Ok(ws) = open(&url(), tls).await {
            return ws;
        }
    }
}

/// Replaces a dead socket: every call waiting on it fails (it may or may
/// not have reached the relay), a new socket is opened, and every
/// confirmed topic is subscribed again, which makes the relay hand over
/// what it kept for them.
async fn recover(
    url: &UrlSource,
    tls: &Connector,
    topics: &HashSet<String>,
    pending: &mut HashMap<u64, (Option<String>, oneshot::Sender<anyhow::Result<Value>>)>,
) -> Ws {
    for (_, (_, reply)) in pending.drain() {
        answer(reply, Err(dropped()));
    }
    let mut ws = reconnect(url, tls).await;
    for t in topics {
        let frame = json!({
            "id": next_id(), "jsonrpc": "2.0", "method": "irn_subscribe",
            "params": { "topic": t }
        });
        send_unanswered(&mut ws, text(frame)).await;
    }
    ws
}

/// The socket task: sends calls, matches results to them by id, acks and
/// forwards `irn_subscription` pushes, pings, and replaces the socket when
/// it breaks or goes silent. Ends when the [`Relay`] is gone.
async fn run(
    url: UrlSource,
    tls: Connector,
    mut ws: Ws,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    inbox: mpsc::UnboundedSender<Incoming>,
) {
    use tokio::time::{interval_at, sleep_until, Instant, MissedTickBehavior};

    // Topics with a confirmed subscription, renewed after each reconnect.
    let mut topics: HashSet<String> = HashSet::new();
    // Calls sent and not yet answered, by id; a subscribe carries its topic.
    let mut pending: HashMap<u64, (Option<String>, oneshot::Sender<anyhow::Result<Value>>)> =
        HashMap::new();
    // sha256 of every message delivered or published by this client.
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut ping = interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // When the socket last delivered any frame, pongs included.
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(Cmd::Call { method, params, reply }) = cmd else { return };
                if reply.is_closed() {
                    // The caller gave up, typically while a reconnect held
                    // this call in the queue. Sent now, a request could
                    // reach the wallet after the run has moved on.
                    continue;
                }
                if method == "irn_publish" {
                    // A relay may hand our own publication back to us on a
                    // topic we are subscribed to; it is never an answer.
                    let own = params["message"].as_str().unwrap_or_default();
                    seen.insert(Sha256::digest(own.as_bytes()).into());
                }
                let id = next_id();
                let topic = (method == "irn_subscribe")
                    .then(|| params["topic"].as_str().unwrap_or_default().to_string());
                let frame = json!({ "id": id, "jsonrpc": "2.0", "method": method, "params": params });
                if ws.send(text(frame)).await.is_err() {
                    answer(reply, Err(dropped()));
                    continue;
                }
                pending.insert(id, (topic, reply));
            }
            _ = ping.tick() => send_unanswered(&mut ws, Message::Ping(Default::default())).await,
            () = sleep_until(heard + READ_DEADLINE) => {
                ws = recover(&url, &tls, &topics, &mut pending).await;
                heard = Instant::now();
                ping.reset();
            }
            frame = ws.next() => {
                let body = match frame {
                    Some(Ok(m)) => {
                        heard = Instant::now();
                        match m {
                            Message::Text(t) => t,
                            // Pings are answered by tungstenite itself on the
                            // next read; pongs only count as a sign of life.
                            _ => continue,
                        }
                    }
                    Some(Err(_)) | None => {
                        ws = recover(&url, &tls, &topics, &mut pending).await;
                        heard = Instant::now();
                        ping.reset();
                        continue;
                    }
                };
                let Ok(v) = serde_json::from_str::<Value>(body.as_str()) else { continue };
                if v["method"] == "irn_subscription" {
                    let ack = json!({ "id": v["id"], "jsonrpc": "2.0", "result": true });
                    send_unanswered(&mut ws, text(ack)).await;
                    let d = &v["params"]["data"];
                    let Some(message) = d["message"].as_str() else { continue };
                    if seen.insert(Sha256::digest(message.as_bytes()).into()) {
                        #[expect(
                            clippy::let_underscore_must_use,
                            reason = "the Relay is gone, and nobody wants the message"
                        )]
                        let _ = inbox.send(Incoming {
                            topic: d["topic"].as_str().unwrap_or_default().to_string(),
                            message: message.to_string(),
                            tag: d["tag"].as_u64().and_then(|t| u32::try_from(t).ok()).unwrap_or(0),
                        });
                    }
                    continue;
                }
                if let Some(id) = v["id"].as_u64() {
                    if let Some((topic, reply)) = pending.remove(&id) {
                        let r = match v.get("error") {
                            Some(e) => Err(anyhow::anyhow!("relay error: {e}")),
                            None => {
                                if let Some(t) = topic {
                                    topics.insert(t);
                                }
                                Ok(v["result"].clone())
                            }
                        };
                        answer(reply, r);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::deposit::testkit::MockRelay;

    #[tokio::test]
    async fn a_message_reaches_the_other_subscriber() {
        let relay = MockRelay::start().await;
        let mut a = Relay::connect(relay.url()).await.unwrap();
        let b = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        b.publish("t1", "hello", 300, 1100).await.unwrap();
        let got = a.recv(Duration::from_secs(5)).await.unwrap();
        assert_eq!(
            (got.topic.as_str(), got.message.as_str(), got.tag),
            ("t1", "hello", 1100)
        );
    }

    #[tokio::test]
    async fn a_dropped_connection_resubscribes_and_loses_nothing() {
        let relay = MockRelay::start().await;
        let mut a = Relay::connect(relay.url()).await.unwrap();
        let b = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        relay.drop_all();
        // Published while `a` is reconnecting; the relay keeps it for its TTL.
        let publish = async {
            loop {
                if b.publish("t1", "after", 300, 1108).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        publish.await;
        let got = a.recv(Duration::from_secs(10)).await.unwrap();
        assert_eq!(got.message, "after");
    }

    #[tokio::test]
    async fn our_own_publication_is_never_delivered_back() {
        let relay = MockRelay::start().await; // it echoes to every subscriber, the publisher included
        let mut a = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        a.publish("t1", "mine", 300, 1108).await.unwrap();
        assert!(a.recv(Duration::from_millis(500)).await.is_none());
    }

    #[tokio::test]
    async fn a_redelivered_message_is_seen_once() {
        let relay = MockRelay::start().await;
        let mut a = Relay::connect(relay.url()).await.unwrap();
        let b = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        b.publish("t1", "once", 300, 1).await.unwrap();
        assert!(a.recv(Duration::from_secs(5)).await.is_some());
        relay.drop_all(); // the relay redelivers its backlog on resubscribe
        assert!(a.recv(Duration::from_secs(3)).await.is_none());
    }

    #[tokio::test]
    async fn a_stalled_connection_is_replaced_and_loses_nothing() {
        let relay = MockRelay::start().await;
        let mut a = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        tokio::time::pause();
        // Half-open: nothing closes the socket, it just goes silent.
        relay.stall_all();
        let b = Relay::connect(relay.url()).await.unwrap();
        b.publish("t1", "after", 300, 1108).await.unwrap();
        let got = a.recv(Duration::from_secs(600)).await.unwrap();
        assert_eq!(got.message, "after");
        assert_eq!(
            relay.connections(),
            3,
            "`a` reconnected once, `b` connected once"
        );
    }

    #[tokio::test]
    async fn an_idle_connection_is_kept_alive_by_pings() {
        let relay = MockRelay::start().await;
        let mut a = Relay::connect(relay.url()).await.unwrap();
        a.subscribe("t1").await.unwrap();
        tokio::time::pause();
        // Ten quiet minutes: the pongs alone must keep the socket.
        assert!(a.recv(Duration::from_secs(600)).await.is_none());
        assert_eq!(relay.connections(), 1);
    }

    #[tokio::test]
    async fn a_call_given_up_during_a_reconnect_is_never_sent() {
        let relay = MockRelay::start().await;
        let a = Relay::connect(relay.url()).await.unwrap();
        relay.drop_all();
        // `a` has seen the socket close and waits about a second to reconnect.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let gave_up = tokio::time::timeout(
            Duration::from_millis(100),
            a.publish("t1", "stale", 300, 1108),
        )
        .await;
        assert!(gave_up.is_err());
        let mut b = Relay::connect(relay.url()).await.unwrap();
        b.subscribe("t1").await.unwrap();
        // Queued after the stale call, so it would arrive second.
        a.publish("t1", "fresh", 300, 1108).await.unwrap();
        let got = b.recv(Duration::from_secs(5)).await.unwrap();
        assert_eq!(got.message, "fresh");
    }

    #[test]
    fn the_tls_connector_builds_without_a_process_default_provider() {
        // This build links both rustls providers, so rustls has no default
        // to fall back on; a config that does not name one panics here.
        assert!(matches!(
            tls_connector().unwrap(),
            tokio_tungstenite::Connector::Rustls(_)
        ));
    }

    #[tokio::test]
    async fn a_wss_handshake_failure_is_an_error_not_a_panic() {
        // A TCP peer that hangs up at once: the TLS handshake for `wss://`
        // must fail as an error, which needs no network beyond loopback.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                drop(tcp);
            }
        });
        let connected = Relay::connect(format!("wss://{addr}/")).await;
        assert!(connected.is_err());
    }
}
