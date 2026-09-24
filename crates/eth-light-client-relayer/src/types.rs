//! Beacon `LightClientFinalityUpdate` parse + step public-input packing.
//!
//! Layout matches `eth-light-client-prover/src/step.rs` (`STEP_INSTANCE_LEN =
//! 10`) and `EthBeaconLightClient._parsePublicInputs`.

use serde_json::Value;

use crate::error::RelayerError;

/// 32 slots/epoch × 256 epochs/period.
pub const SLOTS_PER_SYNC_PERIOD: u64 = 32 * 256;
/// Step circuit public inputs.
pub const STEP_INSTANCE_LEN: usize = 10;
pub const PUBLIC_INPUT_BYTES: usize = STEP_INSTANCE_LEN * 32;
/// Rotate PI: 12 accumulator limbs + current/next/period.
pub const ROTATE_ACCUMULATOR_LIMBS: usize = 12;
pub const ROTATE_INSTANCE_LEN: usize = ROTATE_ACCUMULATOR_LIMBS + 3;

/// Signing-domain inputs the beacon source resolved for one update: the
/// `fork_version` active at `signature_slot` (fork schedule from
/// `/eth/v1/config/spec`) and the network `genesis_validators_root`
/// (`/eth/v1/beacon/genesis`).
///
/// Handed to the prover subprocess as `BEACON_FORK_VERSION` /
/// `BEACON_GENESIS_VALIDATORS_ROOT` (both are circuit witnesses, the VK does
/// not change). `None` leaves the prover on its own environment, whose default
/// is mainnet Fulu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeaconChainParams {
    pub fork_version: [u8; 4],
    pub genesis_validators_root: [u8; 32],
}

impl BeaconChainParams {
    pub const ENV_FORK_VERSION: &'static str = "BEACON_FORK_VERSION";
    pub const ENV_GENESIS_VALIDATORS_ROOT: &'static str = "BEACON_GENESIS_VALIDATORS_ROOT";

    pub fn fork_version_hex(&self) -> String {
        format!("0x{}", hex::encode(self.fork_version))
    }

    pub fn genesis_validators_root_hex(&self) -> String {
        format!("0x{}", hex::encode(self.genesis_validators_root))
    }
}

/// Parsed Altair light-client finality update (beacon REST shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalityUpdate {
    pub attested_slot: u64,
    pub finalized_slot: u64,
    /// Slot the sync aggregate was signed in (`signature_slot`); the fork
    /// version of the signing domain is the one active at this slot. Falls
    /// back to `attested_slot + 1` when the JSON omits it.
    pub signature_slot: u64,
    pub attested_state_root: [u8; 32],
    pub finalized_beacon_root: [u8; 32],
    pub execution_block_hash: [u8; 32],
    pub participation: u64,
    /// Original JSON, written to disk for the subprocess prover.
    pub raw_json: String,
    /// Bootstrap or `updates?start_period=P-1` JSON (512 pubkeys). Empty in
    /// unit tests.
    pub committee_json: String,
    /// Signing domain for the prover; filled by [`crate::HttpBeaconSource`].
    pub chain: Option<BeaconChainParams>,
}

impl FinalityUpdate {
    pub fn period(&self) -> u64 {
        self.finalized_slot / SLOTS_PER_SYNC_PERIOD
    }

    /// Period of the committee that signed the attested header (the one whose
    /// commitment the step proof exposes as public input 5).
    pub fn signing_period(&self) -> u64 {
        self.attested_slot / SLOTS_PER_SYNC_PERIOD
    }
}

/// Opcode operands for `submitUpdate` (step circuit).
#[derive(Clone, Debug)]
pub struct StepProofBundle {
    pub parsed: StepPublicInputs,
    pub public_inputs: Vec<u8>,
    pub proof: Vec<u8>,
}

