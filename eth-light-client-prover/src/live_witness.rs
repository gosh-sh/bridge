//! Build a [`StepWitness`] from live beacon REST JSON (not a synthetic
//! committee).
//!
//! Committee bytes come from `light_client/bootstrap`
//! (`current_sync_committee`) or from the previous period's
//! `light_client/updates` (`next_sync_committee`). Bits + signature come from
//! `light_client/finality_update`.

use anyhow::{anyhow, Context};
use halo2_base::halo2_proofs::halo2curves::{
    bls12_381::{G1Affine, G1},
    group::{Curve, Group},
};
use serde_json::Value;

use crate::{
    bls_core::{
        decode_sync_committee_bits, deserialize_pubkey, deserialize_signature_compressed,
        participation, signing_root_to_g2, verify_native, SYNC_COMMITTEE_SIZE,
        SYNC_COMMITTEE_THRESHOLD,
    },
    execution::ExecutionPayloadVals,
    signing::{
        native_sync_committee_signing_root, FORK_VERSION_FULU, MAINNET_GENESIS_VALIDATORS_ROOT,
        SEPOLIA_FORK_VERSION_FULU, SEPOLIA_GENESIS_VALIDATORS_ROOT,
    },
    step::{HeaderVals, StepWitness},
};

/// Env var: 4-byte `fork_version` active at the update's `signature_slot`
/// (`0x`-hex, e.g. `0x90000075` for Sepolia Fulu).
pub const ENV_FORK_VERSION: &str = "BEACON_FORK_VERSION";
/// Env var: 32-byte `genesis_validators_root` of the beacon network (`0x`-hex).
pub const ENV_GENESIS_VALIDATORS_ROOT: &str = "BEACON_GENESIS_VALIDATORS_ROOT";

/// Network-specific inputs of the sync-committee signing domain (m0_spec §5).
///
/// Both values are circuit **witnesses** (not public inputs and not constants),
/// so the step VK / VkBlob / `EthBeaconLightClient` do not change between
/// networks; only the witness has to carry the right pair. The committee
/// commitment pins the proof to a network anyway: a committee of one network
/// never signs a domain of another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainParams {
    pub fork_version: [u8; 4],
    pub genesis_validators_root: [u8; 32],
}

impl ChainParams {
    pub const MAINNET_FULU: Self = Self {
        fork_version: FORK_VERSION_FULU,
        genesis_validators_root: MAINNET_GENESIS_VALIDATORS_ROOT,
    };

    pub const SEPOLIA_FULU: Self = Self {
        fork_version: SEPOLIA_FORK_VERSION_FULU,
        genesis_validators_root: SEPOLIA_GENESIS_VALIDATORS_ROOT,
    };

    /// Parse the `0x`-hex pair as delivered by the relayer / operator.
    pub fn parse(fork_version: &str, genesis_validators_root: &str) -> anyhow::Result<Self> {
        let fork: [u8; 4] = hexv(fork_version)
            .with_context(|| format!("{ENV_FORK_VERSION}={fork_version:?}"))?
            .try_into()
            .map_err(|_| anyhow!("{ENV_FORK_VERSION} must be 4 bytes, got {fork_version:?}"))?;
        let gvr = h32(genesis_validators_root).with_context(|| {
            format!("{ENV_GENESIS_VALIDATORS_ROOT}={genesis_validators_root:?}")
        })?;
        Ok(Self {
            fork_version: fork,
            genesis_validators_root: gvr,
        })
    }

    /// `BEACON_FORK_VERSION` + `BEACON_GENESIS_VALIDATORS_ROOT`. Both unset →
    /// [`Self::MAINNET_FULU`] (the historical default). Setting only one of the
    /// two is an error: a half-overridden domain silently proves nothing.
    pub fn from_env() -> anyhow::Result<Self> {
        let fork = std::env::var(ENV_FORK_VERSION)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let gvr = std::env::var(ENV_GENESIS_VALIDATORS_ROOT)
            .ok()
            .filter(|s| !s.trim().is_empty());
        match (fork, gvr) {
            (None, None) => Ok(Self::MAINNET_FULU),
            (Some(f), Some(g)) => Self::parse(f.trim(), g.trim()),
            (Some(_), None) => Err(anyhow!(
                "{ENV_FORK_VERSION} is set but {ENV_GENESIS_VALIDATORS_ROOT} is not"
            )),
            (None, Some(_)) => Err(anyhow!(
                "{ENV_GENESIS_VALIDATORS_ROOT} is set but {ENV_FORK_VERSION} is not"
            )),
        }
    }

