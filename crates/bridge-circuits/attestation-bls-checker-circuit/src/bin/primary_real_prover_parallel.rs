//! Parallelism research binary for the Primary Attestation BLS Checker Circuit.
//!
//! Generates **N independent attestations** (one per worker) for a single
//! `max_signers` setting, then proves+verifies them concurrently. Reports
//! per-worker timings, total wall time, peak RSS, peak CPU% and a derived
//! per-proof memory estimate — so a small run on a modest host can be
//! extrapolated to a larger box.
//!
//! Why a separate binary instead of extending `primary_real_prover`:
//! the parallel run must load PK once and `Arc`-share it across worker
//! threads, otherwise N concurrent provers would each hold their own
//! multi-GB PK copy and the host would thrash on RAM, not CPU.
//!
//! ## Build
//!
//! ```text
//! cargo build --release -p attestation-bls-checker-circuit \
//!     --features bench-bin --bin primary_real_prover_parallel
//! ```
//!
//! ## Run
//!
//! ```text
//! # Defaults: max_signers=300, parallelism=2, no sequential baseline
//! ./primary_real_prover_parallel
//!
//! # With sequential baseline (doubles wall time but lets us compute speedup)
//! ./primary_real_prover_parallel --baseline
//!
//! # Cap rayon's global pool — useful for measuring "how does single-proof
//! # parallelism interact with N concurrent provers"
//! ./primary_real_prover_parallel --parallelism 4 --rayon-threads 2
//!
//! # Heavy host
//! ARTIFACT_DIR=/scratch ./primary_real_prover_parallel \
//!     --max-signers 1000 --parallelism 4 --baseline
//!
//! # Pin log file (default: $ARTIFACT_DIR/primary_real_prover_parallel_ms{}_p{}.log)
//! LOG_FILE=/scratch/par_ms300_p8.log \
//!     ./primary_real_prover_parallel --max-signers 300 --parallelism 8 --baseline
//! ```
//!
//! All `logln!` output goes to stdout AND to the log file (flushed per-line),
//! so the run survives an SSH disconnect — `tail -F` the log later. Default
//! filename embeds `max_signers` and `parallelism` so sweeps don't collide.

use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use attestation_bls_checker_circuit::{
    primary_circuit::PrimaryAttestationBlsCheckerCircuit,
    test_instances::expected_public_instances,
    K, LOOKUP_BITS, NUM_UNUSABLE_ROWS,
};
use bridge_poseidon::{LIMB_BITS, NUM_LIMBS};
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{create_proof, keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::{
        commitment::{KZGCommitmentScheme, ParamsKZG},
        multiopen::ProverSHPLONK,
    },
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use halo2_base::utils::fs::gen_srs;

use gosh_zk_snark_halo2_utils::io::{
    read_pk_from_path, read_vk_from_path, save_config_params, save_pk_to_path,
    save_vk_to_path, try_read_config_params,
};
use gosh_zk_snark_halo2_utils::proof::Proof;

use rand::rngs::OsRng;

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

    log_write(&format!(
        "\n=== primary_real_prover_parallel session start (pid {}) ===",
        std::process::id()
    ));
}

