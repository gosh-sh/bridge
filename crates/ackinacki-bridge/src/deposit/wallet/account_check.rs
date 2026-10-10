//! Step 2: is the account one whose deposit can be proven?
//!
//! The circuit proves a transaction signed by the account's own key and
//! sent straight to the bridge. A smart-contract account (Safe, ERC-4337)
//! sends through its contract or an EntryPoint with calldata beyond the
//! circuit's limit, and its USDC would be stuck. Code on the account
//! catches the deployed ones; a `personal_sign` whose signer is not the
//! account catches the rest, including an ERC-4337 account that is not
//! deployed yet (it signs with ERC-6492, which recovers to nothing).

use alloy_primitives::{Address, Signature};

use crate::{
    args::UsdcAmount,
    deposit::{
        args::Network,
        evm::EvmRead,
        ui::Ui,
        wallet::{Wallet, WalletError, WalletKind},
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// What sits at the account's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeKind {
    /// No code: a plain externally owned account.
    Eoa,
    /// An EIP-7702 delegation designator pointing at `target`.
    Delegated7702 {
        /// The contract the account delegates to.
        target: Address,
    },
    /// Any other code: a smart-contract account.
    Contract {
        /// The code length in bytes.
        len: usize,
    },
}

/// Sorts the code at an address into [`CodeKind`].
pub fn classify_code(code: &[u8]) -> CodeKind {
    match code {
        [] => CodeKind::Eoa,
        [0xef, 0x01, 0x00, rest @ ..] if rest.len() == 20 => CodeKind::Delegated7702 {
            target: Address::from_slice(rest),
        },
        _ => CodeKind::Contract {
            len: code.len(),
        },
    }
}

/// The text the wallet is asked to sign.
pub fn sign_message(op_id: &str, bridge: Address, amount: &UsdcAmount, net: Network) -> String {
    format!(
        "Acki Nacki bridge deposit check\noperation: {op_id}\nbridge: {bridge:#x}\namount: {} \
         USDC\nnetwork: {} ({})\n\nSigning this sends nothing and costs nothing.",
        amount.display(),
        net.name(),
        net.chain_id()
    )
}

/// Accepts `signature` only if it is a 65-byte signature of `message`
/// by `account`'s own key.
pub fn signer_is(message: &str, signature: &[u8], account: Address) -> Result<(), String> {
    if signature.len() != 65 {
        return Err(format!(
            "the wallet signed with a {}-byte signature, not the account's own key (a \
             smart-contract or not-yet-deployed ERC-4337 account signs this way)",
            signature.len()
        ));
    }
    let sig =
        Signature::try_from(signature).map_err(|e| format!("the signature does not parse: {e}"))?;
    let signer = sig
        .recover_address_from_msg(message)
        .map_err(|e| format!("the signature does not recover: {e}"))?;
    if signer != account {
        return Err(format!(
            "the message was signed by {signer}, not by the account {account}"
        ));
    }
    Ok(())
}

/// Warnings for wallet capabilities that would route the deposit through
/// another contract. They never stop the run.
pub fn capability_warnings(caps: &serde_json::Value, chain_id: u64) -> Vec<String> {
    let chain = format!("{chain_id:#x}");
    let Some(c) = caps.get(&chain) else {
        return vec![];
    };
    let mut out = Vec::new();
    if c.pointer("/paymasterService/supported")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        out.push(
            "the wallet can pay gas through a paymaster; keep it off for this deposit: a \
             sponsored transaction is sent through another contract and cannot be proven"
                .into(),
        );
    }
    if c.pointer("/atomic/status")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s != "unsupported")
    {
        out.push(
            "the wallet can batch calls; send the deposit as a single plain transaction: a \
             batched one cannot be proven"
                .into(),
        );
    }
    out
}

/// The refusal error: nothing has been sent yet.
fn refuse(op_id: &str, reason: String) -> CliError {
    CliError::deposit(
        ExitCode::WalletFailed,
        Stage::Wallet,
        Some(op_id),
        format!("{reason} (nothing was sent)"),
    )
}