    /// Human-readable label for logs (`mainnet`, `sepolia`, or the raw pair).
    pub fn label(&self) -> String {
        if self.genesis_validators_root == MAINNET_GENESIS_VALIDATORS_ROOT {
            format!("mainnet fork=0x{}", hex::encode(self.fork_version))
        } else if self.genesis_validators_root == SEPOLIA_GENESIS_VALIDATORS_ROOT {
            format!("sepolia fork=0x{}", hex::encode(self.fork_version))
        } else {
            format!(
                "gvr=0x{} fork=0x{}",
                hex::encode(self.genesis_validators_root),
                hex::encode(self.fork_version)
            )
        }
    }
}

fn hexv(s: &str) -> anyhow::Result<Vec<u8>> {
    Ok(hex::decode(s.trim_start_matches("0x"))?)
}

fn h32(s: &str) -> anyhow::Result<[u8; 32]> {
    hexv(s)?
        .try_into()
        .map_err(|_| anyhow!("expected 32-byte hex"))
}

fn pk48(s: &str) -> anyhow::Result<[u8; 48]> {
    hexv(s)?
        .try_into()
        .map_err(|_| anyhow!("expected 48-byte hex"))
}

fn u64_field(v: &Value) -> anyhow::Result<u64> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    v.as_str()
        .ok_or_else(|| anyhow!("missing u64"))?
        .parse()
        .map_err(|e| anyhow!("u64: {e}"))
}

fn u256_le(s: &str) -> anyhow::Result<[u8; 32]> {
    let val: u128 = s.parse().context("base_fee_per_gas")?;
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&val.to_le_bytes());
    Ok(out)
}

fn unwrap_data(v: &Value) -> &Value {
    if v.is_array() {
        return &v[0]["data"];
    }
    if v.get("data").is_some() {
        &v["data"]
    } else {
        v
    }
}

fn header(data: &Value, which: &str) -> anyhow::Result<HeaderVals> {
    let b = &data[which]["beacon"];
    Ok(HeaderVals {
        slot: u64_field(&b["slot"])?,
        proposer_index: u64_field(&b["proposer_index"])?,
        parent_root: h32(b["parent_root"]
            .as_str()
            .ok_or_else(|| anyhow!("{which}.beacon.parent_root"))?)?,
        state_root: h32(b["state_root"]
            .as_str()
            .ok_or_else(|| anyhow!("{which}.beacon.state_root"))?)?,
        body_root: h32(b["body_root"]
            .as_str()
            .ok_or_else(|| anyhow!("{which}.beacon.body_root"))?)?,
    })
}

fn execution(data: &Value, which: &str) -> anyhow::Result<ExecutionPayloadVals> {
    let e = &data[which]["execution"];
    Ok(ExecutionPayloadVals {
        parent_hash: h32(e["parent_hash"]
            .as_str()
            .ok_or_else(|| anyhow!("parent_hash"))?)?,
        fee_recipient: hexv(
            e["fee_recipient"]
                .as_str()
                .ok_or_else(|| anyhow!("fee_recipient"))?,
        )?
        .try_into()
        .map_err(|_| anyhow!("fee_recipient len"))?,
        state_root: h32(e["state_root"]
            .as_str()
            .ok_or_else(|| anyhow!("exec state_root"))?)?,
        receipts_root: h32(e["receipts_root"]
            .as_str()
            .ok_or_else(|| anyhow!("receipts_root"))?)?,
        logs_bloom: hexv(
            e["logs_bloom"]
                .as_str()
                .ok_or_else(|| anyhow!("logs_bloom"))?,
        )?,
        prev_randao: h32(e["prev_randao"]
            .as_str()
            .ok_or_else(|| anyhow!("prev_randao"))?)?,
        block_number: u64_field(&e["block_number"])?,
        gas_limit: u64_field(&e["gas_limit"])?,
        gas_used: u64_field(&e["gas_used"])?,
        timestamp: u64_field(&e["timestamp"])?,
        extra_data: hexv(e["extra_data"].as_str().unwrap_or("0x"))?,
        base_fee_per_gas: u256_le(
            e["base_fee_per_gas"]
                .as_str()
                .ok_or_else(|| anyhow!("base_fee_per_gas"))?,
        )?,
        block_hash: h32(e["block_hash"]
            .as_str()
            .ok_or_else(|| anyhow!("block_hash"))?)?,
        transactions_root: h32(e["transactions_root"]
            .as_str()
            .ok_or_else(|| anyhow!("transactions_root"))?)?,
        withdrawals_root: h32(e["withdrawals_root"]
            .as_str()
            .ok_or_else(|| anyhow!("withdrawals_root"))?)?,
        blob_gas_used: u64_field(&e["blob_gas_used"])?,
        excess_blob_gas: u64_field(&e["excess_blob_gas"])?,
    })
}

