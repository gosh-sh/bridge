//! Steps 1 to 5 against a local anvil chain running the repository's own
//! AckiNackiBridge and a mock USDC (`script/GenDepositSetup.s.sol`). The
//! wallet is a key the test holds; the Acki Nacki side, where one is
//! needed, is the scripted node of the driver tests.
//!
//! Ignored, and skipped unless `ACKI_ANVIL_IT=1`. They need `anvil`,
//! `forge` and `cast` in `PATH` and the Solidity dependencies of
//! `contracts/ethereum` installed (`make setup`); without them they say
//! what is missing and pass. Run them from `crates/bridge-prover-libraries`
//! with
//!
//! ```text
//! ACKI_ANVIL_IT=1 cargo test -p ackinacki-bridge it_anvil -- --ignored --nocapture
//! ```
#![cfg(test)]

use std::{
    path::PathBuf,
    process::{Child, Command},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::{Address, B256, U256};

use crate::{
    deposit::{
        args::RunMode,
        evm::AlloyEvm,
        evm_confirm::{self, Expect, Negative, Outcome},
        evm_steps::ensure_allowance,
        limits::ShapeViolation,
        preflight::{Deps, Polls},
        run::run_with,
        store::{OpStage, Store},
        testkit::{LocalKeyWallet, World},
        ui::RecordingUi,
        wallet::{TxPurpose, TxRequest, Wallet},
    },
    errors::ExitCode,
};

/// anvil's well-known development keys: accounts 0 and 1 of its default
/// mnemonic, each funded with 10 000 ETH.
const KEY0: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const KEY1: &str = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

/// A running anvil, killed when this is dropped.
struct Anvil {
    /// The anvil process.
    child: Child,
    /// Its JSON-RPC endpoint.
    url: String,
    /// Taken before anything was started: this test starts subprocesses.
    _spawning: crate::test_forks::Hold,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "the test is over either way"
        )]
        let _ = self.child.kill();
        #[expect(clippy::let_underscore_must_use, reason = "only reaps the process")]
        let _ = self.child.wait();
    }
}

/// The repository root. The crate builds through a symlink; the OS
/// resolves `..` past it.
fn repo() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// Why these tests cannot run here; `None` when they can. Starts the
/// tools to ask for their versions, so the caller holds
/// [`crate::test_forks::spawning`].
fn missing() -> Option<String> {
    for tool in ["anvil", "forge", "cast"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return Some(format!("`{tool}` is not in PATH (install Foundry)"));
        }
    }
    let script = repo().join("contracts/ethereum/lib/forge-std/src/Script.sol");
    if !script.exists() {
        return Some(format!(
            "{} is missing: install the Solidity dependencies of contracts/ethereum (make setup)",
            script.display()
        ));
    }
    None
}

/// Starts anvil with `args` on a free port, or says why the test is
/// skipped and returns `None`.
fn anvil(args: &[&str]) -> Option<Anvil> {
    if std::env::var("ACKI_ANVIL_IT").ok().as_deref() != Some("1") {
        eprintln!("skipped: set ACKI_ANVIL_IT=1 to run the anvil tests");
        return None;
    }
    let spawning = crate::test_forks::spawning();
    if let Some(why) = missing() {
        eprintln!("skipped: {why}");
        return None;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let child = Command::new("anvil")
        .args(["--port", &port.to_string(), "--silent"])
        .args(args)
        .spawn()
        .expect("anvil starts");
    let a = Anvil {
        child,
        url: format!("http://127.0.0.1:{port}"),
        _spawning: spawning,
    };
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "anvil did not listen on {port}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    Some(a)
}

/// Deploys the mock USDC and the bridge with `GenDepositSetup.s.sol`
/// from account 0, which ends up holding USDC and a maximal allowance to
/// the bridge. Returns `(usdc, bridge)`.
fn deploy(rpc: &str) -> (Address, Address) {
    let out = Command::new("forge")
        .current_dir(repo().join("contracts/ethereum"))
        .args([
            "script",
            "script/GenDepositSetup.s.sol",
            "--rpc-url",
            rpc,
            "--broadcast",
        ])
        .env("PRIVATE_KEY", KEY0)
        .output()
        .expect("forge runs");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "forge script failed:\n{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let find = |k: &str| {
        text.lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix(k)
                    .map(|v| v.trim().parse::<Address>().expect("an address"))
            })
            .unwrap_or_else(|| panic!("{k} not in forge output:\n{text}"))
    };
    (find("USDC:"), find("BRIDGE:"))
}

