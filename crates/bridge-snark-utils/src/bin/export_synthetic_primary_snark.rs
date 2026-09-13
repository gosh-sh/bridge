//! Hermez-SRS audit helper — Circuit 1A / PrimaryAggregatorVerifier.
//!
//! Synthesises a self-contained Circuit 1A witness via `bridge-test-data-gen`,
//! reuses the cached `primary_{pk,vk,config}` under `--params-dir`, produces a
//! Poseidon inner `.snark`, and prints its path. Feed that into
//! `bridge-evm-aggregator/aggregate-proof --name PrimaryAggregatorVerifier`
//! (with `PARAMS_DIR` pointed at the same Hermez `params/`) — the aggregator
//! then byte-compares its regenerated Yul against the committed
//! `contracts/ethereum/verifiers/PrimaryAggregatorVerifier.bin`. Match ⇒
//! committed verifier was built from Hermez PPoT with the same
//! snark-verifier-sdk + AggregatorConfig; mismatch ⇒ drift, bisect from there.
//!
//! No live GraphQL, no bound-witness cache. The aggregator VK under
//! `VerifierUniversality::Full` is invariant across inner-VK values, so a
//! synthetic bk_set (3 signers) is as valid an audit as a real 300-signer
//! shellnet witness.

use std::path::PathBuf;

use anyhow::Context;
use bridge_gql_fetcher as _; // silence unused-dep warning
use bridge_prover_lib::{
    keys::KeyManager,
    prover,
    transcript::TranscriptKind,
    verifier, Fr,
};
use bridge_snark_utils::{halo2_snark::export_poseidon_snark, proof_export::save_instances_binary};
use bridge_test_data_gen::generator::generate_test_data_all_sign;
use clap::Parser;
use halo2_base::halo2_proofs::poly::commitment::Params;

#[derive(Parser, Debug)]
#[command(name = "export-synthetic-primary-snark")]
struct Args {
    #[arg(long, default_value = "../bridge-prover-libraries/params")]
    params_dir: String,
    #[arg(long, default_value = "/tmp/hermez_audit")]
    snark_dir: String,
    /// Number of signers in the synthetic BK set (all sign).
    #[arg(long, default_value_t = 3)]
    bk_set_size: usize,
    /// `last_seen_block_seqno` public input (must be < synthetic seq_no = 1).
    #[arg(long, default_value_t = 0)]
    last_seen: u32,
}

fn ensure_srs_for(km: &KeyManager, params_dir: &std::path::Path, circuit_k: u32) -> anyhow::Result<()> {
    use std::io::Write;
    let srs_path = params_dir.join(format!("kzg_bn254_{circuit_k}.srs"));
    if srs_path.exists() {
        return Ok(());
    }
    let src = km.fallback.srs();
    let src_k = src.k();
    anyhow::ensure!(
        src_k >= circuit_k,
        "fallback SRS (K={src_k}) smaller than circuit K={circuit_k}",
    );
    let mut p = src.clone();
    if src_k > circuit_k {
        p.downsize(circuit_k);
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&srs_path)?);
    p.write(&mut w)?;
    w.flush()?;
    println!("provisioned {} (downsized from K={src_k} Hermez)", srs_path.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let args = Args::parse();
    let params_dir = PathBuf::from(&args.params_dir);
    let snark_dir = PathBuf::from(&args.snark_dir);
    std::fs::create_dir_all(&snark_dir)?;

    println!(
        "[synthetic-primary] params_dir={} snark_dir={} bk_set_size={}",
        params_dir.display(),
        snark_dir.display(),
        args.bk_set_size,
    );

    let td = generate_test_data_all_sign(args.bk_set_size)
        .context("generate_test_data_all_sign")?;
    println!("synthesised {} keypairs, {}-byte attestation", td.keypairs.len(), td.attestation_bytes.len());

    let mut km = KeyManager::new(&params_dir);
    km.ensure_primary_keys(&td.bk_set).context("ensure_primary_keys")?;
    let k = km.primary_config().k as u32;
    ensure_srs_for(&km, &params_dir, k)?;
    km.load_primary_pk().context("load_primary_pk (cached 3.5 GB blob)")?;

    let out = prover::generate_primary_proof_with_transcript(
        &km,
        &td.attestation_bytes,
        &td.bk_set,
        args.last_seen,
        TranscriptKind::Poseidon,
    )
    .context("generate_primary_proof_with_transcript(Poseidon)")?;

    let instances = vec![
        out.block_id_fr,
        out.bk_set_commitment_fr,
        Fr::from(out.block_seq_no as u64),
        Fr::from(out.last_seen_block_seqno as u64),
    ];
    let ok = verifier::verify_primary_proof_with_transcript(
        &km,
        &out.proof_bytes,
        &instances,
        TranscriptKind::Poseidon,
    );
    km.unload_primary_pk();
    anyhow::ensure!(ok, "self-verify FAIL: refusing to export invalid Poseidon inner snark");
    println!("SELF_VERIFY primary (Poseidon): PASS");

    let proof_path = snark_dir.join("primary.proof.bin");
    std::fs::write(&proof_path, &out.proof_bytes)?;
    let instances_path = snark_dir.join("primary.instances.bin");
    save_instances_binary(&instances, &instances_path)?;
    let snark_path = snark_dir.join("primary.snark");

    export_poseidon_snark(
        &params_dir.join("primary_vk.bin"),
        &params_dir.join("primary_config_params.json"),
        &proof_path,
        &instances,
        &snark_path,
    )?;

    println!(
        "OK: primary snark -> {} ({} B, {} public instances)",
        snark_path.display(),
        std::fs::metadata(&snark_path)?.len(),
        instances.len(),
    );
    println!();
    println!("Next: run the aggregator against Hermez params and byte-compare vs committed .bin");
    println!(
        "  cd ../bridge-evm-aggregator && \\\n    PARAMS_DIR={} \\\n    cargo run --release --bin aggregate-proof -- \\\n      --inner-snark {} \\\n      --name PrimaryAggregatorVerifier \\\n      --verifiers-dir ../../contracts/ethereum/verifiers \\\n      --out {}/primary_calldata.bin",
        params_dir.canonicalize().unwrap_or(params_dir.clone()).display(),
        snark_path.canonicalize().unwrap_or(snark_path.clone()).display(),
        snark_dir.display(),
    );
    Ok(())
}