fn branch(data: &Value, key: &str, nested: Option<&str>) -> anyhow::Result<Vec<[u8; 32]>> {
    let arr = if let Some(n) = nested {
        data[n][key].as_array().or_else(|| data[key].as_array())
    } else {
        data[key].as_array()
    }
    .ok_or_else(|| anyhow!("missing {key}"))?;
    arr.iter()
        .map(|e| h32(e.as_str().ok_or_else(|| anyhow!("{key} entry"))?))
        .collect()
}

/// 512 compressed pubkeys + aggregate from bootstrap (`current_sync_committee`)
/// or a period update (`next_sync_committee`).
pub fn parse_sync_committee(json: &str) -> anyhow::Result<(Vec<[u8; 48]>, [u8; 48])> {
    let v: Value = serde_json::from_str(json).context("committee JSON")?;
    let d = unwrap_data(&v);
    let c = if d
        .get("current_sync_committee")
        .and_then(|x| x.get("pubkeys"))
        .is_some()
    {
        &d["current_sync_committee"]
    } else if d
        .get("next_sync_committee")
        .and_then(|x| x.get("pubkeys"))
        .is_some()
    {
        &d["next_sync_committee"]
    } else {
        return Err(anyhow!(
            "no current_sync_committee / next_sync_committee.pubkeys (bootstrap or updates JSON)"
        ));
    };
    let pubkeys: Vec<[u8; 48]> = c["pubkeys"]
        .as_array()
        .ok_or_else(|| anyhow!("committee.pubkeys"))?
        .iter()
        .map(|p| pk48(p.as_str().unwrap_or_default()))
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        pubkeys.len() == SYNC_COMMITTEE_SIZE,
        "expected {SYNC_COMMITTEE_SIZE} pubkeys, got {}",
        pubkeys.len()
    );
    let aggregate = pk48(
        c["aggregate_pubkey"]
            .as_str()
            .ok_or_else(|| anyhow!("aggregate_pubkey"))?,
    )?;
    Ok((pubkeys, aggregate))
}

/// Live step witness: real headers/branches from `finality_update` + real
/// 512-committee from bootstrap/updates + real `sync_aggregate`.
///
/// Mainnet-Fulu signing domain; see [`step_witness_from_beacon_with_params`]
/// for other networks / forks.
pub fn step_witness_from_beacon(
    finality_json: &str,
    committee_json: &str,
) -> anyhow::Result<StepWitness> {
    step_witness_from_beacon_with_params(finality_json, committee_json, &ChainParams::MAINNET_FULU)
}

