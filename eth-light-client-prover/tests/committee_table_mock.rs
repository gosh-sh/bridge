//! MockProver checks of the rotate's committee circuit on the table SHA-256.
#![cfg(feature = "aggregation")]

use eth_light_client_prover::committee_table::{
    native_committee_commitment, native_instances, CommitteeCircuit, CommitteeWitness,
    COMMITTEE_TABLE_K, NUM_MESSAGES,
};
use eth_light_client_prover::committee::SYNC_COMMITTEE_SIZE;
use eth_light_client_prover::mainnet::rotate_update_from_env;
use halo2_base::halo2_proofs::dev::MockProver;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::Field;

const GLOAS_FORK_SLOT: u64 = 11_296_768;

fn run(circuit: &CommitteeCircuit, instances: Vec<Fr>) -> Result<(), String> {
    MockProver::run(COMMITTEE_TABLE_K as u32, circuit, vec![instances])
        .map_err(|e| format!("{e:?}"))?
        .verify()
        .map_err(|e| format!("{} failures, first {:?}", e.len(), e.first()))
}

fn accepts(w: CommitteeWitness) {
    let inst = native_instances(&w);
    let circuit = CommitteeCircuit::mock(w);
    assert_eq!(circuit.instances(), inst.as_slice());
    run(&circuit, inst).expect("an honest committee is accepted");
}

#[test]
fn an_electra_hand_off_is_accepted() {
    accepts(CommitteeWitness::synthetic(1, GLOAS_FORK_SLOT - 1, GLOAS_FORK_SLOT));
}

#[test]
fn a_gloas_hand_off_is_accepted() {
    accepts(CommitteeWitness::synthetic(2, GLOAS_FORK_SLOT, GLOAS_FORK_SLOT));
}

#[test]
fn every_public_instance_is_bound() {
    let w = CommitteeWitness::synthetic(3, GLOAS_FORK_SLOT + 5, GLOAS_FORK_SLOT);
    let inst = native_instances(&w);
    let circuit = CommitteeCircuit::mock(w);
    for i in 0..inst.len() {
        let mut wrong = inst.clone();
        wrong[i] += Fr::ONE;
        assert!(run(&circuit, wrong).is_err(), "instance {i} is not bound");
    }
}

#[test]
fn the_table_hashing_other_messages_is_refused() {
    let w = CommitteeWitness::synthetic(4, GLOAS_FORK_SLOT + 5, GLOAS_FORK_SLOT);
    let inst = native_instances(&w);
    // A pubkey leaf, an inner node of the committee tree, the container node
    // and the last branch level.
    for m in [7, SYNC_COMMITTEE_SIZE + 3, 2 * SYNC_COMMITTEE_SIZE, NUM_MESSAGES - 1] {
        let circuit = CommitteeCircuit::mock(w.clone()).with_sha_inputs(|msgs| msgs[m][5] ^= 1);
        assert!(run(&circuit, inst.clone()).is_err(), "message {m} is not bound");
    }
}

#[test]
fn the_commitment_is_the_one_step_recomputes() {
    // Same 2-level Poseidon as `rotate::commit_sync_committee_2level`.
    let w = CommitteeWitness::synthetic(5, 1, GLOAS_FORK_SLOT);
    let shuffled = {
        let mut p = w.pubkeys.clone();
        p.swap(0, 1);
        p
    };
    assert_ne!(
        native_committee_commitment(&w.pubkeys, &w.aggregate),
        native_committee_commitment(&shuffled, &w.aggregate),
        "the commitment follows the committee order"
    );
}

/// The live hand-off `ROTATE_UPDATE_PATH` names, with `BEACON_GLOAS_FORK_SLOT`.
/// Prints the instances to compare with a rotate proved the old way.
#[test]
#[ignore]
fn a_live_hand_off_is_accepted() {
    let (path, update) = rotate_update_from_env().unwrap().expect("ROTATE_UPDATE_PATH is set");
    let gloas: u64 = std::env::var("BEACON_GLOAS_FORK_SLOT").unwrap().parse().unwrap();
    let w = CommitteeWitness::from_update(&update, gloas).unwrap();
    let inst = native_instances(&w);
    println!("{}: next_commit 0x{}", path.display(), hex::encode(inst[0].to_bytes().iter().rev().copied().collect::<Vec<_>>()));
    accepts(w);
}
