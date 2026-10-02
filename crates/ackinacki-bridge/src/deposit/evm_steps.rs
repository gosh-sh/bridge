//! Steps 3 and 4 on the EVM side: the allowance, and the deposit request.

use std::{future::Future, time::Duration};

use alloy_primitives::{Address, B256, U256};
use tokio::time::Instant;

use crate::{
    deposit::{
        evm::{read_allowance, revert_of, BlockTag, EvmRead},
        retry::{deadline_after, transient, until, until_with},
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
        /// The block the last approve is in, as this run saw it land: its
        /// receipt's block, or the head at which the allowance read showed
        /// it. A node behind that block answers as if the approve had not
        /// been made, so what depends on it is read there.
        block: u64,
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

/// Builds the request for `purpose`, its gas estimated at `at`, retrying
/// what a retry can fix. A node that says the call would revert is an
/// answer, not a failure: the reason comes back as `Err`, and nothing is
/// retried.
async fn build_or_revert(
    evm: &dyn EvmRead,
    ui: &dyn Ui,
    what: &str,
    from: Address,
    purpose: TxPurpose,
    at: BlockTag,
) -> Result<TxRequest, String> {
    transient(ui, what, || async {
        match TxRequest::build(evm, from, purpose.clone(), at).await {
            Ok(r) => Ok(Ok(r)),
            Err(e) => match revert_of(&e) {
                Some(reason) => Ok(Err(reason)),
                None => Err(e),
            },
        }
    })
    .await
}

/// The text of the last failed read of an approve wait, when there is one.
type LastError = Option<String>;

/// One read of an approve wait, retried until `deadline`; `Err` when the
/// deadline came first, with the last error, or `None` when the node never
/// answered.
async fn read_by<T, F, Fut>(
    ui: &dyn Ui,
    what: &str,
    deadline: Instant,
    f: F,
) -> Result<T, LastError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    until_with(Some(deadline), f, |attempt, e| {
        ui.retry(what, attempt, &format!("{e:#}"))
    })
    .await
    .map_err(|e| e.map(|e| format!("{e:#}")))
}

/// Reads `f` every `poll` until `done` takes its value, all before
/// `deadline`: the reads, their retries and the pauses, none of them past
/// it. `Err(None)` when the deadline came with the reads working,
/// `Err(Some(error))` when it came while they failed.
async fn poll_by<T, F, Fut>(
    ui: &dyn Ui,
    what: &str,
    waiting: &str,
    deadline: Instant,
    poll: Duration,
    mut f: F,
    done: impl Fn(&T) -> bool,
) -> Result<T, LastError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    loop {
        let v = read_by(ui, what, deadline, &mut f)
            .await
            .map_err(|e| Some(e.unwrap_or_else(|| "the node did not answer".into())))?;
        if done(&v) {
            return Ok(v);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(None);
        }
        ui.status(waiting);
        tokio::time::sleep_until((now + poll).min(deadline)).await;
        if Instant::now() >= deadline {
            return Err(None);
        }
    }
}

/// The exit-21 error of an approve wait that reached its deadline: `what`
/// "was not mined in time" (or as `unseen` says) while the reads worked,
/// "could not be confirmed in time" with the last error while they failed.
fn out_of_time(op_id: &str, what: &str, unseen: &str, last: LastError) -> CliError {
    approve_failed(op_id, match last {
        None => format!("{what} {unseen}"),
        Some(e) => format!("{what} could not be confirmed in time: {e}"),
    })
}

