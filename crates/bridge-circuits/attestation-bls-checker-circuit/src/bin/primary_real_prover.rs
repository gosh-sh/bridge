//! Standalone real-prover scaling benchmark for the Primary Attestation BLS
//! Checker Circuit.
//!
//! Runs SRS → keygen → prove → verify for each requested `max_signers` value,
//! with `bk_set_size == max_signers`. Each `max_signers` gets its own VK/PK
//! (because `BaseCircuitParams` change with the cap). Caches SRS/VK/PK/config
//! to disk so re-runs skip the heaviest step.
//!
//! Reports timings for every phase. Designed to be uploaded as a single binary
//! to a benchmark host — see the README for cross-compilation guidance.
//!
//! ## Build (locally, ship `./primary_real_prover`):
//!
//! ```text
//! cargo build --release -p attestation-bls-checker-circuit \
//!     --features bench-bin --bin primary_real_prover
//! # Binary is at target/release/primary_real_prover
//! ```
//!
//! ## Run on the remote host:
//!
//! ```text
//! # Defaults: max_signers ∈ {300, 500, 1000, 2000}, artefacts in ./params
//! ./primary_real_prover
//!
//! # Pick specific cases:
//! ./primary_real_prover 300 500
//!
//! # Direct artefacts elsewhere (e.g. fast scratch disk):
//! ARTIFACT_DIR=/scratch/primary_bench ./primary_real_prover 1000 2000
//!
//! # Override SRS cache dir (halo2-base convention used by `gen_srs`):
//! PARAMS_DIR=/scratch/srs ./primary_real_prover 300
//!
//! # Override log file location
//! # (default: $ARTIFACT_DIR/primary_real_prover.log):
//! LOG_FILE=/scratch/run1.log ./primary_real_prover 2000
//! ```
//!
//! All timings are printed to stdout AND appended live to the log file
//! (flushed per-line). The log lets you recover full output if the SSH
//! session drops while a long run is still computing on the host — just
//! `tail -f` (or `cat`) the log later.
//!
//! Recommended remote invocation that survives terminal disconnect:
//!
//! ```text
//! nohup ./primary_real_prover 2000 >/dev/null 2>&1 &
//! tail -f params/primary_real_prover.log
//! ```

use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use attestation_bls_checker_circuit::{
    primary_circuit::PrimaryAttestationBlsCheckerCircuit,
    test_instances::expected_public_instances,
    K, LOOKUP_BITS, NUM_UNUSABLE_ROWS,
};
use bridge_poseidon::{LIMB_BITS, NUM_LIMBS};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
};
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;
use halo2_base::utils::fs::gen_srs;

use gosh_zk_snark_halo2_utils::io::{
    read_vk_from_path, save_bytes, save_config_params, save_pk_to_path, save_vk_to_path,
    try_read_config_params,
};
use gosh_zk_snark_halo2_utils::proof::Proof;

/// Default cases when no CLI args are passed.
const DEFAULT_MAX_SIGNERS_CASES: &[usize] = &[300, 500, 1000, 2000];

// ---------------------------------------------------------------------------
// Live logging — mirrors every printed line to a file, flushing on each
// write so a dropped SSH session does not lose data.
// ---------------------------------------------------------------------------

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

fn init_log(path: &str) {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("Failed to open log file {}: {}", path, e));
    LOG_FILE
        .set(Mutex::new(file))
        .map_err(|_| "log already initialised")
        .expect("log already initialised");

    // Record a session header so consecutive runs in the same log are
    // visually separated.
    log_write(&format!(
        "\n=== primary_real_prover session start (pid {}) ===",
        std::process::id()
    ));
}

fn log_write(msg: &str) {
    // stdout (line-buffered when attached to a terminal; we still flush
    // explicitly so piped/redirected stdout is not stuck in libc buffers).
    println!("{}", msg);
    let _ = std::io::stdout().flush();
    // log file
    if let Some(lf) = LOG_FILE.get() {
        if let Ok(mut f) = lf.lock() {
            let _ = writeln!(f, "{}", msg);
            let _ = f.flush();
        }
    }
}

/// `println!`-style macro that mirrors output into the log file.
macro_rules! logln {
    () => { log_write("") };
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        log_write(&msg);
    }};
}

