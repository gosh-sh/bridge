//! Prove `BridgeMultiHopProof` (cross-thread hop-chain circuit) with a Poseidon
//! transcript and emit a snark-verifier [`Snark`] for the R15 aggregator
//! pipeline. The resulting `.snark` is the inner SNARK that
//! `bridge-evm-aggregator`'s
//! `export-inner-aggregator --name BridgeMultiHopAggregatorVerifier` wraps
//! into the production Yul EVM verifier.
//!
//! Sibling of [`export_c4_poseidon_snark`]. The multi-hop circuit is the
//! second inner circuit shipped by `bridge-event-prove-circuit` (see
//! `MULTITHREAD_MIGRATION_PLAN.md` §5); a bundle carries one Circuit 4
//! snark plus 0..=`N_BUNDLE_MAX` multi-hop snarks. Each hop snark exposes
//! 2 public inputs (`hopStartBlockId`, `hopEndBlockId`).
//!
//! The reference witness is synthetic
//! (`build_synthetic_multi_hop_keygen_inputs`): only the **circuit shape**
//! determines the aggregator Yul, so any valid `BridgeMultiHopProof` pins
//! the same VK, and real cross-thread bundle snarks generated from live
//! Acki Nacki blocks verify against the emitted verifier byte-for-byte.
//!
//! ```bash
//! cd crates/bridge-snark-utils
//! cargo run --release --bin export-multi-hop-poseidon-snark -- \
//!   --params-dir ../../params \
//!   --snark-dir ../../proofs/bound/poseidon-snark
//!
//! cd ../bridge-evm-aggregator
//! cargo run --release --bin export-inner-aggregator -- \
//!   --inner-snark ../../proofs/bound/poseidon-snark/multi_hop.snark \
//!   --out-dir ../../contracts/ethereum/verifiers \
//!   --name BridgeMultiHopAggregatorVerifier
//! ```

use std::path::PathBuf;

use anyhow::Context;
use bridge_event_prove_circuit::test_helpers::build_synthetic_multi_hop_keygen_inputs;
use bridge_event_prover_lib::{
    prover::generate_multi_hop_proof_from_circuit_with_transcript,
    verifier::verify_multi_hop_proof_with_transcript,
};
use bridge_prover_lib::{
    keys::{KeyManager, MultiHopKeyManager},
    transcript::TranscriptKind,
};
use bridge_snark_utils::{
    halo2_snark::export_poseidon_snark_with_srs_k, proof_export::save_instances_binary,
};
use clap::Parser;
use halo2_base::halo2_proofs::poly::commitment::Params;

/// Ensure `params/kzg_bn254_{k}.srs` exists for the multi-hop circuit degree,
/// downsized from the K=21 ceremony SRS (largest across the four bridge
/// circuits, held by the fallback manager). Without this,
/// `export_poseidon_snark`'s internal `gen_srs(k)` would synthesise a
/// *random* SRS on cache miss — a different `s_g2` than the keygen
/// ceremony, which makes the aggregator unable to verify the inner proof.
/// The downsize preserves `g2`/`s_g2` (degree-independent), so the K=17
/// file shares the K=21 ceremony.
fn ensure_srs_for_multi_hop(
    km: &KeyManager,
    params_dir: &std::path::Path,
    multi_hop_k: u32,
) -> anyhow::Result<()> {
    use std::io::Write;
    let srs_path = params_dir.join(format!("kzg_bn254_{multi_hop_k}.srs"));
    if srs_path.exists() {
        return Ok(());
    }
    let src = km.fallback.srs();
    let src_k = src.k();
    anyhow::ensure!(
        src_k >= multi_hop_k,
        "shared SRS (K={src_k}) is smaller than the multi-hop circuit degree (K={multi_hop_k})"
    );
    let mut p = src.clone();
    if src_k > multi_hop_k {
        p.downsize(multi_hop_k);
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&srs_path)?);
    p.write(&mut w)?;
    w.flush()?;
    println!(
        "provisioned {} (downsized from K={src_k} ceremony)",
        srs_path.display()
    );
    Ok(())
}

/// Fixed seed for the synthetic keygen/reference witness. Deterministic so
/// two runs pin the same VK (the verifier artefact must be reproducible).
/// Distinct from `MultiHopKeyManager::MULTI_HOP_KEYGEN_SEED` on purpose —
/// keygen only pins the constraint system, this bin additionally pins a
/// specific reference witness (public instances) for the exported snark.
const MULTI_HOP_REF_SEED: u64 = 0x1B0D_5EED_5EED_5EEDu64;

