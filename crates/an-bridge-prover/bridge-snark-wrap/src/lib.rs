//! In-process wrap of a Poseidon-transcript Halo2 SHPLONK proof + partner VK
//! into a snark-verifier [`Snark`], bincode-serialized so
//! `bridge-evm-aggregator::aggregate-proof` can consume it directly.
//!
//! # Why this crate exists
//!
//! Historically, `bridge-relayer-daemon` shelled out to
//! `bridge-prover-orchestrator/src/bin/export_1a1b2_poseidon_snark.rs` to obtain
//! the Poseidon `.snark` for each Circuit 1A/1B/2 bundle. That binary
//! independently re-fetched the block from GraphQL, re-ran
//! `real_chain_builder`, and re-proved the same witness — doubling the
//! GQL fetches, chain builds, and Halo2 proves per key block.
//!
//! The double-proving was avoidable: `bridge-prover-lib`'s Halo2 provers already
//! accept a [`bridge_prover_lib::transcript::TranscriptKind`] parameter, so the
//! daemon can request Poseidon-transcript bytes directly from
//! [`bridge_prover_lib::live_driver::LiveProverDriver::poll_next_bundle`] by
//! setting `LiveProverConfig.transcript = TranscriptKind::Poseidon`. What was
//! missing was an in-process way to wrap those raw proof bytes into a
//! snark-verifier `Snark` — this crate provides exactly that.
//!
//! # Why a separate crate, and why a workspace member
//!
//! `snark-verifier` is a heavy dep (pulls all of snark-verifier-sdk +
//! transitive halo2-base). Keeping it out of `bridge-prover-lib` avoids adding
//! that dep to the AN-side `bridge-prover-daemon` (which produces Blake2b
//! bytes for IPC and never needs the wrap).
//!
//! It must live as a member of the `an-bridge-prover` cargo workspace (not a
//! standalone `[workspace]` root like `bridge-evm-aggregator`) so the workspace-
//! root `[patch]` rewrites for `halo2-base` / `halo2-ecc` in
//! `an-bridge-prover/Cargo.toml` apply. Without those patches, `snark-verifier`
//! would resolve halo2-base to axiom's crate, which cannot read the partner
//! `*_vk.bin` VK bytes emitted by `bridge-prover-lib` (`SerdeFormat::RawBytesUnchecked`
//! against `BaseCircuitBuilder<Fr>` from the gosh fork). Same reason
//! `halo2_snark.rs` lives inside `bridge-prover-orchestrator` and not
//! `bridge-evm-aggregator`.
//!
//! # API
//!
//! Two entry points, one per source of proof bytes:
//!
//! * [`wrap_poseidon_snark_in_memory`] — proof bytes come from an in-memory
//!   `Vec<u8>` (the daemon's call site). Returns the bincode-serialized snark
//!   as a `Vec<u8>`.
//! * [`wrap_poseidon_snark_from_files`] — proof bytes on disk. Byte-identical
//!   output to `export_1a1b2_poseidon_snark::finish`, kept so existing CLI
//!   tooling can migrate off the subprocess incrementally.

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
use snark_verifier::system::halo2::{compile, Config};
use snark_verifier_sdk::Snark;

/// VK serde format — must match what `bridge-prover-lib` writes when it
/// exports `*_vk.bin` (currently `RawBytesUnchecked` in
/// `bridge_prover_lib::keys`).
const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;