/// Refuses an account whose deposit could not be proven: code at the
/// address, or a `personal_sign` that does not come from its own key.
/// Capabilities that hint at sponsoring or batching only warn.
#[allow(clippy::too_many_arguments)]
pub async fn check(
    evm: &dyn EvmRead,
    wallet: &mut dyn Wallet,
    ui: &dyn Ui,
    account: Address,
    op_id: &str,
    bridge: Address,
    amount: &UsdcAmount,
    net: Network,
) -> CliResult<()> {
    let code =
        crate::deposit::retry::transient(ui, "reading the account's code", || evm.code(account))
            .await;
    match classify_code(&code) {
        CodeKind::Contract {
            ..
        } => {
            return Err(refuse(
                op_id,
                "deposits from a smart-contract account (Safe, ERC-4337) cannot be proven: their \
                 transactions exceed the circuit's calldata limit"
                    .into(),
            ))
        },
        CodeKind::Delegated7702 {
            target,
        } => ui.warn(&format!(
            "this account delegates to {target} (EIP-7702). Turn off gas sponsoring and batched \
             calls for this deposit: through a sponsor or a batch the transaction is sent via a \
             contract and cannot be proven"
        )),
        CodeKind::Eoa => {},
    }
    if wallet.kind() == WalletKind::WalletConnect {
        let msg = sign_message(op_id, bridge, amount, net);
        let sig = match wallet.personal_sign(account, &msg).await {
            Ok(s) => s,
            Err(WalletError::Rejected) => {
                return Err(refuse(
                    op_id,
                    "the account check signature was rejected".into(),
                ))
            },
            Err(e) => {
                return Err(refuse(
                    op_id,
                    format!("the account check signature failed: {e}"),
                ))
            },
        };
        signer_is(&msg, &sig, account).map_err(|e| refuse(op_id, e))?;
        if let Some(caps) = wallet.capabilities(account).await {
            for w in capability_warnings(&caps, net.chain_id()) {
                ui.warn(&w);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Bytes};
    use serde_json::json;

    use super::*;
    use crate::{
        args::UsdcAmount,
        deposit::{args::Network, testkit::*, ui::RecordingUi},
        errors::ExitCode,
    };

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");

    fn delegation(target: Address) -> Vec<u8> {
        let mut v = vec![0xef, 0x01, 0x00];
        v.extend_from_slice(target.as_slice());
        v
    }

    async fn run_check(evm: &FakeEvm, w: &mut FakeWallet) -> (CliResult<()>, RecordingUi) {
        let ui = RecordingUi::new(true);
        let account = w.account;
        let r = check(
            evm,
            w,
            &ui,
            account,
            "01J9ZQ4X7T8V5N6M3K2P1R0S9A",
            BRIDGE,
            &UsdcAmount(12_500_000),
            Network::Sepolia,
        )
        .await;
        (r, ui)
    }

    #[test]
    fn code_classes() {
        assert_eq!(classify_code(&[]), CodeKind::Eoa);
        let t = address!("63c0c19a282a1b52b07dd5a65b58948a07dae32b");
        assert_eq!(classify_code(&delegation(t)), CodeKind::Delegated7702 {
            target: t
        });
        let mut other23 = delegation(t);
        other23[2] = 0x01;
        assert_eq!(classify_code(&other23), CodeKind::Contract {
            len: 23
        });
        assert!(matches!(
            classify_code(&[0x60, 0x80, 0x60, 0x40]),
            CodeKind::Contract { .. }
        ));
    }

    #[tokio::test]
    async fn a_safe_is_refused_before_anything_is_asked() {
        let evm = FakeEvm::sepolia();
        let mut w = FakeWallet::eoa();
        evm.codes
            .lock()
            .unwrap()
            .insert(w.account, Bytes::from(vec![0x60; 170]));
        let (r, _) = run_check(&evm, &mut w).await;
        let e = r.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::WalletFailed);
        assert!(e.to_string().contains("smart-contract account"));
    }

    #[tokio::test]
    async fn an_undeployed_4337_account_is_caught_by_its_signature() {
        let evm = FakeEvm::sepolia();
        let mut w = FakeWallet::eoa();
        // ERC-6492 wrapped signature: longer than 65 bytes, ends in the magic suffix.
        let mut sig = vec![0u8; 200];
        sig.extend_from_slice(&[0x64, 0x92].repeat(16));
        w.sign_override = Some(Bytes::from(sig));
        let (r, _) = run_check(&evm, &mut w).await;
        assert_eq!(r.unwrap_err().exit_code(), ExitCode::WalletFailed);
    }

    #[tokio::test]
    async fn an_eoa_signing_with_its_key_passes() {
        let evm = FakeEvm::sepolia();
        let mut w = FakeWallet::eoa();
        let (r, ui) = run_check(&evm, &mut w).await;
        r.unwrap();
        assert!(ui.warnings().is_empty());
    }

    #[tokio::test]
    async fn a_7702_delegated_eoa_passes_with_a_warning() {
        let evm = FakeEvm::sepolia();
        let mut w = FakeWallet::eoa();
        evm.codes.lock().unwrap().insert(
            w.account,
            Bytes::from(delegation(address!(
                "63c0c19a282a1b52b07dd5a65b58948a07dae32b"
            ))),
        );
        let (r, ui) = run_check(&evm, &mut w).await;
        r.unwrap();
        assert!(
            ui.warnings().iter().any(|s| s.contains("sponsor")),
            "{:?}",
            ui.warnings()
        );
    }

    #[tokio::test]
    async fn a_signature_by_another_key_is_refused() {
        let evm = FakeEvm::sepolia();
        let mut w = FakeWallet::eoa();
        w.account = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let (r, _) = run_check(&evm, &mut w).await;
        assert_eq!(r.unwrap_err().exit_code(), ExitCode::WalletFailed);
    }

    #[test]
    fn capabilities_only_warn() {
        let caps = json!({ "0xaa36a7": { "paymasterService": { "supported": true }, "atomic": { "status": "supported" } } });
        let w = capability_warnings(&caps, 11_155_111);
        assert_eq!(w.len(), 2);
    }
}
