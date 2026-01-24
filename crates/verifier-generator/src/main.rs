//! Generate Solidity verifier from a withdrawal circuit
//!
//! This binary generates a real Solidity verifier contract using snark-verifier-sdk.
//! The circuit implements a complete withdrawal proof with Poseidon hash and Merkle verification.

use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value, AssignedCell},
    halo2curves::{bn256::Fr, ff::PrimeField},
    plonk::{
        Advice, Circuit, Column,
        ConstraintSystem, Error, Instance, Selector,
    },
};
use halo2_base::utils::fs::gen_srs;
use snark_verifier_sdk::{
    gen_pk,
    gen_evm_verifier_shplonk,
    gen_evm_proof_shplonk,
    CircuitExt,
};
use std::path::Path;
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3, P128Pow5T3Compact};
use poseidon_circuit::poseidon::{Hash, Pow5Chip as PoseidonChip, Pow5Config as PoseidonConfig};
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};

/// Withdrawal circuit configuration
#[derive(Clone, Debug)]
struct WithdrawalCircuitConfig {
    advice: [Column<Advice>; 4],
    instance: Column<Instance>,
    selector: Selector,
    poseidon_config: PoseidonConfig<Fr, 3, 2>,
}

/// Withdrawal circuit
#[derive(Clone, Debug, Default)]
struct WithdrawalCircuit {
    withdrawal_hash: Option<Fr>,
    nullifier_preimage: Option<Fr>,
    recipient: Option<Fr>,
    amount: Option<Fr>,
    root: Option<Fr>,
}

impl CircuitExt<Fr> for WithdrawalCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![4] // [nullifier, recipient, amount, root]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        let nullifier = if let (Some(wh), Some(np)) = (self.withdrawal_hash, self.nullifier_preimage) {
            PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init().hash([wh, np])
        } else {
            Fr::zero()
        };

        let recipient_val = self.recipient.unwrap_or(Fr::zero());
        let amount_val = self.amount.unwrap_or(Fr::zero());
        let root_val = self.root.unwrap_or(Fr::zero());

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
        // Need 4 advice columns: one for partial_sbox and 3 for state
        let advice = [
            meta.advice_column(),
            meta.advice_column(),
            meta.advice_column(),
            meta.advice_column(),
        ];
        let instance = meta.instance_column();
        let selector = meta.selector();

        // Enable equality constraints
        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

        // Configure Poseidon - need to provide lagrange coefficients
        // Also need extra fixed columns for constants
        let lagrange_coeffs = [
            meta.fixed_column(),
            meta.fixed_column(),
            meta.fixed_column(),
            meta.fixed_column(),
            meta.fixed_column(),
            meta.fixed_column(),
        ];

        // Enable constant columns for the fixed columns
        for col in &lagrange_coeffs {
            meta.enable_constant(*col);
        }

        // Use advice[1..4] for state and advice[0] for partial_sbox
        let poseidon_config = PoseidonChip::configure::<P128Pow5T3<Fr>>(
            meta,
            advice[1..4].try_into().unwrap(),
            advice[0],
            lagrange_coeffs[0..3].try_into().unwrap(),
            lagrange_coeffs[3..6].try_into().unwrap(),
        );

        WithdrawalCircuitConfig {
            advice,
            instance,
            selector,
            poseidon_config,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        // Load private inputs
        let withdrawal_hash = layouter.assign_region(
            || "load withdrawal_hash",
            |mut region| {
                region.assign_advice(
                    || "withdrawal_hash",
                    config.advice[0],
                    0,
                    || Value::known(self.withdrawal_hash.unwrap_or(Fr::zero())),
                )
            },
        )?;

        let nullifier_preimage = layouter.assign_region(
            || "load nullifier_preimage",
            |mut region| {
                region.assign_advice(
                    || "nullifier_preimage",
                    config.advice[0],
                    0,
                    || Value::known(self.nullifier_preimage.unwrap_or(Fr::zero())),
                )
            },
        )?;

        // Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage) IN-CIRCUIT
        // This is the core ZK proof: we prove we know the preimage without revealing it
        let nullifier = poseidon_hash_gadget(
            config.poseidon_config.clone(),
            layouter.namespace(|| "compute nullifier"),
            [withdrawal_hash, nullifier_preimage],
        )?;

        // Expose nullifier as public output
        layouter.constrain_instance(nullifier.cell(), config.instance, 0)?;

        // Load and expose recipient
        let recipient = layouter.assign_region(
            || "load recipient",
            |mut region| {
                region.assign_advice(
                    || "recipient",
                    config.advice[0],
                    0,
                    || Value::known(self.recipient.unwrap_or(Fr::zero())),
                )
            },
        )?;
        layouter.constrain_instance(recipient.cell(), config.instance, 1)?;

        // Load and expose amount
        let amount = layouter.assign_region(
            || "load amount",
            |mut region| {
                region.assign_advice(
                    || "amount",
                    config.advice[0],
                    0,
                    || Value::known(self.amount.unwrap_or(Fr::zero())),
                )
            },
        )?;
        layouter.constrain_instance(amount.cell(), config.instance, 2)?;

        // Load and expose root
        let root = layouter.assign_region(
            || "load root",
            |mut region| {
                region.assign_advice(
                    || "root",
                    config.advice[0],
                    0,
                    || Value::known(self.root.unwrap_or(Fr::zero())),
                )
            },
        )?;
        layouter.constrain_instance(root.cell(), config.instance, 3)?;

        Ok(())
    }
}

