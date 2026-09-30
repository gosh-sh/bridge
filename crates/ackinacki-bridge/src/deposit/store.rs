//! One file per deposit operation, `<op-id>.json`, 0600, replaced
//! atomically. The record is written BEFORE each step that could make it
//! wrong: `Requested` before the wallet is asked, `Finalizing` before the
//! first `finalizeDeposit`.

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use crate::errors::{CliError, CliResult, ExitCode};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStage {
    Reserved,
    Requested,
    Signed,
    Confirmed,
    Anchored,
    Proved,
    Finalizing,
    Credited,
    Failed,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    Interrupted,
    Refused,
    Rejected,
    ApproveFailed,
    Reverted,
    NonceConsumed,
    Unprovable,
    Mismatch,
    CreditAborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpParams {
    pub chain_id: u64,
    pub bridge: Address,
    pub to: String,
    pub amount_units: u64,
    /// The Acki Nacki bridge account id (hex) and the network it was reached
    /// on (`an_network_id` of `--gql-endpoint`). Pinned at creation: a resume
    /// under another profile must not finalize this deposit through another
    /// bridge.
    pub an_bridge: String,
    pub an_network: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestInfo {
    pub nonce_before: u64,
    pub from_block: u64,
    pub calldata: Bytes,
    /// The hash the wallet answered with, before its nonce is known.
    pub wallet_hash: Option<B256>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxClaim {
    pub tx_hash: B256,
    pub tx_nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositInfo {
    pub deposit_id: U256,
    pub block_number: u64,
    pub block_hash: B256,
    pub block_log_index: u64,
    pub receipt_log_index: u64,
    /// RLP length of the transaction's access list, as the circuit sees it.
    pub access_list_rlp_len: usize,
    /// Voucher account id (hex), computed with `voucher_code_hash`.
    pub voucher_account: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizeInfo {
    pub voucher_code_hash: String,
    pub voucher_account: String,
    pub sends: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditInfo {
    pub confirm_tx: String,
    pub delivery_tx: String,
    pub via_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub at: OpStage,
    pub reason: FailReason,
    /// What the run that ended the operation said, and its exit code: a
    /// resume of a finished operation answers with the same.
    pub detail: String,
    pub exit: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpRecord {
    pub schema: u32,
    pub op_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub params: OpParams,
    /// `getDepositVoucherCodeHash()` at preflight; the voucher address is
    /// computed with it.
    pub voucher_code_hash: String,
    pub from: Option<Address>,
    pub stage: OpStage,
    pub request: Option<RequestInfo>,
    pub tx: Option<TxClaim>,
    pub deposit: Option<DepositInfo>,
    pub finalize: Option<FinalizeInfo>,
    pub credit: Option<CreditInfo>,
    pub failure: Option<Failure>,
    /// Ever released with `--abandon`. Such an operation is never bound
    /// by `nonce_before` alone.
    pub abandoned_ever: bool,
    pub work_dir: Option<PathBuf>,
    /// For the summary of a finished operation, which needs no network.
    pub an_bridge_dapp: Option<String>,
    pub anchor_writer: Option<String>,
}

impl OpRecord {
    pub fn new(op_id: String, params: OpParams, voucher_code_hash: String) -> Self {
        let now = crate::idempotency::rfc3339_now();
        Self {
            schema: SCHEMA,
            op_id,
            created_at: now.clone(),
            updated_at: now,
            params,
            voucher_code_hash,
            from: None,
            stage: OpStage::Reserved,
            request: None,
            tx: None,
            deposit: None,
            finalize: None,
            credit: None,
            failure: None,
            abandoned_ever: false,
            work_dir: None,
            an_bridge_dapp: None,
            anchor_writer: None,
        }
    }

    pub fn is_unresolved(&self) -> bool {
        matches!(self.stage, OpStage::Requested | OpStage::Signed)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.stage, OpStage::Credited | OpStage::Failed)
    }

    pub fn fail(
        &mut self,
        at: OpStage,
        reason: FailReason,
        exit: ExitCode,
        detail: impl Into<String>,
    ) {
        self.stage = OpStage::Failed;
        self.failure = Some(Failure {
            at,
            reason,
            detail: detail.into(),
            exit: exit.as_i32(),
        });
    }

    /// The fields each stage cannot exist without.
    pub fn validate(&self) -> Result<(), String> {
        use OpStage::*;
        if self.schema != SCHEMA {
            return Err(format!("schema {} is not {SCHEMA}", self.schema));
        }
        let need = |ok: bool, what: &str| {
            if ok {
                Ok(())
            } else {
                Err(format!("stage {:?} without {what}", self.stage))
            }
        };
        match self.stage {
            Reserved => Ok(()),
            Requested => need(
                self.request.is_some() && self.from.is_some(),
                "request and sender",
            ),
            Signed => need(
                self.request.is_some() && self.tx.is_some(),
                "request and transaction",
            ),
            Confirmed | Anchored | Proved => need(
                self.tx.is_some() && self.deposit.is_some(),
                "transaction and deposit",
            ),
            Finalizing => need(
                self.deposit.is_some() && self.finalize.is_some(),
                "deposit and finalize",
            ),
            Credited => need(self.credit.is_some(), "credit"),
            Failed => need(self.failure.is_some(), "failure"),
            Abandoned => need(self.request.is_some(), "request"),
        }
    }
}

fn preflight(reason: String) -> CliError {
    CliError::Preflight {
        reason,
        source: None,
    }
}

/// The directory that holds `path`'s entry; `None` for a root or an empty
/// parent (the current directory always exists).
fn entry_parent(path: &Path) -> Option<&Path> {
    match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Some(Path::new(".")),
        other => other,
    }
}

/// Make `path`'s own directory entry durable.
fn sync_entry_of(path: &Path) -> CliResult<()> {
    let Some(parent) = entry_parent(path) else {
        return Ok(());
    };
    std::fs::File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| {
            preflight(format!(
                "deposit state: could not make {} durable (fsync of {}): {e}",
                path.display(),
                parent.display()
            ))
        })
}

/// Create the levels that do not exist yet, sync each new level's entry
/// (outermost first) and restrict the innermost one to 0700. Does nothing
/// to a directory that already exists.
fn create_missing_levels(dir: &Path) -> CliResult<()> {
    if dir.exists() {
        return Ok(());
    }
    let mut created: Vec<&Path> = Vec::new();
    let mut probe = dir;
    while !probe.exists() {
        created.push(probe);
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p,
            _ => break,
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| {
        preflight(format!(
            "cannot create the deposit state directory {}: {e}",
            dir.display()
        ))
    })?;
    for level in created.iter().rev() {
        sync_entry_of(level)?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        preflight(format!(
            "created {} but could not restrict it to 0700: {e}",
            dir.display()
        ))
    })
}

/// Say, never fix, a state directory that others can reach: the records
/// carry amounts and destination addresses.
fn report_permissive_mode(dir: &Path) {
    let Ok(md) = std::fs::metadata(dir) else {
        return;
    };
    let mode = md.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::warn!(
            state_dir = %dir.display(),
            mode = format!("{mode:04o}"),
            "the deposit state directory can be reached by more than its owner; `chmod 700` is the fix if it was not deliberate",
        );
    }
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open the store, creating the directory if it is missing. Only
    /// directories created here are restricted to 0700 (a failure to do so
    /// is an error); an existing one keeps its mode and is reported when
    /// others can reach it. Every created level is made durable.
    pub fn open(dir: &Path) -> CliResult<Store> {
        if let Some(c) = crate::args::first_control_character(dir) {
            return Err(CliError::Preflight {
                reason: format!("--state-dir contains the control character {c:?}"),
                source: None,
            });
        }
        create_missing_levels(dir)?;
        sync_entry_of(dir)?;
        report_permissive_mode(dir);
        Ok(Store {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn new_op_id() -> String {
        ulid::Ulid::new().to_string()
    }

    pub fn record_path(&self, op: &str) -> PathBuf {
        self.dir.join(format!("{op}.json"))
    }

    pub fn write(&self, rec: &mut OpRecord) -> std::io::Result<()> {
        rec.updated_at = crate::idempotency::rfc3339_now();
        let bytes = serde_json::to_vec_pretty(rec).map_err(std::io::Error::other)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".op-")
            .permissions(std::fs::Permissions::from_mode(0o600))
            .tempfile_in(&self.dir)?;
        tmp.write_all(&bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(self.record_path(&rec.op_id))
            .map_err(|e| e.error)?;
        std::fs::File::open(&self.dir)?.sync_all()
    }

    pub fn load(&self, op: &str) -> CliResult<OpRecord> {
        let path = self.record_path(op);
        let raw = std::fs::read(&path).map_err(|e| CliError::Preflight {
            reason: format!(
                "deposit operation {op}: cannot read {}: {e}",
                path.display()
            ),
            source: None,
        })?;
        let rec: OpRecord = serde_json::from_slice(&raw).map_err(|e| CliError::Preflight {
            reason: format!("deposit operation {op}: {} is damaged: {e}", path.display()),
            source: None,
        })?;
        rec.validate().map_err(|e| CliError::Preflight {
            reason: format!("deposit operation {op}: {e}"),
            source: None,
        })?;
        if rec.op_id != op {
            return Err(CliError::Preflight {
                reason: format!("{} names operation {}, not {op}", path.display(), rec.op_id),
                source: None,
            });
        }
        Ok(rec)
    }

    pub fn list(&self) -> CliResult<Vec<OpRecord>> {
        let rd = std::fs::read_dir(&self.dir).map_err(|e| CliError::Preflight {
            reason: format!("cannot list {}: {e}", self.dir.display()),
            source: None,
        })?;
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(|e| CliError::Preflight {
                reason: format!("cannot list {}: {e}", self.dir.display()),
                source: None,
            })?;
            let name = entry.file_name();
            let Some(op) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if ulid::Ulid::from_string(op).is_err() {
                continue;
            }
            out.push(self.load(op)?);
        }
        out.sort_by(|a, b| a.op_id.cmp(&b.op_id));
        Ok(out)
    }
}

// -- queries. Callers hold the directory lock.

/// An operation that stops a new deposit from starting, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocking {
    pub op_id: String,
    pub stage: OpStage,
    pub why: &'static str,
}

impl std::fmt::Display for Blocking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "deposit operation {} {} (stage {:?}); nothing was sent by this run. Continue it with \
             `ackinacki-bridge deposit --resume {}`, or, after checking in the wallet that its \
             transaction does not exist, release it with `--abandon {}`",
            self.op_id, self.why, self.stage, self.op_id, self.op_id
        )
    }
}

