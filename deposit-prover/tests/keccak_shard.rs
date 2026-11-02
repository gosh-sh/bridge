//! The keccak shard proof binds the deposit circuit's keccak table: the
//! shard's commitment equals public input 11 for an honest table and differs
//! from it for a forged one. Witness generation and MockProver, a minute or
//! so; run from `deposit-prover/`:
//!
//! ```text
//! cargo test --release --test keccak_shard
//! ```
//!
//! The ignored test makes a real proof (k=18, a few minutes, ~4 GB) and needs
//! `data/kzg_params_18.srs`.

use axiom_eth::{
    halo2_proofs::dev::MockProver,
    halo2curves::bn256::Fr,
    keccak::types::OutputKeccakShard,
    utils::{
        component::{promise_loader::single::PromiseLoaderParams, ComponentPromiseResultsInMerkle},
        eth_circuit::EthCircuitImpl,
    },
};
use deposit_prover::{
    circuit_v2::DepositEventCircuitV2,
    keccak_shard::{self, PROMISE_COMMIT_INDEX},
    prover::{get_default_params, CircuitConfig, FIXED_KECCAK_CAPACITY},
    synthetic::{build, SyntheticSpec},
    types::DepositProofInput,
};
use ethers_core::types::{Bytes, H256};
use halo2_base::gates::circuit::CircuitBuilderStage;
use zkevm_hashes::keccak::component::circuit::shard::KeccakComponentShardCircuit;

fn load_proof_00() -> DepositProofInput {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/deposit_10proofs/proof_00/input.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Public input 11 of the deposit circuit on `input`, its keccak table built
/// from `responses` (a `None` hash is computed honestly).
fn deposit_commit(input: &DepositProofInput, responses: Vec<(Bytes, Option<H256>)>) -> Fr {
    let circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Mock,
        DepositEventCircuitV2::new(input.clone(), &CircuitConfig::production()),
        get_default_params(),
        PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY),
    );
    let shard = OutputKeccakShard {
        responses,
        capacity: FIXED_KECCAK_CAPACITY,
    };
    circuit
        .fulfill_keccak_promise_results(ComponentPromiseResultsInMerkle::from_single_shard(
            shard.into_logical_results(),
        ))
        .unwrap();
    circuit.instances()[0][PROMISE_COMMIT_INDEX]
}

fn honest(preimages: &[Vec<u8>]) -> Vec<(Bytes, Option<H256>)> {
    preimages
        .iter()
        .map(|p| (Bytes::from(p.clone()), None))
        .collect()
}

#[test]
fn shard_commit_equals_public_input_11() {
    for (name, input) in [
        ("proof_00", load_proof_00()),
        (
            "type1 + access list",
            build(SyntheticSpec::type1_with_access_list()),
        ),
        (
            "SafeL2 execTransaction",
            build(SyntheticSpec::safe_l2_exec()),
        ),
        ("4337 handleOps", build(SyntheticSpec::erc4337_handle_ops())),
    ] {
        let preimages = keccak_shard::keccak_preimages(&input, &CircuitConfig::production());
        assert!(!preimages.is_empty(), "{name}: no keccak calls");
        assert_eq!(
            deposit_commit(&input, honest(&preimages)),
            keccak_shard::shard_commit(&preimages),
            "{name}"
        );
    }
}

#[test]
fn a_forged_hash_moves_public_input_11_away_from_the_shard_commit() {
    let input = load_proof_00();
    let preimages = keccak_shard::keccak_preimages(&input, &CircuitConfig::production());
    let commit = keccak_shard::shard_commit(&preimages);
    for i in 0..preimages.len() {
        let mut responses = honest(&preimages);
        let mut hash = ethers_core::utils::keccak256(&preimages[i]);
        hash[31] ^= 1;
        responses[i].1 = Some(H256(hash));
        assert_ne!(
            deposit_commit(&input, responses),
            commit,
            "a forged hash of preimage {i} must change public input 11"
        );
    }
}

#[test]
fn shard_circuit_is_satisfied_by_the_deposit_preimages() {
    let preimages = keccak_shard::keccak_preimages(&load_proof_00(), &CircuitConfig::production());
    let commit = keccak_shard::shard_commit(&preimages);
    let params = keccak_shard::shard_params();
    let k = params.k() as u32;
    let circuit = KeccakComponentShardCircuit::<Fr>::new(preimages, params, false);
    MockProver::run(k, &circuit, vec![vec![commit]])
        .unwrap()
        .assert_satisfied();
    let circuit =
        KeccakComponentShardCircuit::<Fr>::new(vec![], keccak_shard::shard_params(), false);
    assert!(
        MockProver::run(k, &circuit, vec![vec![commit]])
            .unwrap()
            .verify()
            .is_err(),
        "the commitment of other preimages must not satisfy the circuit"
    );
}

#[test]
#[ignore = "real k=18 keygen and proof; needs data/kzg_params_18.srs"]
fn real_shard_proof_verifies_through_its_vk_blob() {
    let srs = deposit_prover::load_kzg_params_from_trusted_setup(keccak_shard::KECCAK_SHARD_DEGREE)
        .unwrap();
    let preimages = keccak_shard::keccak_preimages(&load_proof_00(), &CircuitConfig::production());
    let commit = keccak_shard::shard_commit(&preimages);
    let key = keccak_shard::keygen(&srs).unwrap();
    let proof = keccak_shard::prove(&srs, &key, preimages, commit).unwrap();

    let blob = keccak_shard::vk_blob(&key).unwrap();
    let blob = deposit_prover::halo2_tvm_bundle::VkBlob::read(blob.to_bytes().unwrap().as_slice())
        .unwrap();
    let vk = keccak_shard::read_vk(&blob).unwrap();
    assert!(keccak_shard::verify(&srs, &vk, commit, &proof));
    assert!(!keccak_shard::verify(&srs, &vk, commit + Fr::one(), &proof));
}
