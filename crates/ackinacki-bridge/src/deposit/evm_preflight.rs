//! Preflight on the EVM side. Nothing here sends; a failure is exit 2.
//! A read that fails is tried again for [`READ_WINDOW`] first: a
//! load-balanced public RPC fails a request now and then.

use std::{future::Future, time::Duration};

use alloy_primitives::{Address, U256};

use crate::{
    deposit::{
        args::Network,
        evm::{
            read_allowance, read_balance, read_decimals, read_usdc, read_volume_limits, revert_of,
            BlockTag, EvmRead,
        },
        retry::{deadline_after, until_with},
        ui::Ui,
    },
    errors::{CliError, CliResult},
};

/// What the EVM preflight learned.
#[derive(Debug, Clone)]
pub struct EvmPreflight {
    /// The token the bridge takes deposits in.
    pub usdc: Address,
}

/// A preflight refusal (exit 2).
fn refuse(reason: String, e: Option<anyhow::Error>) -> CliError {
    CliError::Preflight {
        reason,
        source: e,
    }
}

/// How long one preflight read is tried again before it refuses the
/// deposit. Nothing is sent yet, so a short window is enough to ride out a
/// public endpoint's hiccup without hanging on an RPC that is really gone.
pub const READ_WINDOW: Duration = Duration::from_secs(120);

/// `f`, tried again with backoff for [`READ_WINDOW`], every failed attempt
/// reported through `ui`; the error is the last attempt's. An answer that
/// cannot change — a call that reverts, or a reply that does not decode as
/// the bridge's — is not tried again: a wrong `--bridge-address` says so at
/// once.
async fn read<T, F, Fut>(ui: &dyn Ui, what: &str, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    until_with(
        Some(deadline_after(READ_WINDOW)),
        || {
            let attempt = f();
            async move {
                match attempt.await {
                    Ok(v) => Ok(Ok(v)),
                    Err(e) if is_final(&e) => Ok(Err(e)),
                    Err(e) => Err(e),
                }
            }
        },
        |attempt, e| ui.retry(what, attempt, &format!("{e:#}")),
    )
    .await
    .map_err(|e| e.unwrap_or_else(|| anyhow::anyhow!("no answer within {READ_WINDOW:?}")))?
}

/// The node answered, and asking again gets the same answer.
fn is_final(e: &anyhow::Error) -> bool {
    revert_of(e).is_some() || e.downcast_ref::<alloy::sol_types::Error>().is_some()
}