fn log_write(msg: &str) {
    println!("{}", msg);
    let _ = std::io::stdout().flush();
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
/// the default hook prints it to stderr.
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
// CLI
// ---------------------------------------------------------------------------

struct Args {
    max_signers: usize,
    parallelism: usize,
    baseline: bool,
    warmup: bool,
    rayon_threads: Option<usize>,
    artifact_dir: String,
}

fn print_help() {
    eprintln!(
        "Usage: primary_real_prover_parallel [OPTIONS]\n\
         \n\
         Options:\n\
           --max-signers <N>       Padding cap (default: 300)\n\
           --parallelism <N>       Concurrent workers (default: 2)\n\
           --baseline              Also run a sequential pass for comparison\n\
           --warmup                Run one discarded proof first (cache + JIT warm-up)\n\
           --rayon-threads <N>     Cap rayon's global pool (default: all cores)\n\
           --help                  Show this help\n\
         \n\
         Env vars:\n\
           ARTIFACT_DIR  Where to cache SRS/VK/PK/proofs (default: ./params)\n\
           PARAMS_DIR    halo2-base SRS cache dir (used by gen_srs)\n\
           LOG_FILE      Pin log file path (default: \
                         $ARTIFACT_DIR/primary_real_prover_parallel_ms{{ms}}_p{{p}}.log)\n"
    );
}

fn parse_args() -> Args {
    let mut a = Args {
        max_signers: 300,
        parallelism: 2,
        baseline: false,
        warmup: false,
        rayon_threads: None,
        artifact_dir: env::var("ARTIFACT_DIR").unwrap_or_else(|_| "params".to_string()),
    };
    let raw: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--max-signers" => {
                a.max_signers = raw
                    .get(i + 1)
                    .unwrap_or_else(|| {
                        print_help();
                        panic!("--max-signers requires a value")
                    })
                    .parse()
                    .expect("--max-signers <usize>");
                i += 2;
            }
            "--parallelism" => {
                a.parallelism = raw
                    .get(i + 1)
                    .unwrap_or_else(|| {
                        print_help();
                        panic!("--parallelism requires a value")
                    })
                    .parse()
                    .expect("--parallelism <usize>");
                i += 2;
            }
            "--baseline" => {
                a.baseline = true;
                i += 1;
            }
            "--warmup" => {
                a.warmup = true;
                i += 1;
            }
            "--rayon-threads" => {
                a.rayon_threads = Some(
                    raw.get(i + 1)
                        .unwrap_or_else(|| {
                            print_help();
                            panic!("--rayon-threads requires a value")
                        })
                        .parse()
                        .expect("--rayon-threads <usize>"),
                );
                i += 2;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                print_help();
                panic!("Unknown argument: {}", other);
            }
        }
    }
    assert!(a.parallelism >= 1, "--parallelism must be ≥ 1");
    a
}

// ---------------------------------------------------------------------------
// Resource sampler (cross-platform via sysinfo)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ResourceStats {
    /// Peak RSS observed in bytes.
    peak_rss: AtomicU64,
    /// Peak per-process CPU% (sum across cores, e.g. 800 = 8 fully loaded cores).
    peak_cpu_percent_x10: AtomicU64,
    /// Running average accumulator: sum and count for cpu%.
    cpu_sum_x10: AtomicU64,
    cpu_samples: AtomicU64,
}

impl ResourceStats {
    fn snapshot(&self) -> (u64, f64, f64) {
        let peak = self.peak_rss.load(Ordering::Relaxed);
        let peak_cpu = self.peak_cpu_percent_x10.load(Ordering::Relaxed) as f64 / 10.0;
        let count = self.cpu_samples.load(Ordering::Relaxed).max(1);
        let avg_cpu =
            (self.cpu_sum_x10.load(Ordering::Relaxed) as f64 / 10.0) / count as f64;
        (peak, peak_cpu, avg_cpu)
    }

    fn reset(&self) {
        self.peak_rss.store(0, Ordering::Relaxed);
        self.peak_cpu_percent_x10.store(0, Ordering::Relaxed);
        self.cpu_sum_x10.store(0, Ordering::Relaxed);
        self.cpu_samples.store(0, Ordering::Relaxed);
    }
}

fn spawn_sampler(
    stop: Arc<AtomicBool>,
    stats: Arc<ResourceStats>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        use sysinfo::{Pid, System};

        let pid = Pid::from(std::process::id() as usize);
        let mut sys = System::new();
        // First refresh seeds the CPU baseline; cpu_usage() returns 0.0 on the
        // very first call, so we burn one sample before recording.
        sys.refresh_processes();

        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(200));
            sys.refresh_processes();
            if let Some(p) = sys.process(pid) {
                let mem = p.memory(); // bytes in sysinfo 0.30+
                let cpu = p.cpu_usage(); // % per core; can exceed 100% if multi-threaded
                let cpu_x10 = (cpu * 10.0) as u64;
                stats.peak_rss.fetch_max(mem, Ordering::Relaxed);
                stats
                    .peak_cpu_percent_x10
                    .fetch_max(cpu_x10, Ordering::Relaxed);
                stats.cpu_sum_x10.fetch_add(cpu_x10, Ordering::Relaxed);
                stats.cpu_samples.fetch_add(1, Ordering::Relaxed);
            }
        }
    })
}

