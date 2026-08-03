//! ETH-side re-prove of the AN→ETH `verifyBlock` circuits — 1A (primary
//! attestation), 1B (fallback attestation) and 2 (layer-hashes movement) — with
//! a **Poseidon** transcript, emitting a snark-verifier [`Snark`] for the R15
//! aggregator pipeline. The sibling of `export-c4-poseidon-snark`, for the
//! attestation/layer leg instead of the withdrawal leg.
//!
//! **All the real work reuses Alina's `bridge-prover-lib` public API** — the same
//! functions `bridge-prover-daemon` drives. The daemon proves Blake2b (the AN-VM
//! `ZKHALO2VERIFYWITHVK` flavour); the ETH aggregator only consumes Poseidon, so
//! this binary re-proves the same live witness with `TranscriptKind::Poseidon`
//! (the `our_side_reprove` architecture). No mock, no synthetic data: the witness
//! is fetched live from an Acki Nacki GraphQL endpoint.
//!
//! ```bash
//! cd crates/bridge-prover-orchestrator
//!
//! # 1A / 1B — attestation (stateless: attestation bytes + bk_set + last_seen)
//! cargo run --release --bin export-1a1b2-poseidon-snark -- \
//!   --endpoint https://shellnet.ackinacki.org/graphql \
//!   --seqno <KEY_BLOCK_SEQNO> --circuit auto \
//!   --params-dir ../../params \
//!   --snark-dir ../../proofs/bound/poseidon-snark
//!
//! # 2 — layer hashes (needs the daemon's persisted chain anchor state.json)
//! cargo run --release --bin export-1a1b2-poseidon-snark -- \
//!   --endpoint https://shellnet.ackinacki.org/graphql \
//!   --seqno <KEY_BLOCK_SEQNO> --circuit layer \
//!   --state /path/to/daemon/state.json \
//!   --params-dir ../../params \
//!   --snark-dir ../../proofs/bound/poseidon-snark
//!
//! # then aggregate → EVM calldata (self-checked vs committed verifier .bin)
//! cd ../bridge-evm-aggregator
//! cargo run --release --bin aggregate-proof -- \
//!   --inner-snark ../../proofs/bound/poseidon-snark/circuit1a.snark \
//!   --name PrimaryAggregatorVerifier \
//!   --verifiers-dir ../../contracts/ethereum/verifiers \
//!   --out ../../proofs/bound/poseidon-snark/circuit1a_calldata.bin
//! ```

use std::path::{Path, PathBuf};

use anyhow::Context;
use bridge_gql_fetcher::{
    attestation_fetcher::{self, AttestationEvidence},
    bk_set_fetcher, gql_client,
};
use bridge_poseidon::compute_bk_set_poseidon;
use bridge_prover_lib::{
    block_id_tree,
    bridge_state::BridgeState,
    layer_prover,
    poseidon_dense::HISTORY_PROOF_WINDOW_SIZE,
    prover, real_chain_builder,
    keys::KeyManager,
    transcript::TranscriptKind,
    verifier, Fr,
};
use bridge_prover_orchestrator::{halo2_snark::export_poseidon_snark_with_srs_k, proof_export::save_instances_binary};
use clap::{Parser, ValueEnum};
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use halo2_base::halo2_proofs::poly::commitment::Params;

/// Which `verifyBlock` circuit to re-prove.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum CircuitSel {
    /// Circuit 1A — primary attestation (`[PRIMARY]` evidence).
    Primary,
    /// Circuit 1B — fallback attestation (`[PRIMARY, FALLBACK]` evidence).
    Fallback,
    /// Circuit 2 — layer-hashes movement (needs `--state`).
    Layer,
    /// Auto-detect 1A vs 1B from the on-chain attestation evidence.
    Auto,
}

