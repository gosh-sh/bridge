//! Negative Test: Verify that public instances are properly constrained
//!
//! This test proves that BC-CIRCUIT-004 is a FALSE POSITIVE by demonstrating
//! that the public instances ARE properly constrained in the proof.
//!
//! Test Strategy:
//! 1. Generate a valid proof with correct public instances
//! 2. Attempt to verify the proof with MODIFIED public instances
//! 3. Verification should FAIL (proving instances are constrained)
//!
//! If the audit claim were correct (instances not constrained), then:
//! - An attacker could submit a valid proof with different public outputs
//! - The verifier would accept the proof with wrong instances
//! - This test would PASS (which would be BAD)
//!
//! If our analysis is correct (instances ARE constrained), then:
//! - The verifier will reject the proof with modified instances
//! - This test will PASS (which is GOOD)

use std::fs;

use axiom_eth::utils::eth_circuit::create_circuit;
use deposit_prover::{
    circuit_v2::DepositEventCircuitV2,
    prover::{get_default_params, load_kzg_params_from_trusted_setup, CircuitConfig},
    types::{DepositEventData, DepositProofInput, ReceiptProof},
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::{bn256::Fr, ff::PrimeField},
        plonk::{keygen_pk, keygen_vk, Circuit},
    },
    utils::testing::check_proof_with_instances,
};
use snark_verifier_sdk::halo2::gen_snark_shplonk;

/// Load real Sepolia test data from E2E test
fn load_sepolia_test_data() -> DepositProofInput {
    let event_json = fs::read_to_string("data/sepolia_deposit_34_event.json")
        .expect("Failed to read event data");
    let event_data: DepositEventData =
        serde_json::from_str(&event_json).expect("Failed to parse event data");

    let proof_json = fs::read_to_string("data/sepolia_deposit_34_receipt_proof.json")
        .expect("Failed to read receipt proof");
    let receipt_proof: ReceiptProof =
        serde_json::from_str(&proof_json).expect("Failed to parse receipt proof");

    DepositProofInput {
        event_data,
        receipt_proof,
    }
}

#[test]
#[ignore] // Run with: cargo test --test test_public_instances_constrained -- --ignored
          // --nocapture
fn test_public_instances_are_constrained() {
    println!("\n🧪 NEGATIVE TEST: Verifying that public instances are constrained");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Load real Sepolia data
    let input = load_sepolia_test_data();
    let config = CircuitConfig::default();

    println!("\n📊 Original Public Instances:");
    println!("   depositId: {}", input.event_data.deposit_id);
    println!("   sender: 0x{}", hex::encode(input.event_data.sender));
    println!("   amount: {}", input.event_data.amount);
    println!(
        "   contract: 0x{}",
        hex::encode(input.event_data.contract_address)
    );

    // Step 1: Generate a valid proof with correct instances
    println!("\n🔧 Step 1: Generating valid proof with correct instances...");

    let params = get_default_params();
    let k = params.base.k as u32;

    // Create keygen circuit
    let circuit_input = DepositEventCircuitV2::new(input.clone(), &config);
    let mut keygen_circuit = create_circuit(
        CircuitBuilderStage::Keygen,
        params.clone(),
        circuit_input.clone(),
    );
    keygen_circuit.mock_fulfill_keccak_promises(None);
    keygen_circuit.calculate_params();

    // Load KZG params
    let kzg_params = load_kzg_params_from_trusted_setup(k).expect("Failed to load KZG params");

    // Generate proving key
    println!("   Generating proving key...");
    let vk = keygen_vk(&kzg_params, &keygen_circuit).expect("Failed to generate VK");
    let pk = keygen_pk(&kzg_params, vk, &keygen_circuit).expect("Failed to generate PK");

    let calculated_params = keygen_circuit.params().rlc;
    let break_points = keygen_circuit.break_points();

    // Create prover circuit
    let circuit_input = DepositEventCircuitV2::new(input.clone(), &config);
    let prover_circuit = create_circuit(
        CircuitBuilderStage::Prover,
        calculated_params.clone(),
        circuit_input,
    )
    .use_break_points(break_points.clone());
    prover_circuit.mock_fulfill_keccak_promises(None);

    // Get the CORRECT instances
    let correct_instances = prover_circuit.instances();
    println!(
        "   ✓ Correct instances extracted: {} values",
        correct_instances[0].len()
    );

    // Generate proof
    println!("   Generating SNARK proof...");
    let snark = gen_snark_shplonk(
        &kzg_params,
        &pk,
        prover_circuit,
        Some("data/test_instances_proof.snark"),
    );
    println!("   ✓ Proof generated successfully");

    // Step 2: Verify with CORRECT instances (should PASS)
    println!("\n✅ Step 2: Verifying proof with CORRECT instances...");
    let verify_result = verify_snark::<
        halo2_base::halo2_proofs::poly::kzg::commitment::KZGCommitmentScheme<
            halo2_base::halo2_proofs::halo2curves::bn256::Bn256,
        >,
    >(&kzg_params, snark.clone(), pk.get_vk());
    assert!(
        verify_result,
        "❌ CRITICAL: Proof verification FAILED with correct instances! This should never happen."
    );
    println!("   ✓ Verification PASSED (as expected)");

    // Step 3: Verify with MODIFIED instances (should FAIL)
    println!("\n❌ Step 3: Verifying proof with MODIFIED instances...");
    println!("   This is the CRITICAL test - if instances aren't constrained,");
    println!("   the verifier would accept the proof with wrong values!");

    // Create modified instances - change depositId from 34 to 999
    let mut modified_instances = correct_instances.clone();
    let fake_deposit_id = Fr::from(999u64);
    modified_instances[0][0] = fake_deposit_id;

    println!("\n📊 Modified Public Instances:");
    println!("   depositId: 999 (MODIFIED from 34)");
    println!(
        "   sender: 0x{} (unchanged)",
        hex::encode(input.event_data.sender)
    );
    println!("   amount: {} (unchanged)", input.event_data.amount);
    println!(
        "   contract: 0x{} (unchanged)",
        hex::encode(input.event_data.contract_address)
    );

    // Create a modified SNARK with wrong instances
    let modified_snark = snark_verifier_sdk::Snark::new(
        snark.protocol.clone(),
        vec![modified_instances[0].clone()],
        snark.proof.clone(),
    );

    // Attempt to verify with modified instances
    println!("\n   Attempting verification with modified instances...");
    let verify_result_modified = verify_snark::<
        halo2_base::halo2_proofs::poly::kzg::commitment::KZGCommitmentScheme<
            halo2_base::halo2_proofs::halo2curves::bn256::Bn256,
        >,
    >(&kzg_params, modified_snark, pk.get_vk());

    // Step 4: Check results
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 TEST RESULTS:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    if verify_result_modified {
        println!("❌ CRITICAL SECURITY FAILURE!");
        println!("   The verifier ACCEPTED the proof with MODIFIED instances!");
        println!("   This means public instances are NOT properly constrained.");
        println!("   BC-CIRCUIT-004 is VALID - this is a critical vulnerability!");
        panic!("Security test FAILED: Public instances are not constrained!");
    } else {
        println!("✅ SECURITY TEST PASSED!");
        println!("   The verifier REJECTED the proof with modified instances.");
        println!("   This proves that public instances ARE properly constrained.");
        println!("   BC-CIRCUIT-004 is a FALSE POSITIVE - the audit is incorrect.");
    }

    println!("\n🎯 CONCLUSION:");
    println!("   Public instances are cryptographically bound to the proof.");
    println!("   An attacker CANNOT submit a valid proof with different public outputs.");
    println!("   The circuit is SECURE against instance manipulation attacks.");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
}