impl StepProofBundle {
    /// Load a prove-one / export_step_vk_blob output directory.
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, RelayerError> {
        let public_inputs = std::fs::read(dir.join("step_public_inputs.bin"))
            .or_else(|_| std::fs::read(dir.join("public_inputs.bin")))
            .map_err(|e| RelayerError::other(format!("read public_inputs: {e}")))?;
        let proof = std::fs::read(dir.join("step_proof_blake2b.bin"))
            .or_else(|_| std::fs::read(dir.join("proof.bin")))
            .map_err(|e| RelayerError::other(format!("read proof: {e}")))?;
        if public_inputs.len() != PUBLIC_INPUT_BYTES {
            return Err(RelayerError::other(format!(
                "public_inputs len {} != {PUBLIC_INPUT_BYTES}",
                public_inputs.len()
            )));
        }
        let finalized_slot = u64::from_le_bytes(public_inputs[32..40].try_into().unwrap());
        let attested_slot = u64::from_le_bytes(public_inputs[0..8].try_into().unwrap());
        Ok(Self {
            parsed: StepPublicInputs {
                attested_slot,
                finalized_slot,
                finalized_beacon_root: [0u8; 32],
                participation: 0,
                committee_commitment: {
                    let mut c = [0u8; 32];
                    c.copy_from_slice(&public_inputs[5 * 32..6 * 32]);
                    c
                },
                execution_block_hash: [0u8; 32],
                attested_state_root: [0u8; 32],
            },
            public_inputs,
            proof,
        })
    }
}

/// Decoded step PI (same order as the 10 LE Fr words).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepPublicInputs {
    pub attested_slot: u64,
    pub finalized_slot: u64,
    pub finalized_beacon_root: [u8; 32],
    pub participation: u64,
    pub committee_commitment: [u8; 32],
    pub execution_block_hash: [u8; 32],
    pub attested_state_root: [u8; 32],
}

/// Opcode operands for `submitRotate`.
#[derive(Clone, Debug)]
pub struct RotateProofBundle {
    pub public_inputs: Vec<u8>,
    pub proof: Vec<u8>,
    pub period: u64,
}

impl RotateProofBundle {
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, RelayerError> {
        let public_inputs = std::fs::read(dir.join("rotate_public_inputs.bin"))
            .map_err(|e| RelayerError::other(format!("read rotate_public_inputs.bin: {e}")))?;
        let proof = std::fs::read(dir.join("rotate_proof_blake2b.bin"))
            .map_err(|e| RelayerError::other(format!("read rotate_proof_blake2b.bin: {e}")))?;
        let want = ROTATE_INSTANCE_LEN * 32;
        if public_inputs.len() < want {
            return Err(RelayerError::other(format!(
                "rotate PI len {} < {want}",
                public_inputs.len()
            )));
        }
        let off = (ROTATE_INSTANCE_LEN - 1) * 32;
        let period = u64::from_le_bytes(public_inputs[off..off + 8].try_into().unwrap());
        Ok(Self {
            public_inputs,
            proof,
            period,
        })
    }
}

pub fn parse_finality_update(json: &str) -> Result<FinalityUpdate, RelayerError> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| RelayerError::beacon(format!("finality_update JSON: {e}")))?;
    let data = if v.get("data").is_some() {
        &v["data"]
    } else {
        &v
    };
    let attested = beacon_header(data, "attested_header")?;
    let finalized = beacon_header(data, "finalized_header")?;
    let exec_hash = hex32(
        data["finalized_header"]["execution"]["block_hash"]
            .as_str()
            .ok_or_else(|| RelayerError::beacon("missing finalized execution.block_hash"))?,
    )?;
    let bits = data["sync_aggregate"]["sync_committee_bits"]
        .as_str()
        .ok_or_else(|| RelayerError::beacon("missing sync_committee_bits"))?;
    let signature_slot = if data.get("signature_slot").is_some() {
        parse_u64_field(&data["signature_slot"], "signature_slot")?
    } else {
        attested.0 + 1
    };
    Ok(FinalityUpdate {
        attested_slot: attested.0,
        finalized_slot: finalized.0,
        signature_slot,
        attested_state_root: attested.1,
        finalized_beacon_root: finalized.1,
        execution_block_hash: exec_hash,
        participation: popcount_bits(bits)?,
        raw_json: json.to_string(),
        committee_json: String::new(),
        chain: None,
    })
}