#[derive(Parser, Debug)]
#[command(
    name = "export-1a1b2-poseidon-snark",
    about = "Live re-prove of Circuits 1A/1B/2 (Poseidon) + Snark export for the R15 aggregator"
)]
struct Args {
    /// Acki Nacki GraphQL endpoint (e.g. https://shellnet.ackinacki.org/graphql).
    #[arg(long)]
    endpoint: String,
    /// Target key-block sequence number.
    #[arg(long)]
    seqno: u64,
    /// Which circuit to prove. `auto` classifies attestation evidence (1A vs 1B).
    #[arg(long, value_enum, default_value_t = CircuitSel::Auto)]
    circuit: CircuitSel,
    /// `last_seen_block_seqno` public input for the attestation circuits (1A/1B).
    #[arg(long, default_value_t = 0)]
    last_seen: u32,
    /// Daemon `state.json` (required for `--circuit layer`): the persisted chain
    /// anchor `real_chain_builder::build_real_chain` walks back from.
    #[arg(long)]
    state: Option<PathBuf>,
    /// Optional BK-set JSON fallback if the endpoint doesn't expose the set.
    #[arg(long)]
    bk_set_config: Option<String>,
    #[arg(long, default_value = "../../params")]
    params_dir: String,
    #[arg(long, default_value = "../../proofs/bound/poseidon-snark")]
    snark_dir: String,
    /// Output snark basename (without extension). Defaults to the circuit tag
    /// (`circuit1a` / `circuit1b` / `circuit2`).
    #[arg(long)]
    name: Option<String>,
    /// Fast pre-flight: fetch the target block's `block_merkle_tree_leaves[2]`
    /// (the node's BK-set Poseidon commitment) and compare it to
    /// `compute_bk_set_poseidon(bk_set)`. Prints both and exits WITHOUT proving.
    /// A mismatch means the loaded BK set does not match the set that signed the
    /// block (stale config / chain rotated) — the root cause of a 1A/1B/2
    /// self-verify FAIL, catchable in ~1s instead of a multi-minute prove.
    #[arg(long, default_value_t = false)]
    check_bk_set: bool,
}

/// Provision `params/kzg_bn254_{k}.srs` for `circuit_k`, downsized from the
/// fallback manager's K=21 ceremony SRS (the largest across the four circuits)
/// so `export_poseidon_snark`'s internal `gen_srs(k)` re-loads the ceremony
/// (matching `g2`/`s_g2`) instead of synthesising a random SRS on cache miss.
/// Identical to the C4 exporter's `ensure_srs_for_event`, generalised to any
/// degree.
fn ensure_srs_for(km: &KeyManager, params_dir: &Path, circuit_k: u32) -> anyhow::Result<()> {
    use std::io::Write;
    let srs_path = params_dir.join(format!("kzg_bn254_{circuit_k}.srs"));
    if srs_path.exists() {
        return Ok(());
    }
    let src = km.fallback.srs();
    let src_k = src.k();
    anyhow::ensure!(
        src_k >= circuit_k,
        "shared SRS (K={src_k}) is smaller than the circuit degree (K={circuit_k})"
    );
    let mut p = src.clone();
    if src_k > circuit_k {
        p.downsize(circuit_k);
    }
    let mut w = std::io::BufWriter::new(std::fs::File::create(&srs_path)?);
    p.write(&mut w)?;
    w.flush()?;
    println!("provisioned {} (downsized from K={src_k} ceremony)", srs_path.display());
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let gql = gql_client::create_client(&args.endpoint)
        .with_context(|| format!("create GraphQL client for {}", args.endpoint))?;

    // BK set — the former GraphQL fetch (`fetch_bk_set`) was disabled on
    // 2026-07-22 as architecturally broken (see `bk_set_fetcher.rs`), so the
    // config file is now the only source.
    let cfg = args.bk_set_config.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "--bk-set-config <path> is required (GraphQL BK-set fetch is disabled)",
        )
    })?;
    let bk_set = bk_set_fetcher::load_bk_set_from_config(cfg)?;
    println!("BK set loaded: {} keepers", bk_set.len());

    // Fast pre-flight: is this the set that actually signed the block?
    if args.check_bk_set {
        let (bk_commit, _) = compute_bk_set_poseidon(&bk_set);
        let ours: [u8; 32] = bk_commit.to_repr();
        let block = gql
            .query_proof_block_by_seqno(args.seqno)
            .await
            .context("query_proof_block_by_seqno (check-bk-set)")?;
        let leaves = block.block_merkle_tree_leaves.ok_or_else(|| {
            anyhow::anyhow!("block {} has no block_merkle_tree_leaves", args.seqno)
        })?;
        let node = leaves[2];
        let matches = ours == node;
        println!("bk_set_poseidon(ours) = {}", hex::encode(ours));
        println!("block.leaves[2] (node) = {}", hex::encode(node));
        println!("MATCH: {}", if matches { "YES — correct set" } else { "NO — wrong/stale set" });
        anyhow::ensure!(matches, "BK-set commitment mismatch for block {}", args.seqno);
        return Ok(());
    }

    let mut km = KeyManager::new(&params_dir);

    // Resolve `auto` → 1A/1B by inspecting the live attestation evidence.
    let selected = match args.circuit {
        CircuitSel::Layer => CircuitSel::Layer,
        CircuitSel::Primary => CircuitSel::Primary,
        CircuitSel::Fallback => CircuitSel::Fallback,
        CircuitSel::Auto => {
            let evidence = attestation_fetcher::fetch_attestation_evidence(&gql, args.seqno as u32)
                .await
                .context("fetch_attestation_evidence (auto classify)")?;
            match evidence {
                AttestationEvidence::Primary(_) => {
                    println!("auto: block {} is PRIMARY -> Circuit 1A", args.seqno);
                    CircuitSel::Primary
                }
                AttestationEvidence::Fallback { .. } => {
                    println!("auto: block {} is FALLBACK -> Circuit 1B", args.seqno);
                    CircuitSel::Fallback
                }
            }
        }
    };

    match selected {
        CircuitSel::Primary => {
            prove_primary(&mut km, &gql, &params_dir, &snark_dir, &bk_set, &args).await
        }
        CircuitSel::Fallback => {
            prove_fallback(&mut km, &gql, &params_dir, &snark_dir, &bk_set, &args).await
        }
        CircuitSel::Layer => {
            prove_layer(&mut km, &gql, &params_dir, &snark_dir, &bk_set, &args).await
        }
        CircuitSel::Auto => unreachable!("auto resolved above"),
    }
}