/// Asks the wallet for one `approve(bridge, amount)` and waits until it is
/// mined (a hash) or its effect is visible in the allowance (no hash). The
/// wait has `wait_limit` from the wallet's answer — the hash, or the QR
/// code shown — not from the request: the time the user takes to confirm
/// is not in it. Every read, retry and pause is inside it, and so is
/// whatever the caller reads next with the deadline returned. Returns the
/// hash, that deadline and the block the approve is in: its receipt's, or
/// the head at which the allowance read showed it.
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
) -> CliResult<(Option<B256>, Instant, u64)> {
    let purpose = TxPurpose::Approve {
        token: usdc,
        spender: bridge,
        amount,
    };
    let req = match build_or_revert(
        evm,
        ui,
        "estimating approve",
        from,
        purpose,
        BlockTag::Latest,
    )
    .await
    {
        Ok(r) => r,
        Err(reason) => {
            return Err(approve_failed(
                op_id,
                format!("approve would revert: {reason}"),
            ))
        },
    };
    let sent = wallet.send_transaction(ui, &req).await;
    let deadline = deadline_after(wait_limit);
    match sent {
        Ok(h) => {
            // One confirmation: an approve that is reorged out and replayed moves no money.
            let r = poll_by(
                ui,
                "reading the approve receipt",
                "waiting for the approve to be mined",
                deadline,
                poll,
                || evm.receipt(h),
                Option::is_some,
            )
            .await
            .map_err(|last| {
                out_of_time(
                    op_id,
                    &format!("approve {h}"),
                    "was not mined in time",
                    last,
                )
            })?;
            let r = r.expect("the wait ends once the receipt is there");
            if !r.status {
                return Err(approve_failed(op_id, format!("approve {h} reverted")));
            }
            Ok((Some(h), deadline, r.block_number))
        },
        Err(WalletError::NoHash) => {
            // A reset must land on exactly zero; a real approve may be
            // raised in the wallet, and any limit that covers it will do.
            let landed = |a: &U256| {
                if amount.is_zero() {
                    a.is_zero()
                } else {
                    *a >= amount
                }
            };
            // Each poll reads at the head the node reports, so the block
            // in which the allowance is first seen is known: the reads
            // that follow are made there.
            let (block, _) = poll_by(
                ui,
                "reading the allowance",
                "waiting for the approve from the QR code",
                deadline,
                poll,
                || async {
                    let head = evm
                        .header(BlockTag::Latest)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("the node has no latest block"))?
                        .number;
                    let a = read_allowance(evm, usdc, from, bridge, BlockTag::Number(head)).await?;
                    Ok((head, a))
                },
                |(_, a)| landed(a),
            )
            .await
            .map_err(|last| {
                out_of_time(
                    op_id,
                    "the approve from the QR code",
                    "was not seen on chain in time",
                    last,
                )
            })?;
            Ok((None, deadline, block))
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
/// user edit the limit. Each approve's wait, the re-read after the last one
/// included, ends at `wait_limit` with exit 21. The re-read is made at the
/// block the last approve is in: a node that does not have that block yet
/// answers with an error, which is read again, rather than with the
/// allowance before the approve.
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
        read_allowance(evm, usdc, from, bridge, BlockTag::Latest)
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
    let (tx, deadline, block) = send_approve(
        evm, wallet, ui, usdc, bridge, from, need, op_id, wait_limit, poll,
    )
    .await?;
    // The wallet lets the user edit the spending limit, including down.
    let after = read_by(ui, "reading the allowance", deadline, || {
        read_allowance(evm, usdc, from, bridge, BlockTag::Number(block))
    })
    .await
    .map_err(|last| {
        let last = last
            .map(|e| format!(" (last error: {e})"))
            .unwrap_or_default();
        approve_failed(
            op_id,
            format!(
                "the approve could not be confirmed: the allowance could not be read back within \
                 --pair-timeout-s{last}"
            ),
        )
    })?;
    if after < need {
        return Err(approve_failed(
            op_id,
            format!("the wallet set the spending limit to {after} units, the deposit needs {need}"),
        ));
    }
    Ok(ApproveOutcome::Approved {
        tx,
        block,
    })
}

/// What the deposit request step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    /// The wallet returned the hash and the chain shows the transaction.
    Signed(crate::deposit::store::TxClaim),
    /// The outcome is unknown; the driver searches the chain for it.
    Search {
        /// What is left of `--recovery-window-s` for the search. The window
        /// starts with the wallet's answer.
        window: Duration,
    },
}

/// Closes the operation as refused before anything was requested and
/// returns the exit-2 error. A record that cannot be written is reported in
/// the same error: nothing was sent either way.
fn refuse_before_request(
    store: &crate::deposit::store::Store,
    rec: &mut crate::deposit::store::OpRecord,
    why: String,
) -> CliError {
    use crate::deposit::store::FailReason;
    let at = rec.stage;
    rec.fail(
        at,
        FailReason::Refused,
        ExitCode::PreflightRefused,
        why.clone(),
    );
    let reason = match store.write(rec) {
        Ok(()) => format!("{why}; nothing was sent"),
        Err(e) => format!("{why}; nothing was sent (the record could not be updated: {e})"),
    };
    CliError::Preflight {
        reason,
        source: None,
    }
}

