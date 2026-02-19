//! Binary to replay Blake2b transcript verification and generate:
//! 1. EVM calldata (BE, uncompressed points)
//! 2. Reference challenge values for testing
//! 3. Transcript trace for debugging
//!
//! Usage:
//!   cd poseidon-proof
//!   cargo run --bin generate-calldata

use std::fs;

use halo2_base::{
    gates::{
        circuit::{builder::RangeCircuitBuilder, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        halo2curves::{
            bn256::{Fr, G1Affine},
            group::GroupEncoding,
        },
        plonk::{keygen_pk, keygen_vk},
        transcript::{
            Blake2bRead, Challenge255, EncodedChallenge, Transcript, TranscriptRead,
            TranscriptReadBuffer,
        },
    },
    utils::{fs::gen_srs, testing::check_proof_with_instances},
};
use poseidon_proof::circuit::PoseidonPreimageCircuit;

const K: u32 = 12;
const UNUSABLE_ROWS: usize = 9;

fn main() {
    let lookup_bits = K as usize - 1;

    println!("=== Blake2b Calldata Generator ===");

    // --- Keygen phase (same as generate_proof.rs) ---
    println!("[1/4] Key generation...");
    let mut builder = RangeCircuitBuilder::from_stage(CircuitBuilderStage::Keygen)
        .use_k(K as usize)
        .use_instance_columns(1);
    builder.set_lookup_bits(lookup_bits);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let default_circuit = PoseidonPreimageCircuit::default_for_keygen(K, UNUSABLE_ROWS);
    let res = default_circuit.closure(builder.pool(0), &range);
    builder.assigned_instances[0] = res;

    let t_cells_lookup = builder
        .lookup_manager()
        .iter()
        .map(|lm| lm.total_rows())
        .sum::<usize>();
    let lookup_bits_ = if t_cells_lookup == 0 {
        None
    } else {
        Some(lookup_bits)
    };
    builder.config_params.lookup_bits = lookup_bits_;
    let _config_params = builder.calculate_params(Some(UNUSABLE_ROWS));

    let params = gen_srs(K);
    let vk = keygen_vk(&params, &builder).unwrap();
    let pk = keygen_pk(&params, vk.clone(), &builder).unwrap();
    let break_points = builder.break_points();
    let config_params = builder.config_params.clone();
    drop(builder);

    // --- Load proof and public inputs ---
    println!("[2/4] Loading proof and public inputs...");

    // Instead of loading from file, generate a fresh proof to ensure consistency
    let secret = Fr::from(42u64);
    let hash = poseidon_proof::poseidon::poseidon_hash(&[secret]);
    let pub_inputs: Vec<Fr> = vec![hash];

    let mut prover_builder =
        RangeCircuitBuilder::prover(config_params, break_points).use_instance_columns(1);
    let range2 = RangeChip::new(lookup_bits, prover_builder.lookup_manager().clone());
    let circuit = PoseidonPreimageCircuit::new(K, UNUSABLE_ROWS, secret);
    let instances = circuit.closure(prover_builder.pool(0), &range2);
    prover_builder.assigned_instances[0] = instances;

    let proof_bytes =
        halo2_base::utils::testing::gen_proof_with_instances(&params, &pk, prover_builder, &[
            &pub_inputs,
        ]);

    println!("  Proof size: {} bytes", proof_bytes.len());
    println!("  Number of public inputs: {}", pub_inputs.len());
    for (i, pi) in pub_inputs.iter().enumerate() {
        let bytes = pi.to_bytes();
        println!("  Public input {}: 0x{}", i, hex::encode(bytes));
    }

    // --- Verify the proof ---
    println!("[3/5] Verifying proof with Blake2b transcript...");
    check_proof_with_instances(&params, &vk, &proof_bytes, &[&pub_inputs], true);
    println!("  ✓ Proof verified successfully!");

    // --- Output VK transcript_repr ---
    let vk_repr = vk.transcript_repr();
    let vk_repr_le = vk_repr.to_bytes();
    let mut vk_repr_be = vk_repr_le.to_vec();
    vk_repr_be.reverse();
    println!("  VK transcript_repr (LE): 0x{}", hex::encode(&vk_repr_le));
    println!("  VK transcript_repr (BE): 0x{}", hex::encode(&vk_repr_be));

    // --- Parse proof structure ---
    println!("[4/5] Parsing proof structure...");

    // The proof bytes are a flat concatenation of:
    // - Compressed EC points (32 bytes each, LE)
    // - Scalars (32 bytes each, LE)
    // in the order they were written during proving.
    //
    // For SHPLONK with this circuit, the structure is:
    // Phase 0 advice commitments, then challenges, then more commitments, etc.
    //
    // We need to replay the transcript to figure out the exact structure.
    // Let's parse the proof bytes manually.

    let mut offset = 0;
    let mut items: Vec<ProofItem> = Vec::new();

    // The proof structure follows the verification flow in verify_proof:
    // 1. VK hash is written to transcript (not in proof bytes)
    // 2. Instance values are written to transcript (not in proof bytes)
    // 3. For each phase: a. Read advice commitments (compressed points) b. Squeeze
    //    challenges
    // 4. Read lookup permuted commitments
    // 5. Squeeze theta, beta, gamma
    // 6. Read permutation product commitments
    // 7. Read lookup product commitments
    // 8. Read vanishing random commitment
    // 9. Squeeze y
    // 10. Read vanishing quotient commitments
    // 11. Squeeze x
    // 12. Read evaluations (scalars)
    // 13. Squeeze challenges for SHPLONK
    // 14. Read W, W' points

    // We need to know the circuit structure to parse correctly.
    // Let's get this from the VK.
    let cs = vk.cs();
    println!("\n  Circuit structure:");
    println!("    num_advice_columns: {}", cs.num_advice_columns());
    println!("    num_instance_columns: {}", cs.num_instance_columns());
    println!("    num_fixed_columns: {}", cs.num_fixed_columns());
    println!("    num_challenges: {}", cs.num_challenges());
    println!("    advice_column_phase: {:?}", cs.advice_column_phase());
    println!("    challenge_phase: {:?}", cs.challenge_phase());
    println!("    num_lookups: {}", cs.lookups().len());
    println!(
        "    permutation columns: {}",
        cs.permutation().get_columns().len()
    );
    println!("    advice_queries: {}", cs.advice_queries().len());
    println!("    instance_queries: {}", cs.instance_queries().len());
    println!("    fixed_queries: {}", cs.fixed_queries().len());
    println!("    gates: {}", cs.gates().len());

    // Count advice columns per phase
    let advice_phases = cs.advice_column_phase();
    let mut phase_set: Vec<u8> = advice_phases.to_vec();
    phase_set.sort();
    phase_set.dedup();
    println!("    phases: {:?}", phase_set);

    for phase in &phase_set {
        let count = advice_phases.iter().filter(|p| **p == *phase).count();
        println!("    advice columns in phase {}: {}", phase, count);
    }

    // Count permutation product commitments
    let num_perm_columns = cs.permutation().get_columns().len();
    let chunk_size = cs.degree() - 2;
    let num_perm_products = (num_perm_columns + chunk_size - 1) / chunk_size;
    println!("    degree: {}", cs.degree());
    println!("    permutation chunk_size: {}", chunk_size);
    println!("    num_perm_products: {}", num_perm_products);

    // Count quotient commitments
    let num_quotient_chunks = cs.degree() - 1;
    println!("    num_quotient_chunks: {}", num_quotient_chunks);

    // Now parse the proof
    println!("\n  Proof items:");

    // Phase 0 advice commitments
    let phase0_count = advice_phases.iter().filter(|p| **p == 0).count();
    for i in 0..phase0_count {
        let point = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Phase 0 advice commitment {}: ({}, {})",
            items.len(),
            i,
            hex::encode(&point.x.to_bytes()[..8]),
            hex::encode(&point.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point));
    }

    // Phase 0 challenges
    for (i, (phase, _)) in cs.challenge_phase().iter().zip(0..).enumerate() {
        if *phase == 0 {
            println!("    [squeeze] Challenge {} (phase 0)", i);
        }
    }

    // Check if there are more phases
    let has_phase1 = advice_phases.iter().any(|p| *p == 1);
    if has_phase1 {
        let phase1_count = advice_phases.iter().filter(|p| **p == 1).count();
        for i in 0..phase1_count {
            let point = read_compressed_point(&proof_bytes, &mut offset);
            println!(
                "    [{}] Phase 1 advice commitment {}: ({}, {})",
                items.len(),
                i,
                hex::encode(&point.x.to_bytes()[..8]),
                hex::encode(&point.y.to_bytes()[..8])
            );
            items.push(ProofItem::Point(point));
        }
    }

    // Theta challenge (squeeze)
    println!("    [squeeze] Theta challenge");

    // Lookup permuted commitments (2 points per lookup: permuted_input,
    // permuted_table)
    for i in 0..cs.lookups().len() {
        let point1 = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Lookup {} permuted_input: ({}, {})",
            items.len(),
            i,
            hex::encode(&point1.x.to_bytes()[..8]),
            hex::encode(&point1.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point1));

        let point2 = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Lookup {} permuted_table: ({}, {})",
            items.len(),
            i,
            hex::encode(&point2.x.to_bytes()[..8]),
            hex::encode(&point2.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point2));
    }

    // Beta, Gamma challenges (squeeze)
    println!("    [squeeze] Beta challenge");
    println!("    [squeeze] Gamma challenge");

    // Permutation product commitments
    for i in 0..num_perm_products {
        let point = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Permutation product {}: ({}, {})",
            items.len(),
            i,
            hex::encode(&point.x.to_bytes()[..8]),
            hex::encode(&point.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point));
    }

    // Lookup product commitments
    for i in 0..cs.lookups().len() {
        let point = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Lookup {} product: ({}, {})",
            items.len(),
            i,
            hex::encode(&point.x.to_bytes()[..8]),
            hex::encode(&point.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point));
    }

    // Vanishing random commitment
    let point = read_compressed_point(&proof_bytes, &mut offset);
    println!(
        "    [{}] Vanishing random: ({}, {})",
        items.len(),
        hex::encode(&point.x.to_bytes()[..8]),
        hex::encode(&point.y.to_bytes()[..8])
    );
    items.push(ProofItem::Point(point));

    // Y challenge (squeeze)
    println!("    [squeeze] Y challenge");

    // Vanishing quotient commitments
    for i in 0..num_quotient_chunks {
        let point = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] Quotient chunk {}: ({}, {})",
            items.len(),
            i,
            hex::encode(&point.x.to_bytes()[..8]),
            hex::encode(&point.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point));
    }

    // X challenge (squeeze)
    println!("    [squeeze] X challenge");

    // Evaluations
    // instance_evals: not read from transcript (computed from instances)
    // advice_evals: cs.advice_queries.len()
    let num_advice_evals = cs.advice_queries().len();
    for i in 0..num_advice_evals {
        let scalar = read_scalar(&proof_bytes, &mut offset);
        println!(
            "    [{}] Advice eval {}: 0x{}...",
            items.len(),
            i,
            hex::encode(&scalar.to_bytes()[..8])
        );
        items.push(ProofItem::Scalar(scalar));
    }

    // fixed_evals: cs.fixed_queries.len()
    let num_fixed_evals = cs.fixed_queries().len();
    for i in 0..num_fixed_evals {
        let scalar = read_scalar(&proof_bytes, &mut offset);
        println!(
            "    [{}] Fixed eval {}: 0x{}...",
            items.len(),
            i,
            hex::encode(&scalar.to_bytes()[..8])
        );
        items.push(ProofItem::Scalar(scalar));
    }

    // vanishing eval (h(x))
    let scalar = read_scalar(&proof_bytes, &mut offset);
    println!(
        "    [{}] Vanishing eval: 0x{}...",
        items.len(),
        hex::encode(&scalar.to_bytes()[..8])
    );
    items.push(ProofItem::Scalar(scalar));

    // permutation common evals: num_permutation_fixed columns
    let num_perm_fixed = cs.permutation().get_columns().len();
    for i in 0..num_perm_fixed {
        let scalar = read_scalar(&proof_bytes, &mut offset);
        println!(
            "    [{}] Permutation common eval {}: 0x{}...",
            items.len(),
            i,
            hex::encode(&scalar.to_bytes()[..8])
        );
        items.push(ProofItem::Scalar(scalar));
    }

    // permutation product evals: for each product, read eval and eval_next
    // Also read eval_prev for products after the first
    for i in 0..num_perm_products {
        let scalar = read_scalar(&proof_bytes, &mut offset);
        println!(
            "    [{}] Perm product {} eval: 0x{}...",
            items.len(),
            i,
            hex::encode(&scalar.to_bytes()[..8])
        );
        items.push(ProofItem::Scalar(scalar));

        let scalar = read_scalar(&proof_bytes, &mut offset);
        println!(
            "    [{}] Perm product {} eval_next: 0x{}...",
            items.len(),
            i,
            hex::encode(&scalar.to_bytes()[..8])
        );
        items.push(ProofItem::Scalar(scalar));

        if i > 0 {
            let scalar = read_scalar(&proof_bytes, &mut offset);
            println!(
                "    [{}] Perm product {} eval_prev: 0x{}...",
                items.len(),
                i,
                hex::encode(&scalar.to_bytes()[..8])
            );
            items.push(ProofItem::Scalar(scalar));
        }
    }

    // lookup evals: for each lookup, read product_eval, product_next_eval,
    // permuted_input_eval, permuted_input_inv_eval, permuted_table_eval
    for i in 0..cs.lookups().len() {
        for (_j, name) in [
            "product_eval",
            "product_next_eval",
            "permuted_input_eval",
            "permuted_input_inv_eval",
            "permuted_table_eval",
        ]
        .iter()
        .enumerate()
        {
            let scalar = read_scalar(&proof_bytes, &mut offset);
            println!(
                "    [{}] Lookup {} {}: 0x{}...",
                items.len(),
                i,
                name,
                hex::encode(&scalar.to_bytes()[..8])
            );
            items.push(ProofItem::Scalar(scalar));
        }
    }

    // SHPLONK challenges (squeeze)
    println!("    [squeeze] SHPLONK challenges");

    // W point
    let point = read_compressed_point(&proof_bytes, &mut offset);
    println!(
        "    [{}] W: ({}, {})",
        items.len(),
        hex::encode(&point.x.to_bytes()[..8]),
        hex::encode(&point.y.to_bytes()[..8])
    );
    items.push(ProofItem::Point(point));

    // W' point
    if offset < proof_bytes.len() {
        let point = read_compressed_point(&proof_bytes, &mut offset);
        println!(
            "    [{}] W': ({}, {})",
            items.len(),
            hex::encode(&point.x.to_bytes()[..8]),
            hex::encode(&point.y.to_bytes()[..8])
        );
        items.push(ProofItem::Point(point));
    }

    println!("\n  Total items: {}", items.len());
    println!("  Bytes consumed: {} / {}", offset, proof_bytes.len());
    if offset < proof_bytes.len() {
        println!("  WARNING: {} bytes remaining!", proof_bytes.len() - offset);
    }

    // Count points and scalars
    let num_points = items
        .iter()
        .filter(|i| matches!(i, ProofItem::Point(_)))
        .count();
    let num_scalars = items
        .iter()
        .filter(|i| matches!(i, ProofItem::Scalar(_)))
        .count();
    println!("  Points: {}, Scalars: {}", num_points, num_scalars);
    println!(
        "  Expected size: {} compressed + {} scalars = {} bytes",
        num_points * 32,
        num_scalars * 32,
        num_points * 32 + num_scalars * 32
    );

    // --- Generate EVM calldata ---
    println!("\n[5/5] Generating EVM calldata...");

    // EVM calldata layout (same as Keccak verifier):
    // - 1 instance (32 bytes BE)
    // - 13 EC points uncompressed (13 * 64 = 832 bytes, x BE then y BE)
    // - 32 scalars (32 * 32 = 1024 bytes BE)
    // - 2 EC points uncompressed (2 * 64 = 128 bytes, W and W')
    // Total: 32 + 832 + 1024 + 128 = 2016 bytes

    let mut calldata = Vec::new();

    // Public input (BE 32 bytes)
    let pi_bytes = pub_inputs[0].to_bytes(); // LE
    let mut pi_be = pi_bytes.to_vec();
    pi_be.reverse();
    calldata.extend_from_slice(&pi_be);

    // Convert proof items to EVM format
    for item in &items {
        match item {
            ProofItem::Point(p) => {
                // Uncompressed point: x (32 bytes BE) then y (32 bytes BE)
                let x_le = p.x.to_bytes();
                let y_le = p.y.to_bytes();
                let mut x_be = x_le.to_vec();
                x_be.reverse();
                let mut y_be = y_le.to_vec();
                y_be.reverse();
                calldata.extend_from_slice(&x_be);
                calldata.extend_from_slice(&y_be);
            },
            ProofItem::Scalar(s) => {
                // Scalar: 32 bytes BE
                let s_le = s.to_bytes();
                let mut s_be = s_le.to_vec();
                s_be.reverse();
                calldata.extend_from_slice(&s_be);
            },
        }
    }

    println!("  Calldata size: {} bytes", calldata.len());
    println!(
        "  Expected: {} (1 instance + {} points * 64 + {} scalars * 32)",
        32 + num_points * 64 + num_scalars * 32,
        num_points,
        num_scalars
    );

    // Save calldata
    std::fs::create_dir_all("data").unwrap();
    std::fs::write("data/evm_calldata.bin", &calldata).unwrap();
    std::fs::write("data/evm_calldata.hex", hex::encode(&calldata)).unwrap();

    // Also save the proof bytes and public inputs for reference
    std::fs::write("data/proof.bin", &proof_bytes).unwrap();
    std::fs::write("data/proof.hex", hex::encode(&proof_bytes)).unwrap();
    let pub_input_bytes: Vec<u8> = pub_inputs
        .iter()
        .flat_map(|f| f.to_bytes().to_vec())
        .collect();
    std::fs::write("data/public_inputs.bin", &pub_input_bytes).unwrap();
    std::fs::write("data/public_inputs.hex", hex::encode(&pub_input_bytes)).unwrap();

    println!("\n=== Artifacts saved ===");
    println!("  data/evm_calldata.bin  - EVM calldata (BE, uncompressed)");
    println!("  data/evm_calldata.hex  - EVM calldata (hex)");
    println!("  data/proof.bin         - Blake2b proof bytes");
    println!("  data/proof.hex         - Blake2b proof bytes (hex)");
    println!("  data/public_inputs.bin - Public inputs");
    println!("  data/public_inputs.hex - Public inputs (hex)");

    // --- Replay Blake2b transcript to extract all challenges ---
    println!("\n[6/6] Replaying Blake2b transcript to extract challenges...");
    let vk_repr = vk.transcript_repr();
    let vk_repr_le = vk_repr.to_bytes();
    let mut vk_repr_be = vk_repr_le.to_vec();
    vk_repr_be.reverse();
    println!("  VK transcript_repr (LE): 0x{}", hex::encode(&vk_repr_le));
    println!("  VK transcript_repr (BE): 0x{}", hex::encode(&vk_repr_be));
    {
        let mut transcript =
            Blake2bRead::<_, G1Affine, Challenge255<G1Affine>>::init(&proof_bytes[..]);

        // 1. VK hash_into: common_scalar(transcript_repr)
        transcript.common_scalar(vk_repr).unwrap();

        // 2. Instance values: common_scalar for each public input
        for pi in &pub_inputs {
            transcript.common_scalar(*pi).unwrap();
        }

        // 3. Phase 0 advice commitments
        let phase0_count = cs.advice_column_phase().iter().filter(|p| **p == 0).count();
        for _ in 0..phase0_count {
            let _point = transcript.read_point().unwrap();
        }

        // Phase 0 challenges (if any)
        for (phase, _) in cs.challenge_phase().iter().zip(0..) {
            if *phase == 0 {
                let _ch = transcript.squeeze_challenge();
            }
        }

        // Phase 1 advice commitments (if any)
        let phase1_count = cs.advice_column_phase().iter().filter(|p| **p == 1).count();
        for _ in 0..phase1_count {
            let _point = transcript.read_point().unwrap();
        }

        // 4. Theta challenge
        let theta: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut theta_be = theta.get_scalar().to_bytes().to_vec();
        theta_be.reverse();
        println!("  theta (BE): 0x{}", hex::encode(&theta_be));

        // 5. Lookup permuted commitments
        for _ in 0..cs.lookups().len() {
            let _p1 = transcript.read_point().unwrap();
            let _p2 = transcript.read_point().unwrap();
        }

        // 6. Beta challenge
        let beta: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut beta_be = beta.get_scalar().to_bytes().to_vec();
        beta_be.reverse();
        println!("  beta  (BE): 0x{}", hex::encode(&beta_be));

        // 7. Gamma challenge
        let gamma: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut gamma_be = gamma.get_scalar().to_bytes().to_vec();
        gamma_be.reverse();
        println!("  gamma (BE): 0x{}", hex::encode(&gamma_be));

        // 8. Permutation product commitments
        for _ in 0..num_perm_products {
            let _point = transcript.read_point().unwrap();
        }

        // 9. Lookup product commitments
        for _ in 0..cs.lookups().len() {
            let _point = transcript.read_point().unwrap();
        }

        // 10. Vanishing random commitment
        let _vanishing_random = transcript.read_point().unwrap();

        // 11. Y challenge
        let y_ch: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut y_be = y_ch.get_scalar().to_bytes().to_vec();
        y_be.reverse();
        println!("  y     (BE): 0x{}", hex::encode(&y_be));

        // 12. Quotient commitments
        for _ in 0..num_quotient_chunks {
            let _point = transcript.read_point().unwrap();
        }

        // 13. X challenge
        let x_ch: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut x_be = x_ch.get_scalar().to_bytes().to_vec();
        x_be.reverse();
        println!("  x     (BE): 0x{}", hex::encode(&x_be));

        // 14. Read all evaluations (scalars)
        for _ in 0..cs.advice_queries().len() {
            let _s = transcript.read_scalar().unwrap();
        }
        for _ in 0..cs.fixed_queries().len() {
            let _s = transcript.read_scalar().unwrap();
        }
        let _s = transcript.read_scalar().unwrap(); // vanishing eval
        for _ in 0..cs.permutation().get_columns().len() {
            let _s = transcript.read_scalar().unwrap();
        }
        for i in 0..num_perm_products {
            let _s = transcript.read_scalar().unwrap();
            let _s = transcript.read_scalar().unwrap();
            if i > 0 {
                let _s = transcript.read_scalar().unwrap();
            }
        }
        for _ in 0..cs.lookups().len() {
            for _ in 0..5 {
                let _s = transcript.read_scalar().unwrap();
            }
        }

        // 15. SHPLONK challenges: v and u
        let v_ch: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut v_be = v_ch.get_scalar().to_bytes().to_vec();
        v_be.reverse();
        println!("  v     (BE): 0x{}", hex::encode(&v_be));

        let u_ch: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut u_be = u_ch.get_scalar().to_bytes().to_vec();
        u_be.reverse();
        println!("  u     (BE): 0x{}", hex::encode(&u_be));

        // 16. W point
        let _w = transcript.read_point().unwrap();

        // 17. Final challenge (for SHPLONK)
        let final_ch: Challenge255<G1Affine> = transcript.squeeze_challenge();
        let mut final_be = final_ch.get_scalar().to_bytes().to_vec();
        final_be.reverse();
        println!("  final (BE): 0x{}", hex::encode(&final_be));

        // Save challenges to file for Solidity test
        let challenges_json = format!(
            r#"{{"vk_repr":"0x{}","theta":"0x{}","beta":"0x{}","gamma":"0x{}","y":"0x{}","x":"0x{}","v":"0x{}","u":"0x{}","final":"0x{}"}}"#,
            hex::encode(&vk_repr_be),
            hex::encode(&theta_be),
            hex::encode(&beta_be),
            hex::encode(&gamma_be),
            hex::encode(&y_be),
            hex::encode(&x_be),
            hex::encode(&v_be),
            hex::encode(&u_be),
            hex::encode(&final_be),
        );
        fs::write("data/challenges.json", &challenges_json).unwrap();
        println!("\n  Saved challenges to data/challenges.json");
    }
}

#[derive(Debug)]
enum ProofItem {
    Point(G1Affine),
    Scalar(Fr),
}

fn read_compressed_point(data: &[u8], offset: &mut usize) -> G1Affine {
    let mut bytes = <G1Affine as GroupEncoding>::Repr::default();
    bytes.as_mut().copy_from_slice(&data[*offset..*offset + 32]);
    *offset += 32;
    let point: Option<G1Affine> = G1Affine::from_bytes(&bytes).into();
    point.expect("Invalid compressed point")
}

fn read_scalar(data: &[u8], offset: &mut usize) -> Fr {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&data[*offset..*offset + 32]);
    *offset += 32;
    Fr::from_bytes(&bytes).unwrap()
}
