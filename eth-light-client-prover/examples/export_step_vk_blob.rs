//! M5 — emit the **production** light-client "step" `VkBlob` + a sample proof +
//! public-inputs triple, keyed on the **Hermez** KZG ceremony, and self-verify
//! it through the EXACT read + verify path the AN `ZKHALO2VERIFYWITHVK` opcode
//! runs.
//!
//! ## Why Hermez, not the chain ceremony
//!
//! Since 2026-07-23 the `ZKHALO2VERIFYWITHVK` opcode
//! (`tvm-sdk/tvm_vm/src/executor/zk_halo2_utils.rs::KZG_S_G2_BYTES`) is keyed
//! on the **Hermez Perpetual Powers of Tau** `[s]·G2` (`92 8f af b3 …`, valid
//! for any K ≤ 28). A KZG verifier only checks openings against `[s]·G2`, and
//! the VK's fixed / permutation commitments are `Σ cᵢ·[τ^i]G1` — both are
//! ceremony (τ) specific. So a VkBlob + proof the opcode will accept MUST be
//! keyed on the Hermez SRS. The legacy Dark DEX `ZKHALO2VERIFY`
//! (`DARK_DEX_KZG_S_G2_BYTES`, `c6 02 8a cf …`) still uses the chain ceremony —
//! irrelevant here.
//!
//! ## What this emits (`out/step_vkblob/`)
//!
//! - `step_vk_blob.bin`          — **Base v1** `VkBlob` (magic `VKBLOB\0\0`,
//!   version 1, transcript Blake2b, shape Base, `BaseCircuitParams` JSON +
//!   `VerifyingKey::write(RawBytes)`). This is the mature wire the opcode reads
//!   TODAY (Dark DEX / fallback) — the step is a `BaseCircuitBuilder`, so Base
//!   v1 applies directly (no v2/RLC fork-bump dependency).
//! - `step_base_circuit_params.json` — the carried config, for reference.
//! - `step_public_inputs.bin`    — `STEP_INSTANCE_LEN × 32` LE `Fr` (the
//!   opcode's `public_inputs_cell` payload).
//! - `step_proof_blake2b.bin`    — raw SHPLONK proof bytes (the `proof_cell`).
//!
//! ## Opcode-faithful self-check
//!
//! After emitting, this reparses the blob and runs, byte-for-byte, the opcode's
//! Base branch:
//!   `VerifyingKey::read::<_, BaseCircuitBuilder<Fr>>(RawBytes, base_params)`
//! then `verify_proof::<KZG, VerifierSHPLONK, Blake2bRead, SingleStrategy>`
//! against `srs.verifier_params()`. Because the SRS is the Hermez ceremony, its
//! `(g[0], g2, s_g2)` are the same points the opcode rebuilds its params from
//! (`build_shared_kzg_params`), so a PASS here is a faithful proxy for on-AN
//! acceptance. The VK bytes are also asserted byte-stable across a read→write.
//!
//! ## Run (n14)
//!
//! ```bash
//! # 1. Fetch the Hermez k=19 raw SRS (~67 MB) into data/:
//! mkdir -p data && \
//!   wget -O data/kzg_params_19.srs \
//!   https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-19
//! # 2. Emit + self-verify (heavy: ~9 min, ~42 GB RSS at k=19):
//! cargo run --release --example export_step_vk_blob
//! ```
//!
//! Overridable via env: `STEP_SRS_PATH` (default `data/kzg_params_19.srs`),
//! `STEP_OUT_DIR` (default `out/step_vkblob`).

use std::{fs, path::Path, time::Instant};

