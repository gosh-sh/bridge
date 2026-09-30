//! Steps 3 and 4 on the EVM side: the allowance, and the deposit request.

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};

use crate::{
    deposit::{
        evm::{read_allowance, EvmRead},
        retry::transient,
        ui::Ui,
        wallet::{TxPurpose, TxRequest, Wallet, WalletError},
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// What the approve step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// The allowance already covered the deposit.
    Skipped,
    /// The allowance now covers the deposit; `tx` is the last approve the
    /// wallet reported, `None` when it reports no hashes.
    Approved {
        /// The approve transaction, when the wallet returned its hash.
        tx: Option<B256>,
    },
}

/// The exit-21 error of this step.
fn approve_failed(op_id: &str, why: String) -> CliError {
    CliError::deposit(
        ExitCode::ApproveFailed,
        Stage::Approve,
        Some(op_id),
        format!("{why}; no USDC moved"),
    )
}

/// Asks the wallet for one `approve(bridge, amount)` and waits until it is
/// mined (a hash) or its effect is visible in the allowance (no hash).
#[allow(clippy::too_many_arguments)]
async fn send_approve(
    evm: &dyn EvmRead,
    wallet: &mut dyn Wallet,
    ui: &dyn Ui,
    usdc: Address,
    bridge: Address,
    from: Address,
    amount: U256,
    op_id: &str,
    wait_limit: Duration,
    poll: Duration,
) -> CliResult<Option<B256>> {
    let purpose = TxPurpose::Approve {
        token: usdc,
        spender: bridge,
        amount,
    };
    let req = transient(ui, "estimating approve", || {
        TxRequest::build(evm, from, purpose.clone())
    })
    .await;
    match wallet.send_transaction(ui, &req).await {
        Ok(h) => {
            // One confirmation: an approve that is reorged out and replayed moves no money.
            let started = tokio::time::Instant::now();
            loop {
                if let Some(r) =
                    transient(ui, "reading the approve receipt", || evm.receipt(h)).await
                {
                    if !r.status {
                        return Err(approve_failed(op_id, format!("approve {h} reverted")));
                    }
                    return Ok(Some(h));
                }
                if started.elapsed() > wait_limit {
                    return Err(approve_failed(
                        op_id,
                        format!("approve {h} was not mined in time"),
                    ));
                }
                ui.status("waiting for the approve to be mined");
                tokio::time::sleep(poll).await;
            }
        },
        Err(WalletError::NoHash) => {
            let started = tokio::time::Instant::now();
            loop {
                let a = transient(ui, "reading the allowance", || {
                    read_allowance(evm, usdc, from, bridge)
                })
                .await;
                // A reset must land on exactly zero; a real approve may be
                // raised in the wallet, and any limit that covers it will do.
                let landed = if amount.is_zero() {
                    a.is_zero()
                } else {
                    a >= amount
                };
                if landed {
                    return Ok(None);
                }
                if started.elapsed() > wait_limit {
                    return Err(approve_failed(
                        op_id,
                        "the approve from the QR code was not seen on chain in time".into(),
                    ));
                }
                ui.status("waiting for the approve from the QR code");
                tokio::time::sleep(poll).await;
            }
        },
        Err(WalletError::Rejected) => Err(approve_failed(
            op_id,
            "approve was rejected in the wallet".into(),
        )),
        Err(e) => Err(approve_failed(op_id, format!("approve failed: {e}"))),
    }
}

/// Makes sure the bridge may pull `amount` of USDC from `from`: nothing to
/// do when it already may, otherwise an approve (after a reset to zero when
/// a smaller allowance is set), then a re-read, because the wallet lets the
/// user edit the limit.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_allowance(
    evm: &dyn EvmRead,
    wallet: &mut dyn Wallet,
    ui: &dyn Ui,
    usdc: Address,
    bridge: Address,
    from: Address,
    amount: u64,
    op_id: &str,
    wait_limit: Duration,
    poll: Duration,
) -> CliResult<ApproveOutcome> {
    let need = U256::from(amount);
    let have = transient(ui, "reading the allowance", || {
        read_allowance(evm, usdc, from, bridge)
    })
    .await;
    if have >= need {
        return Ok(ApproveOutcome::Skipped);
    }
    if have > U256::ZERO {
        // Some tokens refuse to change a non-zero allowance to another non-zero one.
        send_approve(
            evm,
            wallet,
            ui,
            usdc,
            bridge,
            from,
            U256::ZERO,
            op_id,
            wait_limit,
            poll,
        )
        .await?;
    }
    let tx = send_approve(
        evm, wallet, ui, usdc, bridge, from, need, op_id, wait_limit, poll,
    )
    .await?;
    // The wallet lets the user edit the spending limit, including down.
    let after = transient(ui, "reading the allowance", || {
        read_allowance(evm, usdc, from, bridge)
    })
    .await;
    if after < need {
        return Err(approve_failed(
            op_id,
            format!("the wallet set the spending limit to {after} units, the deposit needs {need}"),
        ));
    }
    Ok(ApproveOutcome::Approved {
        tx,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy::sol_types::{SolCall, SolValue};
    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::*;
    use crate::{
        deposit::{evm::IErc20, testkit::*, ui::RecordingUi, wallet::TxPurpose},
        errors::ExitCode,
    };

    const USDC: Address = address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");

    fn allowance_seq(evm: &FakeEvm, seq: &[u64]) {
        evm.script_call(
            USDC,
            IErc20::allowanceCall::SELECTOR,
            seq.iter()
                .map(|v| Bytes::from(U256::from(*v).abi_encode()))
                .collect(),
        );
    }

    fn mined_ok(evm: &FakeEvm, h: B256) {
        evm.script_receipt(h, vec![Some(deposit_receipt(
            h,
            &header(10, 1),
            true,
            vec![],
        ))]);
    }

    async fn go(evm: &FakeEvm, w: &mut FakeWallet) -> CliResult<ApproveOutcome> {
        let ui = RecordingUi::new(true);
        let from = w.account;
        ensure_allowance(
            evm,
            w,
            &ui,
            USDC,
            BRIDGE,
            from,
            12_500_000,
            "OP",
            Duration::from_secs(60),
            Duration::from_secs(1),
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn enough_allowance_skips_the_step() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[20_000_000]);
        let mut w = FakeWallet::eoa();
        assert_eq!(go(&evm, &mut w).await.unwrap(), ApproveOutcome::Skipped);
        assert!(w.sent.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_wallet_that_lowered_the_limit_stops_before_the_deposit() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0, 1_000_000]);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        mined_ok(&evm, B256::repeat_byte(1));
        let e = go(&evm, &mut w).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed);
        assert!(
            e.to_string().contains("1000000") && e.to_string().contains("12500000"),
            "{e}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_partial_allowance_is_reset_to_zero_first() {
        let evm = FakeEvm::sepolia();
        // Two reads: the first one, and the check after the second approve.
        // Both approves return a hash, so nothing reads the allowance in between.
        allowance_seq(&evm, &[5, 12_500_000]);
        let mut w = FakeWallet::eoa();
        for b in [1u8, 2] {
            w.send_results.push_back(Ok(B256::repeat_byte(b)));
            mined_ok(&evm, B256::repeat_byte(b));
        }
        assert!(matches!(
            go(&evm, &mut w).await.unwrap(),
            ApproveOutcome::Approved { .. }
        ));
        let amounts: Vec<U256> = w
            .sent
            .iter()
            .map(|t| match &t.purpose {
                TxPurpose::Approve {
                    amount, ..
                } => *amount,
                _ => panic!(),
            })
            .collect();
        assert_eq!(amounts, vec![U256::ZERO, U256::from(12_500_000u64)]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_eip681_approve_raised_in_the_wallet_is_accepted() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0, 0, 12_500_005]);
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        assert!(matches!(
            go(&evm, &mut w).await.unwrap(),
            ApproveOutcome::Approved {
                tx: None
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reverted_or_rejected_approve_is_exit_21() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        evm.script_receipt(B256::repeat_byte(1), vec![Some(deposit_receipt(
            B256::repeat_byte(1),
            &header(10, 1),
            false,
            vec![],
        ))]);
        assert_eq!(
            go(&evm, &mut w).await.unwrap_err().exit_code(),
            ExitCode::ApproveFailed
        );
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::Rejected));
        assert_eq!(
            go(&evm, &mut w).await.unwrap_err().exit_code(),
            ExitCode::ApproveFailed
        );
    }
}
