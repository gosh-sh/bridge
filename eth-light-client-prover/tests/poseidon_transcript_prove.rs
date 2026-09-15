//! M6 transcript bridge — the vendored Poseidon transcript produces a **real**
//! Halo2 SHPLONK proof that verifies through halo2's own `create_proof` /
//! `verify_proof` (not just the byte round-trip unit tests in the module).
//!
//! This is the fast (k=8, ~1 s) proof that
//! `poseidon_transcript::{PoseidonWrite, PoseidonRead}` is wired correctly into
//! the halo2 prover/verifier — the exact transcript flavour a shard proof will
//! use so `bridge-snark-utils::export_poseidon_snark` can wrap it into a
//! `snark_verifier_sdk::Snark` for `crates/bridge-evm-aggregator`. The heavy
//! real-shard emit (Hermez k=20 + file export) lives in
//! `examples/export_shard_snark.rs`.
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo test --test poseidon_transcript_prove
//! ```

use eth_light_client_prover::poseidon_transcript::{PoseidonChallenge, PoseidonRead, PoseidonWrite};

use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::gates::{GateChip, GateInstructions};
use halo2_base::halo2_proofs::halo2curves::bn256::{Bn256, Fr};
use halo2_base::halo2_proofs::plonk::{create_proof, keygen_pk, keygen_vk, verify_proof};
use halo2_base::halo2_proofs::poly::commitment::ParamsProver;
use halo2_base::halo2_proofs::poly::kzg::commitment::{KZGCommitmentScheme, ParamsKZG};
use halo2_base::halo2_proofs::poly::kzg::multiopen::{ProverSHPLONK, VerifierSHPLONK};
use halo2_base::halo2_proofs::poly::kzg::strategy::SingleStrategy;
use rand::rngs::OsRng;

const K: u32 = 8;

/// Build a trivial `a * b == c` circuit exposing `c` as the single public
/// instance. `stage`/`config`/`break_points` mirror the keygen→prover handoff
/// used by `examples/export_step_vk_blob.rs`.
fn build(
    prover: Option<(halo2_base::gates::circuit::BaseCircuitParams, Vec<Vec<usize>>)>,
) -> (BaseCircuitBuilder<Fr>, Vec<Fr>) {
    let mut b = match &prover {
        Some((config, bp)) => {
            BaseCircuitBuilder::<Fr>::prover(config.clone(), bp.clone()).use_instance_columns(1)
        }
        None => BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
            .use_k(K as usize)
            .use_instance_columns(1),
    };
    let gate = GateChip::<Fr>::default();
    let ctx = b.main(0);
    let a_w = ctx.load_witness(Fr::from(7u64));
    let b_w = ctx.load_witness(Fr::from(11u64));
    let c_w = gate.mul(ctx, a_w, b_w);
    let c_val = *c_w.value();
    b.assigned_instances[0] = vec![c_w];
    if prover.is_none() {
        // no lookups in a pure gate circuit
        b.config_params.lookup_bits = None;
    }
    (b, vec![c_val])
}

#[test]
fn poseidon_transcript_real_proof_verifies() {
    let params = ParamsKZG::<Bn256>::setup(K, OsRng);

    // ---- keygen ----
    let (mut kb, public) = build(None);
    let config = kb.calculate_params(Some(9));
    assert_eq!(public, vec![Fr::from(77u64)], "7*11 == 77");
    let vk = keygen_vk(&params, &kb).expect("keygen_vk");
    let pk = keygen_pk(&params, vk, &kb).expect("keygen_pk");
    let break_points = kb.break_points();
    drop(kb);

    // ---- prove with the vendored Poseidon transcript ----
    let (pb, inst) = build(Some((config, break_points)));
    let instances: &[&[Fr]] = &[&inst];
    let mut transcript = PoseidonWrite::init(Vec::<u8>::new());
    create_proof::<KZGCommitmentScheme<Bn256>, ProverSHPLONK<'_, Bn256>, PoseidonChallenge, _, _, _>(
        &params,
        &pk,
        &[pb],
        &[instances],
        OsRng,
        &mut transcript,
    )
    .expect("create_proof (Poseidon transcript)");
    let proof = transcript.finalize();
    assert!(!proof.is_empty(), "proof must be non-empty");

    // ---- verify with the vendored Poseidon transcript ----
    let verify = |proof_bytes: &[u8]| -> bool {
        let strategy = SingleStrategy::new(&params);
        let mut t = PoseidonRead::init(proof_bytes);
        verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            PoseidonChallenge,
            PoseidonRead<&[u8]>,
            SingleStrategy<'_, Bn256>,
        >(params.verifier_params(), pk.get_vk(), strategy, &[instances], &mut t)
        .is_ok()
    };

    assert!(verify(&proof), "valid Poseidon proof must verify");

    // Tamper: flip one proof byte → must be rejected.
    let mut bad = proof.clone();
    let i = bad.len() / 2;
    bad[i] ^= 0x01;
    assert!(!verify(&bad), "tampered Poseidon proof must be rejected");
}
