//! The receipt proof of a deposit, kept in its work directory between the
//! `prove` step and `finalizeDeposit`, so a `--resume` sends the same
//! arguments without reading the EVM chain again.

use std::path::Path;

use deposit_receipt::DepositWitness;

use crate::deposit::store::DepositInfo;

/// The file in the work directory that holds it.
pub const FILE: &str = "receipt_proof.json";

/// The receipt proof in `work`, if there is one that reads.
pub fn load(work: &Path) -> Option<DepositWitness> {
    let bytes = std::fs::read(work.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes `w` to `work`, whole or not at all.
pub fn save(work: &Path, w: &DepositWitness) -> std::io::Result<()> {
    let tmp = work.join(format!("{FILE}.tmp"));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(w).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, work.join(FILE))
}

/// `w` proves the deposit `dep` on chain `chain_id`: the same block and the
/// same log of its receipt.
pub fn proves(w: &DepositWitness, chain_id: u64, dep: &DepositInfo) -> bool {
    w.chain_id == chain_id
        && w.block_hash == dep.block_hash
        && w.block_number == dep.block_number
        && u64::from(w.log_index) == dep.receipt_log_index
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, U256};

    use super::*;
    use crate::deposit::testkit::{fake_witness, header};

    fn dep() -> DepositInfo {
        DepositInfo {
            deposit_id: U256::from(5),
            block_number: 100,
            block_hash: B256::repeat_byte(1),
            block_log_index: 41,
            receipt_log_index: 1,
            voucher_account: String::new(),
        }
    }

    #[test]
    fn a_saved_receipt_proof_reads_back_and_proves_only_its_own_deposit() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()), None);
        let w = fake_witness(11_155_111, &header(100, 1), 1);
        save(d.path(), &w).unwrap();
        assert_eq!(load(d.path()), Some(w.clone()));
        assert!(!d.path().join(format!("{FILE}.tmp")).exists());
        assert!(proves(&w, 11_155_111, &dep()));
        assert!(!proves(&w, 1, &dep()));
        assert!(!proves(
            &fake_witness(11_155_111, &header(100, 2), 1),
            11_155_111,
            &dep()
        ));
        assert!(!proves(
            &fake_witness(11_155_111, &header(100, 1), 0),
            11_155_111,
            &dep()
        ));
    }

    #[test]
    fn a_file_that_does_not_read_is_no_receipt_proof() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(FILE), b"{\"chainId\":").unwrap();
        assert_eq!(load(d.path()), None);
    }
}
