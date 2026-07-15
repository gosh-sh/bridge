//! Wrap a Poseidon-transcript Halo2 SHPLONK proof + VK into snark-verifier [`Snark`].
//!
//! Partner circuits use gosh `halo2-base` for keygen; VK serde uses
//! `BaseCircuitBuilder<Fr>` as the reference type. The axiom fork shares the
//! same wire format, so we load VK/config here for aggregator consumption.

use std::{
    fs::File,
    io::BufReader,
    path::Path,
};

use anyhow::Context;
use halo2_base::{
    gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
    halo2_proofs::{
        halo2curves::bn256::{Fr, G1Affine},
        plonk::VerifyingKey,
        SerdeFormat,
    },
};
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;
use snark_verifier::system::halo2::{compile, Config};
use snark_verifier_sdk::Snark;

const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;

/// Load partner-style VK + config and emit bincode [`Snark`].
pub fn export_poseidon_snark(
    vk_path: &Path,
    config_path: &Path,
    proof_path: &Path,
    instances_path: &Path,
    out_path: &Path,
    num_instances: usize,
) -> anyhow::Result<()> {
    let config: BaseCircuitParams = serde_json::from_str(
        &std::fs::read_to_string(config_path)
            .with_context(|| format!("read config {}", config_path.display()))?,
    )?;

    let vk = load_vk(vk_path, &config)?;
    let proof = std::fs::read(proof_path)
        .with_context(|| format!("read proof {}", proof_path.display()))?;
    let instances = load_instances(instances_path)?;

    if instances.len() != num_instances {
        anyhow::bail!(
            "expected {num_instances} instances, got {} in {}",
            instances.len(),
            instances_path.display()
        );
    }

    let prev = std::env::current_dir().ok();
    if let Some(parent) = vk_path.parent() {
        std::env::set_var("PARAMS_DIR", parent);
    }
    let params = halo2_base::utils::fs::gen_srs(config.k as u32);
    if let Some(p) = prev {
        let _ = std::env::set_current_dir(p);
    }

    let protocol = compile(
        &params,
        &vk,
        Config::kzg().with_num_instance(vec![num_instances]),
    );
    let snark = Snark::new(protocol, vec![instances], proof);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = bincode::serialize(&snark)?;
    std::fs::write(out_path, &bytes)?;

    Ok(())
}

fn load_vk(path: &Path, config: &BaseCircuitParams) -> anyhow::Result<VerifyingKey<G1Affine>> {
    let file = File::open(path).with_context(|| format!("open vk {}", path.display()))?;
    let mut reader = BufReader::new(file);
    VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(&mut reader, SERDE_FMT, config.clone())
        .map_err(|e| anyhow::anyhow!("vk read {}: {e}", path.display()))
}

fn load_instances(path: &Path) -> anyhow::Result<Vec<Fr>> {
    let bytes = std::fs::read(path)?;
    if !bytes.len().is_multiple_of(32) {
        anyhow::bail!(
            "instances file {} size {} not multiple of 32",
            path.display(),
            bytes.len()
        );
    }
    let mut out = Vec::with_capacity(bytes.len() / 32);
    for chunk in bytes.chunks_exact(32) {
        let mut repr = <Fr as PrimeField>::Repr::default();
        repr.as_mut().copy_from_slice(chunk);
        out.push(
            Fr::from_repr(repr)
                .into_option()
                .ok_or_else(|| anyhow::anyhow!("invalid Fr in {}", path.display()))?,
        );
    }
    Ok(out)
}

/// Smoke: deserialize a written `.snark` back.
#[allow(dead_code)]
pub fn read_snark(path: &Path) -> anyhow::Result<Snark> {
    let bytes = std::fs::read(path)?;
    Ok(bincode::deserialize(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator;

    #[test]
    fn multiply_inner_snark_bincode_round_trip() {
        let params = halo2_base::utils::fs::gen_srs(aggregator::K_INNER_SPIKE);
        let inner = aggregator::prove_inner(&params, Fr::from(3u64), Fr::from(5u64)).unwrap();
        let bytes = bincode::serialize(&inner).unwrap();
        let loaded: Snark = bincode::deserialize(&bytes).unwrap();
        assert_eq!(loaded.instances[0][0], Fr::from(15u64));
    }
}