/// Wrap Poseidon-transcript proof bytes into a snark-verifier [`Snark`] and
/// return the bincode-serialized bytes.
///
/// The caller writes the return value to disk (e.g. a `NamedTempFile`) and
/// hands the path to `bridge-evm-aggregator::aggregate-proof` as `--inner-snark`.
///
/// # Arguments
///
/// * `vk_path` — partner-style VK bytes (from `bridge_prover_lib::keys`,
///   `SerdeFormat::RawBytesUnchecked` against `BaseCircuitBuilder<Fr>`).
/// * `config_path` — matching `{circuit}_config_params.json` used at keygen.
/// * `srs_k_override` — force this SRS degree instead of `config.k`. Required
///   for the layer circuit whose VK was keygen'd against the shared K=20
///   ceremony SRS while `layer_config_params.json` records `k=17`; without an
///   override, `snark_verifier::system::halo2::compile` panics with
///   `assertion left(20) == right(17)`. Pass `Some(20)` for `layer_hashes`;
///   leave `None` for circuits where `config.k` matches the SRS.
/// * `proof_bytes` — raw Poseidon-transcript proof (from
///   `bridge_prover_lib::prover` / `layer_prover`).
/// * `instances` — public instance Fr vector (in the same order the aggregator
///   circuit expects; see the `instances` construction in the export CLI's
///   `prove_primary` / `prove_fallback` / `prove_layer`).
pub fn wrap_poseidon_snark_in_memory(
    vk_path: &Path,
    config_path: &Path,
    srs_k_override: Option<u32>,
    proof_bytes: &[u8],
    instances: &[Fr],
) -> anyhow::Result<Vec<u8>> {
    let config: BaseCircuitParams = serde_json::from_str(
        &std::fs::read_to_string(config_path)
            .with_context(|| format!("read config {}", config_path.display()))?,
    )?;

    let vk = load_vk(vk_path, &config)?;

    // Provision SRS from PARAMS_DIR = parent(vk_path). Same convention as the
    // original `export_poseidon_snark_with_srs_k` — `gen_srs` reads this env
    // var to locate `kzg_bn254_{k}.srs`. We snapshot cwd around the call to
    // avoid surprising the caller if `gen_srs` chdirs (older halo2-base
    // versions did).
    let prev_cwd = std::env::current_dir().ok();
    if let Some(parent) = vk_path.parent() {
        std::env::set_var("PARAMS_DIR", parent);
    }
    let srs_k = srs_k_override.unwrap_or(config.k as u32);
    let params = halo2_base::utils::fs::gen_srs(srs_k);
    if let Some(p) = prev_cwd {
        let _ = std::env::set_current_dir(p);
    }
    // Refuse to run against toxic-waste SRS: `gen_srs` silently generates
    // an unsafe SRS when PARAMS_DIR is missing `kzg_bn254_{k}.srs`. Same
    // Hermez PPoT anchor `bridge-prover-lib::keys::load_srs` uses.
    bridge_prover_lib::keys::assert_hermez_srs(&params)?;

    let protocol = compile(
        &params,
        &vk,
        Config::kzg().with_num_instance(vec![instances.len()]),
    );
    let snark = Snark::new(protocol, vec![instances.to_vec()], proof_bytes.to_vec());

    bincode::serialize(&snark).context("bincode::serialize(Snark)")
}

/// File-oriented variant of [`wrap_poseidon_snark_in_memory`]. Byte-identical
/// output to the original `bridge-prover-orchestrator::halo2_snark::export_poseidon_snark_with_srs_k`,
/// kept so existing paths that already have proof + snark on disk can migrate
/// off the subprocess incrementally.
pub fn wrap_poseidon_snark_from_files(
    vk_path: &Path,
    config_path: &Path,
    srs_k_override: Option<u32>,
    proof_path: &Path,
    instances: &[Fr],
    out_path: &Path,
) -> anyhow::Result<()> {
    let proof = std::fs::read(proof_path)
        .with_context(|| format!("read proof {}", proof_path.display()))?;
    let snark_bytes = wrap_poseidon_snark_in_memory(
        vk_path,
        config_path,
        srs_k_override,
        &proof,
        instances,
    )?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, snark_bytes)
        .with_context(|| format!("write snark {}", out_path.display()))?;
    Ok(())
}

fn load_vk(path: &Path, config: &BaseCircuitParams) -> anyhow::Result<VerifyingKey<G1Affine>> {
    let file = File::open(path).with_context(|| format!("open vk {}", path.display()))?;
    let mut reader = BufReader::new(file);
    VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut reader,
        SERDE_FMT,
        config.clone(),
    )
    .map_err(|e| anyhow::anyhow!("vk read {}: {e}", path.display()))
}
