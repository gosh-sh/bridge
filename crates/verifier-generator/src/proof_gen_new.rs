//! Generate ZK proofs for testing using scroll-tech/halo2
//!
//! This binary generates real Halo2 proofs for specific public inputs
//! to be used in Solidity tests.

use halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    halo2curves::group::ff::PrimeField,
    circuit::{Layouter, Value, AssignedCell, SimpleFloorPlanner},
    plonk::{Circuit, ConstraintSystem, Error, Column, Advice, Instance, Selector, create_proof, keygen_pk, keygen_vk},
    poly::commitment::ParamsProver,
    poly::kzg::commitment::{KZGCommitmentScheme, ParamsKZG},
    poly::kzg::multiopen::ProverSHPLONK,
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3Compact};
use poseidon_circuit::{
    poseidon::{Hash, Pow5Chip as PoseidonChip, Pow5Config as PoseidonConfig},
    Hashable,
};
use rand::rngs::OsRng;
use std::env;

/// Withdrawal circuit configuration
#[derive(Clone, Debug)]
struct WithdrawalCircuitConfig {
    advice: [Column<Advice>; 3],
    instance: Column<Instance>,
    selector: Selector,
    poseidon_config: PoseidonConfig<Fr, 3, 2>,
}

/// Withdrawal circuit
///
/// This circuit verifies a withdrawal by:
/// 1. Computing nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
/// 2. Exposing public inputs: [nullifier, recipient, amount, root]
#[derive(Clone, Debug, Default)]
struct WithdrawalCircuit {
    withdrawal_hash: Option<Fr>,
    nullifier_preimage: Option<Fr>,
    recipient: Option<Fr>,
    amount: Option<Fr>,
    root: Option<Fr>,
}

impl Circuit<Fr> for WithdrawalCircuit {
    type Config = WithdrawalCircuitConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self::default()
    }

    fn configure(meta: &mut ConstraintSystem<Fr>) -> Self::Config {
        let advice = [
            meta.advice_column(),
            meta.advice_column(),
            meta.advice_column(),
        ];
        let instance = meta.instance_column();
        let selector = meta.selector();

        // Enable equality for advice columns
        for col in &advice {
            meta.enable_equality(*col);
        }
        meta.enable_equality(instance);

        // Configure Poseidon
        let poseidon_config = PoseidonChip::configure::<poseidon_base::primitives::P128Pow5T3<Fr>>(
            meta,
            advice[0..3].try_into().unwrap(),
            advice[0],
            advice[1],
            advice[2],
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

        // Compute nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
        let nullifier = poseidon_hash_gadget(
            config.poseidon_config.clone(),
            layouter.namespace(|| "compute nullifier"),
            [withdrawal_hash, nullifier_preimage],
        )?;

        // Expose public inputs
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

/// Poseidon hash gadget
fn poseidon_hash_gadget<const L: usize>(
    config: PoseidonConfig<Fr, 3, 2>,
    mut layouter: impl Layouter<Fr>,
    messages: [AssignedCell<Fr, Fr>; L],
) -> Result<AssignedCell<Fr, Fr>, Error> {
    let chip = PoseidonChip::construct(config);
    let hasher = Hash::<_, _, poseidon_base::primitives::P128Pow5T3<Fr>, ConstantLength<L>, 3, 2>::init(
        chip,
        layouter.namespace(|| "init poseidon hasher"),
    )?;
    hasher.hash(layouter.namespace(|| "hash"), messages)
}

/// Compute Poseidon hash outside circuit
fn poseidon_hash<const L: usize>(message: [Fr; L]) -> Fr {
    PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<L>, 3, 2>::init().hash(message)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() != 6 {
        eprintln!("Usage: {} <withdrawal_hash> <nullifier_preimage> <recipient> <amount> <root>", args[0]);
        eprintln!("All arguments should be decimal numbers");
        std::process::exit(1);
    }

    // Parse inputs
    let withdrawal_hash = Fr::from_str_vartime(&args[1]).expect("Invalid withdrawal_hash");
    let nullifier_preimage = Fr::from_str_vartime(&args[2]).expect("Invalid nullifier_preimage");
    let recipient = Fr::from_str_vartime(&args[3]).expect("Invalid recipient");
    let amount = Fr::from_str_vartime(&args[4]).expect("Invalid amount");
    let root_str = &args[5];
    
    // Parse root (handle 0x prefix)
    let root = if root_str.starts_with("0x") {
        Fr::from_str_vartime(&root_str[2..]).expect("Invalid root")
    } else {
        Fr::from_str_vartime(root_str).expect("Invalid root")
    };

    // Compute expected nullifier
    let expected_nullifier = poseidon_hash([withdrawal_hash, nullifier_preimage]);
    
    eprintln!("Inputs:");
    eprintln!("  withdrawal_hash: {}", withdrawal_hash);
    eprintln!("  nullifier_preimage: {}", nullifier_preimage);
    eprintln!("  recipient: {}", recipient);
    eprintln!("  amount: {}", amount);
    eprintln!("  root: {}", root);
    eprintln!("Expected nullifier: 0x{}", hex::encode(expected_nullifier.to_repr().as_ref()));

    // Create circuit
    let circuit = WithdrawalCircuit {
        withdrawal_hash: Some(withdrawal_hash),
        nullifier_preimage: Some(nullifier_preimage),
        recipient: Some(recipient),
        amount: Some(amount),
        root: Some(root),
    };

    // Setup parameters (k=10 for small circuit)
    let k = 10;
    let params: ParamsKZG<Bn256> = ParamsKZG::new(k);

    // Generate keys
    eprintln!("Generating verification key...");
    let vk = keygen_vk(&params, &circuit).expect("vk generation failed");
    
    eprintln!("Generating proving key...");
    let pk = keygen_pk(&params, vk, &circuit).expect("pk generation failed");

    // Public inputs
    let pub_inputs = vec![expected_nullifier, recipient, amount, root];

    // Generate proof
    eprintln!("Generating proof...");
    let mut transcript = Blake2bWrite::<_, _, Challenge255<_>>::init(vec![]);
    
    create_proof::<KZGCommitmentScheme<Bn256>, ProverSHPLONK<_>, _, _, _, _>(
        &params,
        &pk,
        &[circuit],
        &[&[&pub_inputs]],
        OsRng,
        &mut transcript,
    )
    .expect("proof generation failed");

    let proof = transcript.finalize();
    
    // Output proof as hex
    println!("{}", hex::encode(&proof));
    
    eprintln!("Proof generated successfully ({} bytes)", proof.len());
}