fn beacon_header(data: &Value, which: &str) -> Result<(u64, [u8; 32]), RelayerError> {
    let h = &data[which]["beacon"];
    let slot = parse_u64_field(&h["slot"], &format!("{which}.beacon.slot"))?;
    let root = hex32(
        h["state_root"]
            .as_str()
            .ok_or_else(|| RelayerError::beacon(format!("missing {which}.beacon.state_root")))?,
    )?;
    Ok((slot, root))
}

fn parse_u64_field(v: &Value, name: &str) -> Result<u64, RelayerError> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    let s = v
        .as_str()
        .ok_or_else(|| RelayerError::beacon(format!("missing {name}")))?;
    s.parse::<u64>()
        .map_err(|e| RelayerError::beacon(format!("{name}={s}: {e}")))
}

fn hex32(s: &str) -> Result<[u8; 32], RelayerError> {
    let raw = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| RelayerError::beacon(format!("hex {s}: {e}")))?;
    raw.try_into()
        .map_err(|_| RelayerError::beacon(format!("expected 32 bytes, got {s}")))
}

fn popcount_bits(hex_bits: &str) -> Result<u64, RelayerError> {
    let raw = hex::decode(hex_bits.trim_start_matches("0x"))
        .map_err(|e| RelayerError::beacon(format!("sync_committee_bits: {e}")))?;
    Ok(raw.iter().map(|b| u64::from(b.count_ones())).sum())
}

/// Pack 10 LE-Fr words. 32-byte roots are split `hi = root[0..16] ‖ 0`,
/// `lo = root[16..32] ‖ 0` (circuit `node_hi_lo` / contract `(hi<<128)|lo`).
pub fn pack_step_public_inputs(
    update: &FinalityUpdate,
    committee_commitment: [u8; 32],
) -> (Vec<u8>, StepPublicInputs) {
    let parsed = StepPublicInputs {
        attested_slot: update.attested_slot,
        finalized_slot: update.finalized_slot,
        finalized_beacon_root: update.finalized_beacon_root,
        participation: update.participation,
        committee_commitment,
        execution_block_hash: update.execution_block_hash,
        attested_state_root: update.attested_state_root,
    };
    let mut out = Vec::with_capacity(PUBLIC_INPUT_BYTES);
    out.extend_from_slice(&u64_le_fr(parsed.attested_slot));
    out.extend_from_slice(&u64_le_fr(parsed.finalized_slot));
    let (bh, bl) = root_hi_lo(&parsed.finalized_beacon_root);
    out.extend_from_slice(&bh);
    out.extend_from_slice(&bl);
    out.extend_from_slice(&u64_le_fr(parsed.participation));
    out.extend_from_slice(&parsed.committee_commitment);
    let (eh, el) = root_hi_lo(&parsed.execution_block_hash);
    out.extend_from_slice(&eh);
    out.extend_from_slice(&el);
    let (sh, sl) = root_hi_lo(&parsed.attested_state_root);
    out.extend_from_slice(&sh);
    out.extend_from_slice(&sl);
    (out, parsed)
}

pub fn u64_le_fr(n: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&n.to_le_bytes());
    out
}

