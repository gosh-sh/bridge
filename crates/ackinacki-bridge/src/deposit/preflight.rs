//! The checks a new deposit passes before the wallet is asked for
//! anything, in order: the EVM chain and bridge, the Acki Nacki bridge,
//! whether the light client can anchor the deposit, the prover directory
//! and whether its tools run on this host, the work directory, and the
//! operations still open in the state directory. A failure is
//! exit 2 (exit 3 for an open operation of the same deposit), and nothing
//! has been sent. Nothing here asks the wallet or creates an operation.
//!
//! A resume passes a smaller set of checks, only those its operation's
//! recorded stage still needs ([`context_for_resume`]).

use std::{io::Write as _, path::Path, sync::Arc, time::Duration};

use alloy_primitives::{Address, U256};
use serde_json::json;

use crate::{
    args::{UsdcAmount, USDC_DECIMALS},
    deposit::{
        an::{AnRead, AnSend},
        an_preflight::{self, AnPreflight},
        args::{an_network_id, AnTarget, DepositParams, Network},
        evm::{approve_calldata, deposit_calldata, EvmRead},
        evm_preflight::{self, EvmPreflight},
        lc_readiness::{self, AnchorPlan, HistoryCapped, LcFailure},
        locks::ProverLock,
        prover::{probe_tools, TOOL_PROBE_TIMEOUT},
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

/// `--amount` in the deposited token's base units.
///
/// The flag is parsed as a decimal amount with USDC's six places, so the
/// bridge's own USDC keeps its documented behaviour exactly, a token with
/// more decimals scales up, and one with fewer has to divide exactly —
/// an amount its precision cannot hold is refused rather than truncated.
/// The ceiling is `u64`: `gosh.mintecc` takes a `uint64`, which is also
/// the contract's `MAX_DEPOSIT_AMOUNT`.
fn base_units(typed: UsdcAmount, decimals: u8, token: Address) -> CliResult<u64> {
    let micros = U256::from(typed.0);
    let listed = u32::from(decimals);
    let scaled = if listed >= USDC_DECIMALS {
        let factor = U256::from(10u64).pow(U256::from(listed - USDC_DECIMALS));
        micros.checked_mul(factor).ok_or_else(|| {
            refuse(format!(
                "{} of {token} does not fit in 256 bits at {decimals} decimals",
                typed.display()
            ))
        })?
    } else {
        let factor = U256::from(10u64).pow(U256::from(USDC_DECIMALS - listed));
        if !(micros % factor).is_zero() {
            return Err(refuse(format!(
                "{token} has {decimals} decimals and cannot hold {}: give an amount with at most \
                 {decimals} fractional digits",
                typed.display()
            )));
        }
        micros / factor
    };
    u64::try_from(scaled).map_err(|_| {
        refuse(format!(
            "{} of {token} is {scaled} base units, above the bridge's MAX_DEPOSIT_AMOUNT of {} \
             base units",
            typed.display(),
            u64::MAX
        ))
    })
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
    // Which ERC-20 this deposit moves. `--token` names it; left out, the
    // bridge's own `usdc()` is deposited, which is what every single-token
    // invocation meant. Either way it has to be listed and open for
    // deposits, and the per-token caps are the bridge's, not the USDC
    // constants.
    let bridge_usdc = crate::deposit::evm::read_usdc(d.evm.as_ref(), bridge)
        .await
        .map_err(|e| refuse(format!("cannot read the bridge's usdc(): {e}")))?;
    let token = p.token.unwrap_or(bridge_usdc);
    let listing = crate::deposit::evm::read_token_listing(d.evm.as_ref(), bridge, token)
        .await
        .map_err(|e| refuse(format!("cannot read the bridge's listing for {token}: {e}")))?
        .ok_or_else(|| {
            refuse(format!(
                "{token} is not listed on bridge {bridge}; the owner lists a token with \
                 listToken(token, tokenId, decimals, maxTransfer, tvlCap)"
            ))
        })?;
    if !listing.deposits_enabled {
        return Err(refuse(format!(
            "deposits are closed for {token} (token id {}); withdrawals are unaffected",
            listing.token_id
        )));
    }
    // `--amount` is the token's own decimal amount, so the listing's
    // precision — not USDC's six places — decides the base units the
    // bridge is asked for.
    let amount = base_units(typed, listing.decimals, token)?;
    if U256::from(amount) > listing.max_transfer {
        return Err(refuse(format!(
            "deposit exceeds the bridge's per-token maxTransfer for {token} ({} base units)",
            listing.max_transfer
        )));
    }
    if listing.liabilities > listing.tvl_cap
        || U256::from(amount) > listing.tvl_cap - listing.liabilities
    {
        return Err(refuse(format!(
            "bridge {bridge} would exceed its tvlCap for {token}; wait for withdrawals and retry"
        )));
    }
    let evm = evm_preflight::run(
        d.evm.as_ref(),
        d.ui.as_ref(),
        net,
        bridge,
        token,
        listing.decimals,
        amount,
        from,
    )
    .await?;
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
    // The Acki Nacki side mints bridged USDC for every proof it accepts and
    // measures it against USDC-denominated limits, so until a deployment
    // carries the token registry only the EVM bridge's own token can be
    // credited. Refusing here keeps the deposit from leaving L1 for a credit
    // that would be wrong or rejected.
    if token != bridge_usdc && an.version < an_preflight::AN_TOKEN_REGISTRY_BRIDGE_VERSION {
        return Err(refuse(format!(
            "the Acki Nacki bridge is version {}, which credits every accepted deposit as bridged \
             USDC whatever token the proof carries; {} and newer credit the proven token. \
             Depositing {token} would be minted as USDC or refused after the money has left L1. \
             Deposit {bridge_usdc}, or wait for the Acki Nacki bridge to be upgraded",
            an.version,
            an_preflight::AN_TOKEN_REGISTRY_BRIDGE_VERSION
        )));
    }
    let limits =
        an_preflight::read_volume_limits(d.an.as_ref(), bridge_acc, net.chain_id()).await?;
    let requested = U256::from(amount);
    if requested > limits.max_transfer {
        return Err(refuse(format!(
            "deposit exceeds AN MAX_TRANSFER_AMOUNT ({} micro-USDC)",
            limits.max_transfer
        )));
    }
    if limits.outstanding > limits.max_tvl || requested > limits.max_tvl - limits.outstanding {
        return Err(refuse(format!(
            "AN chain {} would exceed MAX_BRIDGE_TVL; wait for withdrawals and retry",
            net.chain_id()
        )));
    }
    let plan = anchor_plan(d, bridge_acc, net.chain_id(), &an).await?;
    let prover = usable_prover(p.prover_dir.as_deref().expect("validated")).await?;
    // The proof files go there: a path that cannot hold them is refused
    // now, not at step 7 with the USDC already in the bridge.
    writable_work_dir(p.work_dir.as_deref().expect("validated for a fresh run"))
        .map_err(|e| refuse(format!("--work-dir (BRIDGE_WORK_DIR): {e}")))?;
    let params = OpParams {
        chain_id: net.chain_id(),
        bridge,
        to: to.extended(),
        amount_units: amount,
        token,
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

/// The prover directory at `root`, checked as a proof needs it: its files
/// ([`check_prover_dir`]), its lock, and its two tools, run once. A refusal
/// names what failed.
async fn usable_prover(root: &Path) -> CliResult<ProverDir> {
    let dir = check_prover_dir(root).map_err(refuse)?;
    // The prover's lock is probed now: a filesystem without flock is
    // refused here, before the wallet, not when the proof is due with the
    // USDC already in the bridge. Held means another deposit is proving,
    // which is fine; the lock is let go at once.
    drop(ProverLock::try_take(&dir)?);
    // Executable is not yet runnable on this host.
    probe_tools(&dir, TOOL_PROBE_TIMEOUT)
        .await
        .map_err(refuse)?;
    Ok(dir)
}

/// Creates the work directory `dir` if it is missing and proves that a
/// file can be written there: a probe file is created, written, synced and
/// removed. The error names the directory and quotes the OS.
fn writable_work_dir(dir: &Path) -> Result<(), String> {
    let failed = |what: &str, e: std::io::Error| format!("{} {what}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(|e| failed("cannot be created", e))?;
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    let written = std::fs::File::create(&probe).and_then(|mut f| {
        f.write_all(b"probe")?;
        f.sync_all()
    });
    let removed = std::fs::remove_file(&probe);
    written.map_err(|e| failed("cannot be written", e))?;
    removed.map_err(|e| failed("keeps a file it cannot remove", e))
}

/// The anchor plan on a chain the bridge has an Arbitrum verifier for:
/// `arbitrum-anchors` proves the block through the verifier, and while owner
/// anchors are on the owner can anchor it too, so the wait names the owner's
/// call as it does on a chain with a light client.
fn verifier_plan(an: &AnPreflight) -> AnchorPlan {
    if an.owner_anchors_enabled {
        AnchorPlan::Owner {
            lc_ready: true,
        }
    } else {
        AnchorPlan::LightClient
    }
}

/// Who anchors the deposit's block. On a chain the bridge has an Arbitrum
/// verifier for, the verifier, in the light client's place, and the owner
/// as well while owner anchors are on. Otherwise,
/// with the owner's anchors on, the light client's checks only choose the
/// status text: they read a bounded history, and a read that fails leaves
/// the light client out with a warning. With them off, the deposit goes
/// ahead only if the light client passes all three checks, and a read that
/// fails refuses it.
async fn anchor_plan(
    d: &Deps,
    bridge_acc: [u8; 32],
    chain_id: u64,
    an: &AnPreflight,
) -> CliResult<AnchorPlan> {
    if an.l2_verifier.is_some() {
        return Ok(verifier_plan(an));
    }
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
/// the prover directory and the work directory are checked only while a
/// proof is still to be built. The bridge version is checked always:
/// resending `finalizeDeposit` is safe only on a fixed bridge.
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
            other => other.into_reason(),
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
    let prover = resume_prover(p, rec, ui).await.map_err(held)?;
    // The proof files go to the work directory the operation records.
    if proof_to_build(rec.stage) {
        if let Some(work) = crate::deposit::run::work_dir_of(p, rec) {
            writable_work_dir(&work).map_err(|e| {
                held(refuse(format!(
                    "the work directory of operation {op}, set by --work-dir (BRIDGE_WORK_DIR) \
                     when it was made: {e}"
                )))
            })?;
        }
    }
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
        _ if an.l2_verifier.is_some() => verifier_plan(&an),
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
        // resumed operation is past them, and the record names the token
        // its calldata used.
        token: rec.params.token,
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
    if an.l2_verifier.is_some() {
        return verifier_plan(an);
    }
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

/// Whether an operation at `stage` still has its proof to build.
fn proof_to_build(stage: OpStage) -> bool {
    matches!(
        stage,
        OpStage::Requested
            | OpStage::Signed
            | OpStage::Abandoned
            | OpStage::Confirmed
            | OpStage::Anchored
    )
}

/// The prover directory a resume needs. While a proof is still to be
/// built it must be given and pass the checks of a new deposit's preflight
/// ([`usable_prover`]), its tools run once included. After the proof, a
/// broken or missing directory is only a warning: the proof may be on
/// disk, or somebody may have finalized the deposit already. Without
/// `--deposit-prover-dir` the directory is the empty path, which the prover
/// refuses by naming the flag should the proof have to be built again.
async fn resume_prover(p: &DepositParams, rec: &OpRecord, ui: &dyn Ui) -> CliResult<ProverDir> {
    let to_prove = proof_to_build(rec.stage);
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
        return usable_prover(root).await;
    }
    let checked = match check_prover_dir(root) {
        Ok(dir) => probe_tools(&dir, TOOL_PROBE_TIMEOUT).await.map(|()| dir),
        Err(e) => Err(e),
    };
    Ok(checked.unwrap_or_else(|e| no_prover(format!("the prover directory is unusable ({e})"))))
}

/// What `--dry-run` prints after a passed preflight: both transactions'
/// calldata, their EIP-681 URIs and who would anchor the deposit — the
/// writer, and whether the light client passed its checks, which with the
/// owner's anchors on tells "the bridge owner or a relayer's proof" from
/// "the bridge owner" alone. No operation exists, and nothing was sent.
pub fn dry_run_summary(p: &DepositParams, c: &Checked) -> DepositSuccess {
    let net: Network = p.network.expect("validated for a fresh run");
    let to = p.to.expect("validated for a fresh run");
    let amount = c.params.amount_units;
    let token = c.params.token;
    let approve = TxPurpose::Approve {
        token,
        spender: c.params.bridge,
        amount: U256::from(amount),
    };
    let deposit = TxPurpose::Deposit {
        bridge: c.params.bridge,
        token,
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
        network: net.arg(),
        chain_id: net.chain_id(),
        amount: p.amount.expect("validated for a fresh run").display(),
        to: to.extended(),
        deposit: None,
        anchor: Some(json!({ "writer": writer, "light_client_ready": light_client_ready })),
        tx: Some(json!({
            "approve_calldata": format!("0x{}", hex::encode(approve_calldata(c.params.bridge, U256::from(amount)))),
            "deposit_calldata": format!("0x{}", hex::encode(deposit_calldata(token, amount, to.account_b256()))),
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
        let world = World::healthy(); // FakeEvm + FakeAn + prover dir, see
                                      // testkit
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
            .starts_with("0x26b3293f"));
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
    async fn an_arbitrum_verifier_takes_the_light_clients_place() {
        let world = World::healthy();
        world.arbitrum_verifier();
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &world.deps(), &store, None).await.unwrap();
        assert_eq!(c.plan, AnchorPlan::LightClient);
        assert_eq!(c.an.light_client, None);
        assert!(!c.an.owner_anchors_enabled);
    }

    /// While owner anchors are on, the owner can anchor an Arbitrum block as
    /// well, and the wait names the owner's call as it does on Sepolia.
    #[tokio::test]
    async fn with_owner_anchors_on_a_verifier_chain_also_waits_for_the_owner() {
        let world = World::healthy();
        world.arbitrum_verifier();
        world.an.getter(W_BRIDGE_ACC, "isOwnerAnchorsEnabled", vec![
            json!({ "value0": true }),
        ]);
        let p = world.params(RunMode::Fresh);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &world.deps(), &store, None).await.unwrap();
        assert_eq!(c.plan, AnchorPlan::Owner {
            lc_ready: true
        });
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
                token: alloy_primitives::Address::repeat_byte(0x7c),
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

    /// The caps a deposit is refused by are the bridge's listing for the
    /// token, not the USDC constants. Each refusal is exit 2 and names the
    /// token; nothing is sent and nothing is written.
    #[tokio::test]
    async fn the_listing_decides_every_volume_refusal() {
        for (set_up, needle) in [
            (
                Box::new(|w: &World| w.evm.set_token_unlisted(W_BRIDGE)) as Box<dyn Fn(&World)>,
                "is not listed",
            ),
            (
                Box::new(|w: &World| {
                    w.evm.set_token_listing(
                        W_BRIDGE,
                        W_USDC,
                        3,
                        6,
                        false,
                        50_000_000,
                        10_000_000_000,
                        0,
                    )
                }),
                "deposits are closed",
            ),
            (
                Box::new(|w: &World| {
                    w.evm.set_token_listing(
                        W_BRIDGE,
                        W_USDC,
                        3,
                        6,
                        true,
                        u128::from(W_AMOUNT) - 1,
                        10_000_000_000,
                        0,
                    )
                }),
                "maxTransfer",
            ),
            (
                Box::new(|w: &World| {
                    w.evm.set_token_listing(
                        W_BRIDGE,
                        W_USDC,
                        3,
                        6,
                        true,
                        50_000_000,
                        10_000_000_000,
                        10_000_000_000 - u128::from(W_AMOUNT) + 1,
                    )
                }),
                "tvlCap",
            ),
        ] {
            let world = World::healthy();
            set_up(&world);
            let p = world.params(RunMode::Fresh);
            let store = Store::open(&p.state_dir).unwrap();
            let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
            let e = check(&p, &world.deps(), &store, None).await.unwrap_err();
            assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
            assert!(e.to_string().contains(needle), "{needle}: {e}");
            assert_eq!(store.list().unwrap().len(), 0);
            assert_eq!(*world.an.sent.lock().unwrap(), 0);
        }
    }

    /// `--amount` is read in the deposited token's own precision. The USDC
    /// route keeps its six places exactly; a token with more decimals
    /// scales up; one with fewer refuses an amount it cannot hold; and the
    /// `uint64` ceiling `gosh.mintecc` imposes is still the ceiling.
    #[test]
    fn an_amount_is_scaled_by_the_tokens_own_precision() {
        let t = Address::repeat_byte(0x11);
        assert_eq!(base_units(UsdcAmount(1_000_000), 6, t).unwrap(), 1_000_000);
        assert_eq!(
            base_units(UsdcAmount(1_000_000), 18, t).unwrap(),
            1_000_000_000_000_000_000
        );
        assert_eq!(base_units(UsdcAmount(1_500_000), 2, t).unwrap(), 150);
        let e = base_units(UsdcAmount(1_505_000), 2, t).unwrap_err();
        assert!(e.to_string().contains("cannot hold"), "{e}");
        assert_eq!(
            base_units(UsdcAmount(18_446_744), 18, t).unwrap(),
            18_446_744_000_000_000_000
        );
        let e = base_units(UsdcAmount(18_446_745), 18, t).unwrap_err();
        assert!(e.to_string().contains("MAX_DEPOSIT_AMOUNT"), "{e}");
    }

    /// A second listed ERC-20 is refused before the wallet is asked, while
    /// the Acki Nacki bridge still credits every accepted proof as bridged
    /// USDC. The money must not leave L1 for a credit that would be wrong.
    #[tokio::test]
    async fn a_second_token_is_refused_until_the_an_bridge_credits_it() {
        use alloy::sol_types::{SolCall, SolValue};

        use crate::deposit::evm::IErc20;
        const DAI: Address = alloy_primitives::address!("68194a729c2450ad26072b3d33adacbcef39d574");
        const ONE: u128 = 1_000_000_000_000_000_000;
        let world = World::healthy();
        world
            .evm
            .set_token_listing(W_BRIDGE, DAI, 7, 18, true, ONE * 50, ONE * 10_000, 0);
        world.evm.set_call(
            DAI,
            IErc20::decimalsCall::SELECTOR,
            U256::from(18u8).abi_encode().into(),
        );
        world.evm.set_call(
            DAI,
            IErc20::balanceOfCall::SELECTOR,
            U256::from(ONE * 2).abi_encode().into(),
        );
        world.evm.set_call(
            DAI,
            IErc20::allowanceCall::SELECTOR,
            U256::ZERO.abi_encode().into(),
        );
        let mut p = world.params(RunMode::Fresh);
        p.token = Some(DAI);
        p.amount = Some(UsdcAmount(1_000_000));
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let e = check(&p, &world.deps(), &store, Some(sepolia_sender()))
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
        assert!(e.to_string().contains("bridged USDC"), "{e}");
        assert!(
            e.to_string()
                .contains(&an_preflight::AN_TOKEN_REGISTRY_BRIDGE_VERSION.to_string()),
            "{e}"
        );
        assert_eq!(store.list().unwrap().len(), 0);
        assert_eq!(*world.an.sent.lock().unwrap(), 0);
    }

    /// Naming the bridge's own token explicitly is the same deposit as
    /// leaving `--token` out: the listing is read either way, and the
    /// calldata carries the token.
    #[tokio::test]
    async fn naming_the_bridges_own_token_is_the_same_deposit() {
        let world = World::healthy();
        let mut p = world.params(RunMode::DryRun);
        p.token = Some(W_USDC);
        let store = Store::open(&p.state_dir).unwrap();
        let _dir = DirLock::try_take(&p.state_dir).unwrap().unwrap();
        let c = check(&p, &world.deps(), &store, None).await.unwrap();
        assert_eq!(c.params.token, W_USDC);
        assert_eq!(c.params.amount_units, u64::from(W_AMOUNT));
        assert_eq!(c.evm.token, W_USDC);
        assert_eq!(c.evm.decimals, 6);
        let j = serde_json::to_value(dry_run_summary(&p, &c)).unwrap();
        let calldata = j["tx"]["deposit_calldata"].as_str().unwrap().to_string();
        assert!(calldata.starts_with("0x26b3293f"), "{calldata}");
        assert!(
            calldata.contains(&format!("{:x}", W_USDC)),
            "the calldata names the deposited token: {calldata}"
        );
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