/// Builds the deposit request, estimating gas at `at` — the block of the
/// approve this run sent, the head otherwise — and fees. A deposit the node
/// says would revert there is refused before the wallet is asked (exit 2,
/// the operation closed as refused); any other estimate failure is
/// retried.
#[allow(clippy::too_many_arguments)]
pub async fn build_deposit_request(
    evm: &dyn EvmRead,
    ui: &dyn Ui,
    store: &crate::deposit::store::Store,
    rec: &mut crate::deposit::store::OpRecord,
    from: Address,
    bridge: Address,
    amount: u64,
    account: B256,
    at: BlockTag,
) -> CliResult<TxRequest> {
    let purpose = TxPurpose::Deposit {
        bridge,
        amount,
        account,
    };
    build_or_revert(evm, ui, "estimating the deposit", from, purpose, at)
        .await
        .map_err(|reason| {
            refuse_before_request(store, rec, format!("deposit() would revert: {reason}"))
        })
}

/// Step 4: asks the wallet for the deposit. `Requested` reaches the disk
/// before the wallet is asked, so a crash in between is found by the
/// search, and never repeated blindly. `window` is `--recovery-window-s`:
/// a transaction the node does not show before it ends is left to the
/// search, which starts from the hash the wallet returned. `approved_at`
/// is the block of the approve this run sent, if it sent one: the nonce
/// the deposit is expected at counts that approve even on a node that
/// has not seen it.
#[allow(clippy::too_many_arguments)]
pub async fn request_deposit(
    evm: &dyn EvmRead,
    wallet: &mut dyn Wallet,
    ui: &dyn Ui,
    store: &crate::deposit::store::Store,
    rec: &mut crate::deposit::store::OpRecord,
    tx: &TxRequest,
    window: Duration,
    approved_at: Option<u64>,
) -> CliResult<RequestOutcome> {
    use crate::deposit::store::{FailReason, OpStage, RequestInfo, TxClaim};
    // The owner may have paused the bridge since the preflight; deposit()
    // would revert and the user would pay gas for it.
    let paused = transient(ui, "reading the bridge pause flag", || {
        evm.bridge_paused(tx.to)
    })
    .await;
    if paused == Some(true) {
        return Err(refuse_before_request(
            store,
            rec,
            "the EVM bridge is paused by its owner: deposit() would revert".into(),
        ));
    }
    let from = tx.from;
    let pending = transient(ui, "reading the account nonce", || {
        evm.tx_count(from, BlockTag::Pending)
    })
    .await;
    let nonce_before = match approved_at {
        None => pending,
        Some(b) => pending.max(
            transient(ui, "reading the account nonce at the approve", || {
                evm.tx_count(from, BlockTag::Number(b))
            })
            .await,
        ),
    };
    // Where a search for the transaction starts. A reorg onto a shorter
    // branch can put it below the head seen now, never below the finalized
    // block: a transaction sent after this read cannot land there.
    let from_block = transient(ui, "reading the finalized block", || async {
        evm.header(BlockTag::Finalized)
            .await?
            .map(|h| h.number)
            .ok_or_else(|| anyhow::anyhow!("the node has no finalized block"))
    })
    .await;
    rec.stage = OpStage::Requested;
    rec.request = Some(RequestInfo {
        nonce_before,
        from_block,
        calldata: tx.data.clone(),
        wallet_hash: None,
    });
    store.write(rec).map_err(|e| CliError::Preflight {
        reason: format!(
            "cannot record operation {} before asking the wallet: {e} (nothing was sent)",
            rec.op_id
        ),
        source: None,
    })?;
    let op = rec.op_id.clone();
    let post_send = |e: std::io::Error| {
        CliError::deposit(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            Some(&op),
            format!(
                "the deposit was requested but its state could not be written: {e}; run --resume \
                 {op}"
            ),
        )
    };
    let sent = wallet.send_transaction(ui, tx).await;
    let answered = tokio::time::Instant::now();
    let deadline = answered.checked_add(window);
    let left = || {
        deadline.map_or(window, |d| {
            d.saturating_duration_since(tokio::time::Instant::now())
        })
    };
    match sent {
        Ok(h) => {
            if let Some(q) = rec.request.as_mut() {
                q.wallet_hash = Some(h);
            }
            store.write(rec).map_err(post_send)?;
            // The wallet may have broadcast it elsewhere, dropped or
            // replaced it: the node may never show it.
            let Some(seen) = until(ui, "reading the deposit transaction", deadline, || async {
                evm.transaction(h)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("{h} is not visible yet"))
            })
            .await
            else {
                ui.warn(&format!(
                    "the node does not show the transaction {h:#x} the wallet returned; searching \
                     the chain for it"
                ));
                return Ok(RequestOutcome::Search {
                    window: left(),
                });
            };
            let claim = TxClaim {
                tx_hash: h,
                tx_nonce: seen.nonce,
            };
            rec.stage = OpStage::Signed;
            rec.tx = Some(claim);
            store.write(rec).map_err(post_send)?;
            Ok(RequestOutcome::Signed(claim))
        },
        Err(WalletError::Rejected) => {
            let why = "the deposit was rejected in the wallet; nothing was sent";
            rec.fail(
                OpStage::Requested,
                FailReason::Rejected,
                ExitCode::WalletFailed,
                why,
            );
            store.write(rec).map_err(post_send)?;
            Err(CliError::deposit(
                ExitCode::WalletFailed,
                Stage::Deposit,
                Some(&op),
                why,
            ))
        },
        Err(WalletError::NoHash)
        | Err(WalletError::Disconnected(_))
        | Err(WalletError::Timeout)
        | Err(WalletError::Other(_)) => {
            ui.status("the wallet did not return a transaction hash; searching the chain for it");
            Ok(RequestOutcome::Search {
                window: left(),
            })
        },
        Err(WalletError::Unsupported(what)) => Err(CliError::deposit(
            ExitCode::DepositOutcomeUnknown,
            Stage::Deposit,
            Some(&op),
            format!("the wallet cannot {what}; run --resume {op} once the transaction is sent"),
        )),
    }
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
    /// `--recovery-window-s`.
    const WINDOW: Duration = Duration::from_secs(60);

    fn allowance_seq(evm: &FakeEvm, seq: &[u64]) {
        evm.script_call(
            USDC,
            IErc20::allowanceCall::SELECTOR,
            seq.iter()
                .map(|v| Bytes::from(U256::from(*v).abi_encode()))
                .collect(),
        );
    }

    /// What a node behind the approve answers for the allowance at its
    /// head; reads pinned to a block answer from [`allowance_seq`].
    fn stale_allowance_seq(evm: &FakeEvm, seq: &[u64]) {
        evm.stale_calls.lock().unwrap().insert(
            (USDC, IErc20::allowanceCall::SELECTOR),
            Script::new(
                seq.iter()
                    .map(|v| Bytes::from(U256::from(*v).abi_encode()))
                    .collect::<Vec<_>>(),
            ),
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
        evm.latest.set([header(10, 1)]);
        allowance_seq(&evm, &[0, 0, 12_500_005]);
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        assert!(matches!(
            go(&evm, &mut w).await.unwrap(),
            ApproveOutcome::Approved {
                tx: None,
                block: 10
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

    // ---- reads after the approve, on a node behind the one that saw it ----

    #[tokio::test(start_paused = true)]
    async fn the_allowance_is_read_back_at_the_block_the_approve_landed_in() {
        let evm = FakeEvm::sepolia();
        // The chain: approved. A node behind the receipt's: not yet.
        allowance_seq(&evm, &[12_500_000]);
        stale_allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        let h = B256::repeat_byte(1);
        w.send_results.push_back(Ok(h));
        mined_ok(&evm, h); // block 10
        let out = go(&evm, &mut w).await.unwrap();
        assert!(
            matches!(out, ApproveOutcome::Approved { tx: Some(t), block: 10 } if t == h),
            "{out:?}"
        );
        assert_eq!(*evm.pinned_reads.lock().unwrap(), vec![10]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_limit_lowered_in_the_wallet_is_still_exit_21_at_the_approves_block() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[1_000_000]);
        stale_allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        mined_ok(&evm, B256::repeat_byte(1));
        let e = go(&evm, &mut w).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed);
        assert!(
            e.to_string()
                .contains("the wallet set the spending limit to 1000000 units"),
            "{e}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_eip681_approve_is_read_at_the_head_each_poll_saw() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(20, 1), header(21, 2)]);
        // Not there at block 20, there at 21; the node's head reads lag.
        allowance_seq(&evm, &[0, 12_500_005]);
        stale_allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        let out = go(&evm, &mut w).await.unwrap();
        assert!(
            matches!(out, ApproveOutcome::Approved { tx: None, block: 21 }),
            "{out:?}"
        );
        // Two polls, then the read-back at the block where it landed.
        assert_eq!(*evm.pinned_reads.lock().unwrap(), vec![20, 21, 21]);
    }

    // ---- the approve wait under an RPC that keeps failing ----

    /// `go` within an hour of the paused clock, and how long it took.
    async fn timed(evm: &FakeEvm, w: &mut FakeWallet) -> (CliResult<ApproveOutcome>, Duration) {
        let t0 = tokio::time::Instant::now();
        let r = tokio::time::timeout(Duration::from_secs(3600), go(evm, w))
            .await
            .expect("--pair-timeout-s must end the approve wait");
        (r, t0.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn an_eip681_approve_whose_allowance_cannot_be_read_ends_in_time() {
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(10, 1)]);
        allowance_seq(&evm, &[0]);
        *evm.calls_before_failing.lock().unwrap() = Some(1); // the read before the request
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        let (r, took) = timed(&evm, &mut w).await;
        let e = r.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed, "{e}");
        assert!(took <= Duration::from_secs(60), "{took:?}");
        let m = e.to_string();
        assert!(m.contains("could not be confirmed in time"), "{m}");
        assert!(m.contains("503") && m.contains("no USDC moved"), "{m}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_allowance_that_cannot_be_read_after_the_approve_ends_in_time() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0]);
        *evm.calls_before_failing.lock().unwrap() = Some(1);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        mined_ok(&evm, B256::repeat_byte(1));
        let (r, took) = timed(&evm, &mut w).await;
        let e = r.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed, "{e}");
        assert!(took <= Duration::from_secs(60), "{took:?}");
        let m = e.to_string();
        assert!(
            m.contains("could not be read back within --pair-timeout-s"),
            "{m}"
        );
        assert!(m.contains("503"), "{m}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_allowance_read_back_after_the_deadline_names_the_time_limit() {
        // The approve is mined at once; the node answers every call, only
        // slower than what is left of --pair-timeout-s.
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0, 12_500_000]);
        *evm.call_delay.lock().unwrap() = Duration::from_secs(120);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        mined_ok(&evm, B256::repeat_byte(1));
        let e = go(&evm, &mut w).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed, "{e}");
        let m = e.to_string();
        assert!(
            m.contains("could not be read back within --pair-timeout-s"),
            "{m}"
        );
        assert!(!m.contains("did not answer"), "{m}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_rpc_and_a_slow_chain_still_settle_the_approve_in_time() {
        // Three failed receipt reads, then the approve is there.
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0, 12_500_000]);
        evm.fail_receipts
            .store(3, std::sync::atomic::Ordering::SeqCst);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(1)));
        mined_ok(&evm, B256::repeat_byte(1));
        let (r, _) = timed(&evm, &mut w).await;
        assert_eq!(r.unwrap(), ApproveOutcome::Approved {
            tx: Some(B256::repeat_byte(1)),
            block: 10
        });
        // Read fine, never mined: said so, in time.
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        w.send_results.push_back(Ok(B256::repeat_byte(2)));
        let (r, took) = timed(&evm, &mut w).await;
        let e = r.unwrap_err();
        assert!(took <= Duration::from_secs(60), "{took:?}");
        assert!(e.to_string().contains("was not mined in time"), "{e}");
        // An EIP-681 approve that never lands, the reads working.
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(10, 1)]);
        allowance_seq(&evm, &[0]);
        let mut w = FakeWallet::eoa();
        w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        let (r, took) = timed(&evm, &mut w).await;
        let e = r.unwrap_err();
        assert!(took <= Duration::from_secs(60), "{took:?}");
        assert!(
            e.to_string().contains("was not seen on chain in time"),
            "{e}"
        );
    }

    use crate::deposit::{
        store::{FailReason, OpParams, OpRecord, OpStage, Store, TxClaim},
        wallet::TxRequest,
    };

    struct Setup {
        _dir: tempfile::TempDir,
        store: Store,
        evm: FakeEvm,
        w: FakeWallet,
        rec: OpRecord,
        tx: TxRequest,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let evm = FakeEvm::sepolia();
        evm.latest.set([header(100, 1)]);
        evm.finalized.set([Some(header(90, 2))]);
        let w = FakeWallet::eoa();
        let from = w.account;
        evm.counts.lock().unwrap().insert((from, "pending"), 7);
        let mut rec = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 11_155_111,
                bridge: BRIDGE,
                to: "x".into(),
                amount_units: 1,
                an_bridge: "1a".repeat(32),
                an_network: "https://shellnet.ackinacki.org:443".into(),
            },
            "bd44".into(),
        );
        rec.from = Some(from);
        store.write(&mut rec).unwrap();
        let tx = TxRequest {
            from,
            to: BRIDGE,
            data: Bytes::new(),
            gas: 1,
            fees: crate::deposit::evm::Fees {
                max_fee_per_gas: 1,
                max_priority_fee_per_gas: 1,
            },
            purpose: TxPurpose::Deposit {
                bridge: BRIDGE,
                amount: 1,
                account: B256::ZERO,
            },
        };
        Setup {
            _dir: dir,
            store,
            evm,
            w,
            rec,
            tx,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn requested_is_on_disk_before_the_wallet_is_asked() {
        use std::sync::{Arc, Mutex};
        let mut s = setup();
        let seen = Arc::new(Mutex::new(None));
        let (path, seen2) = (s.store.record_path(&s.rec.op_id), seen.clone());
        s.w.before_send = Some(Box::new(move || {
            let r: OpRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            *seen2.lock().unwrap() = Some(r.stage);
        }));
        let h = B256::repeat_byte(3);
        s.w.send_results.push_back(Ok(h));
        s.evm
            .txs
            .lock()
            .unwrap()
            .insert(h, deposit_tx(h, s.w.account, BRIDGE, 7, 2, Bytes::new()));
        let ui = RecordingUi::new(true);
        let out = request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), Some(OpStage::Requested));
        assert_eq!(
            out,
            RequestOutcome::Signed(TxClaim {
                tx_hash: h,
                tx_nonce: 7
            })
        );
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.stage, OpStage::Signed);
        assert_eq!(back.request.unwrap().nonce_before, 7);
    }

    #[tokio::test(start_paused = true)]
    async fn the_search_starts_at_the_finalized_block_read_before_the_request() {
        // A reorg onto a shorter branch can put the deposit below the head
        // seen at the request, never below the finalized block. The node
        // has no finalized block at first: that is read again, not
        // replaced by the head.
        let mut s = setup();
        s.evm.finalized.set([None, Some(header(90, 2))]);
        s.w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        let ui = RecordingUi::new(true);
        request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
            .await
            .unwrap();
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.request.unwrap().from_block, 90, "not the head, 100");
        assert!(
            ui.events()
                .iter()
                .any(|e| matches!(e, crate::deposit::ui::UiEvent::Retry(..))),
            "{:?}",
            ui.events()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hash_the_node_never_shows_goes_to_the_search_when_the_window_ends() {
        // The wallet broadcast elsewhere, dropped or replaced it: the read
        // must not hold the run, and the directory lock, without end.
        let mut s = setup();
        let h = B256::repeat_byte(3);
        s.w.send_results.push_back(Ok(h));
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let out = tokio::time::timeout(
            Duration::from_secs(3600),
            request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None),
        )
        .await
        .expect("--recovery-window-s must end the read")
        .unwrap();
        let RequestOutcome::Search {
            window,
        } = out
        else {
            panic!("{out:?}")
        };
        assert!(
            t0.elapsed() + window <= WINDOW,
            "the read and the search share the window: {:?} + {window:?}",
            t0.elapsed()
        );
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.stage, OpStage::Requested, "the outcome is unknown");
        assert_eq!(
            back.request.unwrap().wallet_hash,
            Some(h),
            "the search starts from it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_deposit_closes_the_operation_with_exit_20() {
        let mut s = setup();
        s.w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::Rejected));
        let ui = RecordingUi::new(true);
        let e = request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::WalletFailed);
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.stage, OpStage::Failed);
        assert_eq!(back.failure.unwrap().reason, FailReason::Rejected);
    }

    #[tokio::test(start_paused = true)]
    async fn no_hash_hands_over_to_the_search_and_keeps_the_lock_state() {
        let mut s = setup();
        s.w.send_results
            .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
        let ui = RecordingUi::new(true);
        let out = request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
            .await
            .unwrap();
        assert_eq!(out, RequestOutcome::Search {
            window: WINDOW
        });
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(
            back.stage,
            OpStage::Requested,
            "the outcome is unknown until the search decides"
        );
        assert!(back.is_unresolved());
    }

    #[tokio::test(start_paused = true)]
    async fn a_bridge_paused_since_the_preflight_refuses_before_the_request() {
        let mut s = setup();
        *s.evm.paused.lock().unwrap() = Some(true);
        let ui = RecordingUi::new(true);
        let e = request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
        assert!(e.to_string().contains("paused by its owner"), "{e}");
        assert!(s.w.sent.is_empty(), "the wallet was never asked");
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.stage, OpStage::Failed);
        assert_eq!(back.failure.unwrap().reason, FailReason::Refused);
        assert!(back.request.is_none());
        for absent in [None, Some(false)] {
            let mut s = setup();
            *s.evm.paused.lock().unwrap() = absent;
            s.w.send_results
                .push_back(Err(crate::deposit::wallet::WalletError::NoHash));
            let out = request_deposit(&s.evm, &mut s.w, &ui, &s.store, &mut s.rec, &s.tx, WINDOW, None)
                .await
                .unwrap();
            assert_eq!(
                out,
                RequestOutcome::Search {
                    window: WINDOW
                },
                "{absent:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_deposit_estimate_that_reverts_is_refused_before_the_request() {
        let mut s = setup();
        *s.evm.estimate_revert.lock().unwrap() = Some("BridgePaused".into());
        let ui = RecordingUi::new(true);
        let from = s.w.account;
        let e = build_deposit_request(
            &s.evm,
            &ui,
            &s.store,
            &mut s.rec,
            from,
            BRIDGE,
            1,
            B256::ZERO,
            BlockTag::Latest,
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::PreflightRefused);
        assert!(e.to_string().contains("BridgePaused"), "{e}");
        let back = s.store.load(&s.rec.op_id).unwrap();
        assert_eq!(back.stage, OpStage::Failed);
        assert_eq!(back.failure.unwrap().reason, FailReason::Refused);
        // Without the revert the request is built.
        let mut s = setup();
        let from = s.w.account;
        let tx = build_deposit_request(
            &s.evm,
            &ui,
            &s.store,
            &mut s.rec,
            from,
            BRIDGE,
            1,
            B256::ZERO,
            BlockTag::Latest,
        )
        .await
        .unwrap();
        assert_eq!(tx.gas, 100_000);
    }

    #[tokio::test(start_paused = true)]
    async fn an_estimate_that_fails_in_transport_is_retried_not_refused() {
        let mut s = setup();
        s.evm
            .estimate_transport_failures
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let ui = RecordingUi::new(true);
        let from = s.w.account;
        let tx = build_deposit_request(
            &s.evm,
            &ui,
            &s.store,
            &mut s.rec,
            from,
            BRIDGE,
            1,
            B256::ZERO,
            BlockTag::Latest,
        )
        .await
        .unwrap();
        assert_eq!(tx.gas, 100_000);
        assert_eq!(s.store.load(&s.rec.op_id).unwrap().stage, OpStage::Reserved);
    }

    #[tokio::test(start_paused = true)]
    async fn an_approve_estimate_that_reverts_is_exit_21_and_sends_nothing() {
        let evm = FakeEvm::sepolia();
        allowance_seq(&evm, &[0]);
        *evm.estimate_revert.lock().unwrap() = Some("ERC20: paused".into());
        let mut w = FakeWallet::eoa();
        let e = go(&evm, &mut w).await.unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::ApproveFailed);
        assert!(
            e.to_string()
                .contains("approve would revert: ERC20: paused"),
            "{e}"
        );
        assert!(w.sent.is_empty());
    }
}