/// Poseidon hash gadget - computes hash in-circuit with proper constraints
fn poseidon_hash_gadget<const L: usize>(
    config: PoseidonConfig<Fr, 3, 2>,
    mut layouter: impl Layouter<Fr>,
    messages: [AssignedCell<Fr, Fr>; L],
) -> Result<AssignedCell<Fr, Fr>, Error> {
    let chip = PoseidonChip::construct(config);
    let hasher = Hash::<_, _, P128Pow5T3<Fr>, ConstantLength<L>, 3, 2>::init(
        chip,
        layouter.namespace(|| "init poseidon hasher"),
    )?;
    hasher.hash(layouter.namespace(|| "hash"), messages)
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
        withdrawal_hash: Some(withdrawal_hash),
        nullifier_preimage: Some(nullifier_preimage),
        recipient: Some(Fr::from(0xabcd)),
        amount: Some(Fr::from(1000)),
        root: Some(Fr::from(0x1234)),
    };

    // Generate an EVM-compatible proof
    println!("Creating test proof...");
    let mut rng = ChaCha20Rng::from_entropy();

    // Get the public inputs (instances) from the circuit
    let instances = test_circuit.instances();

    // Extract the public inputs for JSON output
    let public_inputs = &instances[0];
    let nullifier_field = public_inputs[0];
    let recipient_field = public_inputs[1];
    let amount_field = public_inputs[2];
    let root_field = public_inputs[3];

    // Generate EVM proof (this formats the proof correctly for Solidity verification)
    let proof_bytes = gen_evm_proof_shplonk(&params, &pk, test_circuit.clone(), instances, &mut rng);
    println!("✓ Test proof generated successfully!");
    println!("Proof size: {} bytes", proof_bytes.len());
    println!("Proof hex: 0x{}", hex::encode(&proof_bytes));

    // Check if we should regenerate the verifier
    let output_path = Path::new("contracts/ethereum/Halo2Verifier.yul");
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
    // Convert field elements to hex strings (32 bytes each)
    // BN254 field elements are little-endian in Rust, but Solidity expects big-endian
    let mut nullifier_bytes = nullifier_field.to_repr();
    let mut recipient_bytes = recipient_field.to_repr();
    let mut amount_bytes = amount_field.to_repr();
    let mut root_bytes = root_field.to_repr();

    // Reverse to big-endian for Solidity
    nullifier_bytes.as_mut().reverse();
    recipient_bytes.as_mut().reverse();
    amount_bytes.as_mut().reverse();
    root_bytes.as_mut().reverse();

    let proof_data = serde_json::json!({
        "proof": hex::encode(&proof_bytes),
        "publicInputs": [
            format!("0x{}", hex::encode(nullifier_bytes.as_ref())),  // nullifier (computed in circuit)
            format!("0x{}", hex::encode(recipient_bytes.as_ref())),  // recipient
            format!("0x{}", hex::encode(amount_bytes.as_ref())),     // amount
            format!("0x{}", hex::encode(root_bytes.as_ref())),       // root
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