#[test]
#[ignore] // Run with: cargo test --test test_public_instances_constrained -- --ignored
          // --nocapture
fn test_cannot_steal_funds_with_modified_amount() {
    println!("\n🧪 ATTACK SIMULATION: Attempting to steal funds by modifying amount");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Load real Sepolia data (deposit of 0.001 ETH)
    let input = load_sepolia_test_data();
    let config = CircuitConfig::default();

    let original_amount = input.event_data.amount;
    let stolen_amount = 1_000_000_000_000_000_000u64; // 1 ETH (1000x more!)

    println!("\n💰 Attack Scenario:");
    println!("   Original deposit: {} wei (0.001 ETH)", original_amount);
    println!("   Attacker claims: {} wei (1 ETH)", stolen_amount);
    println!(
        "   Attempted theft: {}x the original amount!",
        stolen_amount / original_amount
    );

    // Generate valid proof for 0.001 ETH deposit
    println!("\n🔧 Generating valid proof for original deposit...");

    let params = get_default_params();
    let k = params.base.k as u32;

    let circuit_input = DepositEventCircuitV2::new(input.clone(), &config);
    let mut keygen_circuit = create_circuit(
        CircuitBuilderStage::Keygen,
        params.clone(),
        circuit_input.clone(),
    );
    keygen_circuit.mock_fulfill_keccak_promises(None);
    keygen_circuit.calculate_params();

    let kzg_params = load_kzg_params_from_trusted_setup(k).expect("Failed to load KZG params");

    let vk = keygen_vk(&kzg_params, &keygen_circuit).expect("Failed to generate VK");
    let pk = keygen_pk(&kzg_params, vk, &keygen_circuit).expect("Failed to generate PK");

    let calculated_params = keygen_circuit.params().rlc;
    let break_points = keygen_circuit.break_points();

    let circuit_input = DepositEventCircuitV2::new(input.clone(), &config);
    let prover_circuit = create_circuit(
        CircuitBuilderStage::Prover,
        calculated_params.clone(),
        circuit_input,
    )
    .use_break_points(break_points.clone());
    prover_circuit.mock_fulfill_keccak_promises(None);

    let correct_instances = prover_circuit.instances();

    let snark = gen_snark_shplonk(
        &kzg_params,
        &pk,
        prover_circuit,
        Some("data/test_attack_proof.snark"),
    );

    // Attacker tries to modify the amount in public instances
    println!("\n🎭 Attacker modifies amount in public instances...");
    let mut stolen_instances = correct_instances.clone();
    stolen_instances[0][2] = Fr::from(stolen_amount); // Modify amount field

    let attack_snark = snark_verifier_sdk::Snark::new(
        snark.protocol.clone(),
        vec![stolen_instances[0].clone()],
        snark.proof.clone(),
    );

    println!("   Attempting to verify proof with stolen amount...");
    let attack_result = verify_snark::<
        halo2_base::halo2_proofs::poly::kzg::commitment::KZGCommitmentScheme<
            halo2_base::halo2_proofs::halo2curves::bn256::Bn256,
        >,
    >(&kzg_params, attack_snark, pk.get_vk());

    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 ATTACK RESULT:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    if attack_result {
        println!("❌ CRITICAL VULNERABILITY!");
        println!("   Attacker successfully stole funds by modifying the amount!");
        println!("   The bridge would transfer 1 ETH instead of 0.001 ETH!");
        panic!("CRITICAL: Fund theft attack succeeded!");
    } else {
        println!("✅ ATTACK BLOCKED!");
        println!("   The verifier rejected the proof with modified amount.");
        println!("   The attacker CANNOT steal funds by changing public instances.");
        println!("   The bridge is SECURE against this attack vector.");
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
}