use eth_light_client_prover::{
    bls_core::{signing_root_to_g2, verify_native},
    execution::ExecutionPayloadVals,
    live_witness::{step_witness_from_beacon_with_params, ChainParams},
    signing::native_sync_committee_signing_root,
    step::{verify_step, HeaderVals, StepWitness, STEP_INSTANCE_LEN},
};
use gosh_sha256_chip::Sha256Chip;
use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        halo2curves::{
            bls12_381::{
                G1Affine as BlsG1Affine, G2Affine as BlsG2Affine, G1 as BlsG1, G2 as BlsG2,
            },
            bn256::{Bn256, Fr, G1Affine},
            ff::Field,
            group::{Curve, Group},
            serde::SerdeObject,
        },
        plonk::{keygen_pk, keygen_vk, verify_proof, VerifyingKey},
        poly::{
            commitment::{Params, ParamsProver},
            kzg::{
                commitment::{KZGCommitmentScheme, ParamsKZG},
                multiopen::VerifierSHPLONK,
                strategy::SingleStrategy,
            },
        },
        transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer},
        SerdeFormat,
    },
    utils::testing::gen_proof_with_instances,
};
use rand::rngs::OsRng;
use serde_json::Value;

const FIXTURE: &str = include_str!("../fixtures/mainnet/finality_update.json");

/// Frozen `VkBlob` magic (see `deposit-prover/src/halo2_tvm_bundle.rs`).
const VK_BLOB_MAGIC: &[u8; 8] = b"VKBLOB\x00\x00";
/// Hermez `[s]·G2` raw-bytes head — the ceremony the opcode embeds.
const HERMEZ_SG2_HEAD: [u8; 4] = [0x92, 0x8f, 0xaf, 0xb3];

const K: u32 = 19;
const LOOKUP_BITS: usize = 18;

// ---------------------------------------------------------------------------
// Fixture → StepWitness (mirrors tests/step_mock_prover.rs::build_witness).
// The VK is witness-independent (its shape depends only on the constraint
// system), so the synthetic-committee witness yields the production VK; the
// emitted proof is a valid sample over that witness.
// ---------------------------------------------------------------------------

