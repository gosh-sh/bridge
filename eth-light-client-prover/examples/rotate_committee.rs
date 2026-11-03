//! The rotate on the table SHA-256: two proofs instead of the shard tree.
//!
//! ```text
//!   committee circuit, k=18      SSZ root of next_sync_committee, its branch to
//!        │                       the attested state_root (gindex 87 | 2946), the
//!        │                       Poseidon commitment → [next, sr_hi, sr_lo,
//!        │                       attested_slot, gloas_fork_slot]
//!        ▼  stock AggregationCircuit over that one snark
//!   wrap, k=WRAP_K               12 accumulator limbs + the 5 committee instances,
//!                                proved under Blake2b for ZKHALO2VERIFYWITHVK
//! ```
//!
//! The rotate binds no committee of its own: the light client checks the step
//! proof of the same update, whose signers are the current committee, and that
//! its attested state root and slot are the ones here.
//!
//! ## Run
//!
//! ```bash
//! cd eth-light-client-prover
//! /usr/bin/time -v env OUTER_SRS=../params/kzg_bn254_21.srs \
//!   ROTATE_UPDATE_PATH=update.json ROTATE_KEY_DIR=keys \
//!   cargo run --release --features aggregation --example rotate_committee
//! ```
//!
//! Env: `OUTER_SRS` (required, Hermez k=21; the committee proof reads it cut
//! to k=18), `WRAP_K` (default 21), `ROTATE_UPDATE_PATH` (the live update the
//! committee is taken from; a synthetic committee without it),
//! `BEACON_GLOAS_FORK_SLOT`, `ROTATE_KEY_DIR` (keeps both proving keys between
//! runs), `ROTATE_OUT` (default `out/rotate_committee`).

use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use eth_light_client_prover::{
    committee_table::{
        native_instances, CommitteeCircuit, CommitteeCircuitParams, CommitteeWitness,
        COMMITTEE_INSTANCE_LEN, COMMITTEE_TABLE_K,
    },
    key_cache::KeyCache,
    live_witness::ChainParams,
    mainnet::{rotate_update_from_env, SLOTS_PER_SYNC_PERIOD},
};
use halo2_base::{
    gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, CircuitBuilderStage},
    halo2_proofs::{
        halo2curves::{
            bn256::{pairing, Bn256, Fq, Fr, G1Affine},
            ff::Field,
            CurveAffine,
        },
        plonk::{verify_proof, VerifyingKey},
        poly::{
            commitment::{Params, ParamsProver},
            kzg::{
                commitment::{KZGCommitmentScheme, ParamsKZG},
                multiopen::VerifierSHPLONK,
                strategy::SingleStrategy,
            },
        },
        transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer},
        SerdeFormat,
    },
    utils::testing::gen_proof_with_instances,
};
use snark_verifier_sdk::{
    gen_pk,
    halo2::{
        aggregation::{AggregationCircuit, AggregationConfigParams, VerifierUniversality},
        gen_snark_shplonk,
    },
    CircuitExt, SHPLONK,
};

/// Frozen `VkBlob` magic (see `deposit-prover/src/halo2_tvm_bundle.rs`).
const VK_BLOB_MAGIC: &[u8; 8] = b"VKBLOB\x00\x00";
/// `lhs.x ‖ lhs.y ‖ rhs.x ‖ rhs.y`, 3 limbs of 88 bits each. Byte 11 of the
/// VkBlob: the opcode runs the KZG decider over `instances[0..12]`.
const ACCUMULATOR_LIMBS: usize = 12;
const LIMB_BITS: usize = 88;

fn load_srs(path: &Path, k: u32) -> anyhow::Result<ParamsKZG<Bn256>> {
    anyhow::ensure!(path.exists(), "SRS not found at {}", path.display());
    let mut params = ParamsKZG::<Bn256>::read(&mut fs::File::open(path)?)?;
    if params.k() > k {
        params.downsize(k);
    }
    anyhow::ensure!(params.k() == k, "SRS k={} != required {k}", params.k());
    Ok(params)
}

fn write_chunk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Base v1 VkBlob, as `rotate_tree_n8.rs` writes it.
fn encode_base_v1_vkblob(config: &BaseCircuitParams, vk: &VerifyingKey<G1Affine>) -> Vec<u8> {
    let mut vk_bytes = Vec::new();
    vk.write(&mut vk_bytes, SerdeFormat::RawBytes)
        .expect("VerifyingKey::write(RawBytes)");
    let mut out = Vec::new();
    out.extend_from_slice(VK_BLOB_MAGIC);
    out.push(1); // version = 1 (Base)
    out.push(0); // transcript = Blake2b
    out.push(0); // shape = Base
    out.push(ACCUMULATOR_LIMBS as u8);
    out.extend_from_slice(&[0u8; 4]);
    write_chunk(
        &mut out,
        &serde_json::to_vec(config).expect("serialise BaseCircuitParams"),
    );
    write_chunk(&mut out, &vk_bytes);
    out
}

