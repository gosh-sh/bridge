//! The checks a new deposit passes before the wallet is asked for
//! anything, in order: the EVM chain and bridge, the Acki Nacki bridge,
//! whether the light client can anchor the deposit, the prover directory,
//! and the operations still open in the state directory. A failure is
//! exit 2 (exit 3 for an open operation of the same deposit), and nothing
//! has been sent. Nothing here asks the wallet or creates an operation.
//!
//! A resume passes a smaller set of checks, only those its operation's
//! recorded stage still needs ([`context_for_resume`]).

use std::{sync::Arc, time::Duration};

use alloy_primitives::{Address, U256};
use serde_json::json;

use crate::{
    deposit::{
        an::{AnRead, AnSend},
        an_preflight::{self, AnPreflight},
        args::{an_network_id, AnTarget, DepositParams, Network},
        evm::{approve_calldata, deposit_calldata, EvmRead},
        evm_preflight::{self, EvmPreflight},
        lc_readiness::{self, AnchorPlan, HistoryCapped, LcFailure},
        locks::ProverLock,
        prover_files::{check_prover_dir, ProverDir},
        retry::transient,
        run::RunCx,
        signals,
        store::{blocking_by_params, close_interrupted, OpParams, OpRecord, OpStage, Store},
        ui::Ui,
        wallet::{eip681, TxPurpose},
        DepositSuccess,
    },
    errors::{CliError, CliResult, ExitCode, Stage},
};

/// An Acki Nacki node the driver both reads and sends `finalizeDeposit` to.
pub trait AnBoth: AnRead + AnSend {}

/// Every node that reads and sends is one.
impl<T: AnRead + AnSend> AnBoth for T {}

/// How often each wait of the driver reads its chain again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polls {
    /// The EVM confirmation and the recovery search.
    pub evm: Duration,
    /// The anchor wait on Acki Nacki.
    pub anchor: Duration,
    /// The credit confirmation on Acki Nacki.
    pub credit: Duration,
    /// The wallet's transactions and the allowance after `approve`.
    pub wallet: Duration,
}

impl Polls {
    /// The intervals of a live run: one EVM block, the anchor every 30 s,
    /// the credit every 10 s, the wallet every 3 s.
    pub fn live() -> Self {
        Polls {
            evm: Duration::from_secs(12),
            anchor: Duration::from_secs(30),
            credit: Duration::from_secs(10),
            wallet: Duration::from_secs(3),
        }
    }
}

/// What a deposit run talks to, built once by the driver.
pub struct Deps {
    /// The EVM node behind `--rpc-url`.
    pub evm: Arc<dyn EvmRead>,
    /// The Acki Nacki node behind `--gql-endpoint`.
    pub an: Arc<dyn AnBoth>,
    /// Where progress is reported.
    pub ui: Arc<dyn Ui>,
    /// How often the waits read again.
    pub polls: Polls,
    /// The bridge version this run requires; `MIN_BRIDGE_VERSION` in a live
    /// run.
    pub min_bridge: Option<an_preflight::BridgeVersion>,
    /// The operation this run drives, for the signal handler's exit code.
    pub current_op: Arc<std::sync::Mutex<Option<String>>>,
}

/// What a passed preflight learned: enough to reserve the operation and
/// run it.
#[derive(Debug, Clone)]
pub struct Checked {
    /// The EVM side: the bridge's token, the sender's balance if known,
    /// the head the checks ran at.
    pub evm: EvmPreflight,
    /// The Acki Nacki side: the bridge's dapp, version, anchor config,
    /// voucher code and the recipient.
    pub an: AnPreflight,
    /// Who is expected to anchor the deposit's block.
    pub plan: AnchorPlan,
    /// The prover directory that passed its checks.
    pub prover: ProverDir,
    /// The parameters the operation is created with.
    pub params: OpParams,
}

/// A preflight refusal (exit 2).
fn refuse(reason: String) -> CliError {
    CliError::Preflight {
        reason,
        source: None,
    }
}

