//! The WalletConnect v2 wallet: a paired session over the relay.

pub mod crypto;
pub mod jwt;
pub mod relay;
pub mod session;

use std::{path::PathBuf, time::Duration};

use alloy_primitives::{Address, Bytes, B256};
use async_trait::async_trait;
use serde_json::json;

use crate::deposit::{
    args::Network,
    ui::Ui,
    wallet::{TxRequest, Wallet, WalletError, WalletKind},
};

/// How [`WalletConnectWallet`] reaches the relay and the wallet.
pub struct WcConfig {
    /// The relay's `wss://` base URL.
    pub relay_url: String,
    /// The WalletConnect Cloud project id.
    pub project_id: String,
    /// The network the deposit runs on.
    pub network: Network,
    /// `--from-address`: the account the wallet must share.
    pub expect_from: Option<Address>,
    /// How long to wait for the wallet to scan and approve.
    pub pair_timeout: Duration,
    /// How long to wait for the wallet to answer one request.
    pub request_timeout: Duration,
    /// `--qr-out`: also write the pairing code to this `.svg` or `.png`.
    pub qr_out: Option<PathBuf>,
}

/// A wallet reached through a WalletConnect session.
pub struct WalletConnectWallet {
    /// Settings.
    cfg: WcConfig,
    /// The relay connection, once paired.
    relay: Option<relay::Relay>,
    /// The session, once paired.
    session: Option<session::Session>,
}

impl WalletConnectWallet {
    /// A wallet that has not paired yet.
    pub fn new(cfg: WcConfig) -> Self {
        Self {
            cfg,
            relay: None,
            session: None,
        }
    }

    /// The relay and the session, or `Disconnected` before `connect` and
    /// after `close`.
    fn parts(&mut self) -> Result<(&mut relay::Relay, &mut session::Session), WalletError> {
        match (self.relay.as_mut(), self.session.as_mut()) {
            (Some(r), Some(s)) => Ok((r, s)),
            _ => Err(WalletError::Disconnected("no wallet session".into())),
        }
    }

    /// One request to the wallet on the deposit network.
    async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, WalletError> {
        let chain = self.cfg.network.caip2();
        let t = self.cfg.request_timeout;
        let (r, s) = self.parts()?;
        session::request(r, s, &chain, method, params, t).await
    }

    /// The account the settled session `s` shares on the deposit network:
    /// asks the wallet to switch to the network, or to add it, when the
    /// session has no account there, and holds the account to
    /// `--from-address` when one is given.
    async fn account_on_network(
        &self,
        r: &mut relay::Relay,
        s: &mut session::Session,
    ) -> Result<Address, WalletError> {
        let net = self.cfg.network;
        if s.accounts_on(&net.caip2()).is_empty() {
            let any_chain = s
                .accounts
                .first()
                .and_then(|a| a.rsplit_once(':'))
                .map(|(c, _)| c.to_string())
                .ok_or_else(|| WalletError::Other("the wallet shared no accounts".into()))?;
            let hex_id = format!("{:#x}", net.chain_id());
            let t = self.cfg.request_timeout;
            match session::request(
                r,
                s,
                &any_chain,
                "wallet_switchEthereumChain",
                json!([{ "chainId": hex_id }]),
                t,
            )
            .await
            {
                Ok(_) => {},
                Err(WalletError::Other(e)) if e.starts_with("4902") => {
                    session::request(
                        r,
                        s,
                        &any_chain,
                        "wallet_addEthereumChain",
                        json!([net.eip3085()]),
                        t,
                    )
                    .await?;
                },
                Err(e) => return Err(e),
            }
            session::wait_for_accounts_on(r, s, &net.caip2(), t)
                .await
                .map_err(|e| match e {
                    WalletError::Timeout => {
                        WalletError::Other(format!("the wallet did not add {}", net.name()))
                    },
                    other => other,
                })?;
        }
        let accounts = s.accounts_on(&net.caip2());
        match self.cfg.expect_from {
            Some(f) if accounts.contains(&f) => Ok(f),
            Some(f) => Err(WalletError::Other(format!(
                "the wallet connected {accounts:?}, not --from-address {f}"
            ))),
            None => Ok(accounts[0]),
        }
    }
}