/// The VkBlob back into its config and verifying key, the way the opcode reads
/// it.
fn read_base_v1_vkblob(bytes: &[u8]) -> anyhow::Result<VerifyingKey<G1Affine>> {
    anyhow::ensure!(
        bytes.len() >= 16 && &bytes[0..8] == VK_BLOB_MAGIC,
        "VkBlob header"
    );
    anyhow::ensure!(
        bytes[8..12] == [1, 0, 0, ACCUMULATOR_LIMBS as u8],
        "VkBlob version bytes"
    );
    let chunk = |off: usize| -> (&[u8], usize) {
        let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        (&bytes[off + 4..off + 4 + len], off + 4 + len)
    };
    let (cfg, off) = chunk(16);
    let (mut vk, _) = chunk(off);
    let cfg: BaseCircuitParams = serde_json::from_slice(cfg)?;
    VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(&mut vk, SerdeFormat::RawBytes, cfg)
        .map_err(|e| anyhow::anyhow!("VerifyingKey::read: {e}"))
}

/// One G1 coordinate from its little-endian 88-bit limbs.
fn coord_from_limbs(limbs: &[Fr]) -> Option<Fq> {
    let mut base = [0u8; 32];
    base[LIMB_BITS / 8] = 1;
    let base = Option::<Fq>::from(Fq::from_bytes(&base))?;
    let (mut acc, mut p) = (Fq::ZERO, Fq::ONE);
    for l in limbs {
        acc += Option::<Fq>::from(Fq::from_bytes(&l.to_bytes()))? * p;
        p *= base;
    }
    Some(acc)
}