/// Every check a new deposit must pass before the wallet is asked, in
/// order; the first that fails ends the run. `from` is the sender when it
/// is already known (`--from-address`); without it the balance is checked
/// after pairing.
///
/// The last check reads the open operations and closes reservations whose
/// run died, so the caller holds the state directory's lock
/// ([`crate::deposit::locks::DirLock`]) around this call and the creation
/// of the operation.
pub async fn check(
    p: &DepositParams,
    d: &Deps,
    store: &Store,
    from: Option<Address>,
) -> CliResult<Checked> {
    let net = p.network.expect("validated for a fresh run");
    let typed = p.amount.expect("validated for a fresh run");
    let to = p.to.expect("validated for a fresh run");
    let bridge = p.bridge.expect("validated");
    let bridge_acc = p.usdc_bridge_account.expect("validated");
    // The bridge takes at most `u64::MAX` micro-USDC in one deposit.
    let amount = u64::try_from(typed.0).map_err(|_| {
        refuse(format!(
            "{} USDC is above the bridge's MAX_DEPOSIT_AMOUNT of {} micro-USDC",
            typed.display(),
            u64::MAX
        ))
    })?;
    let evm = evm_preflight::run(d.evm.as_ref(), net, bridge, amount, from).await?;
    let an = an_preflight::run_with(
        d.an.as_ref(),
        bridge_acc,
        net.chain_id(),
        bridge,
        &to,
        d.ui.as_ref(),
        d.min_bridge,
        cfg!(feature = "dev-unfixed-bridge"),
        false,
    )
    .await?;
    let plan = anchor_plan(d, bridge_acc, net.chain_id(), &an).await?;
    let prover = check_prover_dir(p.prover_dir.as_deref().expect("validated")).map_err(refuse)?;
    // The prover's lock is probed now: a filesystem without flock is
    // refused here, before the wallet, not when the proof is due with the
    // USDC already in the bridge. Held means another deposit is proving,
    // which is fine; the lock is let go at once.
    drop(ProverLock::try_take(&prover)?);
    let params = OpParams {
        chain_id: net.chain_id(),
        bridge,
        to: to.extended(),
        amount_units: amount,
        an_bridge: hex::encode(bridge_acc),
        an_network: an_network_id(p.gql_endpoint.as_deref().expect("validated"))?,
    };
    let mut recs = store.list()?;
    close_interrupted(store, &mut recs)?;
    if let Some(b) = blocking_by_params(store, &recs, &params)? {
        return Err(CliError::deposit(
            ExitCode::DuplicateRefused,
            Stage::Preflight,
            Some(&b.op_id),
            b.to_string(),
        ));
    }
    Ok(Checked {
        evm,
        an,
        plan,
        prover,
        params,
    })
}

/// Who anchors the deposit's block. With the owner's anchors on, the
/// light client's checks only choose the status text: they read a bounded
/// history, and a read that fails leaves the light client out with a
/// warning. With them off, the deposit goes ahead only if the light client
/// passes all three checks, and a read that fails refuses it.
async fn anchor_plan(
    d: &Deps,
    bridge_acc: [u8; 32],
    chain_id: u64,
    an: &AnPreflight,
) -> CliResult<AnchorPlan> {
    let owner = an.owner_anchors_enabled;
    let readiness = match an.light_client {
        None => Err(LcFailure::NoLightClient),
        Some(lc) => {
            let cap = owner.then_some(lc_readiness::OWNER_MODE_PAGE_CAP);
            match lc_readiness::observe(
                d.an.as_ref(),
                d.evm.as_ref(),
                bridge_acc,
                lc,
                chain_id,
                d.ui.as_ref(),
                cap,
            )
            .await
            {
                Ok(o) => lc_readiness::judge(&o, chain_id),
                // A long history is what owner mode normally looks like: the
                // bridge's inbound messages only grow, and there may be no
                // switch-off in them at all. Only a read that failed is
                // worth a warning.
                Err(e) if owner => {
                    match e.downcast_ref::<HistoryCapped>() {
                        Some(c) => d.ui.status(&format!(
                            "{c}; the anchor is expected from the bridge owner"
                        )),
                        None => d.ui.warn(&format!(
                            "{e:#}; whether the light client can anchor this deposit is unknown, \
                             so the anchor is expected from the bridge owner"
                        )),
                    }
                    return Ok(AnchorPlan::Owner {
                        lc_ready: false,
                    });
                },
                Err(e) => {
                    return Err(refuse(format!(
                        "owner anchors are switched off, and whether the light client can anchor \
                         this deposit cannot be checked: {e:#}"
                    )))
                },
            }
        },
    };
    lc_readiness::plan(owner, an.light_client, readiness).map_err(|f| {
        refuse(format!(
            "owner anchors are switched off and the light client cannot anchor this deposit: {f}"
        ))
    })
}