fn fmt_bytes(bytes: u64) -> String {
    let gb = bytes as f64 / 1024.0_f64.powi(3);
    if gb >= 1.0 {
        format!("{:.2} GB", gb)
    } else {
        let mb = bytes as f64 / 1024.0_f64.powi(2);
        format!("{:.0} MB", mb)
    }
}

// ---------------------------------------------------------------------------
// Circuit + proof helpers
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

/// Mirror of `gosh_zk_snark_halo2_utils::proof::Proof::generate_proof` but
/// taking an already-loaded `&ProvingKey`, so worker threads can share a
/// single PK via `Arc<ProvingKey>` instead of each one paying the
/// deserialization + memory cost.
fn prove_in_memory(
    params: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
    circuit: PrimaryAttestationBlsCheckerCircuit<Fr>,
    pub_inputs: &[&[Fr]],
) -> Vec<u8> {
    let instances: Vec<Vec<Fr>> = pub_inputs.iter().map(|s| s.to_vec()).collect();
    let instance_refs: Vec<&[Fr]> = instances.iter().map(|v| v.as_slice()).collect();
    let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
    create_proof::<
        KZGCommitmentScheme<Bn256>,
        ProverSHPLONK<'_, Bn256>,
        Challenge255<G1Affine>,
        _,
        Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
        _,
    >(
        params,
        pk,
        &[circuit],
        &[&instance_refs],
        OsRng,
        &mut transcript,
    )
    .expect("create_proof failed");
    transcript.finalize()
}

// ---------------------------------------------------------------------------
// Setup: keygen or load (cached per max_signers)
// ---------------------------------------------------------------------------

fn keygen_and_cache(
    params: &ParamsKZG<Bn256>,
    max_signers: usize,
    vk_path: &str,
    pk_path: &str,
    config_path: &str,
) -> (VerifyingKey<G1Affine>, ProvingKey<G1Affine>, BaseCircuitParams) {
    logln!("  Cache miss — running keygen (bk_set_size = {})", max_signers);
    let t = Instant::now();
    let (ref_circuit, _) = build_circuit_for_bk_set(max_signers, max_signers, None);
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

    (vk, pk, base_params)
}

fn load_or_keygen(
    params: &ParamsKZG<Bn256>,
    max_signers: usize,
    artifact_dir: &str,
) -> (VerifyingKey<G1Affine>, ProvingKey<G1Affine>, BaseCircuitParams) {
    let vk_path = format!("{}/primary_max{}_vk.bin", artifact_dir, max_signers);
    let pk_path = format!("{}/primary_max{}_pk.bin", artifact_dir, max_signers);
    let config_path =
        format!("{}/primary_max{}_config_params.json", artifact_dir, max_signers);

    let cached = try_read_config_params(&config_path);
    if let Some(cfg) = cached {
        if Path::new(&vk_path).exists() && Path::new(&pk_path).exists() {
            logln!("  Loading VK + PK from cache");
            let t = Instant::now();
            let vk = read_vk_from_path(&vk_path, &cfg);
            let pk = read_pk_from_path(&pk_path, &cfg);
            logln!("[timing] load VK + PK: {:?}", t.elapsed());
            return (vk, pk, cfg);
        }
    }
    keygen_and_cache(params, max_signers, &vk_path, &pk_path, &config_path)
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

/// One end-to-end "case": build circuit + generate proof + verify×5.
/// Returns (construction_time, proof_time, avg_verify_time, proof_bytes).
fn prove_and_verify_one(
    params: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
    vk: &VerifyingKey<G1Affine>,
    base_params: &BaseCircuitParams,
    max_signers: usize,
) -> (Duration, Duration, Duration, usize) {
    let bk_set_size = max_signers;

    let t = Instant::now();
    let (circuit, instances) =
        build_circuit_for_bk_set(bk_set_size, max_signers, Some(base_params));
    let construction = t.elapsed();

    let inst_refs: Vec<&[Fr]> = vec![instances.as_slice()];

    let t = Instant::now();
    let proof_bytes = prove_in_memory(params, pk, circuit, &inst_refs);
    let proof_time = t.elapsed();

    let proof = Proof::new(proof_bytes);
    let mut verify_times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let valid = proof.verify_with_vk(vk, params, &inst_refs);
        verify_times.push(t.elapsed());
        assert!(valid, "Proof verification failed");
    }
    let avg_verify = verify_times.iter().sum::<Duration>() / 5;

    (construction, proof_time, avg_verify, proof.as_bytes().len())
}

