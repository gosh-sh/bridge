//! `--qr-mode local-key`: no wallet, the CLI signs with a key it is given.
//! For unattended runs, where nobody is there to scan a code. The key comes
//! from [`LOCAL_KEY_ENV`](crate::deposit::args::LOCAL_KEY_ENV), never from
//! the command line, and is registered as a secret the output never prints.

use std::time::Duration;

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::{local::PrivateKeySigner, SignerSync as _},
};
use alloy_primitives::{Address, Bytes, B256};
use async_trait::async_trait;

use crate::deposit::{
    ui::Ui,
    wallet::{TxRequest, Wallet, WalletError, WalletKind},
};

/// A wallet that is a private key and the RPC node behind `--rpc-url`.
pub struct LocalKeyWallet {
    signer: PrivateKeySigner,
    rpc_url: String,
    chain_id: u64,
    request_timeout: Duration,
    /// The nonce after the last transaction this wallet broadcast. A
    /// load-balanced `--rpc-url` can answer the pending nonce from a node
    /// that has not seen that transaction yet (the deposit right after its
    /// approve), so the wallet never signs below it.
    next_nonce: Option<u64>,
}

/// The nonce to sign at: the node's pending count, but never below the one
/// after this wallet's own last transaction.
fn next_nonce(pending: u64, after_last: Option<u64>) -> u64 {
    pending.max(after_last.unwrap_or(0))
}

impl LocalKeyWallet {
    /// Signs with `key` (hex, with or without `0x`) for chain `chain_id`
    /// and broadcasts through `rpc_url`. The error never quotes the key.
    pub fn new(key: &str, rpc_url: String, chain_id: u64) -> Result<Self, String> {
        let signer = key
            .trim()
            .parse::<PrivateKeySigner>()
            .map_err(|_| "is not a secp256k1 private key (64 hex characters)".to_string())?;
        Ok(LocalKeyWallet {
            signer,
            rpc_url,
            chain_id,
            request_timeout: Duration::from_secs(60),
            next_nonce: None,
        })
    }

    /// The account the key signs for.
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// One request to the node may take at most `t`.
    #[cfg(test)]
    pub fn with_request_timeout(mut self, t: Duration) -> Self {
        self.request_timeout = t;
        self
    }
}

#[async_trait]
impl Wallet for LocalKeyWallet {
    fn kind(&self) -> WalletKind {
        WalletKind::LocalKey
    }

    async fn connect(&mut self, _: &dyn Ui) -> Result<Address, WalletError> {
        Ok(self.address())
    }

    async fn personal_sign(
        &mut self,
        account: Address,
        message: &str,
    ) -> Result<Bytes, WalletError> {
        if account != self.address() {
            return Err(WalletError::Unsupported(
                "sign for an account other than its key's",
            ));
        }
        self.signer
            .sign_message_sync(message.as_bytes())
            .map(|s| Bytes::from(s.as_bytes().to_vec()))
            .map_err(|e| WalletError::Other(e.to_string()))
    }

    async fn capabilities(&mut self, _: Address) -> Option<serde_json::Value> {
        None
    }