/// The checks of a resume: only what the operation's recorded stage still
/// needs, with the parameters from its record. Continuing a deposit that
/// may already be on chain must not depend on the checks that guard a new
/// one: a paused Acki Nacki bridge, its lost trust in the EVM bridge or a
/// moved voucher code is a warning, the recipient is not checked, the light
/// client's readiness is read only before the anchor and is a warning, and
/// the prover directory is checked only while a proof is still to be
/// built. The bridge version is checked always: resending
/// `finalizeDeposit` is safe only on a fixed bridge.
///
/// An RPC endpoint that serves another chain than the operation's is the
/// command line pointing at another network: exit 2, as any other
/// contradiction with the record. Every other refusal comes after the
/// wallet was asked for the deposit and takes the exit an interrupt at the
/// recorded stage would give, never exit 2.
pub async fn context_for_resume(p: &DepositParams, d: &Deps, rec: &OpRecord) -> CliResult<RunCx> {
    let ui = d.ui.as_ref();
    let op = &rec.op_id;
    let held = |e: CliError| {
        let why = match e {
            CliError::Deposit {
                ..
            } => return e,
            CliError::Preflight {
                reason, ..
            } => reason,
            other => other.to_string(),
        };
        match signals::exit_for(Some(rec.stage)) {
            // Only a record that never asked the wallet gets here with 2.
            ExitCode::PreflightRefused => refuse(why),
            exit => CliError::deposit(
                exit,
                Stage::Preflight,
                Some(op),
                format!(
                    "{why}; operation {op} is unchanged: once that is fixed, run --resume {op} \
                     again"
                ),
            ),
        }
    };
    // Flags and a local directory only: known before any chain is read,
    // and those reads retry for as long as an endpoint is down.
    let prover = resume_prover(p, rec, ui).map_err(held)?;
    let net = Network::from_chain_id(rec.params.chain_id).ok_or_else(|| {
        held(refuse(format!(
            "operation {op} is on chain {}, which this build does not know",
            rec.params.chain_id
        )))
    })?;
    let id = transient(ui, "reading the EVM chain id", || d.evm.chain_id()).await;
    if id != net.chain_id() {
        return Err(refuse(format!(
            "--rpc-url serves chain {id}; operation {op} is on chain {}. Use the profile it was \
             made with (nothing was sent)",
            net.chain_id()
        )));
    }
    let mut bridge_acc = [0u8; 32];
    hex::decode_to_slice(&rec.params.an_bridge, &mut bridge_acc).map_err(|e| {
        held(refuse(format!(
            "the record names a damaged Acki Nacki bridge id: {e}"
        )))
    })?;
    // The recipient is not checked: the deposit is made, and it names it.
    let to = AnTarget::parse(&rec.params.to).map_err(held)?;
    let an = an_preflight::run_with(
        d.an.as_ref(),
        bridge_acc,
        net.chain_id(),
        rec.params.bridge,
        &to,
        ui,
        d.min_bridge,
        cfg!(feature = "dev-unfixed-bridge"),
        true,
    )
    .await
    .map_err(held)?;
    let plan = match rec.stage {
        OpStage::Requested | OpStage::Signed | OpStage::Abandoned | OpStage::Confirmed => {
            resume_plan(d, bridge_acc, net.chain_id(), &an).await
        },
        // Past the anchor wait: only a 224 at step 8 brings the run back to
        // it, and then the anchor is expected from whoever the bridge has.
        _ if an.owner_anchors_enabled => AnchorPlan::Owner {
            lc_ready: false,
        },
        _ => AnchorPlan::LightClient,
    };
    let rpc_url = p
        .rpc_url
        .clone()
        .ok_or_else(|| held(refuse("--rpc-url (RPC_URL) is required to resume".into())))?;
    Ok(RunCx {
        plan,
        bridge_acc,
        bridge_dapp: an.bridge_dapp,
        light_client: an.light_client,
        prover,
        rpc_url,
        // Only the steps before the deposit request read the token; a
        // resumed operation is past them.
        usdc: Address::ZERO,
    })
}