async fn prove_primary(
    km: &mut KeyManager,
    gql: &gql_client::GqlClient,
    params_dir: &Path,
    snark_dir: &Path,
    bk_set: &std::collections::HashMap<u16, Vec<u8>>,
    args: &Args,
) -> anyhow::Result<()> {
    let evidence = attestation_fetcher::fetch_attestation_evidence(gql, args.seqno as u32)
        .await
        .context("fetch_attestation_evidence")?;
    let primary = match &evidence {
        AttestationEvidence::Primary(p) => p.clone(),
        AttestationEvidence::Fallback { primary, .. } => {
            println!(
                "warning: block {} is FALLBACK evidence but --circuit primary forced; \
                 proving the PRIMARY half only (use --circuit fallback for the real 1B proof)",
                args.seqno
            );
            primary.clone()
        }
    };

    km.ensure_primary_keys(bk_set).context("ensure_primary_keys (keygen)")?;
    let k = km.primary_config().k as u32;
    ensure_srs_for(km, params_dir, k)?;
    km.load_primary_pk().context("load_primary_pk")?;

    let out = prover::generate_primary_proof_with_transcript(
        km,
        &primary.raw_bytes,
        bk_set,
        args.last_seen,
        TranscriptKind::Poseidon,
    )
    .context("Circuit 1A Poseidon proof generation failed")?;

    let instances = vec![
        out.block_id_fr,
        out.bk_set_commitment_fr,
        Fr::from(out.block_seq_no as u64),
        Fr::from(out.last_seen_block_seqno as u64),
    ];
    let ok = verifier::verify_primary_proof_with_transcript(
        km,
        &out.proof_bytes,
        &instances,
        TranscriptKind::Poseidon,
    );
    km.unload_primary_pk();
    finish("circuit1a", "primary", ok, &out.proof_bytes, &instances, params_dir, snark_dir, args)
}