fn h32(s: &str) -> [u8; 32] {
    hex::decode(s.trim_start_matches("0x"))
        .unwrap()
        .try_into()
        .unwrap()
}
fn hexv(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap()
}
fn u64s(v: &Value) -> u64 {
    v.as_str().unwrap().parse().unwrap()
}
fn u256_le(s: &str) -> [u8; 32] {
    let val: u128 = s.parse().unwrap();
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&val.to_le_bytes());
    out
}
fn header(v: &Value, which: &str) -> HeaderVals {
    let b = &v["data"][which]["beacon"];
    HeaderVals {
        slot: b["slot"].as_str().unwrap().parse().unwrap(),
        proposer_index: b["proposer_index"].as_str().unwrap().parse().unwrap(),
        parent_root: h32(b["parent_root"].as_str().unwrap()),
        state_root: h32(b["state_root"].as_str().unwrap()),
        body_root: h32(b["body_root"].as_str().unwrap()),
    }
}
fn execution(v: &Value, which: &str) -> ExecutionPayloadVals {
    let e = &v["data"][which]["execution"];
    ExecutionPayloadVals {
        parent_hash: h32(e["parent_hash"].as_str().unwrap()),
        fee_recipient: hexv(e["fee_recipient"].as_str().unwrap())
            .try_into()
            .unwrap(),
        state_root: h32(e["state_root"].as_str().unwrap()),
        receipts_root: h32(e["receipts_root"].as_str().unwrap()),
        logs_bloom: hexv(e["logs_bloom"].as_str().unwrap()),
        prev_randao: h32(e["prev_randao"].as_str().unwrap()),
        block_number: u64s(&e["block_number"]),
        gas_limit: u64s(&e["gas_limit"]),
        gas_used: u64s(&e["gas_used"]),
        timestamp: u64s(&e["timestamp"]),
        extra_data: hexv(e["extra_data"].as_str().unwrap()),
        base_fee_per_gas: u256_le(e["base_fee_per_gas"].as_str().unwrap()),
        block_hash: h32(e["block_hash"].as_str().unwrap()),
        transactions_root: h32(e["transactions_root"].as_str().unwrap()),
        withdrawals_root: h32(e["withdrawals_root"].as_str().unwrap()),
        blob_gas_used: u64s(&e["blob_gas_used"]),
        excess_blob_gas: u64s(&e["excess_blob_gas"]),
    }
}
fn execution_branch(v: &Value, which: &str) -> Vec<[u8; 32]> {
    v["data"][which]["execution_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect()
}

fn fixture_json() -> anyhow::Result<String> {
    match std::env::var("FINALITY_UPDATE_PATH") {
        Ok(p) => Ok(std::fs::read_to_string(&p)?),
        Err(_) => Ok(FIXTURE.to_string()),
    }
}

fn build_witness() -> anyhow::Result<StepWitness> {
    let finality = fixture_json()?;
    // Signing domain: `BEACON_FORK_VERSION` + `BEACON_GENESIS_VALIDATORS_ROOT`
    // (the relayer fills them from the beacon node), default mainnet Fulu.
    // Witness-only, so the VK below is the same for every network.
    let params = ChainParams::from_env()?;
    println!("Signing domain: {}", params.label());
    if let Ok(p) = std::env::var("COMMITTEE_JSON_PATH").or_else(|_| std::env::var("BOOTSTRAP_PATH"))
    {
        let committee = std::fs::read_to_string(&p)?;
        return step_witness_from_beacon_with_params(&finality, &committee, &params);
    }
    eprintln!(
        "WARN: COMMITTEE_JSON_PATH/BOOTSTRAP_PATH unset — synthetic 512-committee (VK emit only, \
         not chain-valid)"
    );
    let v: Value = serde_json::from_str(&finality)?;
    let attested = header(&v, "attested_header");
    let finalized = header(&v, "finalized_header");
    let finality_branch: Vec<[u8; 32]> = v["data"]["finality_branch"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| h32(e.as_str().unwrap()))
        .collect();

    let signing_root = native_sync_committee_signing_root(
        attested.slot,
        attested.proposer_index,
        &attested.parent_root,
        &attested.state_root,
        &attested.body_root,
        &params.fork_version,
        &params.genesis_validators_root,
    );
    let msg_hash = signing_root_to_g2(&signing_root);
    let hm = BlsG2::from(msg_hash);

    let mut pubkeys: Vec<BlsG1Affine> = Vec::with_capacity(512);
    let mut agg = BlsG1::identity();
    let mut sig = BlsG2::identity();
    for _ in 0..512 {
        let sk = <BlsG1 as Group>::Scalar::random(OsRng);
        pubkeys.push((BlsG1::generator() * sk).to_affine());
        agg += BlsG1::generator() * sk;
        sig += hm * sk;
    }
    let signature: BlsG2Affine = sig.to_affine();
    assert!(
        verify_native(&signature, &agg.to_affine(), &msg_hash),
        "synthetic committee must verify"
    );

    let pubkeys_compressed: Vec<[u8; 48]> = pubkeys.iter().map(|p| p.to_compressed_be()).collect();
    let aggregate_pubkey = agg.to_affine().to_compressed_be();

    Ok(StepWitness {
        attested,
        finalized,
        finality_branch,
        fork_version: params.fork_version,
        genesis_validators_root: params.genesis_validators_root,
        pubkeys,
        pubkeys_compressed,
        aggregate_pubkey,
        bits: vec![true; 512],
        signature,
        finalized_execution: execution(&v, "finalized_header"),
        execution_branch: execution_branch(&v, "finalized_header"),
    })
}

// ---------------------------------------------------------------------------
// Base v1 VkBlob writer (byte-identical to deposit-prover's
// `VkBlob::from_native(..).to_bytes()` for the Base shape).
// ---------------------------------------------------------------------------

fn write_chunk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn encode_base_v1_vkblob(config: &BaseCircuitParams, vk: &VerifyingKey<G1Affine>) -> Vec<u8> {
    let mut vk_bytes = Vec::new();
    vk.write(&mut vk_bytes, SerdeFormat::RawBytes)
        .expect("VerifyingKey::write(RawBytes)");
    let cfg_json = serde_json::to_vec(config).expect("serialise BaseCircuitParams");

    let mut out = Vec::new();
    out.extend_from_slice(VK_BLOB_MAGIC);
    out.push(1); // version = 1 (Base, frozen)
    out.push(0); // transcript_kind = 0 (Blake2b)
    out.push(0); // circuit_shape = 0 (Base; reserved 0 in v1)
    out.extend_from_slice(&[0u8; 5]); // reserved
    write_chunk(&mut out, &cfg_json);
    write_chunk(&mut out, &vk_bytes);
    out
}

/// Reparse a Base v1 VkBlob → (BaseCircuitParams, vk_bytes). Mirrors the header
/// checks in `VkBlob::read` for the Base branch.
fn parse_base_v1_vkblob(bytes: &[u8]) -> (BaseCircuitParams, Vec<u8>) {
    assert!(bytes.len() >= 16, "VkBlob too short");
    assert_eq!(&bytes[0..8], VK_BLOB_MAGIC, "VkBlob magic mismatch");
    assert_eq!(bytes[8], 1, "expected v1 (Base)");
    assert_eq!(bytes[9], 0, "expected Blake2b transcript");
    assert_eq!(bytes[10], 0, "v1 shape byte must be reserved 0");
    let mut off = 16;
    let cfg_len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let cfg: BaseCircuitParams = serde_json::from_slice(&bytes[off..off + cfg_len]).unwrap();
    off += cfg_len;
    let vk_len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let vk_bytes = bytes[off..off + vk_len].to_vec();
    (cfg, vk_bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn main() -> anyhow::Result<()> {
    println!("=== M5: export production step VkBlob (Hermez k={K}) ===\n");

    let srs_path =
        std::env::var("STEP_SRS_PATH").unwrap_or_else(|_| "data/kzg_params_19.srs".to_string());
    let out_dir = std::env::var("STEP_OUT_DIR").unwrap_or_else(|_| "out/step_vkblob".to_string());
    fs::create_dir_all(&out_dir)?;

    // ---- Load the Hermez SRS ------------------------------------------------
    if !Path::new(&srs_path).exists() {
        anyhow::bail!(
            "Hermez SRS not found at {srs_path}.\n  Fetch it with:\n    mkdir -p data && wget -O \
             {srs_path} https://trusted-setup-halo2kzg.s3.eu-central-1.amazonaws.com/hermez-raw-{K}"
        );
    }
    println!("Loading SRS {srs_path} ...");
    let mut f = fs::File::open(&srs_path)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    println!("  loaded k={}", params.k());
    if params.k() > K {
        println!("  downsizing {} -> {K}", params.k());
        params.downsize(K);
    }
    anyhow::ensure!(params.k() == K, "SRS k={} != required {K}", params.k());

    // Ceremony sanity: the SRS s_g2 must be the Hermez point the opcode embeds.
    let sg2 = params.s_g2().to_raw_bytes();
    println!(
        "  s_g2 head={:02x?} tail={:02x?}",
        &sg2[0..4],
        &sg2[sg2.len() - 3..]
    );
    anyhow::ensure!(
        sg2[0..4] == HERMEZ_SG2_HEAD,
        "SRS s_g2 head {:02x?} != Hermez {:02x?} — WRONG ceremony (opcode would reject)",
        &sg2[0..4],
        HERMEZ_SG2_HEAD
    );
    println!("  OK: Hermez ceremony (opcode-aligned KZG_S_G2_BYTES)\n");

    let w = build_witness()?;

    // ---- Keygen circuit -----------------------------------------------------
    let mut kb = BaseCircuitBuilder::<Fr>::from_stage(CircuitBuilderStage::Keygen)
        .use_k(K as usize)
        .use_lookup_bits(LOOKUP_BITS)
        .use_instance_columns(1);
    let range = RangeChip::new(LOOKUP_BITS, kb.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(kb.pool(0), &range, &sha, &w);
    assert_eq!(pi.instances.len(), STEP_INSTANCE_LEN);
    kb.assigned_instances[0] = pi.instances.clone();
    kb.config_params.lookup_bits = Some(LOOKUP_BITS);
    let config = kb.calculate_params(Some(9));
    println!("Circuit config: {config:?}\n");

    let t = Instant::now();
    let vk = keygen_vk(&params, &kb).map_err(|e| anyhow::anyhow!("keygen_vk: {e:?}"))?;
    println!("keygen_vk {:?}", t.elapsed());

    // Serialise the VkBlob BEFORE keygen_pk moves `vk`.
    let vk_blob = encode_base_v1_vkblob(&config, &vk);

    let t = Instant::now();
    let pk = keygen_pk(&params, vk, &kb).map_err(|e| anyhow::anyhow!("keygen_pk: {e:?}"))?;
    println!("keygen_pk {:?}", t.elapsed());
    let break_points = kb.break_points();
    drop(kb);

    // ---- Prover circuit (identical shape) -----------------------------------
    let mut pb = BaseCircuitBuilder::prover(config.clone(), break_points).use_instance_columns(1);
    let range = RangeChip::new(LOOKUP_BITS, pb.lookup_manager().clone());
    let sha = Sha256Chip::new(&range);
    let pi = verify_step(pb.pool(0), &range, &sha, &w);
    let inst: Vec<Fr> = pi.instances.iter().map(|c| *c.value()).collect();
    pb.assigned_instances[0] = pi.instances.clone();

    let t = Instant::now();
    let proof = gen_proof_with_instances(&params, &pk, pb, &[&inst]);
    println!("prove {:?}", t.elapsed());

    // ---- Encode the public-inputs operand (N × 32 LE Fr) --------------------
    let mut pi_bytes = Vec::with_capacity(inst.len() * 32);
    for fr in &inst {
        pi_bytes.extend_from_slice(fr.to_bytes().as_ref());
    }

    // ---- Opcode-faithful self-check -----------------------------------------
    println!("\nSelf-check (opcode Base read + SHPLONK verify path)...");
    let (cfg_back, vk_bytes_back) = parse_base_v1_vkblob(&vk_blob);
    assert_eq!(cfg_back.k, config.k, "config.k round-trip");
    let mut slice: &[u8] = &vk_bytes_back;
    let vk_reread = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
        &mut slice,
        SerdeFormat::RawBytes,
        cfg_back.clone(),
    )
    .map_err(|e| anyhow::anyhow!("opcode-path VerifyingKey::read failed: {e}"))?;
    // VK bytes must be byte-stable across read->write.
    let mut reser = Vec::new();
    vk_reread.write(&mut reser, SerdeFormat::RawBytes)?;
    anyhow::ensure!(
        reser == vk_bytes_back,
        "VK bytes not byte-stable across round-trip"
    );
    anyhow::ensure!(
        vk_reread.get_domain().k() == K,
        "VK domain.k {} != {K}",
        vk_reread.get_domain().k()
    );
    println!("  OK: VK reads back via BaseCircuitBuilder<Fr> + carried BaseCircuitParams");

    let verifier_params = params.verifier_params();
    let strategy = SingleStrategy::new(&params);
    let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(proof.as_slice());
    let instance_refs: &[&[Fr]] = &[&inst];
    let ok = verify_proof::<
        KZGCommitmentScheme<Bn256>,
        VerifierSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
        SingleStrategy<'_, Bn256>,
    >(
        verifier_params,
        &vk_reread,
        strategy,
        &[instance_refs],
        &mut transcript,
    )
    .is_ok();
    anyhow::ensure!(ok, "SHPLONK verify_proof (opcode path) REJECTED the proof");
    println!("  OK: verify_proof (VerifierSHPLONK + Blake2b + Hermez verifier_params) ACCEPTED\n");

    // ---- Write artifacts ----------------------------------------------------
    let vk_blob_path = format!("{out_dir}/step_vk_blob.bin");
    let cfg_path = format!("{out_dir}/step_base_circuit_params.json");
    let pi_path = format!("{out_dir}/step_public_inputs.bin");
    let proof_path = format!("{out_dir}/step_proof_blake2b.bin");
    fs::write(&vk_blob_path, &vk_blob)?;
    fs::write(&cfg_path, serde_json::to_vec_pretty(&config)?)?;
    fs::write(&pi_path, &pi_bytes)?;
    fs::write(&proof_path, &proof)?;

    println!("=== RESULT: PASS — production step VkBlob is opcode-readable ===");
    println!(
        "  {vk_blob_path}  ({} B, sha256 {})",
        vk_blob.len(),
        sha256_hex(&vk_blob)
    );
    println!(
        "  {pi_path}  ({} B = {} × 32 Fr)",
        pi_bytes.len(),
        inst.len()
    );
    println!(
        "  {proof_path}  ({} B, sha256 {})",
        proof.len(),
        sha256_hex(&proof)
    );
    println!("  {cfg_path}");
    Ok(())
}
