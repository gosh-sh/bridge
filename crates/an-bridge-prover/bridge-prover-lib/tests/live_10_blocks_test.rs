//! Integration test: prove and verify 10 consecutive blocks from a live node.
//! Requires: running acki-nacki node at localhost, ./bk_set.json

use std::path::Path;
use std::time::{Duration, Instant};

const NUM_BLOCKS: u32 = 10;
const GQL_ENDPOINT: &str = "http://localhost/graphql";

#[tokio::test]
async fn test_prove_10_live_blocks() {
    let bk_set_path = if Path::new("bk_set.json").exists() {
        "bk_set.json"
    } else if Path::new("../bk_set.json").exists() {
        "../bk_set.json"
    } else {
        eprintln!("Skipping: bk_set.json not found");
        return;
    };
    let params_dir = if Path::new("params").exists() {
        "params"
    } else if Path::new("../params").exists() {
        "../params"
    } else {
        "params"
    };

    println!("\n{}", "=".repeat(70));
    println!("  LIVE TEST: Prove and verify {} consecutive blocks", NUM_BLOCKS);
    println!("{}", "=".repeat(70));
    let t_total = Instant::now();

    // 1. Load BK set.
    let bk_set = bridge_prover_lib::bk_set_fetcher::load_bk_set_from_config(bk_set_path)
        .expect("failed to load BK set");
    let (bk_set_commitment, _) = bridge_prover_lib::poseidon::compute_bk_set_poseidon(&bk_set);
    println!("\nBK set: {} signers", bk_set.len());
    println!("BK set Poseidon commitment: {:?}", bk_set_commitment);
    for (idx, pk) in bk_set.iter() {
        println!("  signer {}: {}", idx, hex::encode(pk));
    }

    // 2. Initialize key manager.
    println!("\n--- Loading keys ---");
    let t = Instant::now();
    let mut key_manager = bridge_prover_lib::keys::KeyManager::new(Path::new(params_dir));
    key_manager.ensure_primary_keys(&bk_set).expect("keygen failed");
    println!("[timing] key load/gen: {:?}", t.elapsed());

    // 3. Connect to node.
    let gql = bridge_prover_lib::gql_client::create_client(GQL_ENDPOINT)
        .expect("failed to create GQL client");

    // 4. Find a starting point: pick a recent block.
    let blocks = gql.query_latest_blocks(20).await.expect("failed to query blocks");
    if blocks.is_empty() {
        eprintln!("No blocks found on the node");
        return;
    }
    // Start from a block ~15 behind the latest (so all attestations are available).
    let latest_seq = blocks.iter().map(|(_, s)| *s).max().unwrap();
    let start_seq = if latest_seq > 15 { (latest_seq - 15) as u32 } else { 1 };
    println!("\nLatest block on node: seq_no={}", latest_seq);
    println!("Starting from block: seq_no={}", start_seq);

    // 5. Prove and verify loop.
    let mut last_seen: u32 = start_seq.saturating_sub(1);
    let mut results: Vec<BlockResult> = Vec::new();
    let mut total_proof_time = Duration::ZERO;
    let mut total_verify_time = Duration::ZERO;

    for i in 0..NUM_BLOCKS {
        let target = last_seen + 1;
        println!("\n{}", "-".repeat(70));
        println!("Block {}/{}: seq_no={}", i + 1, NUM_BLOCKS, target);
        println!("{}", "-".repeat(70));

        // Fetch attestation.
        let t = Instant::now();
        let att = match bridge_prover_lib::attestation_fetcher::fetch_attestation_for_block(
            &gql, target,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                println!("  SKIP: attestation not found: {}", e);
                results.push(BlockResult {
                    seq_no: target,
                    status: "SKIP".to_string(),
                    proof_time: Duration::ZERO,
                    verify_time: Duration::ZERO,
                    proof_size: 0,
                    signers: 0,
                    target_type: "?".to_string(),
                    error: Some(e.to_string()),
                });
                last_seen = target;
                continue;
            }
        };
        let fetch_time = t.elapsed();
        let target_type_str = if att.target_type == 0 { "Primary" } else { "Fallback" };
        let num_signers = att.signature_occurrences.len();
        println!("  attestation: type={}, signers={}, fetch_time={:?}",
            target_type_str, num_signers, fetch_time);
        println!("  block_id:    {}", hex::encode(&att.block_id));
        println!("  parent_id:   {}", hex::encode(&att.parent_block_id));
        println!("  env_hash:    {}", hex::encode(&att.envelope_hash));
        println!("  signers:     {:?}", att.signature_occurrences);
        println!("  raw_bytes:   {} bytes", att.raw_bytes.len());

        if att.target_type != 0 {
            println!("  SKIP: fallback attestation");
            results.push(BlockResult {
                seq_no: target,
                status: "SKIP-FALLBACK".to_string(),
                proof_time: Duration::ZERO,
                verify_time: Duration::ZERO,
                proof_size: 0,
                signers: num_signers,
                target_type: target_type_str.to_string(),
                error: None,
            });
            last_seen = target;
            continue;
        }

        // Off-circuit BLS verification.
        let t = Instant::now();
        {
            let sig_bytes = bridge_parsers::attestation_data_parser::parse_signature_bytes(&att.raw_bytes);
            let entries = bridge_parsers::attestation_data_parser::parse_signer_entries(&att.raw_bytes);
            let att_data = bridge_parsers::attestation_data_parser::parse_attestation_data_bytes(&att.raw_bytes);
            let signature = gosh_bls_verification::helpers::deserialize_g2_signature(sig_bytes);
            let msg_hash = gosh_bls_verification::helpers::compute_msg_hash(&att_data[..120]);
            let pks = gosh_bls_verification::helpers::resolve_pubkeys(&entries, &bk_set);
            let agg_pk = gosh_bls_verification::helpers::compute_agg_pubkey(&pks);
            let ok = gosh_bls_verification::helpers::verify_bls_native(&signature, &agg_pk, &msg_hash);
            assert!(ok, "Off-circuit BLS failed for block {}", target);
        }
        println!("  off-circuit BLS: PASSED ({:?})", t.elapsed());

        // Generate proof.
        let t = Instant::now();
        let proof_output = match bridge_prover_lib::prover::generate_primary_proof(
            &key_manager,
            &att.raw_bytes,
            &bk_set,
            last_seen,
        ) {
            Ok(p) => p,
            Err(e) => {
                println!("  PROOF GENERATION FAILED: {}", e);
                results.push(BlockResult {
                    seq_no: target,
                    status: "PROOF-FAIL".to_string(),
                    proof_time: t.elapsed(),
                    verify_time: Duration::ZERO,
                    proof_size: 0,
                    signers: num_signers,
                    target_type: target_type_str.to_string(),
                    error: Some(e.to_string()),
                });
                last_seen = target;
                continue;
            }
        };
        let proof_time = t.elapsed();
        total_proof_time += proof_time;
        println!("  proof: {} bytes, generated in {:?}", proof_output.proof_bytes.len(), proof_time);
        println!("  instances:");
        println!("    [0] block_id: {:?}", proof_output.block_id_fr);
        println!("    [1] bk_commit:     {:?}", proof_output.bk_set_commitment_fr);
        println!("    [2] block_seq_no:  {}", proof_output.block_seq_no);
        println!("    [3] last_seen:     {}", proof_output.last_seen_block_seqno);

        // Verify proof.
        let t = Instant::now();
        let instances = vec![
            proof_output.block_id_fr,
            proof_output.bk_set_commitment_fr,
            bridge_prover_lib::Fr::from(proof_output.block_seq_no as u64),
            bridge_prover_lib::Fr::from(last_seen as u64),
        ];
        let verified = bridge_prover_lib::verifier::verify_primary_proof(
            &key_manager,
            &proof_output.proof_bytes,
            &instances,
        );
        let verify_time = t.elapsed();
        total_verify_time += verify_time;
        let status = if verified { "OK" } else { "VERIFY-FAIL" };
        println!("  verification: {} ({:?})", status, verify_time);

        results.push(BlockResult {
            seq_no: target,
            status: status.to_string(),
            proof_time,
            verify_time,
            proof_size: proof_output.proof_bytes.len(),
            signers: num_signers,
            target_type: target_type_str.to_string(),
            error: if verified { None } else { Some("proof verification failed".into()) },
        });

        last_seen = target;
    }

    // 6. Summary.
    let elapsed = t_total.elapsed();
    let ok_count = results.iter().filter(|r| r.status == "OK").count();
    let fail_count = results.iter().filter(|r| r.status.contains("FAIL")).count();
    let skip_count = results.iter().filter(|r| r.status.starts_with("SKIP")).count();

    println!("\n{}", "=".repeat(70));
    println!("  SUMMARY");
    println!("{}", "=".repeat(70));
    println!("  Total time:         {:?}", elapsed);
    println!("  Blocks processed:   {}", results.len());
    println!("  Verified OK:        {}", ok_count);
    println!("  Verification FAIL:  {}", fail_count);
    println!("  Skipped:            {}", skip_count);
    if ok_count > 0 {
        println!("  Avg proof time:     {:?}", total_proof_time / ok_count as u32);
        println!("  Avg verify time:    {:?}", total_verify_time / ok_count as u32);
    }

    println!("\n  Per-block results:");
    println!("  {:>6} {:>8} {:>7} {:>10} {:>10} {:>6}", "seq_no", "status", "type", "proof_t", "verify_t", "size");
    for r in &results {
        println!("  {:>6} {:>8} {:>7} {:>10} {:>10} {:>6}",
            r.seq_no, r.status, r.target_type,
            format!("{:.1?}", r.proof_time),
            format!("{:.1?}", r.verify_time),
            r.proof_size,
        );
    }

    if !results.iter().any(|r| r.status.contains("FAIL")) {
        println!("\n  ALL BLOCKS VERIFIED SUCCESSFULLY!");
    } else {
        println!("\n  FAILURES:");
        for r in &results {
            if let Some(err) = &r.error {
                println!("    seq_no={}: {}", r.seq_no, err);
            }
        }
    }

    assert_eq!(fail_count, 0, "{} blocks failed verification", fail_count);
}

struct BlockResult {
    seq_no: u32,
    status: String,
    proof_time: Duration,
    verify_time: Duration,
    proof_size: usize,
    signers: usize,
    target_type: String,
    error: Option<String>,
}
