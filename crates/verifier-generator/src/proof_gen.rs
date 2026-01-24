//! Generate ZK proofs for testing
//!
//! This binary generates real Halo2 proofs for specific public inputs
//! to be used in Solidity tests.

use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value, AssignedCell},
    halo2curves::bn256::Fr,
    halo2curves::ff::PrimeField,
    plonk::{
        Advice, Circuit, Column,
        ConstraintSystem, Error, Instance, Selector,
    },
};
use halo2_base::utils::fs::gen_srs;
use snark_verifier_sdk::{
    gen_pk,
    gen_evm_proof_shplonk,
    CircuitExt,
};
use std::env;
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
        vec![4]
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

        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

        // Configure Poseidon - need to provide lagrange coefficients
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
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();

    if args.len() != 6 {
        eprintln!("Usage: {} <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root>", args[0]);
        eprintln!("Example: {} 12345 67890 0xabcd 1000 0x1234", args[0]);
        eprintln!();
        eprintln!("All values can be decimal or hex (0x prefix)");
        eprintln!("For bytes32 values, use hex format");
        eprintln!();
        eprintln!("Note: The circuit will compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)");
        std::process::exit(1);
    }

    let withdrawal_hash = parse_field_element(&args[1], "withdrawal_hash");
    let nullifier_preimage = parse_field_element(&args[2], "nullifier_preimage");
    let recipient = parse_field_element(&args[3], "recipient");
    let amount = parse_field_element(&args[4], "amount");
    let root = parse_field_element(&args[5], "root");

    // Compute nullifier using scroll-tech/poseidon
    let nullifier = PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init()
        .hash([withdrawal_hash, nullifier_preimage]);

    println!("Generating proof for:");
    println!("  withdrawal_hash: {}", format_field_element(&withdrawal_hash));
    println!("  nullifier_preimage: {}", format_field_element(&nullifier_preimage));
    println!("  computed nullifier: {}", format_field_element(&nullifier));
    println!("  recipient: {}", format_field_element(&recipient));
    println!("  amount: {}", format_field_element(&amount));
    println!("  root: {}", format_field_element(&root));
    println!();

    // Circuit size parameter
    let k = 10;

    // Setup parameters
    println!("Setting up KZG parameters (k={})...", k);
    let params = gen_srs(k);

    // Create circuit for keygen
    let circuit = WithdrawalCircuit::default();

    // Generate proving key
    println!("Generating proving key...");
    let pk = gen_pk(&params, &circuit, None);

    // Create circuit with witness values
    println!("Creating circuit with witness values...");
    let test_circuit = WithdrawalCircuit {
        withdrawal_hash: Some(withdrawal_hash),
        nullifier_preimage: Some(nullifier_preimage),
        recipient: Some(recipient),
        amount: Some(amount),
        root: Some(root),
    };

    // Generate proof
    println!("Generating proof...");
    let mut rng = ChaCha20Rng::from_entropy();

    // Get the public inputs (instances) from the circuit
    let instances = test_circuit.instances();

    // First verify the circuit is satisfied using MockProver
    println!("Running MockProver to verify circuit constraints...");
    use halo2_proofs::dev::MockProver;
    MockProver::run(k, &test_circuit, instances.clone())
        .expect("MockProver::run failed")
        .assert_satisfied_par();
    println!("✓ Circuit constraints satisfied!");
    println!();

    // Generate EVM proof (this formats the proof correctly for Solidity verification)
    let proof_bytes = gen_evm_proof_shplonk(&params, &pk, test_circuit, instances, &mut rng);
    println!("✓ Proof generated successfully!");
    println!();
    println!("Proof size: {} bytes", proof_bytes.len());
    println!("Proof hex: 0x{}", hex::encode(&proof_bytes));
    println!();

    // Output JSON format for Solidity tests
    let json_output = serde_json::json!({
        "proof": hex::encode(&proof_bytes),
        "publicInputs": {
            "nullifier": format_field_element(&nullifier),
            "recipient": format_field_element(&recipient),
            "amount": format_field_element(&amount),
            "root": format_field_element(&root),
        }
    });

    println!("JSON output:");
    println!("{}", serde_json::to_string_pretty(&json_output).unwrap());
}

/// Parse a field element from string (supports decimal and hex)
fn parse_field_element(s: &str, name: &str) -> Fr {
    if s.starts_with("0x") || s.starts_with("0X") {
        // Parse as hex - handle bytes32 values
        let hex_str = &s[2..];

        // Decode hex to bytes
        let bytes = hex::decode(hex_str)
            .unwrap_or_else(|_| panic!("Invalid hex {} value: {}", name, s));

        // Pad to 32 bytes (big-endian)
        let mut padded = [0u8; 32];
        if bytes.len() <= 32 {
            padded[32 - bytes.len()..].copy_from_slice(&bytes);
        } else {
            panic!("{} value too large (max 32 bytes): {}", name, s);
        }

        // Convert bytes to field element (little-endian for Fr)
        let mut repr = <Fr as PrimeField>::Repr::default();
        // Reverse for little-endian
        let mut le_bytes = padded;
        le_bytes.reverse();
        repr.as_mut().copy_from_slice(&le_bytes);

        // from_repr returns CtOption - use into() to unwrap or panic
        let option = Fr::from_repr(repr);
        if option.is_some().into() {
            option.unwrap()
        } else {
            panic!("{} value not in field (value too large for BN254 scalar field): {}", name, s)
        }
    } else {
        // Parse as decimal
        let val: u64 = s.parse()
            .unwrap_or_else(|_| panic!("Invalid {} value: {}", name, s));
        Fr::from(val)
    }
}

/// Format field element as 0x-prefixed hex string (32 bytes, big-endian for Solidity)
fn format_field_element(f: &Fr) -> String {
    let mut bytes = f.to_repr();
    // Fr::to_repr() returns little-endian, but Solidity expects big-endian
    bytes.as_mut().reverse();
    format!("0x{}", hex::encode(bytes.as_ref()))
}

