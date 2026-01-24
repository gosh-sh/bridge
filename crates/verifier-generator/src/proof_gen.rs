//! Generate ZK proofs for testing
//!
//! This binary generates real Halo2 proofs for specific public inputs
//! to be used in Solidity tests.

use halo2_base::halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
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
    gen_snark_shplonk,
    CircuitExt,
};
use std::env;
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3Compact};
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};

/// Withdrawal circuit configuration
#[derive(Clone, Debug)]
struct WithdrawalCircuitConfig {
    advice: [Column<Advice>; 3],
    instance: Column<Instance>,
    selector: Selector,
}

/// Withdrawal circuit (same as in main.rs)
#[derive(Clone, Debug, Default)]
struct WithdrawalCircuit {
    withdrawal_hash: Value<Fr>,
    nullifier_preimage: Value<Fr>,
    merkle_proof: Vec<Value<Fr>>,
    merkle_path_indices: Vec<Value<Fr>>,
    recipient: Value<Fr>,
    amount: Value<Fr>,
    root: Value<Fr>,
}

impl CircuitExt<Fr> for WithdrawalCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![4]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        let mut nullifier = Fr::zero();
        let mut recipient_val = Fr::zero();
        let mut amount_val = Fr::zero();
        let mut root_val = Fr::zero();

        let withdrawal_hash_opt = self.withdrawal_hash.clone();
        let nullifier_preimage_opt = self.nullifier_preimage.clone();

        withdrawal_hash_opt.zip(nullifier_preimage_opt).map(|(wh, np)| {
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

        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

        meta.create_gate("poseidon_hash", |meta| {
            let s = meta.query_selector(selector);
            let a = meta.query_advice(advice[0], halo2_base::halo2_proofs::poly::Rotation::cur());
            let b = meta.query_advice(advice[1], halo2_base::halo2_proofs::poly::Rotation::cur());
            let c = meta.query_advice(advice[2], halo2_base::halo2_proofs::poly::Rotation::cur());
            vec![s * (a + b - c)]
        });

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
        // Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
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

                Ok((wh_cell, np_cell, nullifier_cell, recipient_cell, amount_cell, root_cell))
            },
        )?;

        // Constrain public inputs/outputs to instance column
        layouter.constrain_instance(nullifier_cell.cell(), config.instance, 0)?;
        layouter.constrain_instance(recipient_cell.cell(), config.instance, 1)?;
        layouter.constrain_instance(amount_cell.cell(), config.instance, 2)?;
        layouter.constrain_instance(root_cell.cell(), config.instance, 3)?;

        Ok(())
    }
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
        withdrawal_hash: Value::known(withdrawal_hash),
        nullifier_preimage: Value::known(nullifier_preimage),
        merkle_proof: vec![],
        merkle_path_indices: vec![],
        recipient: Value::known(recipient),
        amount: Value::known(amount),
        root: Value::known(root),
    };

    // Generate proof
    println!("Generating proof...");
    let mut rng = ChaCha20Rng::from_entropy();
    let snark = gen_snark_shplonk(&params, &pk, test_circuit, &mut rng, None::<&str>)
        .expect("Failed to generate SNARK proof");

    let proof_bytes = snark.proof;
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