#[derive(Parser, Debug)]
#[command(
    name = "export-multi-hop-poseidon-snark",
    about = "Poseidon re-prove of BridgeMultiHopProof + Snark export for the R15 aggregator"
)]
struct Args {
    #[arg(long, default_value = "../../params")]
    params_dir: String,
    #[arg(long, default_value = "../../proofs/bound/poseidon-snark")]
    snark_dir: String,
    /// Output snark basename (without extension).
    #[arg(long, default_value = "multi_hop")]
    name: String,
    /// Seed for the synthetic reference witness. The default pins the same
    /// VK as the committed verifier; pass a different value to produce a
    /// distinct inner snark (different public inputs, same circuit shape)
    /// for the M7 universality check — the deployed verifier must accept
    /// both.
    #[arg(long)]
    seed: Option<u64>,
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

    // KeyManager facade loads primary/fallback/layer/event; we only need it
    // here for the K=21 fallback SRS to downsize from. The multi-hop
    // manager is standalone (not yet wired into the facade — parallels how
    // export-c4 predates the facade's ownership of the event manager).
    let km = KeyManager::new(&params_dir);
    let mut mhkm = MultiHopKeyManager::new(&params_dir);
    mhkm.ensure_keys()
        .context("ensure_multi_hop_keys failed (keygen)")?;

    // Provision the multi-hop-degree (K=17) SRS from the same ceremony as
    // keygen before the Snark export re-loads it via gen_srs. Downsized
    // from the fallback manager's K=21 slice so the K=17 file shares the
    // ceremony's g2/s_g2.
    let multi_hop_k = mhkm.config().k as u32;
    ensure_srs_for_multi_hop(&km, &params_dir, multi_hop_k)?;

    mhkm.load_pk().context("load_multi_hop_pk failed")?;

    let seed = args.seed.unwrap_or(MULTI_HOP_REF_SEED);
    println!("proving SYNTHETIC witness (seed={seed:#x})");
    let (circuit, instances) = build_synthetic_multi_hop_keygen_inputs(seed);
    let out = generate_multi_hop_proof_from_circuit_with_transcript(
        &mhkm,
        circuit,
        instances,
        TranscriptKind::Poseidon,
    )
    .context("BridgeMultiHopProof Poseidon proof generation failed (synthetic)")?;

    // Native Poseidon self-verify — refuse to export an invalid inner snark
    // (stale/mismatched multi-hop keys are the usual culprit; delete
    // params/multi_hop_{vk,pk}.bin + multi_hop_config_params.json and re-run).
    let ok = verify_multi_hop_proof_with_transcript(
        &mhkm,
        &out.proof_bytes,
        &out.public_instances,
        TranscriptKind::Poseidon,
    );
    mhkm.unload_pk();
    println!(
        "SELF_VERIFY multi_hop (Poseidon native): {}",
        if ok { "PASS" } else { "FAIL" }
    );
    anyhow::ensure!(
        ok,
        "BridgeMultiHopProof Poseidon inner snark failed native verification — refusing to export \
         an invalid snark. Regenerate multi-hop keys against the current circuit shape."
    );

    let proof_path = snark_dir.join(format!("{}.proof.bin", args.name));
    std::fs::write(&proof_path, &out.proof_bytes)?;
    let instances_path = snark_dir.join(format!("{}.instances.bin", args.name));
    save_instances_binary(&out.public_instances, &instances_path)?;
    let out_snark = snark_dir.join(format!("{}.snark", args.name));

    // Multi-hop circuit uses config.k = 17 and VK is keygen'd against K=17
    // (see `MultiHopKeyManager::KEYGEN_SRS_K`). Pass explicit SRS override
    // for symmetry with sibling exporters — snark-verifier's `compile()`
    // sees `params.k = 17 == vk.domain.k`, no downsize needed.
    export_poseidon_snark_with_srs_k(
        &params_dir.join("multi_hop_vk.bin"),
        &params_dir.join("multi_hop_config_params.json"),
        Some(MultiHopKeyManager::KEYGEN_SRS_K),
        &proof_path,
        &out.public_instances,
        &out_snark,
    )?;

    println!(
        "OK: multi_hop -> {} ({} B, {} public instances)",
        out_snark.display(),
        std::fs::metadata(&out_snark)?.len(),
        out.public_instances.len(),
    );
    Ok(())
}