/// Checks the chain, the bridge, its token, the pause, the depositor's
/// balance and that the RPC can serve what the prover fetches.
pub async fn run(
    evm: &dyn EvmRead,
    ui: &dyn Ui,
    net: Network,
    bridge: Address,
    amount: u64,
    from: Option<Address>,
) -> CliResult<EvmPreflight> {
    let id = read(ui, "reading the EVM chain id", || evm.chain_id())
        .await
        .map_err(|e| refuse("RPC: eth_chainId failed".into(), Some(e)))?;
    if id != net.chain_id() {
        return Err(refuse(
            format!(
                "--rpc-url serves chain id {id}, but --network {} is chain id {}",
                net.arg(),
                net.chain_id()
            ),
            None,
        ));
    }
    let code = read(ui, "reading the EVM bridge's code", || evm.code(bridge))
        .await
        .map_err(|e| refuse("RPC: eth_getCode failed".into(), Some(e)))?;
    if code.is_empty() {
        return Err(refuse(
            format!(
                "there is no contract at --bridge-address {bridge} on {}",
                net.name()
            ),
            None,
        ));
    }
    let usdc = read(ui, "reading the EVM bridge's token", || {
        read_usdc(evm, bridge)
    })
    .await
    .map_err(|e| {
        refuse(
            format!("{bridge}.usdc() failed: is this an AckiNackiBridge?"),
            Some(e),
        )
    })?;
    let paused = read(ui, "reading the EVM bridge's pause", || {
        evm.bridge_paused(bridge)
    })
    .await
    .map_err(|e| refuse(format!("{bridge}.paused() failed"), Some(e)))?;
    if paused == Some(true) {
        return Err(refuse(
            "the EVM bridge is paused by its owner: deposit() would revert".into(),
            None,
        ));
    }
    let limits = read(ui, "reading the EVM bridge's limits", || {
        read_volume_limits(evm, bridge)
    })
    .await
    .map_err(|e| {
        refuse(
            format!("{bridge}: required deposit volume-limit getters failed"),
            Some(e),
        )
    })?;
    let requested = U256::from(amount);
    if requested > limits.max_transfer {
        return Err(refuse(
            format!(
                "deposit amount {amount} micro-USDC exceeds MAX_TRANSFER_AMOUNT ({})",
                limits.max_transfer
            ),
            None,
        ));
    }
    if limits.outstanding > limits.max_tvl || requested > limits.max_tvl - limits.outstanding {
        return Err(refuse(
            format!(
                "deposit would exceed MAX_BRIDGE_TVL ({} micro-USDC); outstanding is {}. Wait for \
                 withdrawals and retry",
                limits.max_tvl, limits.outstanding
            ),
            None,
        ));
    }
    let dec = read(ui, "reading the token's decimals", || {
        read_decimals(evm, usdc)
    })
    .await
    .map_err(|e| refuse(format!("{usdc}.decimals() failed"), Some(e)))?;
    if dec != 6 {
        return Err(refuse(
            format!(
                "the bridge's token {usdc} has {dec} decimals, not 6: this is not the USDC the \
                 bridge is built for"
            ),
            None,
        ));
    }
    if let Some(f) = from {
        let b = read(ui, "reading the sender's balance", || {
            read_balance(evm, usdc, f)
        })
        .await
        .map_err(|e| refuse(format!("{usdc}.balanceOf failed"), Some(e)))?;
        if b < U256::from(amount) {
            return Err(refuse(
                format!("{f} holds {b} USDC units, the deposit needs {amount}: balance too low"),
                None,
            ));
        }
        // The approve step reads it: a token that cannot answer is refused
        // here, before the wallet is asked for anything.
        read(ui, "reading the sender's allowance", || {
            read_allowance(evm, usdc, f, bridge, BlockTag::Latest)
        })
        .await
        .map_err(|e| refuse(format!("{usdc}.allowance failed"), Some(e)))?;
    }
    let head = read(ui, "reading the latest EVM block", || {
        evm.header(BlockTag::Latest)
    })
    .await
    .map_err(|e| refuse("RPC: latest block unavailable".into(), Some(e)))?
    .ok_or_else(|| refuse("RPC: latest block unavailable".into(), None))?;
    read(ui, "reading the latest EVM block's receipts", || {
        evm.probe_block(BlockTag::Number(head.number))
    })
    .await
    .map_err(|e| {
        refuse(
            format!(
                "--rpc-url cannot serve what the prover needs (every receipt and raw transaction \
                 of a block): {e}"
            ),
            None,
        )
    })?;
    Ok(EvmPreflight {
        usdc,
    })
}

#[cfg(test)]
mod tests {
    use alloy::sol_types::{SolCall, SolValue};
    use alloy_primitives::{address, Address, Bytes, U256};

    use super::*;
    use crate::deposit::{
        args::Network,
        evm::{IDeposit, IErc20},
        testkit::{sepolia_sender, *},
        ui::RecordingUi,
    };

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
    const USDC: Address = address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");

    fn healthy() -> FakeEvm {
        let f = FakeEvm::sepolia();
        f.set_volume_limits(BRIDGE);
        f.codes
            .lock()
            .unwrap()
            .insert(BRIDGE, Bytes::from(vec![0x60, 0x80]));
        f.set_call(
            BRIDGE,
            IDeposit::usdcCall::SELECTOR,
            USDC.abi_encode().into(),
        );
        f.set_call(
            USDC,
            IErc20::decimalsCall::SELECTOR,
            U256::from(6u8).abi_encode().into(),
        );
        f.set_call(
            USDC,
            IErc20::balanceOfCall::SELECTOR,
            U256::from(20_000_000u64).abi_encode().into(),
        );
        f.set_call(
            USDC,
            IErc20::allowanceCall::SELECTOR,
            U256::ZERO.abi_encode().into(),
        );
        f.latest.set([header(100, 1)]);
        f
    }

    #[tokio::test(start_paused = true)]
    async fn a_healthy_chain_passes() {
        let p = run(
            &healthy(),
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(sepolia_sender()),
        )
        .await
        .unwrap();
        assert_eq!(p.usdc, USDC);
    }