/// Whom a resumed operation waits for at step 6. The light client's
/// readiness only informs here: the deposit is made, and whatever the
/// checks say there is nothing else to wait for, so a failed check or a
/// failed read is a warning, never a refusal. With the owner's anchors on,
/// the history reads are capped as in a new deposit's preflight.
async fn resume_plan(
    d: &Deps,
    bridge_acc: [u8; 32],
    chain_id: u64,
    an: &AnPreflight,
) -> AnchorPlan {
    let ui = d.ui.as_ref();
    let owner = an.owner_anchors_enabled;
    let readiness = match an.light_client {
        None => Err(LcFailure::NoLightClient),
        Some(lc) => {
            let cap = owner.then_some(lc_readiness::OWNER_MODE_PAGE_CAP);
            match lc_readiness::observe(
                d.an.as_ref(),
                d.evm.as_ref(),
                bridge_acc,
                lc,
                chain_id,
                ui,
                cap,
            )
            .await
            {
                Ok(o) => lc_readiness::judge(&o, chain_id),
                Err(e) => {
                    match e.downcast_ref::<HistoryCapped>() {
                        Some(c) if owner => ui.status(&format!(
                            "{c}; the anchor is expected from the bridge owner"
                        )),
                        _ => ui.warn(&format!(
                            "{e:#}; whether the light client can anchor this deposit is unknown"
                        )),
                    }
                    return if owner {
                        AnchorPlan::Owner {
                            lc_ready: false,
                        }
                    } else {
                        AnchorPlan::LightClient
                    };
                },
            }
        },
    };
    lc_readiness::plan(owner, an.light_client, readiness).unwrap_or_else(|f| {
        ui.warn(&format!(
            "owner anchors are switched off and the light client cannot anchor this deposit yet \
             ({f}); waiting for it anyway, as only the bridge operator can help"
        ));
        AnchorPlan::LightClient
    })
}

/// The prover directory a resume needs. While a proof is still to be
/// built it must be given and pass its checks, and its lock is probed as a
/// new deposit's preflight probes it. After the proof, a broken or missing
/// directory is only a warning: the proof may be on disk, or somebody may
/// have finalized the deposit already. Without `--deposit-prover-dir` the
/// directory is the empty path, which the prover refuses by naming the
/// flag should the proof have to be built again.
fn resume_prover(p: &DepositParams, rec: &OpRecord, ui: &dyn Ui) -> CliResult<ProverDir> {
    let to_prove = matches!(
        rec.stage,
        OpStage::Requested
            | OpStage::Signed
            | OpStage::Abandoned
            | OpStage::Confirmed
            | OpStage::Anchored
    );
    let no_prover = |why: String| {
        let on_disk = crate::deposit::run::work_dir_of(p, rec)
            .and_then(|w| crate::deposit::prover::load(&w))
            .is_some();
        if !on_disk {
            ui.warn(&format!(
                "the proof is not on disk and {why}; it is needed only if the deposit is not \
                 finalized yet"
            ));
        }
        ProverDir {
            root: p.prover_dir.clone().unwrap_or_default(),
        }
    };
    let Some(root) = &p.prover_dir else {
        if to_prove {
            return Err(refuse(
                "--deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR) is needed to build the proof of \
                 this deposit, and none was given"
                    .into(),
            ));
        }
        return Ok(no_prover(
            "no --deposit-prover-dir (BRIDGE_DEPOSIT_PROVER_DIR) was given".into(),
        ));
    };
    if to_prove {
        let dir = check_prover_dir(root).map_err(refuse)?;
        drop(ProverLock::try_take(&dir)?);
        return Ok(dir);
    }
    Ok(check_prover_dir(root)
        .unwrap_or_else(|e| no_prover(format!("the prover directory is unusable ({e})"))))
}