/// The decider the opcode runs: `e(lhs, g2) == e(rhs, s_g2)`.
fn decide(acc: &[Fr], params: &ParamsKZG<Bn256>) -> bool {
    let point = |l: &[Fr]| -> Option<G1Affine> {
        Option::from(G1Affine::from_xy(
            coord_from_limbs(&l[0..3])?,
            coord_from_limbs(&l[3..6])?,
        ))
    };
    match (point(&acc[0..6]), point(&acc[6..12])) {
        (Some(lhs), Some(rhs)) => pairing(&lhs, &params.g2()) == pairing(&rhs, &params.s_g2()),
        _ => false,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn fr_hex(f: &Fr) -> String {
    hex::encode(f.to_bytes().iter().rev().copied().collect::<Vec<u8>>())
}

fn main() -> anyhow::Result<()> {
    let srs = PathBuf::from(
        std::env::var("OUTER_SRS")
            .map_err(|_| anyhow::anyhow!("OUTER_SRS is not set (Hermez kzg_bn254_21.srs)"))?,
    );
    let wrap_k: u32 = std::env::var("WRAP_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(21);
    let out = PathBuf::from(
        std::env::var("ROTATE_OUT").unwrap_or_else(|_| "out/rotate_committee".to_string()),
    );
    fs::create_dir_all(&out)?;
    let gloas_fork_slot = ChainParams::from_env()?.gloas_fork_slot;
    let witness = match rotate_update_from_env()? {
        Some((path, update)) => {
            println!("committee of {}", path.display());
            CommitteeWitness::from_update(&update, gloas_fork_slot)?
        },
        None => {
            println!("synthetic committee");
            CommitteeWitness::synthetic(1, 9_412 * SLOTS_PER_SYNC_PERIOD, gloas_fork_slot)
        },
    };
    let expected = native_instances(&witness);
    println!(
        "attested slot {}, gloas fork slot {}, next_commit 0x{}\n",
        witness.attested_slot,
        witness.gloas_fork_slot,
        fr_hex(&expected[0])
    );
    let keys = KeyCache::from_env()?;

    // ---- committee circuit ------------------------------------------------
    let params_c = load_srs(&srs, COMMITTEE_TABLE_K as u32)?;
    let stored = match &keys {
        Some(keys) => {
            keys.load::<CommitteeCircuit, CommitteeCircuitParams>("committee", |p| p.clone())?
        },
        None => None,
    };
    let (c_params, c_bp, c_pk) = match stored {
        Some(key) => {
            println!(
                "committee: proving key from {}",
                keys.as_ref().unwrap().dir().display()
            );
            (key.params, key.break_points, key.pk)
        },
        None => {
            let mut kg = CommitteeCircuit::keygen(witness.clone());
            let p = kg.calculate_params();
            println!("committee: {:?}", p.base);
            let t = Instant::now();
            let pk = gen_pk(&params_c, &kg, None);
            println!("committee: keygen {:.1}s", t.elapsed().as_secs_f64());
            let bp = kg.break_points();
            if let Some(keys) = &keys {
                keys.store("committee", &p, &bp, &pk)?;
            }
            (p, bp, pk)
        },
    };
    let mut c_vk = Vec::new();
    c_pk.get_vk().write(&mut c_vk, SerdeFormat::RawBytes)?;
    let t = Instant::now();
    let committee = gen_snark_shplonk(
        &params_c,
        &c_pk,
        CommitteeCircuit::prover(witness.clone(), c_params, c_bp),
        None::<&Path>,
    );
    println!(
        "committee: prove {:.1}s, {} B, vk sha256 {}\n",
        t.elapsed().as_secs_f64(),
        committee.proof().len(),
        sha256_hex(&c_vk)
    );
    anyhow::ensure!(
        committee.instances == vec![expected.clone()],
        "committee instances"
    );
    drop(c_pk);
    drop(params_c);

    // ---- wrap ---------------------------------------------------------------
    let params_w = load_srs(&srs, wrap_k)?;
    let wrap_keys = keys
        .as_ref()
        .map(|k| k.with_tag(&format!("committee-vk-{}", sha256_hex(&c_vk))));
    let stored = match &wrap_keys {
        Some(keys) => {
            keys.load::<AggregationCircuit, AggregationConfigParams>("committee-wrap", |c| *c)?
        },
        None => None,
    };
    let (w_params, w_bp, w_pk) = match stored {
        Some(key) => {
            println!(
                "wrap: proving key from {}",
                wrap_keys.as_ref().unwrap().dir().display()
            );
            (key.params, key.break_points, key.pk)
        },
        None => {
            let cfg = AggregationConfigParams {
                degree: wrap_k,
                lookup_bits: (wrap_k - 1) as usize,
                ..Default::default()
            };
            let mut kg = AggregationCircuit::new::<SHPLONK>(
                CircuitBuilderStage::Keygen,
                cfg,
                &params_w,
                vec![committee.clone()],
                VerifierUniversality::None,
            );
            kg.expose_previous_instances(false);
            let p = kg.calculate_params(Some(10));
            println!("wrap: {p:?}");
            let t = Instant::now();
            let pk = gen_pk(&params_w, &kg, None);
            println!("wrap: keygen {:.1}s", t.elapsed().as_secs_f64());
            let bp = kg.break_points();
            if let Some(keys) = &wrap_keys {
                keys.store("committee-wrap", &p, &bp, &pk)?;
            }
            (p, bp, pk)
        },
    };
    let mut wrap = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        w_params,
        &params_w,
        vec![committee],
        VerifierUniversality::None,
    )
    .use_break_points(w_bp);
    wrap.expose_previous_instances(false);
    let inst = wrap.instances().remove(0);
    anyhow::ensure!(
        inst.len() == ACCUMULATOR_LIMBS + COMMITTEE_INSTANCE_LEN
            && inst[ACCUMULATOR_LIMBS..] == expected[..],
        "wrap instances: {} of them, not 12 accumulator limbs + the committee's",
        inst.len()
    );
    let t = Instant::now();
    let proof = gen_proof_with_instances(&params_w, &w_pk, wrap, &[&inst]);
    println!(
        "wrap: prove {:.1}s, {} B",
        t.elapsed().as_secs_f64(),
        proof.len()
    );

    // ---- what the opcode does with it ----------------------------------------
    let base_params: BaseCircuitParams = w_params.into();
    let vk_blob = encode_base_v1_vkblob(&base_params, w_pk.get_vk());
    let vk = read_base_v1_vkblob(&vk_blob)?;
    let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(proof.as_slice());
    let refs: &[&[Fr]] = &[&inst];
    verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
        SingleStrategy<'_, Bn256>,
    >(
        params_w.verifier_params(),
        &vk,
        SingleStrategy::new(&params_w),
        &[refs],
        &mut transcript,
    )
    .map_err(|e| anyhow::anyhow!("the read-back VkBlob rejects the proof: {e:?}"))?;
    anyhow::ensure!(
        decide(&inst[..ACCUMULATOR_LIMBS], &params_w),
        "the decider rejects the accumulator"
    );
    let mut tampered = inst[..ACCUMULATOR_LIMBS].to_vec();
    tampered[0] += Fr::ONE;
    anyhow::ensure!(
        !decide(&tampered, &params_w),
        "the decider accepts a tampered accumulator"
    );
    println!(
        "VkBlob reads back, the proof verifies, the decider accepts it and refuses a tampered one"
    );

    let inst_bytes: Vec<u8> = inst.iter().flat_map(|f| f.to_bytes()).collect();
    fs::write(out.join("rotate_vk_blob.bin"), &vk_blob)?;
    fs::write(out.join("rotate_public_inputs.bin"), &inst_bytes)?;
    fs::write(out.join("rotate_proof_blake2b.bin"), &proof)?;
    fs::write(
        out.join("rotate_base_circuit_params.json"),
        serde_json::to_vec_pretty(&base_params)?,
    )?;
    println!(
        "wrote {}: VkBlob {} B (sha256 {}), {} instances, proof {} B",
        out.display(),
        vk_blob.len(),
        sha256_hex(&vk_blob),
        inst.len(),
        proof.len()
    );
    Ok(())
}
