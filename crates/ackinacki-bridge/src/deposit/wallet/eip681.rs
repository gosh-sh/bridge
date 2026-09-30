//! EIP-681: two `ethereum:` URIs, one per transaction, for wallets that
//! cannot pair. No session, no signature, no transaction hash, and no way
//! to ask for a type-2 transaction — so a wallet that sends a legacy one
//! makes the deposit unprovable. The user accepts that risk explicitly.

use alloy_primitives::{Address, Bytes, B256};
use async_trait::async_trait;

use super::{TxPurpose, TxRequest, Wallet, WalletError, WalletKind};
use crate::deposit::ui::Ui;

/// The `ethereum:` URI of the transaction for `p` on `chain_id`.
pub fn uri(chain_id: u64, p: &TxPurpose) -> String {
    match p {
        TxPurpose::Approve {
            token,
            spender,
            amount,
        } => {
            format!("ethereum:{token:#x}@{chain_id}/approve?address={spender:#x}&uint256={amount}")
        },
        TxPurpose::Deposit {
            bridge,
            amount,
            account,
        } => {
            format!(
                "ethereum:{bridge:#x}@{chain_id}/deposit?uint256={amount}&int8=0&bytes32={account:\
                 #x}"
            )
        },
    }
}

/// The fields of `p` in words, shown beside the QR code.
pub fn decoded(p: &TxPurpose) -> Vec<(String, String)> {
    match p {
        TxPurpose::Approve {
            token,
            spender,
            amount,
        } => vec![
            ("contract".into(), format!("{token:#x} (USDC)")),
            ("function".into(), "approve(address,uint256)".into()),
            ("spender".into(), format!("{spender:#x} (the bridge)")),
            ("amount".into(), format!("{amount} units")),
        ],
        TxPurpose::Deposit {
            bridge,
            amount,
            account,
        } => vec![
            ("contract".into(), format!("{bridge:#x} (AckiNackiBridge)")),
            ("function".into(), "deposit(uint256,int8,bytes32)".into()),
            ("amount".into(), format!("{amount} units")),
            ("anWorkchain".into(), "0".into()),
            ("anAccount".into(), format!("{account:#x}")),
        ],
    }
}

/// The wallet mode without a session.
pub struct Eip681Wallet {
    from: Address,
    chain_id: u64,
}

impl Eip681Wallet {
    /// A wallet for the account `from` on `chain_id`.
    pub fn new(from: Address, chain_id: u64) -> Self {
        Self {
            from,
            chain_id,
        }
    }
}

/// What the person must accept before this mode is used.
const RISK: &str = "EIP-681 cannot ask the wallet for a type-2 (EIP-1559) transaction. A wallet \
                    that sends a legacy transaction makes this deposit unprovable: the USDC stays \
                    in the bridge on the EVM side and only its operator can return it. The wallet \
                    will not add the network either, and some wallets drop the call data. \
                    Continue?";

#[async_trait]
impl Wallet for Eip681Wallet {
    fn kind(&self) -> WalletKind {
        WalletKind::Eip681
    }

    async fn connect(&mut self, ui: &dyn Ui) -> Result<Address, WalletError> {
        ui.warn(RISK);
        if !ui.confirm(RISK) {
            return Err(WalletError::Rejected);
        }
        Ok(self.from)
    }

    async fn personal_sign(&mut self, _: Address, _: &str) -> Result<Bytes, WalletError> {
        Err(WalletError::Unsupported("sign a message over EIP-681"))
    }

    async fn capabilities(&mut self, _: Address) -> Option<serde_json::Value> {
        None
    }

    async fn send_transaction(&mut self, ui: &dyn Ui, tx: &TxRequest) -> Result<B256, WalletError> {
        ui.qr(&uri(self.chain_id, &tx.purpose), &decoded(&tx.purpose));
        Err(WalletError::NoHash)
    }

    async fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, B256, U256};

    use super::*;

    #[test]
    fn uris_match_the_documented_shape() {
        let usdc = address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
        let bridge = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
        assert_eq!(
            uri(11_155_111, &TxPurpose::Approve {
                token: usdc,
                spender: bridge,
                amount: U256::from(12_500_000u64)
            }),
            format!("ethereum:{usdc:#x}@11155111/approve?address={bridge:#x}&uint256=12500000")
        );
        let acc = B256::repeat_byte(0xa3);
        assert_eq!(
            uri(11_155_111, &TxPurpose::Deposit {
                bridge,
                amount: 12_500_000,
                account: acc
            }),
            format!(
                "ethereum:{bridge:#x}@11155111/deposit?uint256=12500000&int8=0&bytes32={acc:#x}"
            )
        );
    }

    #[tokio::test]
    async fn an_eip681_send_returns_no_hash_after_showing_the_qr() {
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let mut w = Eip681Wallet::new(from, 11_155_111);
        assert_eq!(w.connect(&ui).await.unwrap(), from);
        let tx = TxRequest {
            from,
            to: address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7"),
            data: Default::default(),
            gas: 1,
            fees: crate::deposit::evm::Fees {
                max_fee_per_gas: 1,
                max_priority_fee_per_gas: 1,
            },
            purpose: TxPurpose::Deposit {
                bridge: Default::default(),
                amount: 1,
                account: Default::default(),
            },
        };
        assert_eq!(w.send_transaction(&ui, &tx).await, Err(WalletError::NoHash));
        assert!(ui
            .events()
            .iter()
            .any(|e| matches!(e, crate::deposit::ui::UiEvent::Qr(_))));
    }

    #[tokio::test]
    async fn eip681_asks_to_accept_the_legacy_transaction_risk() {
        let ui = crate::deposit::ui::RecordingUi::new(false);
        let mut w = Eip681Wallet::new(Default::default(), 11_155_111);
        assert_eq!(w.connect(&ui).await, Err(WalletError::Rejected));
    }
}