/// What `--dry-run` prints after a passed preflight: both transactions'
/// calldata, their EIP-681 URIs and who would anchor the deposit — the
/// writer, and whether the light client passed its checks, which with the
/// owner's anchors on tells "the bridge owner or the light client" from
/// "the bridge owner" alone. No operation exists, and nothing was sent.
pub fn dry_run_summary(p: &DepositParams, c: &Checked) -> DepositSuccess {
    let net: Network = p.network.expect("validated for a fresh run");
    let to = p.to.expect("validated for a fresh run");
    let amount = c.params.amount_units;
    let approve = TxPurpose::Approve {
        token: c.evm.usdc,
        spender: c.params.bridge,
        amount: U256::from(amount),
    };
    let deposit = TxPurpose::Deposit {
        bridge: c.params.bridge,
        amount,
        account: to.account_b256(),
    };
    // The anchor's writer as the summary names it, and whether the light
    // client can anchor the deposit.
    let (writer, light_client_ready) = match c.plan {
        AnchorPlan::LightClient => ("light-client", true),
        AnchorPlan::Owner {
            lc_ready,
        } => ("owner", lc_ready),
    };
    DepositSuccess {
        op_id: None,
        dry_run: true,
        network: net.name().into(),
        chain_id: net.chain_id(),
        amount: p.amount.expect("validated for a fresh run").display(),
        to: to.extended(),
        deposit: None,
        anchor: Some(json!({ "writer": writer, "light_client_ready": light_client_ready })),
        tx: Some(json!({
            "approve_calldata": format!("0x{}", hex::encode(approve_calldata(c.params.bridge, U256::from(amount)))),
            "deposit_calldata": format!("0x{}", hex::encode(deposit_calldata(amount, to.account_b256()))),
            "eip681": [eip681::uri(net.chain_id(), &approve), eip681::uri(net.chain_id(), &deposit)],
            "walletconnect": "a pairing URI is generated when the deposit runs",
        })),
        confirmation: None,
        balance: None,
        abandoned: false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::Ordering, Arc};

    use super::*;
    use crate::deposit::{
        args::RunMode,
        lc_readiness::{AnchorPlan, OWNER_MODE_PAGE_CAP},
        locks::{DirLock, ProverLock},
        store::{FailReason, OpParams, OpRecord, OpStage, Store},
        testkit::*,
        ui::RecordingUi,
    };

    #[tokio::test]
    async fn a_dry_run_sends_nothing_and_writes_nothing() {
        let world = World::healthy(); // FakeEvm + FakeAn + prover dir, see testkit
        let p = world.params(RunMode::DryRun);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &world.deps(), &store, None).await.unwrap();
        let s = dry_run_summary(&p, &c);
        assert!(s.dry_run);
        assert_eq!(s.op_id, None);
        assert_eq!(store.list().unwrap().len(), 0);
        assert_eq!(*world.an.sent.lock().unwrap(), 0);
        let j = serde_json::to_value(&s).unwrap();
        assert!(j["tx"]["deposit_calldata"]
            .as_str()
            .unwrap()
            .starts_with("0xa41d0229"));
        assert!(j["tx"]["approve_calldata"]
            .as_str()
            .unwrap()
            .starts_with("0x095ea7b3"));
        let uris = j["tx"]["eip681"].as_array().unwrap();
        assert_eq!(uris.len(), 2, "{uris:?}");
        assert!(uris[0].as_str().unwrap().contains("/approve?"), "{uris:?}");
        assert!(uris[1].as_str().unwrap().contains("/deposit?"), "{uris:?}");
        assert_eq!(j["anchor"]["writer"], "owner");
        // The light client's head is no block on the chain: owner only.
        assert_eq!(j["anchor"]["light_client_ready"], false);
        assert_eq!(j["to"], World::target().extended());
        assert_eq!(j["amount"], "12.500000");
    }

    #[tokio::test]
    async fn owner_anchors_off_and_the_shellnet_light_client_is_exit_2_naming_check_2() {
        let world = World::healthy();
        world.owner_anchors(false);
        world.light_client_head_in_pi_form(); // the state of 2026-09-28
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(e.to_string().contains("check 2"), "{e}");
        // The head was found in its reversed-halves form; what fails is
        // the bridge's answer for the real block hash.
        assert!(
            e.to_string().contains("does not accept its head block"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn a_busy_prover_directory_does_not_fail_preflight() {
        let world = World::healthy();
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let _proving = ProverLock::try_take(&world.prover.1).unwrap().unwrap();
        check(&p, &world.deps(), &store, None).await.unwrap();
    }

    #[tokio::test]
    async fn the_four_combinations_of_pause_and_anchor_config() {
        for (paused, owner, lc_ready, ok) in [
            (false, true, false, true),
            (false, false, true, true),
            (true, true, false, false),
            (false, false, false, false),
            (true, false, true, false),
        ] {
            let world = World::healthy();
            world.paused(paused);
            world.owner_anchors(owner);
            if lc_ready {
                world.light_client_ready();
            }
            let p = world.params(RunMode::Fresh);
            let store = Store::open(&p.state_dir).unwrap();
            let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
            assert_eq!(
                check(&p, &world.deps(), &store, None).await.is_ok(),
                ok,
                "paused={paused} owner={owner} lc={lc_ready}"
            );
        }
    }

    #[tokio::test]
    async fn a_ready_light_client_is_named_whether_or_not_the_owner_can_anchor() {
        for (owner, plan, writer) in [
            (
                true,
                AnchorPlan::Owner {
                    lc_ready: true,
                },
                "owner",
            ),
            (false, AnchorPlan::LightClient, "light-client"),
        ] {
            let world = World::healthy();
            world.owner_anchors(owner);
            world.light_client_ready();
            let p = world.params(RunMode::DryRun);
            let store = Store::open(&p.state_dir).unwrap();
            let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
            let c = check(&p, &world.deps(), &store, None).await.unwrap();
            assert_eq!(c.plan, plan, "owner={owner}");
            let j = serde_json::to_value(dry_run_summary(&p, &c)).unwrap();
            assert_eq!(j["anchor"]["writer"], writer, "owner={owner}");
            assert_eq!(j["anchor"]["light_client_ready"], true, "owner={owner}");
        }
    }

    #[tokio::test]
    async fn with_owner_anchors_on_a_failed_light_client_read_is_a_warning_not_a_refusal() {
        let world = World::healthy();
        world
            .an
            .failing_getters
            .lock()
            .unwrap()
            .insert("getConfig".into());
        let ui = Arc::new(RecordingUi::new(true));
        let mut d = world.deps();
        d.ui = ui.clone();
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &d, &store, None).await.unwrap();
        assert_eq!(c.plan, AnchorPlan::Owner {
            lc_ready: false
        });
        assert!(
            ui.warnings()
                .iter()
                .any(|w| w.contains("could not read the light client's getConfig")),
            "{:?}",
            ui.warnings()
        );
    }

    #[tokio::test]
    async fn with_owner_anchors_off_a_failed_light_client_read_is_exit_2_naming_it() {
        let world = World::healthy();
        world.owner_anchors(false);
        world.light_client_ready();
        world
            .an
            .failing_getters
            .lock()
            .unwrap()
            .insert("getConfig".into());
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(
            e.to_string()
                .contains("could not read the light client's getConfig"),
            "{e}"
        );
    }

    /// The bridge's inbound history: `n` messages, none a switch-off,
    /// served one per page.
    fn long_history_without_a_switch_off(world: &World, n: usize) {
        let bridge = format!("0:{}", hex::encode(W_BRIDGE_ACC));
        let items = (0..n)
            .map(|i| msg(&format!("m{i}"), "ExtIn", "", &bridge, None))
            .collect();
        world.an.ext_in.lock().unwrap().insert(W_BRIDGE_ACC, items);
        world.an.page_size.store(1, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn owner_mode_reads_a_bounded_history_and_light_client_mode_reads_all_of_it() {
        let pages = OWNER_MODE_PAGE_CAP + 2;

        let owner = World::healthy();
        owner.light_client_ready();
        long_history_without_a_switch_off(&owner, pages);
        let ui = Arc::new(RecordingUi::new(true));
        let mut d = owner.deps();
        d.ui = ui.clone();
        let p = owner.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &d, &store, None).await.unwrap();
        assert_eq!(c.plan, AnchorPlan::Owner {
            lc_ready: false
        });
        assert_eq!(
            owner.an.pages_read.load(Ordering::SeqCst) as usize,
            OWNER_MODE_PAGE_CAP
        );
        // A history longer than the cap is what owner mode normally
        // looks like: a status line, not a warning.
        assert!(
            !ui.warnings()
                .iter()
                .any(|w| w.contains("longer than") || w.contains("light client")),
            "{:?}",
            ui.warnings()
        );
        assert!(
            ui.statuses().iter().any(|s| s.contains("longer than")
                && s.contains("pages")
                && s.contains("bridge owner")),
            "{:?}",
            ui.statuses()
        );

        let lc = World::healthy();
        lc.owner_anchors(false);
        lc.light_client_ready();
        long_history_without_a_switch_off(&lc, pages);
        let p = lc.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &lc.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(e.to_string().contains("check 3"), "{e}");
        assert_eq!(lc.an.pages_read.load(Ordering::SeqCst) as usize, pages);
    }

    #[tokio::test]
    async fn in_owner_mode_a_history_that_cannot_be_read_warns() {
        let world = World::healthy();
        world.light_client_ready();
        world.an.failing_ext.lock().unwrap().insert(W_BRIDGE_ACC);
        let ui = Arc::new(RecordingUi::new(true));
        let mut d = world.deps();
        d.ui = ui.clone();
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &d, &store, None).await.unwrap();
        assert_eq!(c.plan, AnchorPlan::Owner {
            lc_ready: false
        });
        assert!(
            ui.warnings()
                .iter()
                .any(|w| w.contains("could not read the bridge's inbound external messages")),
            "{:?}",
            ui.warnings()
        );
        assert!(
            !ui.statuses().iter().any(|s| s.contains("longer than")),
            "{:?}",
            ui.statuses()
        );
    }

    #[tokio::test]
    async fn an_unknown_outcome_for_the_same_deposit_is_exit_3_with_its_op_id() {
        let world = World::healthy();
        let op = world.left_requested_operation_from(world.wallet.account);
        let store = Store::open(world.state.path()).unwrap();
        let _dir = DirLock::try_take(world.state.path()).unwrap().unwrap();
        let p = world.params(RunMode::Fresh);
        let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::DuplicateRefused);
        assert_eq!(e.op_id(), Some(op.as_str()));
        assert!(e.to_string().contains(&format!("--resume {op}")), "{e}");
        // Another amount is another deposit.
        let other = world.params_with_other_amount(RunMode::Fresh);
        check(&other, &world.deps(), &store, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_reservation_whose_run_died_is_closed_and_does_not_block() {
        let world = World::healthy();
        let store = Store::open(world.state.path()).unwrap();
        let mut dead = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 11_155_111,
                bridge: W_BRIDGE,
                to: World::target().extended(),
                amount_units: W_AMOUNT,
                an_bridge: hex::encode(W_BRIDGE_ACC),
                an_network: "http://gql.invalid:80".into(),
            },
            hex::encode(crate::deposit::identity::EXPECTED_VOUCHER_CODE_HASH),
        );
        store.write(&mut dead).unwrap();
        let _dir = DirLock::try_take(world.state.path()).unwrap().unwrap();
        let p = world.params(RunMode::Fresh);
        check(&p, &world.deps(), &store, None).await.unwrap();
        let closed = store.load(&dead.op_id).unwrap();
        assert_eq!(closed.stage, OpStage::Failed);
        assert_eq!(closed.failure.unwrap().reason, FailReason::Interrupted);
    }

    #[tokio::test]
    async fn a_prover_directory_without_its_circuit_config_is_exit_2() {
        let world = World::healthy();
        std::fs::remove_file(
            world
                .prover
                .1
                .root
                .join(crate::deposit::prover_files::CIRCUIT_PARAMS),
        )
        .unwrap();
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(e.to_string().contains("circuit_params.json"), "{e}");
    }

    #[tokio::test]
    async fn an_amount_past_the_bridge_limit_is_exit_2() {
        let world = World::healthy();
        let mut p = world.params(RunMode::Fresh);
        p.amount = Some(crate::args::UsdcAmount(u128::from(u64::MAX) + 1));
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(e.to_string().contains("MAX_DEPOSIT_AMOUNT"), "{e}");
    }

    #[test]
    fn live_polls_read_the_anchor_every_30_seconds() {
        let p = Polls::live();
        assert_eq!(p.evm, std::time::Duration::from_secs(12));
        assert_eq!(p.anchor, std::time::Duration::from_secs(30));
        assert_eq!(p.credit, std::time::Duration::from_secs(10));
        assert_eq!(p.wallet, std::time::Duration::from_secs(3));
    }
}
