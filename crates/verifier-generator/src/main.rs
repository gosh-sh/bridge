//! Generate Solidity verifier from a simple Halo2 circuit
//!
//! This binary generates a real Solidity verifier contract using snark-verifier-sdk.
//! The circuit is a simplified withdrawal proof that demonstrates the core concepts.

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
    evm::gen_evm_verifier_shplonk,
    halo2::gen_snark_shplonk,
    CircuitExt,
};
use std::path::Path;

/// Simple withdrawal circuit configuration
#[derive(Clone, Debug)]
struct SimpleCircuitConfig {
    advice: [Column<Advice>; 2],
    instance: Column<Instance>,
    selector: Selector,
}

/// Simple withdrawal circuit
///
/// This is a minimal circuit that just exposes 4 public inputs.
/// In a real implementation, this would verify:
/// - Private inputs: withdrawal_hash, nullifier_preimage
/// - Public outputs: nullifier (computed from private inputs)
/// - Public inputs: recipient, amount, root
///
/// For now, we just expose 4 values as public inputs for testing.
#[derive(Clone, Debug, Default)]
struct SimpleCircuit {
    pub_input_0: Value<Fr>,  // nullifier
    pub_input_1: Value<Fr>,  // recipient
    pub_input_2: Value<Fr>,  // amount
    pub_input_3: Value<Fr>,  // root
}

impl CircuitExt<Fr> for SimpleCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![4] // [nullifier, recipient, amount, root]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        // For instances, we need actual values, not Value<Fr>
        // This is only called when we have known values (during proving)
        let mut val0 = Fr::zero();
        let mut val1 = Fr::zero();
        let mut val2 = Fr::zero();
        let mut val3 = Fr::zero();

        // Extract values if known
        self.pub_input_0.map(|v| { val0 = v; });
        self.pub_input_1.map(|v| { val1 = v; });
        self.pub_input_2.map(|v| { val2 = v; });
        self.pub_input_3.map(|v| { val3 = v; });

        vec![vec![val0, val1, val2, val3]]
    }
}

impl Circuit<Fr> for SimpleCircuit {
    type Config = SimpleCircuitConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = ();

    fn without_witnesses(&self) -> Self {
        Self::default()
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        let advice = [meta.advice_column(), meta.advice_column()];
        let instance = meta.instance_column();
        let selector = meta.selector();

        // Enable equality constraints
        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

        // No gates needed - we're just exposing values

        SimpleCircuitConfig {
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
        let (cell0, cell1, cell2, cell3) = layouter.assign_region(
            || "simple circuit",
            |mut region| {
                // Assign public values to advice columns
                let cell0 = region.assign_advice(config.advice[0], 0, self.pub_input_0);
                let cell1 = region.assign_advice(config.advice[1], 0, self.pub_input_1);
                let cell2 = region.assign_advice(config.advice[0], 1, self.pub_input_2);
                let cell3 = region.assign_advice(config.advice[1], 1, self.pub_input_3);

                Ok((cell0, cell1, cell2, cell3))
            },
        )?;

        // Constrain advice cells to instance column
        layouter.constrain_instance(cell0.cell(), config.instance, 0);
        layouter.constrain_instance(cell1.cell(), config.instance, 1);
        layouter.constrain_instance(cell2.cell(), config.instance, 2);
        layouter.constrain_instance(cell3.cell(), config.instance, 3);

        Ok(())
    }
}

fn main() {
    println!("Generating Solidity verifier for simple withdrawal circuit...");
    println!();

    // Circuit size parameter (k=10 means 2^10 = 1024 rows)
    let k = 10;

    // Setup parameters
    println!("Setting up KZG parameters (k={})...", k);
    let params = gen_srs(k);

    // Create circuit for keygen
    let circuit = SimpleCircuit::default();

    // Generate proving key
    println!("Generating proving key...");
    let pk = gen_pk(&params, &circuit, None);

    // Test the circuit with a sample proof
    println!("Testing circuit with sample values...");
    let test_circuit = SimpleCircuit {
        pub_input_0: Value::known(Fr::from(12345)),  // nullifier
        pub_input_1: Value::known(Fr::from(0xabcd)),  // recipient
        pub_input_2: Value::known(Fr::from(1000)),    // amount
        pub_input_3: Value::known(Fr::from(0x1234)),  // root
    };

    // Generate a SNARK proof
    println!("Creating test proof...");
    let snark = gen_snark_shplonk(&params, &pk, test_circuit.clone(), None::<&str>);
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
        gen_evm_verifier_shplonk::<SimpleCircuit>(
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