/// Runs `cast` with `args` and requires it to succeed.
fn cast(args: &[&str]) {
    let out = Command::new("cast").args(args).output().expect("cast runs");
    assert!(
        out.status.success(),
        "cast {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Mints `units` of the mock USDC to `to`, sent from account 0.
fn mint(rpc: &str, usdc: Address, to: Address, units: u64) {
    cast(&[
        "send",
        &format!("{usdc:#x}"),
        "mint(address,uint256)",
        &format!("{to:#x}"),
        &units.to_string(),
        "--private-key",
        KEY0,
        "--rpc-url",
        rpc,
    ]);
}

#[tokio::test]
#[ignore = "needs anvil, forge and cast; set ACKI_ANVIL_IT=1"]
async fn a_type_2_deposit_is_confirmed_and_a_legacy_one_is_unprovable() {
    let Some(a) = anvil(&[]) else { return };
    let (_usdc, bridge) = deploy(&a.url);
    let evm = AlloyEvm::connect(&a.url).unwrap();
    // The deployer holds USDC and has approved the bridge for max.
    let signer: PrivateKeySigner = KEY0.parse().unwrap();
    let ui = RecordingUi::new(true);
    let acc = B256::repeat_byte(0xa3);
    for (legacy, id) in [(false, 0u64), (true, 1)] {
        let mut w = LocalKeyWallet {
            signer: signer.clone(),
            rpc_url: a.url.clone(),
            legacy,
            approve_cap: None,
        };
        let tx = TxRequest::build(&evm, signer.address(), TxPurpose::Deposit {
            bridge,
            amount: 1_000_000,
            account: acc,
        })
        .await
        .unwrap();
        let h = w.send_transaction(&ui, &tx).await.unwrap();
        // Depth, and finality: anvil finalizes 64 blocks behind its head.
        cast(&["rpc", "anvil_mine", "0x64", "--rpc-url", &a.url]);
        let exp = Expect {
            bridge,
            from: signer.address(),
            calldata: tx.data.clone(),
            amount: 1_000_000,
            account: acc,
            confirmations: 12,
        };
        let out = evm_confirm::confirm(&evm, h, &exp, &ui, Duration::from_millis(200))
            .await
            .unwrap();
        match (legacy, out) {
            (false, Outcome::Confirmed(f)) => {
                assert_eq!(f.deposit_id, U256::from(id));
                assert_eq!(
                    f.receipt_log_index, 0,
                    "the mock USDC emits no Transfer: Deposit is the receipt's only log"
                );
                assert_eq!(f.access_list_rlp_len, 1, "an empty access list");
            },
            (
                true,
                Outcome::Final(Negative::Unprovable(ShapeViolation::NotEip1559 {
                    tx_type: 0,
                })),
            ) => {},
            (_, o) => panic!("legacy={legacy}: {o:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "needs anvil, forge and cast; set ACKI_ANVIL_IT=1"]
async fn a_wallet_that_lowers_the_limit_stops_the_deposit_with_exit_21() {
    let Some(a) = anvil(&[]) else { return };
    let (usdc, bridge) = deploy(&a.url);
    let evm = AlloyEvm::connect(&a.url).unwrap();
    let user: PrivateKeySigner = KEY1.parse().unwrap();
    mint(&a.url, usdc, user.address(), 1_000_000_000);
    let mut w = LocalKeyWallet {
        signer: user.clone(),
        rpc_url: a.url.clone(),
        legacy: false,
        approve_cap: Some(U256::from(1u64)),
    };
    let ui = RecordingUi::new(true);
    let e = ensure_allowance(
        &evm,
        &mut w,
        &ui,
        usdc,
        bridge,
        user.address(),
        1_000_000,
        "OP",
        Duration::from_secs(30),
        Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert_eq!(e.exit_code(), ExitCode::ApproveFailed, "{e}");
}

/// The driver's command line and dependencies for `w`'s Acki Nacki side
/// and the EVM bridge `bridge` on anvil at `url`: two confirmations, short
/// polls, and a short anchor wait (the scripted Acki Nacki node never
/// anchors a block of this chain); and the UI that records the run.
fn on_anvil(
    w: &World,
    url: &str,
    bridge: Address,
) -> (crate::deposit::args::DepositParams, Deps, Arc<RecordingUi>) {
    let mut p = w.params(RunMode::Fresh);
    p.rpc_url = Some(url.to_string());
    p.bridge = Some(bridge);
    p.confirmations = 2;
    p.anchor_timeout = Some(Duration::from_secs(3));
    let ui = Arc::new(RecordingUi::new(true));
    let mut d = w.deps();
    d.evm = Arc::new(AlloyEvm::connect(url).unwrap());
    d.ui = ui.clone();
    let poll = Duration::from_millis(200);
    d.polls = Polls {
        evm: poll,
        anchor: poll,
        credit: poll,
        wallet: poll,
    };
    (p, d, ui)
}

#[tokio::test]
#[ignore = "needs anvil, forge and cast; set ACKI_ANVIL_IT=1"]
async fn the_driver_takes_a_deposit_through_steps_1_to_5_and_a_legacy_one_is_exit_35() {
    // Finality two blocks behind the head.
    let Some(a) = anvil(&["--slots-in-an-epoch", "1"]) else {
        return;
    };
    let (usdc, bridge) = deploy(&a.url);
    // A sender with USDC and nothing approved: step 3 approves for real.
    let user: PrivateKeySigner = KEY1.parse().unwrap();
    mint(&a.url, usdc, user.address(), 1_000_000_000);
    // Only now Sepolia's chain id, which the driver requires: forge takes a
    // chain with that id for Sepolia itself and deploys slowly. And from
    // here on a block a second, as on a real chain: the deposit gets its
    // depth and its finality without anything else being sent.
    cast(&["rpc", "anvil_setChainId", "11155111", "--rpc-url", &a.url]);
    cast(&["rpc", "evm_setIntervalMining", "1", "--rpc-url", &a.url]);

    let w = World::healthy();
    let (p, d, ui) = on_anvil(&w, &a.url, bridge);
    let mut wallet = LocalKeyWallet {
        signer: user.clone(),
        rpc_url: a.url.clone(),
        legacy: false,
        approve_cap: None,
    };
    let e = run_with(&p, &d, &mut wallet).await.unwrap_err();
    assert_eq!(
        e.exit_code(),
        ExitCode::AnWaitTimeout,
        "confirmed on anvil, then no anchor: {e}\n{:#?}",
        ui.events()
    );
    let rec = Store::open(&p.state_dir)
        .unwrap()
        .load(e.op_id().unwrap())
        .unwrap();
    assert_eq!(rec.stage, OpStage::Confirmed);
    assert_eq!(rec.from, Some(user.address()));
    let dep = rec.deposit.unwrap();
    assert_eq!(dep.deposit_id, U256::ZERO, "the bridge's first deposit");
    assert_eq!(
        dep.receipt_log_index, 0,
        "the mock USDC emits no Transfer: Deposit is the receipt's only log"
    );
    assert_eq!(dep.block_log_index, 0);

    // The same sender, from a wallet that sends legacy transactions: the
    // first deposit spent the allowance, so its approve goes out legacy
    // too, which is fine; the deposit is not.
    let w = World::healthy();
    let (p, d, ui) = on_anvil(&w, &a.url, bridge);
    let mut wallet = LocalKeyWallet {
        signer: user.clone(),
        rpc_url: a.url.clone(),
        legacy: true,
        approve_cap: None,
    };
    let e = run_with(&p, &d, &mut wallet).await.unwrap_err();
    assert_eq!(
        e.exit_code(),
        ExitCode::DepositUnprovable,
        "{e}\n{:#?}",
        ui.events()
    );
    assert!(e.to_string().contains("type 0"), "{e}");
    assert!(
        ui.statuses()
            .iter()
            .any(|s| s.contains("waiting for its block to be finalized")),
        "the verdict waited for finality: {:#?}",
        ui.statuses()
    );
    let rec = Store::open(&p.state_dir)
        .unwrap()
        .load(e.op_id().unwrap())
        .unwrap();
    assert_eq!(rec.stage, OpStage::Failed);
}
