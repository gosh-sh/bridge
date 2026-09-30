//! The EVM wallet: the only party that holds the key. The CLI asks it to
//! sign two transactions and one message; everything else is read from
//! the chain.

pub mod account_check;
pub mod eip681;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use serde_json::json;

use crate::deposit::{
    evm::{approve_calldata, deposit_calldata, EvmRead, Fees},
    ui::Ui,
};

/// How the wallet is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletKind {
    /// A paired WalletConnect session.
    WalletConnect,
    /// Two `ethereum:` URIs shown as QR codes; no session.
    Eip681,
}

/// What a transaction is for, and the values it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxPurpose {
    /// `token.approve(spender, amount)`.
    Approve {
        /// The USDC contract.
        token: Address,
        /// The bridge.
        spender: Address,
        /// The allowance, in token units.
        amount: U256,
    },
    /// `bridge.deposit(amount, 0, account)`.
    Deposit {
        /// The bridge.
        bridge: Address,
        /// The deposit, in token units.
        amount: u64,
        /// The Acki Nacki account that is credited.
        account: B256,
    },
}

/// A transaction the wallet is asked to sign and broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRequest {
    /// The signer.
    pub from: Address,
    /// The contract called.
    pub to: Address,
    /// The call data.
    pub data: Bytes,
    /// The gas limit: the estimate plus a quarter.
    pub gas: u64,
    /// The EIP-1559 fee caps.
    pub fees: Fees,
    /// What the transaction is for.
    pub purpose: TxPurpose,
}

impl TxRequest {
    /// Builds the request for `purpose` from `from`, estimating gas and
    /// fees on the node.
    pub async fn build(
        evm: &dyn EvmRead,
        from: Address,
        purpose: TxPurpose,
    ) -> anyhow::Result<TxRequest> {
        let (to, data) = match &purpose {
            TxPurpose::Approve {
                token,
                spender,
                amount,
            } => (*token, approve_calldata(*spender, *amount)),
            TxPurpose::Deposit {
                bridge,
                amount,
                account,
            } => (*bridge, deposit_calldata(*amount, *account)),
        };
        let estimate = evm.estimate_gas(from, to, data.clone()).await?;
        Ok(TxRequest {
            from,
            to,
            data,
            gas: estimate * 5 / 4,
            fees: evm.fees().await?,
            purpose,
        })
    }

    /// `eth_sendTransaction` params. Type 2 with explicit EIP-1559 fees
    /// and no access list: the only shape the deposit circuit proves. A
    /// wallet may still ignore these fields, which is why the shape is
    /// checked again after the broadcast.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "from": format!("{:#x}", self.from),
            "to": format!("{:#x}", self.to),
            "data": format!("0x{}", hex::encode(&self.data)),
            "gas": format!("{:#x}", self.gas),
            "type": "0x2",
            "maxFeePerGas": format!("{:#x}", self.fees.max_fee_per_gas),
            "maxPriorityFeePerGas": format!("{:#x}", self.fees.max_priority_fee_per_gas),
            "value": "0x0",
        })
    }
}

/// Why a wallet call did not produce what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletError {
    /// The user said no (EIP-1193 4001).
    Rejected,
    /// The wallet did not answer in time.
    Timeout,
    /// The session ended.
    Disconnected(String),
    /// The wallet cannot do this.
    Unsupported(&'static str),
    /// The request was shown, and this wallet mode never returns a hash.
    NoHash,
    /// Anything else the wallet reported.
    Other(String),
}

impl std::fmt::Display for WalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalletError::Rejected => write!(f, "the request was rejected in the wallet"),
            WalletError::Timeout => write!(f, "the wallet did not answer in time"),
            WalletError::Disconnected(why) => write!(f, "the wallet session ended: {why}"),
            WalletError::Unsupported(what) => write!(f, "the wallet cannot {what}"),
            WalletError::NoHash => write!(
                f,
                "the wallet does not report transaction hashes in this mode"
            ),
            WalletError::Other(e) => write!(f, "wallet error: {e}"),
        }
    }
}

impl std::error::Error for WalletError {}

/// The wallet as the driver sees it.
#[async_trait]
pub trait Wallet: Send {
    /// Which mode this is.
    fn kind(&self) -> WalletKind;
    /// Pairs (or, for EIP-681, asks for consent) and returns the account.
    async fn connect(&mut self, ui: &dyn Ui) -> Result<Address, WalletError>;
    /// `personal_sign` of `message` by `account`.
    async fn personal_sign(
        &mut self,
        account: Address,
        message: &str,
    ) -> Result<Bytes, WalletError>;
    /// `wallet_getCapabilities` for `account`; `None` when unavailable.
    async fn capabilities(&mut self, account: Address) -> Option<serde_json::Value>;
    /// Asks the wallet to sign and broadcast `tx`; returns its hash.
    async fn send_transaction(&mut self, ui: &dyn Ui, tx: &TxRequest) -> Result<B256, WalletError>;
    /// Ends the session.
    async fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, B256};

    use super::*;
    use crate::deposit::testkit::FakeEvm;

    #[tokio::test]
    async fn the_deposit_request_is_type_2_without_access_list_or_gas_price() {
        let evm = FakeEvm::sepolia();
        let bridge = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
        let tx = TxRequest::build(
            &evm,
            address!("b586356d52eaee055ca569ff412dfeffc5bb2307"),
            TxPurpose::Deposit {
                bridge,
                amount: 12_500_000,
                account: B256::repeat_byte(0xa3),
            },
        )
        .await
        .unwrap();
        assert_eq!(tx.gas, 100_000, "estimate 80 000 × 1.25");
        let j = tx.to_json();
        assert_eq!(j["type"], "0x2");
        assert_eq!(j["value"], "0x0");
        assert!(j.get("gasPrice").is_none());
        assert!(j.get("accessList").is_none());
        assert_eq!(j["to"], format!("{bridge:#x}"));
        assert_eq!(
            j["data"].as_str().unwrap().len(),
            2 + 200,
            "100 bytes of calldata"
        );
    }
}