struct PhaseSummary {
    label: &'static str,
    per_worker: Vec<(Duration, Duration, Duration, usize)>, // (constr, prove, verify, proof_size)
    total: Duration,
    peak_rss: u64,
    peak_cpu_pct: f64,
    avg_cpu_pct: f64,
}

fn run_sequential(
    params: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
    vk: &VerifyingKey<G1Affine>,
    base_params: &BaseCircuitParams,
    max_signers: usize,
    n: usize,
    stats: &Arc<ResourceStats>,
) -> PhaseSummary {
    stats.reset();
    let started = Instant::now();
    let mut per_worker = Vec::with_capacity(n);
    for i in 0..n {
        let t = Instant::now();
        let (c, p, v, sz) =
            prove_and_verify_one(params, pk, vk, base_params, max_signers);
        logln!(
            "  [seq {}/{}] construct={:?} prove={:?} verify={:?} proof={}B (wall {:?})",
            i + 1,
            n,
            c,
            p,
            v,
            sz,
            t.elapsed()
        );
        per_worker.push((c, p, v, sz));
    }
    let total = started.elapsed();
    let (peak_rss, peak_cpu, avg_cpu) = stats.snapshot();
    PhaseSummary {
        label: "sequential",
        per_worker,
        total,
        peak_rss,
        peak_cpu_pct: peak_cpu,
        avg_cpu_pct: avg_cpu,
    }
}

fn run_parallel(
    params: &Arc<ParamsKZG<Bn256>>,
    pk: &Arc<ProvingKey<G1Affine>>,
    vk: &Arc<VerifyingKey<G1Affine>>,
    base_params: &BaseCircuitParams,
    max_signers: usize,
    n: usize,
    stats: &Arc<ResourceStats>,
) -> PhaseSummary {
    stats.reset();
    let started = Instant::now();
    let per_worker: Vec<_> = thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let params = Arc::clone(params);
                let pk = Arc::clone(pk);
                let vk = Arc::clone(vk);
                let base_params = base_params.clone();
                s.spawn(move || {
                    let t = Instant::now();
                    let (c, p, v, sz) = prove_and_verify_one(
                        &params,
                        &pk,
                        &vk,
                        &base_params,
                        max_signers,
                    );
                    let wall = t.elapsed();
                    logln!(
                        "  [par {}/{}] construct={:?} prove={:?} verify={:?} proof={}B (wall {:?})",
                        i + 1,
                        n,
                        c,
                        p,
                        v,
                        sz,
                        wall
                    );
                    (c, p, v, sz)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker panicked"))
            .collect()
    });
    let total = started.elapsed();
    let (peak_rss, peak_cpu, avg_cpu) = stats.snapshot();
    PhaseSummary {
        label: "parallel",
        per_worker,
        total,
        peak_rss,
        peak_cpu_pct: peak_cpu,
        avg_cpu_pct: avg_cpu,
    }
}

fn report_phase(s: &PhaseSummary) {
    logln!("\n--- {} phase ---", s.label);
    let max_prove = s
        .per_worker
        .iter()
        .map(|(_, p, _, _)| *p)
        .max()
        .unwrap_or_default();
    let avg_prove = if s.per_worker.is_empty() {
        Duration::ZERO
    } else {
        s.per_worker.iter().map(|(_, p, _, _)| *p).sum::<Duration>()
            / s.per_worker.len() as u32
    };
    logln!("workers:               {}", s.per_worker.len());
    logln!("total wall:            {:?}", s.total);
    logln!("per-worker prove avg:  {:?}", avg_prove);
    logln!("per-worker prove max:  {:?}", max_prove);
    logln!("peak RSS:              {}", fmt_bytes(s.peak_rss));
    logln!(
        "peak CPU%:             {:.0}% (≈ {:.1} cores)",
        s.peak_cpu_pct,
        s.peak_cpu_pct / 100.0
    );
    logln!(
        "avg CPU%:              {:.0}% (≈ {:.1} cores)",
        s.avg_cpu_pct,
        s.avg_cpu_pct / 100.0
    );
}