    /// Signs `tx` as the type-2 transaction it describes, at the account's
    /// pending nonce or after this wallet's last transaction, whichever is
    /// higher, and broadcasts it. Any failure is `Other`, never
    /// `Rejected`: the node may have taken the transaction before the
    /// answer was lost, and the driver searches the chain for it.
    async fn send_transaction(&mut self, _: &dyn Ui, tx: &TxRequest) -> Result<B256, WalletError> {
        if tx.from != self.address() {
            return Err(WalletError::Unsupported(
                "send from an account other than its key's",
            ));
        }
        let url = self
            .rpc_url
            .parse()
            .map_err(|_| WalletError::Other("--rpc-url is not a URL".into()))?;
        let client = alloy::transports::http::reqwest::Client::builder()
            .timeout(self.request_timeout)
            .build()
            .map_err(|e| WalletError::Other(e.to_string()))?;
        let node = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.signer.clone()))
            .connect_reqwest(client, url);
        let pending = node
            .get_transaction_count(tx.from)
            .pending()
            .await
            .map_err(|e| WalletError::Other(e.to_string()))?;
        let nonce = next_nonce(pending, self.next_nonce);
        let req = TransactionRequest::default()
            .with_from(tx.from)
            .with_to(tx.to)
            .with_input(tx.data.clone())
            .with_nonce(nonce)
            .with_gas_limit(tx.gas)
            .with_max_fee_per_gas(tx.fees.max_fee_per_gas)
            .with_max_priority_fee_per_gas(tx.fees.max_priority_fee_per_gas)
            .with_chain_id(self.chain_id);
        let hash = node
            .send_transaction(req)
            .await
            .map(|pending| *pending.tx_hash())
            .map_err(|e| WalletError::Other(e.to_string()))?;
        self.next_nonce = Some(nonce + 1);
        Ok(hash)
    }

    async fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;
    use crate::deposit::{evm::Fees, ui::RecordingUi, wallet::TxPurpose};

    /// Anvil's first account; a published test key.
    const KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const ADDR: Address = alloy_primitives::address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");

    fn wallet(rpc: &str) -> LocalKeyWallet {
        LocalKeyWallet::new(KEY, rpc.into(), 421_614).unwrap()
    }

    fn approve(from: Address) -> TxRequest {
        TxRequest {
            from,
            to: Address::repeat_byte(0x22),
            data: Bytes::new(),
            gas: 50_000,
            fees: Fees {
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 1_000_000_000,
            },
            purpose: TxPurpose::Approve {
                token: Address::repeat_byte(0x22),
                spender: Address::repeat_byte(0x33),
                amount: U256::from(1u64),
            },
        }
    }

    #[test]
    fn a_lagging_node_does_not_take_the_nonce_back() {
        assert_eq!(next_nonce(5, None), 5, "the first transaction");
        assert_eq!(
            next_nonce(5, Some(6)),
            6,
            "the deposit after its approve, from a node that missed the approve"
        );
        assert_eq!(next_nonce(9, Some(6)), 9, "other transactions since");
    }

    #[test]
    fn the_key_names_its_account_and_a_bad_key_is_not_quoted() {
        assert_eq!(wallet("http://rpc.invalid").address(), ADDR);
        assert_eq!(
            LocalKeyWallet::new(&format!("0x{KEY}\n"), String::new(), 1)
                .unwrap()
                .address(),
            ADDR
        );
        let e = LocalKeyWallet::new("0xdeadbeefcafe", String::new(), 1)
            .err()
            .unwrap();
        assert!(!e.contains("deadbeef"), "{e}");
    }

    #[tokio::test]
    async fn the_account_check_signature_recovers_to_the_key() {
        let mut w = wallet("http://rpc.invalid");
        let sig = w.personal_sign(ADDR, "hello").await.unwrap();
        let sig = alloy_primitives::Signature::try_from(sig.as_ref()).unwrap();
        assert_eq!(sig.recover_address_from_msg("hello").unwrap(), ADDR);
        let other = w.personal_sign(Address::repeat_byte(1), "hello").await;
        assert!(
            matches!(other, Err(WalletError::Unsupported(_))),
            "{other:?}"
        );
    }

    #[tokio::test]
    async fn a_node_that_never_answers_is_an_error_but_never_a_rejection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Accepts and then says nothing.
        let _hold = std::thread::spawn(move || {
            let _conns: Vec<_> = listener.incoming().take(8).collect();
        });
        let mut w =
            wallet(&format!("http://{addr}")).with_request_timeout(Duration::from_millis(300));
        let ui = RecordingUi::new(true);
        let got = tokio::time::timeout(
            Duration::from_secs(10),
            w.send_transaction(&ui, &approve(ADDR)),
        )
        .await
        .expect("the wallet must give up on a silent node");
        assert!(matches!(got, Err(WalletError::Other(_))), "{got:?}");
        let foreign = w
            .send_transaction(&ui, &approve(Address::repeat_byte(1)))
            .await;
        assert!(
            matches!(foreign, Err(WalletError::Unsupported(_))),
            "{foreign:?}"
        );
    }
}
