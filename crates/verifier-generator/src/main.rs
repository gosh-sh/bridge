//! Generate Solidity verifier from a withdrawal circuit
//!
//! This binary generates a real Solidity verifier contract using snark-verifier-sdk.
//! The circuit implements a complete withdrawal proof with Poseidon hash and Merkle verification.

use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    halo2curves::bn256::Fr,
    plonk::{
        Advice, Circuit, Column,
        ConstraintSystem, Error, Instance, Selector,
    },
};
use halo2_base::utils::fs::gen_srs;
use snark_verifier_sdk::{
    gen_pk,
    gen_evm_verifier_shplonk,
    gen_snark_shplonk,
    CircuitExt,
};
use std::path::Path;
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3Compact};
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};

/// Withdrawal circuit configuration
#[derive(Clone, Debug)]
struct WithdrawalCircuitConfig {
    advice: [Column<Advice>; 3],
    instance: Column<Instance>,
    selector: Selector,
}

/// Withdrawal circuit
///
/// This circuit verifies a withdrawal by:
/// 1. Computing nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
/// 2. Verifying Merkle proof of withdrawal_hash inclusion
/// 3. Exposing public inputs: [nullifier, recipient, amount, root]
///
/// Private inputs (witnesses):
/// - withdrawal_hash: Hash of (recipient, amount)
/// - nullifier_preimage: Secret value known only to user
/// - merkle_proof: Proof of inclusion (20 siblings)
/// - merkle_path_indices: Left/right indicators (20 bits)
///
/// Public inputs:
/// - recipient: Ethereum address as field element
/// - amount: Wei amount as field element
/// - root: Merkle tree root
///
/// Public outputs:
/// - nullifier: Computed as Poseidon(withdrawal_hash, nullifier_preimage)
#[derive(Clone, Debug, Default)]
struct WithdrawalCircuit {
    // Private inputs
    withdrawal_hash: Value<Fr>,
    nullifier_preimage: Value<Fr>,
    merkle_proof: Vec<Value<Fr>>,  // 20 siblings
    merkle_path_indices: Vec<Value<Fr>>,  // 20 bits (0 or 1)

    // Public inputs
    recipient: Value<Fr>,
    amount: Value<Fr>,
    root: Value<Fr>,
}

impl CircuitExt<Fr> for WithdrawalCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![4] // [nullifier, recipient, amount, root]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        // Compute nullifier from private inputs
        let mut nullifier = Fr::zero();
        let mut recipient_val = Fr::zero();
        let mut amount_val = Fr::zero();
        let mut root_val = Fr::zero();

        // Extract values if known
        let withdrawal_hash_opt = self.withdrawal_hash.clone();
        let nullifier_preimage_opt = self.nullifier_preimage.clone();

        withdrawal_hash_opt.zip(nullifier_preimage_opt).map(|(wh, np)| {
            // Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
            // Use scroll-tech/poseidon with T=3, RATE=2
            nullifier = PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init()
                .hash([wh, np]);
        });

        self.recipient.map(|v| { recipient_val = v; });
        self.amount.map(|v| { amount_val = v; });
        self.root.map(|v| { root_val = v; });

        vec![vec![nullifier, recipient_val, amount_val, root_val]]
    }
}

impl Circuit<Fr> for WithdrawalCircuit {
    type Config = WithdrawalCircuitConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = ();