/// Install a panic hook that records the panic into the log file before
/// the default hook prints it to stderr — otherwise a panic during a long
/// remote run would only appear on the (already-disconnected) terminal.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("PANIC: {}", info);
        if let Some(lf) = LOG_FILE.get() {
            if let Ok(mut f) = lf.lock() {
                let _ = writeln!(f, "{}", msg);
                let _ = f.flush();
            }
        }
        default_hook(info);
    }));
}

// ---------------------------------------------------------------------------
// Circuit construction (mirrors tests/real_prover_primary.rs)
// ---------------------------------------------------------------------------

fn build_circuit_for_bk_set(
    bk_set_size: usize,
    max_signers: usize,
    shared_params: Option<&BaseCircuitParams>,
) -> (PrimaryAttestationBlsCheckerCircuit<Fr>, Vec<Fr>) {
    let test_data = bridge_test_data_gen::generator::generate_test_data_all_sign(bk_set_size)
        .expect("generate_test_data_all_sign failed");

    let (last_seen_block_seqno, instances) = expected_public_instances(
        &test_data.attestation_bytes,
        &test_data.bk_set,
        max_signers,
    );

    let mut circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
        test_data.attestation_bytes,
        test_data.bk_set,
        last_seen_block_seqno,
        K as usize,
        NUM_UNUSABLE_ROWS,
        LOOKUP_BITS,
        LIMB_BITS,
        NUM_LIMBS,
        max_signers,
    );

    if let Some(sp) = shared_params {
        circuit.override_base_circuit_params(sp.clone());
    }

    (circuit, instances)
}

fn keygen_and_cache(
    params: &ParamsKZG<Bn256>,
    bk_set_size: usize,
    max_signers: usize,
    vk_path: &str,
    pk_path: &str,
    config_path: &str,
) -> (VerifyingKey<G1Affine>, BaseCircuitParams) {
    logln!("  Cache miss — running keygen (bk_set_size = {})", bk_set_size);
    let t = Instant::now();
    let (ref_circuit, _) = build_circuit_for_bk_set(bk_set_size, max_signers, None);
    let base_params = ref_circuit.params.base_circuit_params.clone();
    logln!("  base_circuit_params: {:?}", base_params);
    logln!("[timing] reference circuit construction: {:?}", t.elapsed());

    let t = Instant::now();
    let vk = keygen_vk(params, &ref_circuit).expect("keygen_vk failed");
    logln!("[timing] keygen_vk: {:?}", t.elapsed());

    let t = Instant::now();
    let pk = keygen_pk(params, vk.clone(), &ref_circuit).expect("keygen_pk failed");
    logln!("[timing] keygen_pk: {:?}", t.elapsed());

    let t = Instant::now();
    save_vk_to_path(&vk, vk_path);
    save_pk_to_path(&pk, pk_path);
    save_config_params(&base_params, config_path);
    logln!("[timing] save VK + PK + config: {:?}", t.elapsed());

    (vk, base_params)
}

// ---------------------------------------------------------------------------
// One end-to-end run for a given (max_signers, bk_set_size = max_signers)
// ---------------------------------------------------------------------------

