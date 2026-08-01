//! Prove Circuit 4 (bridge event / withdrawal) with a Poseidon transcript and
//! emit a snark-verifier [`Snark`] for the R15 aggregator pipeline. The
//! resulting `.snark` is the inner SNARK that `bridge-evm-aggregator`'s
//! `export-inner-aggregator --name BridgeWithdrawalAggregatorVerifier` wraps
//! into the production Yul EVM verifier.
//!
//! The reference witness is synthetic by default (`build_synthetic_event_keygen_inputs`):
//! only the **circuit shape** determines the aggregator Yul, so any valid
//! Circuit 4 proof pins the same VK, and real withdrawal proofs generated from
//! live Acki Nacki blocks verify against the emitted verifier byte-for-byte.
//!
//! Pass `--fixture <PrivateWitness.json>` to prove a **real** withdrawal witness
//! (built by the live `bridge-event-witness-builder` from an on-chain
//! `WithdrawalInitiated` event) instead of the synthetic one — the ETH-side
//! prover leg of the M7 pipeline (`our_side_reprove`).
//!
//! ```bash
//! cd crates/bridge-prover-orchestrator
//! cargo run --release --bin export-c4-poseidon-snark -- \
//!   --params-dir ../../params \
//!   --snark-dir ../../proofs/bound/poseidon-snark
//!
//! cd ../bridge-evm-aggregator
//! cargo run --release --bin export-inner-aggregator -- \
//!   --inner-snark ../../proofs/bound/poseidon-snark/circuit4.snark \
//!   --out-dir ../../contracts/ethereum/verifiers \
//!   --name BridgeWithdrawalAggregatorVerifier
//! ```

use std::path::PathBuf;

use anyhow::Context;
use bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs;
use bridge_event_prover_lib::{
    prover::{
        generate_event_proof_from_circuit_with_transcript, generate_event_proof_with_transcript,
    },
    verifier::verify_event_proof_with_transcript,
    PrivateWitness,
};
use bridge_prover_lib::{keys::KeyManager, transcript::TranscriptKind};
use bridge_prover_orchestrator::{
    halo2_snark::export_poseidon_snark_with_srs_k, proof_export::save_instances_binary,
};
use clap::Parser;
use halo2_base::halo2_proofs::poly::commitment::Params;

/// Ensure `params/kzg_bn254_{k}.srs` exists for the event circuit degree,
/// downsized from the fallback manager's K=21 ceremony SRS (the largest across
/// the four circuits). Without this, `export_poseidon_snark`'s internal
/// `gen_srs(k)` would synthesise a *random* SRS on cache miss — a different
/// `s_g2` than the keygen ceremony, which makes the aggregator unable to verify
/// the inner proof. The downsize preserves `g2`/`s_g2` (degree-independent) so
/// the K=19 file shares the K=21 ceremony.
fn ensure_srs_for_event(km: &KeyManager, params_dir: &std::path::Path, event_k: u32) -> anyhow::Result<()> {
    use std::io::Write;
    let srs_path = params_dir.join(format!("kzg_bn254_{event_k}.srs"));
    if srs_path.exists() {
        return Ok(());
    }
    let src = km.fallback.srs();
    let src_k = src.k();
    anyhow::ensure!(
        src_k >= event_k,
        "shared SRS (K={src_k}) is smaller than the event circuit degree (K={event_k})"
    );
    let mut p = src.clone();
    if src_k > event_k {
        p.downsize(event_k);
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

/// Fixed seed for the synthetic keygen/reference witness. Deterministic so two
/// runs pin the same VK (the verifier artefact must be reproducible).
const C4_SEED: u64 = 0xC0FFEE_5E_5E_5E_u64;

#[derive(Parser, Debug)]
#[command(
    name = "export-c4-poseidon-snark",
    about = "Poseidon re-prove of Circuit 4 (bridge event) + Snark export for the R15 aggregator"
)]
struct Args {
    #[arg(long, default_value = "../../params")]
    params_dir: String,
    #[arg(long, default_value = "../../proofs/bound/poseidon-snark")]
    snark_dir: String,
    /// Output snark basename (without extension).
    #[arg(long, default_value = "circuit4")]
    name: String,
    /// Seed for the synthetic reference witness. The default pins the same VK as
    /// the committed verifier; pass a different value to produce a distinct inner
    /// snark (different public inputs, same circuit shape) for the M7
    /// universality check — the deployed verifier must accept both.
    #[arg(long)]
    seed: Option<u64>,
    /// Path to a real `PrivateWitness` JSON (produced by the live
    /// `bridge-event-witness-builder` from an on-chain `WithdrawalInitiated`
    /// event). When set, the inner snark is proven from **real Acki Nacki data**
    /// instead of the synthetic reference witness (`--seed` is then ignored).
    /// The circuit shape — hence the aggregator VK — is identical, so the
    /// resulting snark aggregates against the same committed verifier `.bin`.
    #[arg(long)]
    fixture: Option<PathBuf>,
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

