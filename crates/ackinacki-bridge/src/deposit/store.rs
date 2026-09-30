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

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path) -> CliResult<Store> {
        if let Some(c) = crate::args::first_control_character(dir) {
            return Err(CliError::Preflight {
                reason: format!("--state-dir contains the control character {c:?}"),
                source: None,
            });
        }
        std::fs::create_dir_all(dir).map_err(|e| CliError::Preflight {
            reason: format!(
                "cannot create the deposit state directory {}: {e}",
                dir.display()
            ),
            source: None,
        })?;
        // Created 0700 on first use; an existing directory keeps its mode.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "a directory we do not own keeps its mode"
        )]
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
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
}