/// True when another process holds the operation's lock.
fn is_driven_by_someone(store: &Store, op: &str) -> CliResult<bool> {
    Ok(crate::deposit::locks::OpLock::try_take(store.dir(), op)?.is_none())
}

/// Closes every `Reserved` operation whose lock is free: the run that
/// reserved it is gone and never asked the wallet. Callers hold the
/// directory lock.
pub fn close_interrupted(store: &Store, recs: &mut [OpRecord]) -> CliResult<()> {
    for r in recs.iter_mut().filter(|r| r.stage == OpStage::Reserved) {
        // Taking the lock proves nobody drives it; keep it while writing.
        let Some(_held) = crate::deposit::locks::OpLock::try_take(store.dir(), &r.op_id)? else {
            continue;
        };
        r.fail(
            OpStage::Reserved,
            FailReason::Interrupted,
            ExitCode::PreflightRefused,
            "the run that reserved it exited before asking the wallet",
        );
        store.write(r).map_err(|e| {
            preflight(format!(
                "cannot close interrupted operation {}: {e} (nothing was sent)",
                r.op_id
            ))
        })?;
    }
    Ok(())
}

/// The operation that blocks a new one with the same chain, bridge,
/// recipient and amount: an unknown EVM outcome, or a live reservation.
/// The Acki Nacki endpoint is not part of the key. Callers hold the
/// directory lock.
pub fn blocking_by_params(
    store: &Store,
    recs: &[OpRecord],
    p: &OpParams,
) -> CliResult<Option<Blocking>> {
    let same = |q: &OpParams| {
        q.chain_id == p.chain_id
            && q.bridge == p.bridge
            && q.to == p.to
            && q.amount_units == p.amount_units
    };
    for r in recs.iter().filter(|r| same(&r.params)) {
        if r.is_unresolved() {
            return Ok(Some(Blocking {
                op_id: r.op_id.clone(),
                stage: r.stage,
                why: "requested the same deposit and its outcome is still unknown",
            }));
        }
        if r.stage == OpStage::Reserved && is_driven_by_someone(store, &r.op_id)? {
            return Ok(Some(Blocking {
                op_id: r.op_id.clone(),
                stage: r.stage,
                why: "is being run by another process",
            }));
        }
    }
    Ok(None)
}

