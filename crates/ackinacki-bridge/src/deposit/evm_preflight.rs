//! Preflight on the EVM side. Nothing here sends; a failure is exit 2.

use alloy_primitives::{Address, U256};

use crate::{
    deposit::{
        args::Network,
        evm::{
            read_allowance, read_balance, read_decimals, read_usdc, read_volume_limits, BlockTag,
            EvmRead,
        },
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

/// Checks the chain, the bridge, its token, the pause, the depositor's
/// balance and that the RPC can serve what the prover fetches.
pub async fn run(
    evm: &dyn EvmRead,
    net: Network,
    bridge: Address,
    amount: u64,
    from: Option<Address>,
) -> CliResult<EvmPreflight> {
    let id = evm
        .chain_id()
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
    let code = evm
        .code(bridge)
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
    let usdc = read_usdc(evm, bridge).await.map_err(|e| {
        refuse(
            format!("{bridge}.usdc() failed: is this an AckiNackiBridge?"),
            Some(e),
        )
    })?;
    let paused = evm
        .bridge_paused(bridge)
        .await
        .map_err(|e| refuse(format!("{bridge}.paused() failed"), Some(e)))?;
    if paused == Some(true) {
        return Err(refuse(
            "the EVM bridge is paused by its owner: deposit() would revert".into(),
            None,
        ));
    }
    // A bridge that predates the volume ceilings answers none of these, and
    // enforces nothing, so a deposit against it must not be refused here.
    if let Some((per_tx, tvl, held)) = read_volume_limits(evm, bridge)
        .await
        .map_err(|e| refuse("cannot read the bridge's volume limits".into(), Some(e)))?
    {
        let want = U256::from(amount);
        if want > per_tx {
            return Err(refuse(
                format!(
                    "{} micro-USDC is above the bridge's MAX_TRANSFER_AMOUNT of {per_tx}: \
                     deposit() would revert with TransferTooLarge. Split the amount.",
                    amount
                ),
                None,
            ));
        }
        if held + want > tvl {
            return Err(refuse(
                format!(
                    "the bridge holds {held} micro-USDC of a MAX_TVL of {tvl}, so {} more would \
                     cross the ceiling: deposit() would revert with TvlCapExceeded. Room left: {}.",
                    amount,
                    tvl.saturating_sub(held)
                ),
                None,
            ));
        }
    }
    let dec = read_decimals(evm, usdc)
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
        let b = read_balance(evm, usdc, f)
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
        read_allowance(evm, usdc, f, bridge, BlockTag::Latest)
            .await
            .map_err(|e| refuse(format!("{usdc}.allowance failed"), Some(e)))?;
    }
    let head = evm
        .header(BlockTag::Latest)
        .await
        .map_err(|e| refuse("RPC: latest block unavailable".into(), Some(e)))?
        .ok_or_else(|| refuse("RPC: latest block unavailable".into(), None))?;
    evm.probe_block(BlockTag::Number(head.number))
        .await
        .map_err(|e| {
            refuse(
                format!(
                    "--rpc-url cannot serve what the prover needs (every receipt and raw \
                     transaction of a block): {e}"
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
        testkit::*,
    };

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
    const USDC: Address = address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
    const FROM: Address = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");

    fn healthy() -> FakeEvm {
        let f = FakeEvm::sepolia();
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

    #[tokio::test]
    async fn a_healthy_chain_passes() {
        let p = run(&healthy(), Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
        assert_eq!(p.usdc, USDC);
    }

    #[tokio::test]
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
            let e = run(&fake, Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
                .await
                .unwrap_err();
            assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
            assert!(e.to_string().contains(needle), "{needle}: {e}");
        }
    }

    #[tokio::test]
    async fn a_bridge_without_the_pause_getter_passes() {
        let f = healthy();
        *f.paused.lock().unwrap() = None;
        run(&f, Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_bridge_that_is_not_paused_passes() {
        let f = healthy();
        *f.paused.lock().unwrap() = Some(false);
        run(&f, Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
    }

    fn with_limits(per_tx: u64, tvl: u64, held: u64) -> FakeEvm {
        let f = healthy();
        f.set_call(
            BRIDGE,
            IDeposit::MAX_TRANSFER_AMOUNTCall::SELECTOR,
            U256::from(per_tx).abi_encode().into(),
        );
        f.set_call(
            BRIDGE,
            IDeposit::MAX_TVLCall::SELECTOR,
            U256::from(tvl).abi_encode().into(),
        );
        f.set_call(
            BRIDGE,
            IDeposit::treasuryBalanceCall::SELECTOR,
            U256::from(held).abi_encode().into(),
        );
        f
    }

    #[tokio::test]
    async fn rpc_failures_are_not_an_old_bridge() {
        for successful_reads in 0..3 {
            let f = with_limits(50_000_000, 10_000_000_000, 0);
            *f.calls_before_failing.lock().unwrap() = Some(successful_reads);
            assert!(read_volume_limits(&f, BRIDGE).await.is_err());
        }
    }

    #[tokio::test]
    async fn a_partially_missing_limit_interface_is_refused() {
        let f = with_limits(50_000_000, 10_000_000_000, 0);
        f.set_call(BRIDGE, IDeposit::MAX_TVLCall::SELECTOR, Bytes::new());
        assert!(read_volume_limits(&f, BRIDGE).await.is_err());
    }

    /// A bridge that predates the ceilings answers none of the three reads, so
    /// the deposit must still go through — which is also why every other test
    /// here passes without them.
    #[tokio::test]
    async fn a_bridge_without_the_volume_getters_passes() {
        run(&healthy(), Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_deposit_within_both_ceilings_passes() {
        run(
            &with_limits(50_000_000, 10_000_000_000, 100_000_000),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(FROM),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_deposit_of_exactly_the_per_tx_ceiling_passes() {
        let f = with_limits(12_500_000, 10_000_000_000, 0);
        run(&f, Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_deposit_that_exactly_fills_the_tvl_ceiling_passes() {
        let f = with_limits(50_000_000, 10_000_000_000, 9_987_500_000);
        run(&f, Network::Sepolia, BRIDGE, 12_500_000, Some(FROM))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_deposit_over_the_per_tx_ceiling_is_refused_by_name() {
        let e = run(
            &with_limits(12_499_999, 10_000_000_000, 0),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(FROM),
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        let msg = e.to_string();
        assert!(msg.contains("MAX_TRANSFER_AMOUNT"), "{msg}");
        assert!(msg.contains("TransferTooLarge"), "{msg}");
    }

    #[tokio::test]
    async fn a_deposit_that_crosses_the_tvl_ceiling_is_refused_with_the_room_left() {
        let e = run(
            &with_limits(50_000_000, 10_000_000_000, 9_995_000_000),
            Network::Sepolia,
            BRIDGE,
            12_500_000,
            Some(FROM),
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        let msg = e.to_string();
        assert!(msg.contains("TvlCapExceeded"), "{msg}");
        assert!(
            msg.contains("Room left: 5000000"),
            "the refusal must say how much room is left: {msg}"
        );
    }
}
