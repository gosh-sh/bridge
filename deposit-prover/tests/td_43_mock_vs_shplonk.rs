//! TD-43 / DEP-MOCK-VS-REAL — MockProver green ≠ SHPLONK opcode triple acceptance.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use deposit_prover::{
    audit_circuit_config,
    export_blake2b_deposit_triple,
    test_circuit_mock,
    test_circuit_mock_instances,
    types::DepositProofOutput,
    verify_deposit_opcode_triple,
    verify_proof,
    types::DepositProofInput,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn proof_00_input_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/proof_00/input.json")
}

fn vk_blob_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/deposit_vk_blob.bin")
}

fn audit_proof_00_dir() -> PathBuf {
    repo_root().join("audit/spec/an/fixtures/deposit_10proofs/proof_00")
}

fn load_proof_00_input() -> DepositProofInput {
    let json = fs::read_to_string(proof_00_input_path()).expect("proof_00/input.json");
    serde_json::from_str(&json).expect("parse input.json")
}

#[derive(Clone)]
struct OpcodeTriple {
    proof: Vec<u8>,
    pubin: Vec<u8>,
}

fn local_proof_00_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/deposit_10proofs/proof_00")
}

const OPCODE_PI_BYTES: usize = 12 * 32;

fn try_load_opcode_triple_from(dir: &Path) -> Option<OpcodeTriple> {
    let proof_path = dir.join("proof.bin");
    let pubin_path = dir.join("public_inputs.bin");
    if !proof_path.is_file() || !pubin_path.is_file() {
        return None;
    }
    let pubin = fs::read(&pubin_path).expect("read public_inputs.bin");
    if pubin.len() != OPCODE_PI_BYTES {
        return None;
    }
    Some(OpcodeTriple {
        proof: fs::read(&proof_path).expect("read proof.bin"),
        pubin,
    })
}

fn load_or_generate_opcode_triple() -> OpcodeTriple {
    static CACHE: OnceLock<OpcodeTriple> = OnceLock::new();
    CACHE.get_or_init(|| {
        for dir in [local_proof_00_dir(), audit_proof_00_dir()] {
            if let Some(triple) = try_load_opcode_triple_from(&dir) {
                let vk = fs::read(vk_blob_path()).expect("vk blob");
                let config = audit_circuit_config();
                if verify_deposit_opcode_triple(&vk, &triple.proof, &triple.pubin, config.degree)
                    .is_ok()
                {
                    return triple;
                }
            }
        }
        let input = load_proof_00_input();
        let config = audit_circuit_config();
        let (proof, pubin) =
            export_blake2b_deposit_triple(input, &config).expect("export Blake2b triple");
        OpcodeTriple { proof, pubin }
    })
    .clone()
}

#[test]
fn td_43_proof_00_mock_prover_satisfied() {
    let input = load_proof_00_input();
    test_circuit_mock(input, &audit_circuit_config()).expect("MockProver baseline green");
}

#[test]
fn td_43_mock_green_random_proof_fails_shplonk() {
    let input = load_proof_00_input();
    let config = audit_circuit_config();
    test_circuit_mock(input.clone(), &config).expect("mock green");
    let instances = test_circuit_mock_instances(input, &config, None).expect("instances");
    let pubin = deposit_prover::halo2_tvm_bundle::encode_instances(&instances[0]);
    let vk = fs::read(vk_blob_path()).expect("vk blob");
    let garbage_proof = vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33];
    let err = verify_deposit_opcode_triple(&vk, &garbage_proof, &pubin, config.degree)
        .unwrap_err();
    assert!(
        err.contains("SHPLONK") || err.contains("rejected") || err.contains("verify_proof"),
        "TD-43: random proof must fail opcode path: {err}"
    );
}

#[test]
fn td_43_real_proof_00_passes_opcode_triple() {
    let triple = load_or_generate_opcode_triple();
    let vk = fs::read(vk_blob_path()).expect("vk blob");
    let config = audit_circuit_config();
    verify_deposit_opcode_triple(&vk, &triple.proof, &triple.pubin, config.degree)
        .expect("real Blake2b triple must pass SHPLONK opcode path");
}

#[test]
fn td_43_corrupted_real_proof_fails_shplonk() {
    let input = load_proof_00_input();
    let config = audit_circuit_config();
    test_circuit_mock(input, &config).expect("mock still green after proof corruption");

    let triple = load_or_generate_opcode_triple();
    let mut corrupted = triple.proof.clone();
    corrupted[0] ^= 0x01;
    let vk = fs::read(vk_blob_path()).expect("vk blob");
    assert!(
        verify_deposit_opcode_triple(&vk, &corrupted, &triple.pubin, config.degree).is_err(),
        "1-byte flip must break SHPLONK verify"
    );
}

#[test]
fn td_43_verify_proof_struct_not_crypto() {
    let input = load_proof_00_input();
    let config = audit_circuit_config();
    let chain_id = input.resolve_chain_id(None).expect("witness chain_id");
    let triple = load_or_generate_opcode_triple();
    let vk = fs::read(vk_blob_path()).expect("vk blob");

    verify_deposit_opcode_triple(&vk, &triple.proof, &triple.pubin, config.degree)
        .expect("SHPLONK accepts real Blake2b triple");

    // Opcode proof bytes are not bincode `Snark` — local verify_proof is not SHPLONK.
    let block_hash = ethers::utils::keccak256(&input.receipt_proof.block_header_rlp);
    let mut amount = [0u8; 32];
    amount.copy_from_slice(&input.event_data.amount);
    let bogus = DepositProofOutput::new(
        triple.proof.clone(),
        input.event_data.deposit_id,
        input.event_data.sender,
        amount,
        input.dapp_id,
        input.event_data.an_account,
        input.event_data.contract_address,
        block_hash,
        chain_id,
    );
    assert!(
        verify_proof(&bogus, &config).is_err(),
        "TD-43: Blake2b wire proof is not bincode Snark for verify_proof"
    );
    assert!(
        verify_deposit_opcode_triple(&vk, &[0xde, 0xad, 0xbe, 0xef], &triple.pubin, config.degree)
            .is_err(),
        "random proof bytes fail SHPLONK even when PI layout is valid"
    );
}
