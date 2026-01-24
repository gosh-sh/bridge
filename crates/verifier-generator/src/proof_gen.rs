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
    halo2::gen_snark_shplonk,
    CircuitExt,
};
use std::env;

/// Simple withdrawal circuit configuration
#[derive(Clone, Debug)]
struct SimpleCircuitConfig {
    advice: [Column<Advice>; 2],
    instance: Column<Instance>,
    selector: Selector,
}

/// Simple withdrawal circuit (same as in main.rs)
#[derive(Clone, Debug, Default)]
struct SimpleCircuit {
    pub_input_0: Value<Fr>,  // nullifier
    pub_input_1: Value<Fr>,  // recipient
    pub_input_2: Value<Fr>,  // amount
    pub_input_3: Value<Fr>,  // root
}

impl CircuitExt<Fr> for SimpleCircuit {
    fn num_instance(&self) -> Vec<usize> {
        vec![4]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        let mut val0 = Fr::zero();
        let mut val1 = Fr::zero();
        let mut val2 = Fr::zero();
        let mut val3 = Fr::zero();

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

        meta.enable_equality(instance);
        for col in &advice {
            meta.enable_equality(*col);
        }

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
                let cell0 = region.assign_advice(config.advice[0], 0, self.pub_input_0);
                let cell1 = region.assign_advice(config.advice[1], 0, self.pub_input_1);
                let cell2 = region.assign_advice(config.advice[0], 1, self.pub_input_2);
                let cell3 = region.assign_advice(config.advice[1], 1, self.pub_input_3);

                Ok((cell0, cell1, cell2, cell3))
            },
        )?;

        layouter.constrain_instance(cell0.cell(), config.instance, 0);
        layouter.constrain_instance(cell1.cell(), config.instance, 1);
        layouter.constrain_instance(cell2.cell(), config.instance, 2);
        layouter.constrain_instance(cell3.cell(), config.instance, 3);

        Ok(())
    }
}

fn main() {
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();

    if args.len() != 5 {
        eprintln!("Usage: {} <nullifier> <recipient> <amount> <root>", args[0]);
        eprintln!("Example: {} 12345 0xabcd 1000 0x1234", args[0]);
        eprintln!();
        eprintln!("All values can be decimal or hex (0x prefix)");
        eprintln!("For bytes32 values, use hex format");
        std::process::exit(1);
    }

    let nullifier = parse_field_element(&args[1], "nullifier");
    let recipient = parse_field_element(&args[2], "recipient");
    let amount = parse_field_element(&args[3], "amount");
    let root = parse_field_element(&args[4], "root");

    println!("Generating proof for:");
    println!("  nullifier: {}", format_field_element(&nullifier));
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
    let circuit = SimpleCircuit::default();

    // Generate proving key
    println!("Generating proving key...");
    let pk = gen_pk(&params, &circuit, None);

    // Create circuit with witness values
    println!("Creating circuit with witness values...");
    let test_circuit = SimpleCircuit {
        pub_input_0: Value::known(nullifier),
        pub_input_1: Value::known(recipient),
        pub_input_2: Value::known(amount),
        pub_input_3: Value::known(root),
    };

    // Generate proof
    println!("Generating proof...");
    let snark = gen_snark_shplonk(&params, &pk, test_circuit, None::<&str>);

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