/// The operation, other than `except`, requested from `from` on this chain
/// and bridge whose EVM outcome is still unknown. Callers hold the
/// directory lock.
pub fn blocking_by_sender(
    _store: &Store,
    recs: &[OpRecord],
    chain_id: u64,
    bridge: Address,
    from: Address,
    except: &str,
) -> CliResult<Option<Blocking>> {
    Ok(recs
        .iter()
        .filter(|r| r.op_id != except && r.params.chain_id == chain_id && r.params.bridge == bridge)
        .filter(|r| r.from == Some(from) && r.is_unresolved())
        .map(|r| Blocking {
            op_id: r.op_id.clone(),
            stage: r.stage,
            why: "was requested from the same account and its outcome is still unknown",
        })
        .next())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use alloy_primitives::{address, Bytes, B256, U256};

    use super::*;

    fn params() -> OpParams {
        OpParams {
            chain_id: 11_155_111,
            bridge: address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7"),
            to: format!("{}::{}", "00".repeat(32), "a3".repeat(32)),
            amount_units: 12_500_000,
            an_bridge: "1a".repeat(32),
            an_network: "https://shellnet.ackinacki.org:443".into(),
        }
    }

    #[test]
    fn a_record_round_trips_and_is_owner_only() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        s.write(&mut r).unwrap();
        let back = s.load(&r.op_id).unwrap();
        assert_eq!(back.params, r.params);
        assert_eq!(back.stage, OpStage::Reserved);
        let mode = std::fs::metadata(s.record_path(&r.op_id))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_stage_without_its_data_is_refused_on_load() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        r.stage = OpStage::Signed; // no request, no tx
        assert!(r.validate().is_err());
        std::fs::write(s.record_path(&r.op_id), serde_json::to_vec(&r).unwrap()).unwrap();
        let e = s.load(&r.op_id).unwrap_err();
        assert!(e.to_string().contains(&r.op_id), "{e}");
    }

    #[test]
    fn listing_never_skips_a_record_it_cannot_read() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        s.write(&mut r).unwrap();
        std::fs::write(s.record_path(&Store::new_op_id()), b"{ not json").unwrap();
        assert!(
            s.list().is_err(),
            "a damaged record must not read as a missing one"
        );
    }

    #[test]
    fn unresolved_means_requested_or_signed() {
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        assert!(!r.is_unresolved());
        r.stage = OpStage::Requested;
        r.request = Some(RequestInfo {
            nonce_before: 7,
            from_block: 1,
            calldata: Bytes::new(),
            wallet_hash: None,
        });
        assert!(r.is_unresolved());
        r.stage = OpStage::Signed;
        r.tx = Some(TxClaim {
            tx_hash: B256::repeat_byte(0x11),
            tx_nonce: 7,
        });
        assert!(r.is_unresolved());
        r.stage = OpStage::Abandoned;
        assert!(!r.is_unresolved());
        r.deposit = Some(DepositInfo {
            deposit_id: U256::from(3),
            block_number: 1,
            block_hash: Default::default(),
            block_log_index: 0,
            receipt_log_index: 0,
            access_list_rlp_len: 1,
            voucher_account: String::new(),
        });
        r.stage = OpStage::Finalizing;
        r.finalize = Some(FinalizeInfo {
            voucher_code_hash: "bd44".into(),
            voucher_account: String::new(),
            sends: 1,
        });
        assert!(!r.is_unresolved(), "Finalizing does not block new deposits");
    }

    #[test]
    fn an_existing_directory_keeps_its_mode_and_a_new_one_is_owner_only() {
        let d = tempfile::tempdir().unwrap();
        let existing = d.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        Store::open(&existing).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&existing), 0o755);
        let fresh = d.path().join("a").join("b");
        Store::open(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o700);
    }

    fn requested(s: &Store, p: OpParams, from: Address) -> OpRecord {
        let mut r = OpRecord::new(Store::new_op_id(), p, "bd44".into());
        r.from = Some(from);
        r.stage = OpStage::Requested;
        r.request = Some(RequestInfo {
            nonce_before: 5,
            from_block: 1,
            calldata: Bytes::new(),
            wallet_hash: None,
        });
        s.write(&mut r).unwrap();
        r
    }

    #[test]
    fn a_dead_runs_reservation_is_closed_as_interrupted() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        s.write(&mut r).unwrap();
        let mut recs = s.list().unwrap();
        close_interrupted(&s, &mut recs).unwrap();
        assert_eq!(s.load(&r.op_id).unwrap().stage, OpStage::Failed);
        assert_eq!(
            s.load(&r.op_id).unwrap().failure.unwrap().reason,
            FailReason::Interrupted
        );
    }

    #[test]
    fn a_live_runs_reservation_is_left_alone_and_blocks() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let mut r = OpRecord::new(Store::new_op_id(), params(), "bd44".into());
        s.write(&mut r).unwrap();
        let _live = crate::deposit::locks::OpLock::try_take(d.path(), &r.op_id)
            .unwrap()
            .unwrap();
        let mut recs = s.list().unwrap();
        close_interrupted(&s, &mut recs).unwrap();
        assert_eq!(s.load(&r.op_id).unwrap().stage, OpStage::Reserved);
        assert!(blocking_by_params(&s, &recs, &params()).unwrap().is_some());
    }

    #[test]
    fn an_unknown_outcome_blocks_the_same_parameters_and_the_same_sender() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let r = requested(&s, params(), from);
        let recs = s.list().unwrap();
        let b = blocking_by_params(&s, &recs, &params()).unwrap().unwrap();
        assert_eq!(b.op_id, r.op_id);
        assert!(b.to_string().contains(&format!("--resume {}", r.op_id)));
        let mut other = params();
        other.amount_units += 1;
        assert!(blocking_by_params(&s, &recs, &other).unwrap().is_none());
        assert!(
            blocking_by_sender(&s, &recs, 11_155_111, params().bridge, from, "none")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn another_an_endpoint_does_not_lift_the_block() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let r = requested(&s, params(), from);
        let recs = s.list().unwrap();
        let mut moved = params();
        moved.an_bridge = "2b".repeat(32);
        moved.an_network = "https://other.example:443".into();
        let b = blocking_by_params(&s, &recs, &moved).unwrap().unwrap();
        assert_eq!(b.op_id, r.op_id);
    }

    #[test]
    fn an_abandoned_operation_no_longer_blocks() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path()).unwrap();
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let mut r = requested(&s, params(), from);
        r.stage = OpStage::Abandoned;
        r.abandoned_ever = true;
        s.write(&mut r).unwrap();
        let recs = s.list().unwrap();
        assert!(blocking_by_params(&s, &recs, &params()).unwrap().is_none());
        assert!(
            blocking_by_sender(&s, &recs, 11_155_111, params().bridge, from, "none")
                .unwrap()
                .is_none()
        );
    }
}
