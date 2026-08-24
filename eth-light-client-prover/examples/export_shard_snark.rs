//! M6 transcript bridge — emit a **committee-shard** Halo2 SHPLONK proof under the
//! **Poseidon** Fiat–Shamir transcript, self-verify it, and write the four files
//! `bridge-snark-utils::export_poseidon_snark` needs to wrap it into a
//! `snark_verifier_sdk::Snark` for `crates/bridge-evm-aggregator`.
//!
//! ## Why Poseidon (not Blake2b) here
//!
//! The recursive rotate aggregates N shard proofs inside a snark-verifier
//! `AggregationCircuit`. snark-verifier's in-circuit verifier replays a
//! **Poseidon** transcript, so the inner shard proof MUST use the matching
//! native Poseidon transcript ([`eth_light_client_prover::poseidon_transcript`],
//! vendored from `bridge-prover-lib`, byte-identical to
//! `snark-verifier-sdk`'s `PoseidonTranscript<NativeLoader,_>`). This is the
//! same gosh↔axiom bridge the R15 path uses for Circuit-1A/1B/2/4.
//!
//! ## Why Hermez SRS
//!
//! `export_poseidon_snark` asserts the inner SRS is the Hermez Perpetual Powers
//! of Tau ceremony (`assert_hermez_srs`); the R15 aggregator inputs are all keyed
//! on it. So the shard is keygen'd against Hermez k=20.
//!
//! ## What it emits (`out/shard_snark/`)
//!
//! - `shard_vk.bin`      — `VerifyingKey::write(RawBytesUnchecked)` (the format
//!   `export_poseidon_snark::load_vk` reads, via `BaseCircuitBuilder<Fr>`)
//! - `shard_config.json` — the `BaseCircuitParams` carried alongside the VK
//! - `shard_proof.bin`   — raw SHPLONK proof bytes under the Poseidon transcript
//! - `shard_instances.bin` — `SHARD_INSTANCE_LEN × 32` LE `Fr`
//!   (`[subtree_root_hi, subtree_root_lo, shard_digest]`)
//!
//! ## Next step (the actual aggregation, run on n14)
//!
//! ```bash
//! # 1) emit the shard artifacts (this example):
//! cd eth-light-client-prover && cargo run --release --example export_shard_snark
//! # 2) wrap into a snark-verifier Snark:
//! cd ../crates/bridge-snark-utils   # export_poseidon_snark(vk, config, proof, instances)
//! # 3) aggregate N of them:
//! cd ../bridge-evm-aggregator       # aggregate_inners_direct(vec![shard.snark; N])
//! ```
//!
//! ## Run (n14)
//!
//! ```bash
//! mkdir -p data && wget -O data/kzg_params_20.srs \
//!   https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-20
//! cargo run --release --example export_shard_snark
//! ```
//! Env: `SHARD_SRS_PATH` (default `data/kzg_params_20.srs`), `SHARD_OUT_DIR`
//! (default `out/shard_snark`).

use std::fs;
use std::path::Path;
use std::time::Instant;

use eth_light_client_prover::committee::{
    native_committee_subtree_root, COMMITTEE_SHARDS, PUBKEYS_PER_SHARD,
};
use eth_light_client_prover::mainnet::committee_source;
use eth_light_client_prover::poseidon_transcript::{PoseidonChallenge, PoseidonRead, PoseidonWrite};
use eth_light_client_prover::rotate::{new_committee_hasher, verify_shard, SHARD_INSTANCE_LEN};
use eth_light_client_prover::ssz::load_bytes;

use gosh_sha256_chip::Sha256Chip;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::gates::{RangeChip, RangeInstructions};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr, G1Affine};
use halo2_base::halo2_proofs::halo2curves::serde::SerdeObject;
use halo2_base::halo2_proofs::plonk::{
    create_proof, keygen_pk, keygen_vk, verify_proof, VerifyingKey,
};
use halo2_base::halo2_proofs::poly::commitment::{Params, ParamsProver};
use halo2_base::halo2_proofs::poly::kzg::commitment::{KZGCommitmentScheme, ParamsKZG};
use halo2_base::halo2_proofs::poly::kzg::multiopen::{ProverSHPLONK, VerifierSHPLONK};
use halo2_base::halo2_proofs::poly::kzg::strategy::SingleStrategy;
use halo2_base::halo2_proofs::SerdeFormat;
use rand::rngs::OsRng;