/// [`step_witness_from_beacon`] with an explicit signing domain
/// (`fork_version` at `signature_slot` + `genesis_validators_root`).
pub fn step_witness_from_beacon_with_params(
    finality_json: &str,
    committee_json: &str,
    params: &ChainParams,
) -> anyhow::Result<StepWitness> {
    let v: Value = serde_json::from_str(finality_json).context("finality_update JSON")?;
    let data = unwrap_data(&v);
    let attested = header(data, "attested_header")?;
    let finalized = header(data, "finalized_header")?;
    let finality_branch = branch(data, "finality_branch", None)?;
    let execution_branch = branch(data, "execution_branch", Some("finalized_header"))?;

    let bits_hex = data["sync_aggregate"]["sync_committee_bits"]
        .as_str()
        .ok_or_else(|| anyhow!("sync_committee_bits"))?;
    let bits = decode_sync_committee_bits(&hexv(bits_hex)?);
    anyhow::ensure!(
        participation(&bits) >= SYNC_COMMITTEE_THRESHOLD,
        "participation {} < {SYNC_COMMITTEE_THRESHOLD}",
        participation(&bits)
    );
    let sig = deserialize_signature_compressed(&hexv(
        data["sync_aggregate"]["sync_committee_signature"]
            .as_str()
            .ok_or_else(|| anyhow!("sync_committee_signature"))?,
    )?);

    let (pk_bytes, aggregate_pubkey) = parse_sync_committee(committee_json)?;
    let pubkeys: Vec<G1Affine> = pk_bytes.iter().map(|b| deserialize_pubkey(b)).collect();

    let signing_root = native_sync_committee_signing_root(
        attested.slot,
        attested.proposer_index,
        &attested.parent_root,
        &attested.state_root,
        &attested.body_root,
        &params.fork_version,
        &params.genesis_validators_root,
    );
    let msg_hash = signing_root_to_g2(&signing_root);
    let mut agg = <G1 as Group>::identity();
    for (bit, pk) in bits.iter().zip(pubkeys.iter()) {
        if *bit {
            agg += G1::from(*pk);
        }
    }
    anyhow::ensure!(
        verify_native(&sig, &agg.to_affine(), &msg_hash),
        "live committee+bits+signature do not verify the attested signing_root (wrong committee \
         JSON, wrong network/fork [{}], or truncated finality_update)",
        params.label()
    );

    Ok(StepWitness {
        attested,
        finalized,
        finality_branch,
        fork_version: params.fork_version,
        genesis_validators_root: params.genesis_validators_root,
        pubkeys,
        pubkeys_compressed: pk_bytes,
        aggregate_pubkey,
        bits,
        signature: sig,
        finalized_execution: execution(data, "finalized_header")?,
        execution_branch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_committee_json(n: usize) -> String {
        let pk = format!("0x{}", "ab".repeat(48));
        let agg = format!("0x{}", "cd".repeat(48));
        let pks: Vec<String> = (0..n).map(|_| format!("\"{pk}\"")).collect();
        format!(
            r#"{{"data":{{"next_sync_committee":{{"pubkeys":[{}],"aggregate_pubkey":"{agg}"}}}}}}"#,
            pks.join(",")
        )
    }

    #[test]
    fn committed_finality_update_has_supermajority_bits() {
        let raw = include_str!("../fixtures/mainnet/finality_update.json");
        let v: Value = serde_json::from_str(raw).unwrap();
        let data = unwrap_data(&v);
        let bits_hex = data["sync_aggregate"]["sync_committee_bits"]
            .as_str()
            .unwrap();
        let bits = decode_sync_committee_bits(&hexv(bits_hex).unwrap());
        assert_eq!(bits.len(), SYNC_COMMITTEE_SIZE);
        assert!(participation(&bits) >= SYNC_COMMITTEE_THRESHOLD);
        header(data, "attested_header").unwrap();
        header(data, "finalized_header").unwrap();
        execution(data, "finalized_header").unwrap();
    }

    #[test]
    fn chain_params_parse_sepolia_pair() {
        let p = ChainParams::parse(
            "0x90000075",
            "0xd8ea171f3c94aea21ebc42a1ed61052acf3f9209c00e4efbaaddac09ed9b8078",
        )
        .unwrap();
        assert_eq!(p, ChainParams::SEPOLIA_FULU);
        assert!(p.label().starts_with("sepolia"));
        assert!(ChainParams::MAINNET_FULU.label().starts_with("mainnet"));
    }

    #[test]
    fn chain_params_parse_rejects_bad_lengths() {
        let gvr = format!("0x{}", "11".repeat(32));
        assert!(ChainParams::parse("0x900000", &gvr).is_err());
        assert!(ChainParams::parse("0x90000075", "0x1122").is_err());
        assert!(ChainParams::parse("zz", &gvr).is_err());
    }

    #[test]
    fn parse_sync_committee_reads_512_next_keys() {
        let (pks, agg) = parse_sync_committee(&dummy_committee_json(512)).unwrap();
        assert_eq!(pks.len(), SYNC_COMMITTEE_SIZE);
        assert_eq!(agg, [0xcd; 48]);
        assert_eq!(pks[0], [0xab; 48]);
    }

    #[test]
    fn parse_sync_committee_rejects_short_set() {
        assert!(parse_sync_committee(&dummy_committee_json(511)).is_err());
    }

    #[test]
    fn parse_sync_committee_rejects_unrelated_json() {
        assert!(parse_sync_committee(r#"{"data":{}}"#).is_err());
    }

    #[test]
    fn step_witness_rejects_json_without_committee() {
        let finality = include_str!("../fixtures/mainnet/finality_update.json");
        assert!(step_witness_from_beacon(finality, r#"{"data":{}}"#).is_err());
    }
}