    // KeyManager owns four per-circuit sub-managers; the event sub-manager
    // keygens at K=19 with its own degree-matched SRS.
    let mut km = KeyManager::new(&params_dir);
    km.ensure_event_keys().context("ensure_event_keys failed (keygen)")?;

    // Provision the event-degree SRS (same ceremony as keygen) before the
    // Snark export re-loads it via gen_srs. Downsized from the fallback
    // manager's K=21 slice so the K=19 file shares the ceremony's g2/s_g2.
    let event_k = km.event_config().k as u32;
    ensure_srs_for_event(&km, &params_dir, event_k)?;

    km.load_event_pk().context("load_event_pk failed")?;

    // Either a real PrivateWitness (live AN withdrawal event) or a
    // synthetic-but-valid reference witness. Both drive the identical circuit
    // shape, so the aggregator VK is unchanged; only the public inputs differ.
    let out = if let Some(fixture) = args.fixture.as_ref() {
        println!("proving REAL witness from {}", fixture.display());
        let raw = std::fs::read_to_string(fixture)
            .with_context(|| format!("failed to read fixture {}", fixture.display()))?;
        let witness: PrivateWitness = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse PrivateWitness JSON from {}", fixture.display()))?;
        generate_event_proof_with_transcript(&km, &witness, TranscriptKind::Poseidon)
            .context("Circuit 4 Poseidon proof generation failed (real witness)")?
    } else {
        let seed = args.seed.unwrap_or(C4_SEED);
        println!("proving SYNTHETIC witness (seed={seed:#x})");
        let (circuit, instances) = build_synthetic_event_keygen_inputs(seed);
        generate_event_proof_from_circuit_with_transcript(
            &km,
            circuit,
            instances,
            TranscriptKind::Poseidon,
        )
        .context("Circuit 4 Poseidon proof generation failed (synthetic)")?
    };

    // Native Poseidon self-verify — refuse to export an invalid inner snark
    // (stale/mismatched event keys are the usual culprit; delete
    // params/event_{vk,pk}.bin + event_config_params.json and re-run).
    let ok = verify_event_proof_with_transcript(
        &km,
        &out.proof_bytes,
        &out.public_instances,
        TranscriptKind::Poseidon,
    );
    km.unload_event_pk();
    println!("SELF_VERIFY circuit4 (Poseidon native): {}", if ok { "PASS" } else { "FAIL" });
    anyhow::ensure!(
        ok,
        "Circuit 4 Poseidon inner snark failed native verification — refusing to export an \
         invalid snark. Regenerate event keys against the current circuit shape."
    );

    let proof_path = snark_dir.join(format!("{}.proof.bin", args.name));
    std::fs::write(&proof_path, &out.proof_bytes)?;
    let instances_path = snark_dir.join(format!("{}.instances.bin", args.name));
    save_instances_binary(&out.public_instances, &instances_path)?;
    let out_snark = snark_dir.join(format!("{}.snark", args.name));

    // Event circuit has config.k = 19 but VK was keygen'd against K=20
    // (see `EventKeyManager::KEYGEN_SRS_K`). Pass explicit SRS override so
    // snark-verifier's `compile()` sees `params.k = 20 == vk.domain.k`.
    export_poseidon_snark_with_srs_k(
        &params_dir.join("event_vk.bin"),
        &params_dir.join("event_config_params.json"),
        Some(bridge_prover_lib::keys::EventKeyManager::KEYGEN_SRS_K),
        &proof_path,
        &out.public_instances,
        &out_snark,
    )?;

    println!(
        "OK: circuit4 -> {} ({} B, {} public instances)",
        out_snark.display(),
        std::fs::metadata(&out_snark)?.len(),
        out.public_instances.len(),
    );
    Ok(())
}