// ---------------------------------------------------------------------------
// Extrapolation
// ---------------------------------------------------------------------------

fn print_extrapolation(
    seq: Option<&PhaseSummary>,
    par: &PhaseSummary,
    baseline_rss: u64,
    max_signers: usize,
    parallelism: usize,
    detected_cores: usize,
) {
    logln!("\n=== Extrapolation ===");
    logln!(
        "Idle baseline RSS (after keygen + PK in memory): {}",
        fmt_bytes(baseline_rss)
    );

    let par_extra = par.peak_rss.saturating_sub(baseline_rss);
    let per_proof_extra = par_extra / parallelism as u64;
    logln!(
        "Parallel-phase peak RSS extra (above baseline): {} ({} per concurrent proof)",
        fmt_bytes(par_extra),
        fmt_bytes(per_proof_extra)
    );

    if let Some(seq) = seq {
        let seq_extra = seq.peak_rss.saturating_sub(baseline_rss);
        logln!(
            "Sequential-phase per-proof extra: {} (single proof in flight)",
            fmt_bytes(seq_extra)
        );
        // Memory cost-amplification factor from sequential to parallel:
        let amp = if seq_extra > 0 {
            par_extra as f64 / seq_extra as f64
        } else {
            0.0
        };
        logln!(
            "Cost-amplification (par_extra / seq_extra): {:.2}× for {}× concurrency",
            amp, parallelism
        );

        let speedup = seq.total.as_secs_f64() / par.total.as_secs_f64();
        let eff = speedup / parallelism as f64;
        logln!(
            "Throughput speedup: {:.2}× (efficiency {:.2})",
            speedup, eff
        );
        logln!(
            "Per-proof cores used (sequential phase): ≈ {:.1}",
            seq.avg_cpu_pct / 100.0
        );
    } else {
        logln!(
            "  (run with --baseline to also report sequential timing and \
             per-proof RAM delta)"
        );
    }

    logln!(
        "\nPer-proof core saturation (parallel phase): avg {:.1} / peak {:.1} cores",
        par.avg_cpu_pct / 100.0,
        par.peak_cpu_pct / 100.0,
    );
    logln!(
        "Detected logical cores: {} — if parallel avg-CPU > {} cores, \
         this host's rayon pool is the bottleneck.",
        detected_cores, detected_cores
    );

    // Hard "fits-in-RAM" projection. RSS-overhead per concurrent proof is
    // taken from the parallel phase (more conservative since rayon scratch
    // peaks higher under contention).
    logln!(
        "\nProjected max concurrent proofs at max_signers = {} on other hosts \
         (RAM-bound; CPU may still cap throughput sooner):",
        max_signers
    );
    let cores_per_proof = (par.avg_cpu_pct / 100.0) / parallelism as f64;
    logln!(
        "  RAM    | fits-N (RAM-bound)   (CPU-side: target_cores / {:.1} per proof)",
        cores_per_proof.max(0.1)
    );
    for ram_gb in &[16u64, 32, 64, 128, 256] {
        let avail = ram_gb * 1024 * 1024 * 1024;
        let avail_after_base = avail.saturating_sub(baseline_rss);
        let n_ram = if per_proof_extra == 0 {
            "?".to_string()
        } else {
            (avail_after_base / per_proof_extra).to_string()
        };
        logln!("  {:>4} GB | {:>4}", ram_gb, n_ram);
    }
    logln!(
        "\nThe true sustainable N on another host is min(fits-N, target_cores / cores_per_proof).\n\
         If `cores_per_proof` is close to the number of logical cores, the host is\n\
         already CPU-saturated by a single proof — extra concurrency won't help.",
    );
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();

    // Apply rayon thread cap *before* anything that might touch the global pool.
    if let Some(t) = args.rayon_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .expect("rayon::ThreadPoolBuilder::build_global");
    }

    let detected_cores = rayon::current_num_threads();

    std::fs::create_dir_all(&args.artifact_dir)
        .unwrap_or_else(|e| panic!("Failed to create ARTIFACT_DIR: {}", e));

    // ── Open log file (default name embeds ms + parallelism so sweeps don't collide)
    let log_path = env::var("LOG_FILE").unwrap_or_else(|_| {
        format!(
            "{}/primary_real_prover_parallel_ms{}_p{}.log",
            args.artifact_dir, args.max_signers, args.parallelism
        )
    });
    init_log(&log_path);
    install_panic_hook();

    if args.rayon_threads.is_some() {
        logln!(
            "rayon global pool: capped to {} threads",
            args.rayon_threads.unwrap()
        );
    }

    logln!("primary_real_prover_parallel");
    logln!("  max_signers:   {}", args.max_signers);
    logln!("  parallelism:   {}", args.parallelism);
    logln!("  baseline:      {}", args.baseline);
    logln!("  warmup:        {}", args.warmup);
    logln!(
        "  rayon threads: {} ({})",
        detected_cores,
        if args.rayon_threads.is_some() {
            "capped"
        } else {
            "auto"
        }
    );
    logln!("  artifact dir:  {}", args.artifact_dir);
    logln!("  log file:      {}", log_path);
    logln!(
        "  SRS dir:       {}",
        env::var("PARAMS_DIR").unwrap_or_else(|_| "<default>".to_string())
    );

    // ── SRS ──────────────────────────────────────────────────────
    logln!("\nStep 1: SRS (K={})", K);
    let t = Instant::now();
    let params = Arc::new(gen_srs(K));
    logln!("[timing] SRS load/gen: {:?}", t.elapsed());

    // ── VK + PK (load or keygen) ─────────────────────────────────
    logln!("\nStep 2: VK + PK");
    let t = Instant::now();
    let (vk, pk, base_params) = load_or_keygen(&params, args.max_signers, &args.artifact_dir);
    logln!("[timing] VK + PK total: {:?}", t.elapsed());
    let vk = Arc::new(vk);
    let pk = Arc::new(pk);
    logln!("  base_circuit_params: {:?}", base_params);

    // ── Sampler ──────────────────────────────────────────────────
    let stats = Arc::new(ResourceStats::default());
    let stop_flag = Arc::new(AtomicBool::new(false));
    let sampler = spawn_sampler(Arc::clone(&stop_flag), Arc::clone(&stats));

    // Snapshot RSS *after* PK is fully loaded — this is our "idle baseline".
    // Let the sampler tick once.
    thread::sleep(Duration::from_millis(400));
    let baseline_rss = stats.peak_rss.load(Ordering::Relaxed);
    logln!(
        "  RSS baseline (post-keygen / PK loaded): {}",
        fmt_bytes(baseline_rss)
    );

    // ── Optional warmup ──────────────────────────────────────────
    if args.warmup {
        logln!("\nWarmup: one discarded proof");
        let t = Instant::now();
        let _ = prove_and_verify_one(&params, &pk, &vk, &base_params, args.max_signers);
        logln!("[timing] warmup: {:?}", t.elapsed());
    }

    // ── Sequential baseline (optional) ───────────────────────────
    let seq_summary = if args.baseline {
        logln!(
            "\nStep 3: sequential baseline ({} proofs back-to-back)",
            args.parallelism
        );
        Some(run_sequential(
            &params,
            &pk,
            &vk,
            &base_params,
            args.max_signers,
            args.parallelism,
            &stats,
        ))
    } else {
        None
    };

    // ── Parallel phase ───────────────────────────────────────────
    logln!(
        "\nStep 4: parallel ({} concurrent workers)",
        args.parallelism
    );
    let par_summary = run_parallel(
        &params,
        &pk,
        &vk,
        &base_params,
        args.max_signers,
        args.parallelism,
        &stats,
    );

    // ── Reports ──────────────────────────────────────────────────
    if let Some(s) = &seq_summary {
        report_phase(s);
    }
    report_phase(&par_summary);

    // ── Stop sampler ─────────────────────────────────────────────
    stop_flag.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler thread panic");

    // ── Extrapolation summary ────────────────────────────────────
    print_extrapolation(
        seq_summary.as_ref(),
        &par_summary,
        baseline_rss,
        args.max_signers,
        args.parallelism,
        detected_cores,
    );

}