const K: u32 = 20;
const LOOKUP_BITS: usize = 19;
/// Hermez `[s]·G2` raw-bytes head — the ceremony `export_poseidon_snark` requires.
const HERMEZ_SG2_HEAD: [u8; 4] = [0x92, 0x8f, 0xaf, 0xb3];

fn fr_from_le(bytes: &[u8]) -> Fr {
    let mut acc = Fr::from(0u64);
    let mut base = Fr::from(1u64);
    let byte = Fr::from(256u64);
    for &b in bytes {
        acc += Fr::from(b as u64) * base;
        base *= byte;
    }
    acc
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Build one shard circuit; `prover` carries (config, break_points) for the
/// prover stage, `None` for keygen.
fn build_shard(
    slice: &[[u8; 48]],
    prover: Option<(halo2_base::gates::circuit::BaseCircuitParams, Vec<Vec<usize>>)>,
) -> (BaseCircuitBuilder<Fr>, Vec<Fr>) {
    let mut b = match &prover {
        Some((config, bp)) => {
            BaseCircuitBuilder::<Fr>::prover(config.clone(), bp.clone()).use_instance_columns(1)
        }
        None => BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
            .use_k(K as usize)
            .use_lookup_bits(LOOKUP_BITS)
            .use_instance_columns(1),
    };
    let range = RangeChip::new(LOOKUP_BITS, b.lookup_manager().clone());
    let chip = Sha256Chip::new(&range);
    let flat: Vec<u8> = slice.iter().flat_map(|p| p.iter().copied()).collect();
    let cells = {
        let ctx = b.main(0);
        let hasher = new_committee_hasher(ctx, range.gate());
        let pk = load_bytes(ctx, &flat);
        verify_shard(&chip, &hasher, ctx, range.gate(), &pk)
    };
    let inst: Vec<Fr> = cells.iter().map(|c| *c.value()).collect();
    b.assigned_instances[0] = cells;
    b.config_params.lookup_bits = Some(LOOKUP_BITS);
    (b, inst)
}

fn main() -> anyhow::Result<()> {
    println!("=== M6: export committee-shard Poseidon SHPLONK snark (Hermez k={K}) ===\n");

    let srs_path =
        std::env::var("SHARD_SRS_PATH").unwrap_or_else(|_| "data/kzg_params_20.srs".to_string());
    let out_dir = std::env::var("SHARD_OUT_DIR").unwrap_or_else(|_| "out/shard_snark".to_string());
    fs::create_dir_all(&out_dir)?;

    // ---- Load Hermez SRS ----------------------------------------------------
    if !Path::new(&srs_path).exists() {
        anyhow::bail!(
            "Hermez SRS not found at {srs_path}.\n  Fetch it with:\n    mkdir -p data && wget -O \
             {srs_path} https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-{K}"
        );
    }
    println!("Loading SRS {srs_path} ...");
    let mut f = fs::File::open(&srs_path)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    if params.k() > K {
        println!("  downsizing {} -> {K}", params.k());
        params.downsize(K);
    }
    anyhow::ensure!(params.k() == K, "SRS k={} != required {K}", params.k());
    let sg2 = params.s_g2().to_raw_bytes();
    anyhow::ensure!(
        sg2[0..4] == HERMEZ_SG2_HEAD,
        "SRS s_g2 head {:02x?} != Hermez {:02x?} — export_poseidon_snark would reject",
        &sg2[0..4],
        HERMEZ_SG2_HEAD
    );
    println!("  OK: Hermez ceremony, k={K}\n");

    // ---- Source the COMMITTEE_SHARDS shards (real fixture or synthetic) -----
    let source = committee_source();
    let shards = source.shards;
    anyhow::ensure!(shards.len() == COMMITTEE_SHARDS, "expected {COMMITTEE_SHARDS} shards");
    anyhow::ensure!(
        shards.iter().all(|s| s.len() == PUBKEYS_PER_SHARD),
        "every shard must carry {PUBKEYS_PER_SHARD} pubkeys"
    );
    println!(
        "committee source: {} ({} shards × {PUBKEYS_PER_SHARD} pubkeys)\n",
        source.label,
        shards.len()
    );

    // ---- Keygen ONCE (all shards share the circuit shape) ------------------
    let (mut kb, _) = build_shard(&shards[0], None);
    let config = kb.calculate_params(Some(9));
    println!("Shard circuit config: {config:?}\n");

    let t = Instant::now();
    let vk = keygen_vk(&params, &kb).map_err(|e| anyhow::anyhow!("keygen_vk: {e:?}"))?;
    println!("keygen_vk {:?}", t.elapsed());
    let t = Instant::now();
    let pk = keygen_pk(&params, vk, &kb).map_err(|e| anyhow::anyhow!("keygen_pk: {e:?}"))?;
    println!("keygen_pk {:?}", t.elapsed());
    let break_points = kb.break_points();
    drop(kb);

    // ---- Write the shared VK + config once ---------------------------------
    let mut vk_bytes = Vec::new();
    pk.get_vk()
        .write(&mut vk_bytes, SerdeFormat::RawBytesUnchecked)
        .map_err(|e| anyhow::anyhow!("vk write: {e}"))?;
    // Assert the VK round-trips through the exact reader the aggregator uses.
    let mut slice_rd: &[u8] = &vk_bytes;
    let _vk_rt = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut slice_rd,
        SerdeFormat::RawBytesUnchecked,
        config.clone(),
    )
    .map_err(|e| anyhow::anyhow!("VK does not read back via BaseCircuitBuilder<Fr>: {e}"))?;
    fs::write(format!("{out_dir}/shard_vk.bin"), &vk_bytes)?;
    fs::write(format!("{out_dir}/shard_config.json"), serde_json::to_vec_pretty(&config)?)?;
    println!(
        "shared: shard_vk.bin ({} B, sha256 {}) + shard_config.json\n",
        vk_bytes.len(),
        sha256_hex(&vk_bytes)
    );

    // ---- Prove each distinct shard (reusing pk + break_points) -------------
    for (idx, slice) in shards.iter().enumerate() {
        let subtree = native_committee_subtree_root(slice);
        let native_hi = fr_from_le(&subtree[0..16]);
        let native_lo = fr_from_le(&subtree[16..32]);

        let (pb, inst) = build_shard(slice, Some((config.clone(), break_points.clone())));
        assert_eq!(inst.len(), SHARD_INSTANCE_LEN);
        anyhow::ensure!(inst[0] == native_hi, "shard {idx} instance[0] subtree_root_hi != native");
        anyhow::ensure!(inst[1] == native_lo, "shard {idx} instance[1] subtree_root_lo != native");

        let instances: &[&[Fr]] = &[&inst];
        let t = Instant::now();
        let mut transcript = PoseidonWrite::init(Vec::<u8>::new());
        create_proof::<
            KZGCommitmentScheme<Bn256>,
            ProverSHPLONK<'_, Bn256>,
            PoseidonChallenge,
            _,
            _,
            _,
        >(&params, &pk, &[pb], &[instances], OsRng, &mut transcript)
        .map_err(|e| anyhow::anyhow!("create_proof shard {idx}: {e:?}"))?;
        let proof = transcript.finalize();

        // self-verify under the same Poseidon transcript
        let strategy = SingleStrategy::new(&params);
        let mut vt = PoseidonRead::init(proof.as_slice());
        let ok = verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            PoseidonChallenge,
            PoseidonRead<&[u8]>,
            SingleStrategy<'_, Bn256>,
        >(params.verifier_params(), pk.get_vk(), strategy, &[instances], &mut vt)
        .is_ok();
        anyhow::ensure!(ok, "shard {idx} Poseidon SHPLONK verify_proof REJECTED");

        let mut pi_bytes = Vec::with_capacity(inst.len() * 32);
        for fr in &inst {
            pi_bytes.extend_from_slice(fr.to_bytes().as_ref());
        }
        fs::write(format!("{out_dir}/shard_{idx}_proof.bin"), &proof)?;
        fs::write(format!("{out_dir}/shard_{idx}_instances.bin"), &pi_bytes)?;
        // shard 0 also under the legacy singular names (measure_agg_node / rotate_tree_fold).
        if idx == 0 {
            fs::write(format!("{out_dir}/shard_proof.bin"), &proof)?;
            fs::write(format!("{out_dir}/shard_instances.bin"), &pi_bytes)?;
        }
        println!(
            "shard {idx}: prove {:?} -> {} B (sha256 {}), self-verify PASS",
            t.elapsed(),
            proof.len(),
            sha256_hex(&proof)
        );
    }

    println!(
        "\n=== RESULT: PASS — {} DISTINCT shard Poseidon snarks written to {out_dir} ===",
        shards.len()
    );
    println!("  shard_{{0..{}}}_proof.bin + shard_{{...}}_instances.bin", COMMITTEE_SHARDS - 1);
    println!("  shard_vk.bin + shard_config.json (shared)");
    println!("\nAggregate next: cargo run --release --features aggregation --example rotate_tree_n8");
    Ok(())
}
