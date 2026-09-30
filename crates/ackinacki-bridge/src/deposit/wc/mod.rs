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
}

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
        let sk = ed25519_dalek::SigningKey::from_bytes(&crypto::random_bytes());
        let now = unix_now();
        let auth = jwt::relay_jwt(
            &sk,
            &hex::encode(crypto::random_bytes::<32>()),
            &self.cfg.relay_url,
            now,
            86_400,
        );
        let url = jwt::relay_url(&self.cfg.relay_url, &self.cfg.project_id, &auth);
        let mut r = relay::Relay::connect(url).await.map_err(|e| {
            WalletError::Disconnected(format!("cannot reach the WalletConnect relay: {e:#}"))
        })?;
        let uri = session::PairingUri::new(now, self.cfg.pair_timeout);
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
                &mut r,
                &mut s,
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
                        &mut r,
                        &mut s,
                        &any_chain,
                        "wallet_addEthereumChain",
                        json!([net.eip3085()]),
                        t,
                    )
                    .await?;
                },
                Err(e) => return Err(e),
            }
            let deadline = tokio::time::Instant::now() + t;
            while s.accounts_on(&net.caip2()).is_empty() {
                if tokio::time::Instant::now() > deadline {
                    return Err(WalletError::Other(format!(
                        "the wallet did not add {}",
                        net.name()
                    )));
                }
                // A harmless request lets `request` process the pending
                // session update.
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "only the session update matters here"
                )]
                let _ = session::request(
                    &mut r,
                    &mut s,
                    &any_chain,
                    "eth_accounts",
                    json!([]),
                    Duration::from_secs(5),
                )
                .await;
            }
        }
        let accounts = s.accounts_on(&net.caip2());
        let account = match self.cfg.expect_from {
            Some(f) if accounts.contains(&f) => f,
            Some(f) => {
                return Err(WalletError::Other(format!(
                    "the wallet connected {accounts:?}, not --from-address {f}"
                )))
            },
            None => accounts[0],
        };
        self.relay = Some(r);
        self.session = Some(s);
        Ok(account)
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
}