async fn prove_fallback(
    km: &mut KeyManager,
    gql: &gql_client::GqlClient,
    params_dir: &Path,
    snark_dir: &Path,
    bk_set: &std::collections::HashMap<u16, Vec<u8>>,
    args: &Args,
) -> anyhow::Result<()> {
    let evidence = attestation_fetcher::fetch_attestation_evidence(gql, args.seqno as u32)
        .await
        .context("fetch_attestation_evidence")?;
    let (primary, fallback) = match &evidence {
        AttestationEvidence::Fallback { primary, fallback } => (primary.clone(), fallback.clone()),
        AttestationEvidence::Primary(_) => anyhow::bail!(
            "block {} is PRIMARY evidence (single attestation); it cannot be proven with \
             Circuit 1B (fallback needs a [PRIMARY, FALLBACK] pair). Use --circuit primary.",
            args.seqno
        ),
    };

    km.ensure_fallback_keys(bk_set).context("ensure_fallback_keys (keygen)")?;
    let k = km.fallback_config().k as u32;
    ensure_srs_for(km, params_dir, k)?;
    km.load_fallback_pk().context("load_fallback_pk")?;

    let out = prover::generate_fallback_proof_with_transcript(
        km,
        &primary.raw_bytes,
        &fallback.raw_bytes,
        bk_set,
        args.last_seen,
        TranscriptKind::Poseidon,
    )
    .context("Circuit 1B Poseidon proof generation failed")?;

    let instances = vec![
        out.block_id_fr,
        out.bk_set_commitment_fr,
        Fr::from(out.block_seq_no as u64),
        Fr::from(out.last_seen_block_seqno as u64),
    ];
    let ok = verifier::verify_fallback_proof_with_transcript(
        km,
        &out.proof_bytes,
        &instances,
        TranscriptKind::Poseidon,
    );
    km.unload_fallback_pk();
    finish("circuit1b", "fallback", ok, &out.proof_bytes, &instances, params_dir, snark_dir, args)
}

async fn prove_layer(
    km: &mut KeyManager,
    gql: &gql_client::GqlClient,
    params_dir: &Path,
    snark_dir: &Path,
    bk_set: &std::collections::HashMap<u16, Vec<u8>>,
    args: &Args,
) -> anyhow::Result<()> {
    let state_path = args.state.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "--circuit layer requires --state <daemon state.json> (the persisted chain anchor \
             real_chain_builder walks back from); this is the same real input the Blake2b daemon \
             consumes, not a mock"
        )
    })?;
    let state = BridgeState::load(
        state_path.to_str().context("state path not UTF-8")?,
        HISTORY_PROOF_WINDOW_SIZE,
    )
    .with_context(|| format!("load BridgeState from {}", state_path.display()))?;

    km.ensure_layer_keys().context("ensure_layer_keys (keygen)")?;
    let k = km.layer_config().k as u32;
    ensure_srs_for(km, params_dir, k)?;
    km.load_layer_pk().context("load_layer_pk")?;

    // bk_set Poseidon commitment (fail-fast against block.leaves[2] below).
    let (bk_commit, _) = compute_bk_set_poseidon(bk_set);

    // Replays bridge-prover-daemon::generate_layer_proof_for_key_block over the
    // public lib API (build_layer_hashes_preimage / BlockIdMerkleTree /
    // build_real_chain) — identical witness, Poseidon transcript instead of
    // Blake2b.
    let block = gql
        .query_proof_block_by_seqno(args.seqno)
        .await
        .context("query_proof_block_by_seqno")?;
    let leaves = block
        .block_merkle_tree_leaves
        .ok_or_else(|| anyhow::anyhow!("block {} has no block_merkle_tree_leaves", args.seqno))?;
    anyhow::ensure!(
        !block.history_proofs.is_empty(),
        "block {} has no history_proofs",
        args.seqno
    );

    let num_layers = block.history_proofs.len() as u8;
    let mut root_hashes = Vec::with_capacity(10);
    for i in 1..=10u8 {
        root_hashes.push(block.history_proofs.get(&i).copied().unwrap_or([0u8; 32]));
    }
    let preimage = block_id_tree::build_layer_hashes_preimage(num_layers as usize, &root_hashes);

    let tree = block_id_tree::BlockIdMerkleTree::from_leaves(leaves);
    let siblings = tree.siblings_for_l0();
    println!("block_id from GQL leaves merkle root: {}", hex::encode(tree.block_id()));

    let bk_hash_bytes: [u8; 32] = bk_commit.to_repr();
    anyhow::ensure!(
        bk_hash_bytes == leaves[2],
        "loaded BK set Poseidon commitment ({}) != block.leaves[2] ({}) — bk_set is stale or \
         the chain rotated keys; refresh the BK set",
        hex::encode(bk_hash_bytes),
        hex::encode(leaves[2]),
    );

    let chain = real_chain_builder::build_real_chain(
        gql,
        &state,
        &block.history_proofs,
        args.seqno,
        HISTORY_PROOF_WINDOW_SIZE as u64,
    )
    .await
    .context("build_real_chain")?;
    println!("using REAL chain proofs ({} steps)", chain.num_steps);

    let prev_hash_fr = gosh_dense_balanced_tree::bytes_to_fr(&chain.prev_hash);

    let out = layer_prover::generate_layer_proof_with_transcript(
        km,
        &preimage,
        &siblings,
        prev_hash_fr,
        chain.num_steps,
        &chain.chain_links,
        bk_commit,
        TranscriptKind::Poseidon,
    )
    .context("Circuit 2 Poseidon proof generation failed")?;

    // Rebuild the 14-element public-instance vector for self-verify + export.
    let mut instances = Vec::with_capacity(layer_prover::LAYER_HASHES_NUM_PUBLIC_INPUTS);
    instances.push(out.block_id_fr);
    instances.push(out.bk_set_poseidon_hash_fr);
    instances.push(Fr::from(out.num_layers as u64));
    for h in out.layer_hash_frs.iter() {
        instances.push(*h);
    }
    instances.push(out.prev_max_level_layer_hash_fr);

    let ok = verifier::verify_layer_proof_with_transcript(
        km,
        &out.proof_bytes,
        &instances,
        TranscriptKind::Poseidon,
    );
    km.unload_layer_pk();
    finish("circuit2", "layer", ok, &out.proof_bytes, &instances, params_dir, snark_dir, args)
}