fn root_hi_lo(root: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut hi = [0u8; 32];
    let mut lo = [0u8; 32];
    hi[..16].copy_from_slice(&root[..16]);
    lo[..16].copy_from_slice(&root[16..]);
    (hi, lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json(attested: u64, finalized: u64) -> String {
        let root = format!("0x{}", "11".repeat(32));
        let hash = format!("0x{}", "22".repeat(32));
        // 64 zero bytes → participation 0, plus one 0xff byte would be more;
        // use a single 0x01 at the end of 64 bytes.
        let bits = format!("0x{}01", "00".repeat(63));
        format!(
            r#"{{"data":{{
              "attested_header":{{"beacon":{{"slot":"{attested}","state_root":"{root}"}}}},
              "finalized_header":{{
                "beacon":{{"slot":"{finalized}","state_root":"{root}"}},
                "execution":{{"block_hash":"{hash}"}}
              }},
              "sync_aggregate":{{"sync_committee_bits":"{bits}"}}
            }}}}"#
        )
    }

    #[test]
    fn parses_slots_and_period() {
        let u = parse_finality_update(&sample_json(8192, 8190)).unwrap();
        assert_eq!(u.attested_slot, 8192);
        assert_eq!(u.finalized_slot, 8190);
        assert_eq!(u.period(), 0);
        assert_eq!(u.participation, 1);
        assert_eq!(u.execution_block_hash[0], 0x22);
    }

    #[test]
    fn period_boundary() {
        let u = parse_finality_update(&sample_json(16384, 16384)).unwrap();
        assert_eq!(u.period(), 2);
    }

    #[test]
    fn packed_pi_is_10_words() {
        let u = parse_finality_update(&sample_json(5, 4)).unwrap();
        let (bytes, parsed) = pack_step_public_inputs(&u, [0xAB; 32]);
        assert_eq!(bytes.len(), PUBLIC_INPUT_BYTES);
        assert_eq!(parsed.finalized_slot, 4);
        assert_eq!(&bytes[..8], &5u64.to_le_bytes());
    }

    #[test]
    fn parse_leaves_committee_json_empty() {
        let u = parse_finality_update(&sample_json(1, 1)).unwrap();
        assert!(u.committee_json.is_empty());
        assert!(u.chain.is_none());
    }

    #[test]
    fn signature_slot_defaults_to_attested_plus_one() {
        let u = parse_finality_update(&sample_json(8, 7)).unwrap();
        assert_eq!(u.signature_slot, 9);
        let with = sample_json(8, 7).replacen(
            "\"sync_aggregate\"",
            "\"signature_slot\":\"11\",\"sync_aggregate\"",
            1,
        );
        let u = parse_finality_update(&with).unwrap();
        assert_eq!(u.signature_slot, 11);
        assert_eq!(u.signing_period(), 0);
    }

    #[test]
    fn chain_params_hex_round_trip() {
        let c = BeaconChainParams {
            fork_version: [0x90, 0, 0, 0x75],
            genesis_validators_root: [0xd8; 32],
        };
        assert_eq!(c.fork_version_hex(), "0x90000075");
        assert_eq!(
            c.genesis_validators_root_hex(),
            format!("0x{}", "d8".repeat(32))
        );
    }

    #[test]
    fn step_bundle_from_dir_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut pi = vec![0u8; PUBLIC_INPUT_BYTES];
        pi[..8].copy_from_slice(&7u64.to_le_bytes());
        pi[32..40].copy_from_slice(&6u64.to_le_bytes());
        pi[5 * 32..6 * 32].copy_from_slice(&[0xC0; 32]);
        std::fs::write(dir.path().join("step_public_inputs.bin"), &pi).unwrap();
        std::fs::write(dir.path().join("step_proof_blake2b.bin"), [0xABu8; 8]).unwrap();
        let b = StepProofBundle::from_dir(dir.path()).unwrap();
        assert_eq!(b.parsed.attested_slot, 7);
        assert_eq!(b.parsed.finalized_slot, 6);
        assert_eq!(b.parsed.committee_commitment, [0xC0; 32]);
        assert_eq!(b.proof, vec![0xAB; 8]);
    }

    #[test]
    fn rotate_bundle_from_dir_reads_period_word() {
        let dir = tempfile::tempdir().unwrap();
        let mut pi = vec![0u8; ROTATE_INSTANCE_LEN * 32];
        let off = (ROTATE_INSTANCE_LEN - 1) * 32;
        pi[off..off + 8].copy_from_slice(&42u64.to_le_bytes());
        std::fs::write(dir.path().join("rotate_public_inputs.bin"), &pi).unwrap();
        std::fs::write(dir.path().join("rotate_proof_blake2b.bin"), [0xCDu8; 4]).unwrap();
        let b = RotateProofBundle::from_dir(dir.path()).unwrap();
        assert_eq!(b.period, 42);
        assert_eq!(b.proof, vec![0xCD; 4]);
    }

    #[test]
    fn rotate_bundle_rejects_short_pi() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rotate_public_inputs.bin"), [0u8; 8]).unwrap();
        std::fs::write(dir.path().join("rotate_proof_blake2b.bin"), [0u8; 1]).unwrap();
        assert!(RotateProofBundle::from_dir(dir.path()).is_err());
    }
}