    #[tokio::test(start_paused = true)]
    async fn each_refusal_is_exit_2_and_names_its_cause() {
        let mut wrong_chain = healthy();
        wrong_chain.chain_id = 1;
        let no_code = healthy();
        no_code.codes.lock().unwrap().clear();
        let not_six = healthy();
        not_six.set_call(
            USDC,
            IErc20::decimalsCall::SELECTOR,
            U256::from(18u8).abi_encode().into(),
        );
        let poor = healthy();
        poor.set_call(
            USDC,
            IErc20::balanceOfCall::SELECTOR,
            U256::from(1u64).abi_encode().into(),
        );
        let no_probe = healthy();
        *no_probe.probe_error.lock().unwrap() =
            Some("eth_getRawTransactionByHash not supported".into());
        let paused = healthy();
        *paused.paused.lock().unwrap() = Some(true);
        for (fake, needle) in [
            (wrong_chain, "chain id 1"),
            (no_code, "no contract"),
            (not_six, "decimals"),
            (poor, "balance"),
            (no_probe, "eth_getRawTransactionByHash"),
            (
                paused,
                "the EVM bridge is paused by its owner: deposit() would revert",
            ),
        ] {
            let e = run(
                &fake,
                &RecordingUi::new(true),
                Network::Sepolia,
                BRIDGE,
                12_500_000,
                Some(sepolia_sender()),
            )
            .await
            .unwrap_err();
            assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
            assert!(e.to_string().contains(needle), "{needle}: {e}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_rpc_hiccup_is_tried_again_not_refused() {
        // A load-balanced endpoint whose backend refuses two requests.
        let f = healthy();
        f.probe_failures
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let ui = RecordingUi::new(true);
        run(
            &f,
            &ui,
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(sepolia_sender()),
        )
        .await
        .unwrap();
        let retries = ui
            .events()
            .into_iter()
            .filter(|e| matches!(e, crate::deposit::ui::UiEvent::Retry(..)))
            .count();
        assert_eq!(retries, 2);
    }

    /// `usdc()` on a contract that is not the bridge answers something that
    /// does not decode: refused at once, not after the read window.
    #[tokio::test(start_paused = true)]
    async fn an_answer_that_cannot_change_is_not_tried_again() {
        let f = healthy();
        f.set_call(BRIDGE, IDeposit::usdcCall::SELECTOR, Bytes::new());
        let ui = RecordingUi::new(true);
        let e = run(
            &f,
            &ui,
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(sepolia_sender()),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("is this an AckiNackiBridge"), "{e}");
        let retries = ui
            .events()
            .into_iter()
            .filter(|e| matches!(e, crate::deposit::ui::UiEvent::Retry(..)))
            .count();
        assert_eq!(retries, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bridge_without_the_pause_getter_passes() {
        let f = healthy();
        *f.paused.lock().unwrap() = None;
        run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(sepolia_sender()),
        )
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_bridge_that_is_not_paused_passes() {
        let f = healthy();
        *f.paused.lock().unwrap() = Some(false);
        run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(sepolia_sender()),
        )
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn transfer_ceiling_and_tvl_headroom_are_inclusive() {
        let f = healthy();
        f.set_call(
            BRIDGE,
            IDeposit::treasuryBalanceCall::SELECTOR,
            U256::from(9_950_000_000u64).abi_encode().into(),
        );
        run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            50_000_000,
            None,
        )
        .await
        .unwrap();
        let err = run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            50_000_001,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("MAX_TRANSFER_AMOUNT"), "{err}");
        f.set_call(
            BRIDGE,
            IDeposit::treasuryBalanceCall::SELECTOR,
            U256::from(9_950_000_001u64).abi_encode().into(),
        );
        let err = run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            50_000_000,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("MAX_BRIDGE_TVL"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn missing_or_malformed_limit_getters_refuse_before_wallet_use() {
        for selector in [
            IDeposit::MAX_TRANSFER_AMOUNTCall::SELECTOR,
            IDeposit::MAX_BRIDGE_TVLCall::SELECTOR,
            IDeposit::treasuryBalanceCall::SELECTOR,
        ] {
            for malformed in [false, true] {
                let f = healthy();
                if malformed {
                    f.set_call(BRIDGE, selector, Bytes::new());
                } else {
                    f.calls.lock().unwrap().remove(&(BRIDGE, selector));
                }
                let err = run(
                    &f,
                    &RecordingUi::new(true),
                    Network::Sepolia,
                    BRIDGE,
                    1,
                    None,
                )
                .await
                .unwrap_err();
                assert_eq!(err.exit_code(), crate::errors::ExitCode::PreflightRefused);
                assert!(
                    err.to_string().contains("volume-limit getters failed"),
                    "{err}"
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn transport_failure_while_reading_limits_never_skips_them() {
        let f = healthy();
        *f.calls_before_failing.lock().unwrap() = Some(1);
        let err = run(
            &f,
            &RecordingUi::new(true),
            Network::Sepolia,
            BRIDGE,
            1,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("volume-limit getters failed"),
            "{err}"
        );
    }
}