fn run_case(max_signers: usize, artifact_dir: &str) {
    let bk_set_size = max_signers;

    let vk_path = format!("{}/primary_max{}_vk.bin", artifact_dir, max_signers);
    let pk_path = format!("{}/primary_max{}_pk.bin", artifact_dir, max_signers);
    let config_path =
        format!("{}/primary_max{}_config_params.json", artifact_dir, max_signers);
    let proof_path =
        format!("{}/primary_max{}_proof_bk{}.bin", artifact_dir, max_signers, bk_set_size);
    let instances_path = format!(
        "{}/primary_max{}_instances_bk{}.bin",
        artifact_dir, max_signers, bk_set_size
    );

    let case_started = Instant::now();
    logln!("\n{}", "=".repeat(72));
    logln!(
        "Primary real-prover — max_signers = {}, bk_set_size = {}",
        max_signers, bk_set_size
    );
    logln!("{}", "=".repeat(72));

    // ── Step 1: SRS ────────────────────────────────────────────
    logln!("Step 1: SRS (K={})", K);
    let t = Instant::now();
    let params = gen_srs(K);
    logln!("[timing] SRS load/gen: {:?}", t.elapsed());

    // ── Step 2: VK + PK (cache or keygen) ──────────────────────
    logln!("\nStep 2: VK + PK");
    let cached_config = try_read_config_params(&config_path);
    let (vk, base_params) = if let Some(cfg) = cached_config {
        if Path::new(&vk_path).exists() && Path::new(&pk_path).exists() {
            let vk = read_vk_from_path(&vk_path, &cfg);
            logln!("  Loaded VK + config from cache (PK on disk for prove)");
            (vk, cfg)
        } else {
            keygen_and_cache(
                &params, bk_set_size, max_signers, &vk_path, &pk_path, &config_path,
            )
        }
    } else {
        keygen_and_cache(
            &params, bk_set_size, max_signers, &vk_path, &pk_path, &config_path,
        )
    };

    // ── Step 3: Prove + verify ─────────────────────────────────
    logln!("\nStep 3: prove + verify");
    let t = Instant::now();
    let (circuit, instances) =
        build_circuit_for_bk_set(bk_set_size, max_signers, Some(&base_params));
    logln!("[timing] circuit construction: {:?}", t.elapsed());

    let inst_refs: Vec<&[Fr]> = vec![instances.as_slice()];

    let t = Instant::now();
    let proof = Proof::create_for_circuit_from_paths::<PrimaryAttestationBlsCheckerCircuit<Fr>>(
        &params,
        &pk_path,
        &config_path,
        circuit,
        &inst_refs,
    );
    let proof_time = t.elapsed();
    logln!("[timing] proof generation: {:?}", proof_time);
    logln!("  proof size: {} bytes", proof.as_bytes().len());

    save_bytes(&proof_path, proof.as_bytes());

    let mut verify_times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let valid = proof.verify_with_vk(&vk, &params, &inst_refs);
        verify_times.push(t.elapsed());
        assert!(
            valid,
            "Proof verification failed for max_signers={} bk_set_size={}",
            max_signers, bk_set_size
        );
    }
    let avg_verify = verify_times.iter().sum::<std::time::Duration>() / 5;
    logln!("[timing] verification (avg of 5): {:?}", avg_verify);

    let instances_bytes: Vec<u8> = instances
        .iter()
        .flat_map(|f| f.to_repr().as_ref().to_vec())
        .collect();
    save_bytes(&instances_path, &instances_bytes);

    logln!(
        "[timing] case TOTAL (max_signers = {}): {:?}",
        max_signers,
        case_started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn parse_cases(args: &[String]) -> Vec<usize> {
    if args.is_empty() {
        return DEFAULT_MAX_SIGNERS_CASES.to_vec();
    }
    args.iter()
        .map(|s| {
            s.parse::<usize>()
                .unwrap_or_else(|e| panic!("Invalid max_signers '{}': {}", s, e))
        })
        .collect()
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cases = parse_cases(&args);

    let artifact_dir =
        env::var("ARTIFACT_DIR").unwrap_or_else(|_| "params".to_string());
    std::fs::create_dir_all(&artifact_dir)
        .unwrap_or_else(|e| panic!("Failed to create ARTIFACT_DIR {}: {}", artifact_dir, e));

    let log_path = env::var("LOG_FILE")
        .unwrap_or_else(|_| format!("{}/primary_real_prover.log", artifact_dir));
    init_log(&log_path);
    install_panic_hook();

    logln!("primary_real_prover scaling benchmark");
    logln!("  cases (max_signers): {:?}", cases);
    logln!("  artifact dir: {}", artifact_dir);
    logln!("  log file:     {}", log_path);
    logln!(
        "  SRS dir (PARAMS_DIR env): {}",
        env::var("PARAMS_DIR").unwrap_or_else(|_| "<default: ./params>".to_string())
    );

    let suite_started = Instant::now();
    for &max_signers in &cases {
        run_case(max_signers, &artifact_dir);
    }
    logln!("\n{}", "=".repeat(72));
    logln!("All cases finished in {:?}", suite_started.elapsed());
}