/// How long a relay token is valid: a day.
const RELAY_TOKEN_TTL_S: u64 = 86_400;

/// Unix time in seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[async_trait]
impl Wallet for WalletConnectWallet {
    fn kind(&self) -> WalletKind {
        WalletKind::WalletConnect
    }

    async fn connect(&mut self, ui: &dyn Ui) -> Result<Address, WalletError> {
        // One client key, and a token signed by it for every opening of the
        // socket: a token lives a day, and the wait for the wallet may be
        // longer. The token travels only in the URL, which every printout
        // of a deposit run cuts to its origin.
        let sk = ed25519_dalek::SigningKey::from_bytes(&crypto::random_bytes());
        let (base, project) = (self.cfg.relay_url.clone(), self.cfg.project_id.clone());
        let url = move || {
            let sub = hex::encode(crypto::random_bytes::<32>());
            let auth = jwt::relay_jwt(&sk, &sub, &base, unix_now(), RELAY_TOKEN_TTL_S);
            jwt::relay_url(&base, &project, &auth)
        };
        let mut r = relay::Relay::connect_with(url).await.map_err(|e| {
            WalletError::Disconnected(format!("cannot reach the WalletConnect relay: {e:#}"))
        })?;
        let uri = session::PairingUri::new(unix_now(), self.cfg.pair_timeout);
        let net = self.cfg.network;
        if let Some(p) = &self.cfg.qr_out {
            crate::deposit::qr::write_file(p, &uri.to_uri())
                .map_err(|e| WalletError::Other(format!("{e:#}")))?;
        }
        ui.qr(&uri.to_uri(), &[
            (
                "network".into(),
                format!("{} ({})", net.name(), net.chain_id()),
            ),
            (
                "next".into(),
                "scan with your wallet, approve the connection, then approve two transactions: \
                 approve and deposit"
                    .into(),
            ),
        ]);
        let mut s = session::propose_and_settle(&mut r, &uri, net, self.cfg.pair_timeout).await?;
        // The wallet now holds a session. One that does not become this
        // run's is ended for the wallet too, or it lingers there until it
        // expires; the error that ended it is the answer.
        match self.account_on_network(&mut r, &mut s).await {
            Ok(account) => {
                self.relay = Some(r);
                self.session = Some(s);
                Ok(account)
            },
            Err(e) => {
                session::disconnect(&r, &s).await;
                // Stops the socket task.
                drop(r);
                Err(e)
            },
        }
    }

    async fn personal_sign(
        &mut self,
        account: Address,
        message: &str,
    ) -> Result<Bytes, WalletError> {
        let v = self
            .call(
                "personal_sign",
                json!([
                    format!("0x{}", hex::encode(message)),
                    format!("{account:#x}")
                ]),
            )
            .await?;
        let s = v
            .as_str()
            .ok_or_else(|| WalletError::Other("personal_sign returned no signature".into()))?;
        hex::decode(s.trim_start_matches("0x"))
            .map(Bytes::from)
            .map_err(|e| WalletError::Other(e.to_string()))
    }

    async fn capabilities(&mut self, account: Address) -> Option<serde_json::Value> {
        self.call("wallet_getCapabilities", json!([format!("{account:#x}")]))
            .await
            .ok()
    }