/// Shared tail: self-verify gate + proof/instances/snark export.
#[allow(clippy::too_many_arguments)]
fn finish(
    default_name: &str,
    key_prefix: &str,
    self_verified: bool,
    proof_bytes: &[u8],
    instances: &[Fr],
    params_dir: &Path,
    snark_dir: &Path,
    args: &Args,
) -> anyhow::Result<()> {
    println!(
        "SELF_VERIFY {default_name} (Poseidon native): {}",
        if self_verified { "PASS" } else { "FAIL" }
    );
    anyhow::ensure!(
        self_verified,
        "{default_name} Poseidon inner snark failed native verification — refusing to export an \
         invalid snark. Regenerate {key_prefix} keys against the current circuit shape."
    );

    let name = args.name.clone().unwrap_or_else(|| default_name.to_string());
    let proof_path = snark_dir.join(format!("{name}.proof.bin"));
    std::fs::write(&proof_path, proof_bytes)?;
    let instances_path = snark_dir.join(format!("{name}.instances.bin"));
    save_instances_binary(instances, &instances_path)?;
    let out_snark = snark_dir.join(format!("{name}.snark"));

    // The layer VK is keygen'd against the shared K=20 ceremony SRS while
    // `layer_config_params.json` records k=17 — see the doc-comment on
    // `export_poseidon_snark_with_srs_k`. Without this override the call
    // panics with `assertion left(20) == right(17)` inside snark_verifier's
    // `compile()`. Primary/fallback have config.k matching their keygen SRS,
    // so no override is needed there.
    let srs_k_override = match key_prefix {
        "layer" => Some(20u32),
        _ => None,
    };

    export_poseidon_snark_with_srs_k(
        &params_dir.join(format!("{key_prefix}_vk.bin")),
        &params_dir.join(format!("{key_prefix}_config_params.json")),
        srs_k_override,
        &proof_path,
        instances,
        &out_snark,
    )?;

    println!(
        "OK: {name} -> {} ({} B, {} public instances)",
        out_snark.display(),
        std::fs::metadata(&out_snark)?.len(),
        instances.len(),
    );
    Ok(())
}
