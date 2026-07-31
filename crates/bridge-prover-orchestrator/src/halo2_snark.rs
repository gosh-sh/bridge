//! Wrap a Poseidon-transcript Halo2 SHPLONK proof + partner VK into snark-verifier [`Snark`].
//!
//! Must live in this crate (gosh `halo2-base` / `BaseCircuitBuilder` VK serde). The
//! standalone `bridge-evm-aggregator` tree uses axiom `halo2-lib` and cannot read
//! partner `*_vk.bin` files ("unexpected version byte").

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

/// Load partner-style VK + config and emit bincode [`Snark`] for the aggregator.
pub fn export_poseidon_snark(
    vk_path: &Path,
    config_path: &Path,
    proof_path: &Path,
    instances: &[Fr],
    out_path: &Path,
) -> anyhow::Result<()> {
    let config: BaseCircuitParams = serde_json::from_str(
        &std::fs::read_to_string(config_path)
            .with_context(|| format!("read config {}", config_path.display()))?,
    )?;

    let vk = load_vk(vk_path, &config)?;
    let proof = std::fs::read(proof_path)
        .with_context(|| format!("read proof {}", proof_path.display()))?;

    let prev = std::env::current_dir().ok();
    if let Some(parent) = vk_path.parent() {
        std::env::set_var("PARAMS_DIR", parent);
    }
    let params = halo2_base::utils::fs::gen_srs(config.k as u32);
    if let Some(p) = prev {
        let _ = std::env::set_current_dir(p);
    }
    // Refuse to run against toxic-waste SRS: `gen_srs` silently generates
    // an unsafe SRS when PARAMS_DIR is missing kzg_bn254_{k}.srs. Same
    // Hermez PPoT anchor used by bridge-prover-lib's `load_srs`.
    bridge_prover_lib::keys::assert_hermez_srs(&params)?;

    let protocol = compile(
        &params,
        &vk,
        Config::kzg().with_num_instance(vec![instances.len()]),
    );
    let snark = Snark::new(protocol, vec![instances.to_vec()], proof);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, bincode::serialize(&snark)?)?;

    Ok(())
}

fn load_vk(path: &Path, config: &BaseCircuitParams) -> anyhow::Result<VerifyingKey<G1Affine>> {
    let file = File::open(path).with_context(|| format!("open vk {}", path.display()))?;
    let mut reader = BufReader::new(file);
    VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(&mut reader, SERDE_FMT, config.clone())
        .map_err(|e| anyhow::anyhow!("vk read {}: {e}", path.display()))
}

/// Deserialize instances from a flat 32-byte LE file.
pub fn load_instances_binary(path: &Path) -> anyhow::Result<Vec<Fr>> {
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