    async fn send_transaction(
        &mut self,
        _ui: &dyn Ui,
        tx: &TxRequest,
    ) -> Result<B256, WalletError> {
        let v = self
            .call("eth_sendTransaction", json!([tx.to_json()]))
            .await?;
        v.as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| WalletError::Other(format!("eth_sendTransaction returned {v}")))
    }

    async fn close(&mut self) {
        if let (Some(r), Some(s)) = (self.relay.as_ref(), self.session.as_ref()) {
            session::disconnect(r, s).await;
        }
        self.session = None;
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;
    use crate::deposit::{
        testkit::*,
        ui::{RecordingUi, UiEvent},
        wallet::Wallet,
    };

    fn cfg(relay: &MockRelay, pair_timeout: Duration) -> WcConfig {
        WcConfig {
            relay_url: relay.url(),
            project_id: "test".into(),
            network: Network::Sepolia,
            expect_from: None,
            pair_timeout,
            request_timeout: Duration::from_secs(10),
            qr_out: None,
        }
    }

    /// Plays the phone: waits for the URI the wallet shows, then scans it.
    fn scan_when_shown(
        ui: Arc<RecordingUi>,
        url: String,
        behaviour: PeerBehaviour,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let shown = ui.events().into_iter().find_map(|e| match e {
                    UiEvent::Qr(u) => Some(u),
                    _ => None,
                });
                if let Some(u) = shown {
                    let uri = session::PairingUri::parse(&u).unwrap();
                    let _peer = MockWalletPeer::spawn(url, uri, behaviour);
                    // Keep the peer task alive for the test's duration.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }

    fn behaviour(
        signer: &alloy::signers::local::PrivateKeySigner,
        missing_chain_then_add: bool,
    ) -> PeerBehaviour {
        PeerBehaviour {
            accounts: vec![format!("eip155:11155111:{:#x}", signer.address())],
            signer: signer.clone(),
            send_result: Ok(B256::repeat_byte(0x77)),
            missing_chain_then_add,
            add_update: AddUpdate::Before,
        }
    }

    #[tokio::test]
    async fn a_wallet_without_sepolia_adds_it_when_asked() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut w = WalletConnectWallet::new(cfg(&relay, Duration::from_secs(10)));
        let watcher = scan_when_shown(ui.clone(), relay.url(), behaviour(&signer, true));
        let account = w.connect(ui.as_ref()).await.unwrap();
        watcher.abort();
        assert_eq!(account, signer.address());
    }

    #[tokio::test]
    async fn a_connected_wallet_signs_a_message_and_sends_a_transaction() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut w = WalletConnectWallet::new(cfg(&relay, Duration::from_secs(10)));
        let watcher = scan_when_shown(ui.clone(), relay.url(), behaviour(&signer, false));
        let account = w.connect(ui.as_ref()).await.unwrap();
        let sig = w.personal_sign(account, "hello").await.unwrap();
        assert_eq!(sig.len(), 65);
        let tx = TxRequest {
            from: account,
            to: Address::repeat_byte(0x22),
            data: Bytes::from_static(&[1, 2, 3]),
            gas: 100_000,
            fees: crate::deposit::evm::Fees {
                max_fee_per_gas: 10,
                max_priority_fee_per_gas: 1,
            },
            purpose: crate::deposit::wallet::TxPurpose::Approve {
                token: Address::repeat_byte(0x22),
                spender: Address::repeat_byte(0x33),
                amount: alloy_primitives::U256::from(1),
            },
        };
        let hash = w.send_transaction(ui.as_ref(), &tx).await.unwrap();
        assert_eq!(hash, B256::repeat_byte(0x77));
        w.close().await;
        assert!(matches!(
            w.personal_sign(account, "again").await,
            Err(WalletError::Disconnected(_))
        ));
        watcher.abort();
    }

    #[tokio::test]
    async fn a_from_address_the_wallet_did_not_share_is_refused() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut c = cfg(&relay, Duration::from_secs(10));
        c.expect_from = Some(Address::repeat_byte(0x99));
        let mut w = WalletConnectWallet::new(c);
        let watcher = scan_when_shown(ui.clone(), relay.url(), behaviour(&signer, false));
        let e = w.connect(ui.as_ref()).await.unwrap_err();
        watcher.abort();
        assert!(matches!(e, WalletError::Other(m) if m.contains("--from-address")));
    }

    /// Plays a wallet the test scripts by hand: once the URI is shown, it
    /// pairs sharing `accounts`, then hands the relay, the session topic and
    /// its key to `script`.
    fn scripted_when_shown<F, Fut, T>(
        ui: Arc<RecordingUi>,
        url: String,
        accounts: Vec<String>,
        script: F,
    ) -> tokio::task::JoinHandle<T>
    where
        F: FnOnce(relay::Relay, String, [u8; 32]) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = T> + Send,
        T: Send + 'static,
    {
        tokio::spawn(async move {
            let uri = loop {
                let shown = ui.events().into_iter().find_map(|e| match e {
                    UiEvent::Qr(u) => Some(u),
                    _ => None,
                });
                if let Some(u) = shown {
                    break session::PairingUri::parse(&u).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            let mut r = relay::Relay::connect(url).await.unwrap();
            let (topic, sym) = MockWalletPeer::pair(&mut r, &uri, &accounts).await;
            script(r, topic, sym).await
        })
    }

    /// What the scripted wallet saw after pairing: answers each request with
    /// `answer` and returns the tag of `wc_sessionDelete`, or `None` if the
    /// session went quiet without one.
    async fn until_deleted(
        mut r: relay::Relay,
        topic: String,
        sym: [u8; 32],
        answer: fn(&serde_json::Value) -> serde_json::Value,
    ) -> Option<u32> {
        let mut answered = std::collections::HashSet::new();
        while let Some((v, tag)) =
            MockWalletPeer::next(&mut r, &topic, &sym, Duration::from_secs(10)).await
        {
            if v["method"] == "wc_sessionDelete" {
                return Some(tag);
            }
            if v["method"] == "wc_sessionRequest" && answered.insert(v["id"].as_u64().unwrap()) {
                let reply = answer(&v);
                MockWalletPeer::send(&r, &topic, &sym, reply, session::TAG_REQUEST_RESP).await;
            }
        }
        None
    }

    #[tokio::test]
    async fn a_from_address_the_wallet_did_not_share_ends_the_session_for_the_wallet() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut c = cfg(&relay, Duration::from_secs(10));
        let other = Address::repeat_byte(0x99);
        c.expect_from = Some(other);
        let mut w = WalletConnectWallet::new(c);
        let accounts = vec![format!("eip155:11155111:{:#x}", signer.address())];
        let wallet = scripted_when_shown(ui.clone(), relay.url(), accounts, |r, t, k| {
            until_deleted(r, t, k, |_| serde_json::Value::Null)
        });
        let e = w.connect(ui.as_ref()).await.unwrap_err();
        assert_eq!(
            e,
            WalletError::Other(format!(
                "the wallet connected {:?}, not --from-address {other}",
                vec![signer.address()]
            )),
            "the error is the one that ended the connection"
        );
        let seen = tokio::time::timeout(Duration::from_secs(20), wallet)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seen, Some(session::TAG_DELETE), "the wallet was told");
    }

    #[tokio::test]
    async fn a_chain_switch_the_wallet_refuses_ends_the_session_for_the_wallet() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut w = WalletConnectWallet::new(cfg(&relay, Duration::from_secs(10)));
        // Shared on mainnet only: the CLI asks the wallet to switch.
        let accounts = vec![format!("eip155:1:{:#x}", signer.address())];
        let wallet = scripted_when_shown(ui.clone(), relay.url(), accounts, |r, t, k| {
            until_deleted(r, t, k, |v| {
                assert_eq!(
                    v["params"]["request"]["method"],
                    "wallet_switchEthereumChain"
                );
                serde_json::json!({"id": v["id"], "jsonrpc": "2.0",
                    "error": {"code": 4001, "message": "User rejected"}})
            })
        });
        let e = w.connect(ui.as_ref()).await.unwrap_err();
        assert_eq!(e, WalletError::Rejected);
        let seen = tokio::time::timeout(Duration::from_secs(20), wallet)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seen, Some(session::TAG_DELETE), "the wallet was told");
    }

    #[tokio::test]
    async fn the_configured_pair_timeout_reaches_the_uri_and_the_proposal() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut w = WalletConnectWallet::new(cfg(&relay, Duration::from_secs(600)));
        let watcher = scan_when_shown(ui.clone(), relay.url(), behaviour(&signer, false));
        w.connect(ui.as_ref()).await.unwrap();
        watcher.abort();
        let shown = ui
            .events()
            .into_iter()
            .find_map(|e| match e {
                UiEvent::Qr(u) => Some(u),
                _ => None,
            })
            .unwrap();
        let uri = session::PairingUri::parse(&shown).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(uri.expiry >= now + 590 && uri.expiry <= now + 610);
        let ttls: Vec<u64> = relay
            .published()
            .into_iter()
            .filter(|(t, tag, _)| *t == uri.topic && *tag == u64::from(session::TAG_PROPOSE))
            .map(|(_, _, ttl)| ttl)
            .collect();
        assert_eq!(ttls, vec![600]);
    }

    /// The claims of the relay token a connection's request URI carries.
    fn token_claims(uri: &str) -> serde_json::Value {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine as _};
        let auth = uri
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("auth="))
            .unwrap();
        let claims = auth.split('.').nth(1).unwrap();
        serde_json::from_slice(&B64U.decode(claims).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_reconnect_carries_a_fresh_token_from_the_same_client_key() {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut w = WalletConnectWallet::new(cfg(&relay, Duration::from_secs(10)));
        let watcher = scan_when_shown(ui.clone(), relay.url(), behaviour(&signer, false));
        w.connect(ui.as_ref()).await.unwrap();
        // A token is dated in seconds: more than one has passed by the time
        // the socket is reopened.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        relay.drop_all();
        // The wallet peer's own connections carry no token.
        let ours = || -> Vec<String> {
            relay
                .request_uris()
                .into_iter()
                .filter(|u| u.contains("auth="))
                .collect()
        };
        tokio::time::timeout(Duration::from_secs(20), async {
            while ours().len() < 2 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the client reconnects");
        watcher.abort();
        let uris = ours();
        assert_ne!(uris[0], uris[1], "a new token for the new socket");
        let (first, second) = (token_claims(&uris[0]), token_claims(&uris[1]));
        assert_eq!(first["iss"], second["iss"], "signed by the same client key");
        assert!(
            second["iat"].as_u64().unwrap() > first["iat"].as_u64().unwrap(),
            "{first} {second}"
        );
        assert_eq!(
            second["exp"].as_u64().unwrap() - second["iat"].as_u64().unwrap(),
            86_400
        );
        for u in &uris {
            assert!(u.contains("projectId=test"), "{u}");
        }
    }

    /// A wallet that adds Sepolia but announces it as `add_update` says.
    async fn connect_with(
        add_update: AddUpdate,
        request_timeout: Duration,
    ) -> Result<Address, WalletError> {
        let relay = MockRelay::start().await;
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let ui = Arc::new(RecordingUi::new(true));
        let mut c = cfg(&relay, Duration::from_secs(10));
        c.request_timeout = request_timeout;
        let mut w = WalletConnectWallet::new(c);
        let mut b = behaviour(&signer, true);
        b.add_update = add_update;
        let watcher = scan_when_shown(ui.clone(), relay.url(), b);
        let r = tokio::time::timeout(Duration::from_secs(30), w.connect(ui.as_ref()))
            .await
            .expect("connect must end in bounded time");
        watcher.abort();
        r
    }

    #[tokio::test]
    async fn a_chain_announced_after_the_add_answer_is_waited_for() {
        let account = connect_with(AddUpdate::After, Duration::from_secs(10))
            .await
            .unwrap();
        assert_ne!(account, Address::ZERO);
    }

    #[tokio::test]
    async fn a_wallet_that_never_announces_the_chain_ends_the_wait_without_asking_again() {
        let e = connect_with(AddUpdate::Never, Duration::from_millis(600))
            .await
            .unwrap_err();
        assert_eq!(
            e,
            WalletError::Other("the wallet did not add Sepolia".into())
        );
    }
}
