//! One real deposit from Sepolia to shellnet: the driver with the live EVM
//! and Acki Nacki clients and the deposit prover from its release layout.
//! The wallet is a key the test is given.
//!
//! It spends testnet USDC and ETH, and on a bridge anchored by its owner it
//! waits until the owner accepts the deposit's block by hand. It is compiled
//! only with the `e2e` feature and is ignored even then: run it by name, and
//! only when those funds may be spent. From `crates/bridge-prover-libraries`:
//!
//! ```text
//! BRIDGE_CONFIG=config/bridge_config.shellnet \
//! E2E_SEPOLIA_KEY=<hex private key> \
//! E2E_AN_TO=<dapp_id>::<account_id> \
//! E2E_PROVER_DIR=<absolute path of the unpacked deposit prover> \
//! E2E_AMOUNT=0.01 \
//!   cargo test -p ackinacki-bridge --features e2e,dev-unfixed-bridge \
//!   e2e::one_real_deposit -- --ignored --nocapture
//! ```
//!
//! The test runs in `crates/ackinacki-bridge`, which the profile's relative
//! paths are written for. `E2E_AMOUNT` is in USDC and defaults to 0.01.
//! `dev-unfixed-bridge` is needed while this build names no minimum bridge
//! version. The operation is kept in the profile's state directory, so a run
//! that stops continues with the CLI's own `--resume <op-id>`.
//!
//! It prints the operation id, the depositId, the voucher address and the
//! `confirmDeposit` transaction. The voucher address is the reference value
//! for the offline voucher address computation. Afterwards run the ignored
//! `credit::tests::live_shellnet_confirms_its_newest_finalized_deposit`,
//! which decodes the bridge's newest `DepositFinalized` event: this one.
#![cfg(all(test, feature = "e2e"))]

use std::sync::Arc;

use alloy::signers::local::PrivateKeySigner;
use clap::Parser as _;

use crate::{
    args::{Cli, Command},
    deposit::{args::GlobalFlags, run::run_with, testkit::LocalKeyWallet, ui},
};

/// The value of `k`, which the run cannot go without.
fn var(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set; see the module documentation"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "spends testnet funds: one real deposit from Sepolia to shellnet"]
async fn one_real_deposit_from_sepolia_is_credited_on_shellnet() {
    // The profile, loaded as `main` loads it: values already set win.
    if let Ok(profile) = std::env::var("BRIDGE_CONFIG") {
        dotenvy::from_path(&profile).unwrap_or_else(|e| panic!("BRIDGE_CONFIG={profile}: {e}"));
    }
    // Never echoed, not even in a parse error.
    let signer: PrivateKeySigner = var("E2E_SEPOLIA_KEY")
        .parse()
        .unwrap_or_else(|_| panic!("E2E_SEPOLIA_KEY is not a hex private key"));
    let from = format!("{:#x}", signer.address());
    let amount = std::env::var("E2E_AMOUNT").unwrap_or_else(|_| "0.01".into());
    // The command line a user would type, the rest from the profile. With
    // `--qr-mode eip681` and `--from-address` no WalletConnect project id is
    // needed; the wallet below signs in its place.
    let cli = Cli::try_parse_from([
        "ackinacki-bridge",
        "--yes",
        "deposit",
        "--network",
        "sepolia",
        "--amount",
        &amount,
        "--to",
        &var("E2E_AN_TO"),
        "--deposit-prover-dir",
        &var("E2E_PROVER_DIR"),
        "--qr-mode",
        "eip681",
        "--from-address",
        &from,
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    let Command::Deposit(args) = cli.cmd else {
        unreachable!("the command line names deposit")
    };
    // As in `main`: nothing the run prints carries the endpoints' secrets.
    ui::hide_url_secrets(
        [args.rpc_url.as_deref(), args.gql_endpoint.as_deref()]
            .into_iter()
            .flatten(),
    );
    let g = GlobalFlags {
        json: false,
        yes: true,
        non_interactive: true,
    };
    let p = args.validate(&g).unwrap_or_else(|e| panic!("{e}"));
    let d = super::live_deps(&p, ui::pick(&g, true, false, None), Arc::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut wallet = LocalKeyWallet {
        signer,
        rpc_url: p.rpc_url.clone().expect("RPC_URL, from the profile"),
        legacy: false,
        approve_cap: None,
        request_timeout: crate::deposit::retry::ONE_READ,
    };
    let s = run_with(&p, &d, &mut wallet)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let deposit = s.deposit.clone().expect("a confirmed deposit");
    let credit = s.confirmation.clone().expect("a confirmed credit");
    println!("op-id:          {}", s.op_id.as_deref().unwrap_or("-"));
    println!("depositId:      {}", deposit["deposit_id"]);
    println!("deposit tx:     {}", deposit["tx_hash"]);
    println!("voucher:        {}", credit["voucher"]);
    println!("confirmDeposit: {}", credit["confirm_tx"]);
    println!("{}", serde_json::to_string_pretty(&s).unwrap());
}