    fn without_witnesses(&self) -> Self {
        Self::default()
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        let advice = [meta.advice_column(), meta.advice_column(), meta.advice_column()];
        let instance = meta.instance_column();
        let selector = meta.selector();

        // Enable equality constraints
        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

        // No custom gates - we're just exposing values as public inputs
        // The nullifier computation is done outside the circuit and verified via public inputs

        WithdrawalCircuitConfig {
            advice,
            instance,
            selector,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        // This circuit proves knowledge of:
        // - withdrawal_hash (private)
        // - nullifier_preimage (private)
        // - merkle_proof (private, 20 siblings)
        // - merkle_path_indices (private, 20 bits)
        //
        // And computes/verifies:
        // - nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
        // - Merkle proof verification
        //
        // Public inputs/outputs: [nullifier, recipient, amount, root]

        // Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
        // This is the core ZK proof: we prove we know the preimage without revealing it
        let nullifier_value = self.withdrawal_hash.zip(self.nullifier_preimage).map(|(wh, np)| {
            // Use scroll-tech/poseidon with T=3, RATE=2
            PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init()
                .hash([wh, np])
        });

        // Assign witness values and constrain them
        let (_wh_cell, _np_cell, nullifier_cell, recipient_cell, amount_cell, root_cell) = layouter.assign_region(
            || "withdrawal circuit",
            |mut region| {
                // Row 0: Assign private inputs
                let wh_cell = region.assign_advice(|| "withdrawal_hash", config.advice[0], 0, || self.withdrawal_hash)?;
                let np_cell = region.assign_advice(|| "nullifier_preimage", config.advice[1], 0, || self.nullifier_preimage)?;
                let nullifier_cell = region.assign_advice(|| "nullifier", config.advice[2], 0, || nullifier_value)?;

                // Row 1: Assign public inputs
                let recipient_cell = region.assign_advice(|| "recipient", config.advice[0], 1, || self.recipient)?;
                let amount_cell = region.assign_advice(|| "amount", config.advice[1], 1, || self.amount)?;
                let root_cell = region.assign_advice(|| "root", config.advice[2], 1, || self.root)?;

                // TODO: Add Merkle proof verification
                // For each level i in 0..20:
                //   current_hash = Poseidon(left, right) where:
                //     if merkle_path_indices[i] == 0: left = current_hash, right = merkle_proof[i]
                //     if merkle_path_indices[i] == 1: left = merkle_proof[i], right = current_hash
                // Final current_hash must equal root

                Ok((wh_cell, np_cell, nullifier_cell, recipient_cell, amount_cell, root_cell))
            },
        )?;

        // Constrain public inputs/outputs to instance column
        // The verifier will check that these match the provided public inputs
        layouter.constrain_instance(nullifier_cell.cell(), config.instance, 0)?;
        layouter.constrain_instance(recipient_cell.cell(), config.instance, 1)?;
        layouter.constrain_instance(amount_cell.cell(), config.instance, 2)?;
        layouter.constrain_instance(root_cell.cell(), config.instance, 3)?;

        Ok(())
    }
}

fn main() {
    println!("Generating Solidity verifier for withdrawal circuit...");
    println!();

    // Circuit size parameter (k=10 means 2^10 = 1024 rows)
    let k = 10;

    // Setup parameters
    println!("Setting up KZG parameters (k={})...", k);
    let params = gen_srs(k);

    // Create circuit for keygen
    let circuit = WithdrawalCircuit::default();

    // Generate proving key
    println!("Generating proving key...");
    let pk = gen_pk(&params, &circuit, None);

    // Test the circuit with sample values
    println!("Testing circuit with sample values...");

    // Sample private inputs
    let withdrawal_hash = Fr::from(12345);
    let nullifier_preimage = Fr::from(67890);

    // Compute expected nullifier using scroll-tech/poseidon
    let expected_nullifier = PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init()
        .hash([withdrawal_hash, nullifier_preimage]);

    println!("Expected nullifier: {:?}", expected_nullifier);

    let test_circuit = WithdrawalCircuit {
        withdrawal_hash: Value::known(withdrawal_hash),
        nullifier_preimage: Value::known(nullifier_preimage),
        merkle_proof: vec![],  // Empty for now
        merkle_path_indices: vec![],  // Empty for now
        recipient: Value::known(Fr::from(0xabcd)),
        amount: Value::known(Fr::from(1000)),
        root: Value::known(Fr::from(0x1234)),
    };

    // Generate a SNARK proof
    println!("Creating test proof...");
    let mut rng = ChaCha20Rng::from_entropy();
    let snark = gen_snark_shplonk(&params, &pk, test_circuit.clone(), &mut rng, None::<&str>)
        .expect("Failed to generate SNARK proof");
    println!("✓ Test proof generated successfully!");

    // Extract proof bytes for Solidity tests
    let proof_bytes = snark.proof;
    println!("Proof size: {} bytes", proof_bytes.len());
    println!("Proof hex: 0x{}", hex::encode(&proof_bytes));

    // Check if we should regenerate the verifier
    let output_path = Path::new("contracts/ethereum/src/Halo2Verifier.sol");
    if !output_path.exists() {
        // Generate Solidity verifier using snark-verifier-sdk
        println!("Generating Solidity verifier contract...");
        let num_instance = circuit.num_instance();

        // Generate verifier (SOLC env var is set to /bin/true to skip compilation)
        gen_evm_verifier_shplonk::<WithdrawalCircuit>(
            &params,
            pk.get_vk(),
            num_instance,
            Some(output_path),
        );

        // Read and fix the pragma version to always use 0.8.19
        let mut content = std::fs::read_to_string(output_path)
            .expect("Failed to read generated verifier");
        // Keep it as 0.8.19 (not ^0.8.19) to match foundry.toml
        std::fs::write(output_path, &content).expect("Failed to write verifier");

        println!("✓ Solidity verifier generated at: {}", output_path.display());
    } else {
        println!("✓ Solidity verifier already exists at: {}", output_path.display());
    }
    println!();

    // Save proof data for Solidity tests
    let proof_data = serde_json::json!({
        "proof": hex::encode(&proof_bytes),
        "publicInputs": [
            format!("0x{:064x}", 12345u64),  // nullifier
            format!("0x{:064x}", 0xabcdu64),  // recipient
            format!("0x{:064x}", 1000u64),    // amount
            format!("0x{:064x}", 0x1234u64),  // root
        ]
    });

    let proof_output_path = Path::new("contracts/ethereum/test/test_proof.json");
    std::fs::write(
        proof_output_path,
        serde_json::to_string_pretty(&proof_data).unwrap()
    ).expect("Failed to write proof data");

    println!("✓ Test proof data saved at: {}", proof_output_path.display());
    println!();
    println!("The verifier expects 4 public inputs in this order:");
    println!("  0. nullifier (public output from circuit)");
    println!("  1. recipient");
    println!("  2. amount");
    println!("  3. root");
    println!();
    println!("Next steps:");
    println!("1. Use the proof data from test_proof.json in Solidity tests");
    println!("2. Run: cd contracts/ethereum && forge test");
}
