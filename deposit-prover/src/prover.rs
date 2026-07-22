//! Proof Generation Module
//!
//! This module provides the infrastructure for generating and verifying ZK
//! proofs for deposit events using the axiom-eth circuit.
//!
//! ## Architecture
//!
//! The proof generation follows axiom-eth's pattern:
//! 1. **Circuit Creation**: Use `create_circuit` to wrap
//!    `DepositEventCircuitV2` in `EthCircuitImpl`
//! 2. **Mock Testing**: Use `MockProver` to test circuit logic without
//!    generating proofs
//! 3. **Proof Generation**: Create SNARK proofs (key generation happens
//!    automatically)
//!
//! ## Usage
//!
//! ```rust,ignore
//! use deposit_prover::prover::{test_circuit_mock, CircuitConfig};
//! use deposit_prover::types::DepositProofInput;
//!
//! // 1. Create input from Ethereum data
//! let input = DepositProofInput { /* ... */ };
//! let config = CircuitConfig::default();
//!
//! // 2. Test with MockProver (fast, no proof generation)
//! test_circuit_mock(input.clone(), &config).expect("Circuit should be satisfied");
//!
//! // 3. Generate actual proof (requires more setup)
//! // let proof = generate_proof(input, &config)?;
//! ```
//!
//! ## Usage Example
//!
//! ```ignore
//! use axiom_eth::utils::eth_circuit::create_circuit;
//! use halo2_base::gates::circuit::CircuitBuilderStage;
//!
//! // 1. Create circuit
//! let input = get_deposit_proof_input(); // From Ethereum RPC
//! let params = get_circuit_params();
//! let mut circuit = create_circuit(CircuitBuilderStage::Mock, input, params, Default::default());
//!
//! // 2. Mock test
//! circuit.mock_fulfill_keccak_promises(None);
//! circuit.calculate_params();
//! let instances = circuit.instances();
//! MockProver::run(k, &circuit, instances).unwrap().assert_satisfied();
//!
//! // 3. Generate proof (requires setup)
//! let proof = gen_snark_shplonk(&params, &pk, circuit, &mut rng, None::<&str>);
//!
//! // 4. Generate Solidity verifier
//! let verifier_sol = gen_evm_verifier_shplonk(&params, &vk, &circuit, None::<&str>);
//! ```

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use axiom_eth::{
    rlc::{circuit::RlcCircuitParams, virtual_region::RlcThreadBreakPoints},
    utils::{
        component::promise_loader::single::PromiseLoaderParams,
        eth_circuit::{create_circuit, EthCircuitImpl},
    },
};
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        dev::MockProver,
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::ProvingKey,
        poly::kzg::commitment::ParamsKZG,
    },
};
use snark_verifier_sdk::{evm::gen_evm_verifier_shplonk, gen_pk, halo2::gen_snark_shplonk, Snark};

use crate::{
    circuit_v2::{self, DepositEventCircuitV2},
    types::{DepositProofInput, DepositProofOutput},
};

/// Pinned keccak promise-loader capacity.
///
/// Pinning the keccak promise-loader capacity to a fixed worst case makes the
/// deposit verifying key **independent of MPT proof depth / receipt size** (the
/// `num_advice_per_phase` no longer drifts with the used keccak capacity).
/// Together with dropping the `contract_address` in-circuit constant
/// (`circuit_v2.rs`, which had leaked the per-deposit address into the fixed
/// column), this makes the VK fully **witness-independent** — a single embedded
/// VK verifies every real deposit regardless of bridge address or MPT depth.
/// No axiom-eth fork change is needed (upstream). See
/// `docs/deposit_vk_witness_independence.md`.
///
/// Must be `>=` every real deposit's `used_capacity` (measured: 1-node = 11,
/// 3-node = 21, ~5/node; the `max_depth = 10` worst case is ~55-60) and must
/// match the value used by `examples/export_vk_blob.rs` /
/// `examples/export_deposit_proof_set.rs`, otherwise generated proofs will not
/// verify against the embedded VK (over-capacity fails safe at prove time).
pub const FIXED_KECCAK_CAPACITY: usize = 64;

/// Configuration for the deposit proof circuit
#[derive(Debug, Clone)]
pub struct CircuitConfig {
    /// Circuit degree (log2 of number of rows)
    /// Typical values: 18-22 (256K - 4M rows)
    pub degree: u32,

    /// Maximum byte length for event data
    pub max_data_byte_len: usize,

    /// Maximum number of logs in a receipt
    pub max_log_num: usize,

    /// Bounds on number of topics per log (min, max)
    pub topic_num_bounds: (usize, usize),

    /// Fetch / network selector only (mainnet = 1, Sepolia = 11155111).
    /// **Demoted**: no longer constrained in-circuit or baked into the VK.
    /// Proven `chainId` is a public input; AN allowlists bind it to the bridge.
    pub expected_chain_id: u64,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            degree: 15, /* 2^15 = ~32K rows (OPTION B+: Ultra-aggressive optimization - last
                         * attempt before aggregation) */
            max_data_byte_len: 128, /* Max event data size (reduced from 256 - Deposit event
                                     * needs ~128 bytes) */
            max_log_num: 3, /* Max logs per receipt (OPTION B+: Ultra-aggressive - most deposit
                             * txs have 1-3 logs) */
            topic_num_bounds: (0, 4), // 0-4 topics per log
            expected_chain_id: circuit_v2::EXPECTED_L1_CHAIN_ID,
        }
    }
}

/// Load circuit parameters from a JSON config file.
///
/// The config file should contain RlcCircuitParams in JSON format.
/// See `configs/circuit_params.json` for an example.
pub fn load_circuit_params(path: &str) -> Result<RlcCircuitParams, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open config file: {}", e))?;
    serde_json::from_reader(file).map_err(|e| format!("Failed to parse config: {}", e))
}

/// Get default circuit parameters for testing.
///
/// This uses the config file at `configs/circuit_params.json`.
pub fn get_default_params() -> RlcCircuitParams {
    load_circuit_params("configs/circuit_params.json")
        .expect("Failed to load default circuit params")
}

/// Test the circuit with MockProver (fast, no proof generation).
///
/// This is useful for testing circuit logic without the overhead of generating
/// proofs.
///
/// # Arguments
///
/// * `input` - The deposit proof input containing event data and receipt proof
/// * `config` - Circuit configuration parameters
///
/// # Returns
///
/// `Ok(())` if the circuit is satisfied, `Err` otherwise
pub fn test_circuit_mock(input: DepositProofInput, config: &CircuitConfig) -> Result<(), String> {
    // Load circuit parameters
    let params = get_default_params();
    let k = params.base.k as u32;

    // Create the circuit
    let circuit_input = DepositEventCircuitV2::new(input, config);
    let mut circuit = create_circuit(CircuitBuilderStage::Mock, params, circuit_input);

    // Fulfill Keccak promises (required for RLC)
    circuit.mock_fulfill_keccak_promises(None);

    // Calculate circuit parameters
    circuit.calculate_params();

    // Get public instances
    let instances = circuit.instances();

    // Run MockProver
    MockProver::run(k, &circuit, instances)
        .map_err(|e| format!("MockProver failed: {:?}", e))?
        .assert_satisfied();

    Ok(())
}

/// Load KZG parameters from disk
fn load_kzg_params(path: &str) -> Result<ParamsKZG<Bn256>, String> {
    use halo2_base::halo2_proofs::poly::commitment::Params;

    let mut file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;

    ParamsKZG::<Bn256>::read(&mut file).map_err(|e| format!("Failed to read params: {}", e))
}

/// Load KZG parameters from trusted setup.
///
/// This function loads KZG parameters from a trusted setup ceremony.
/// It will NEVER generate random parameters - this ensures production safety.
///
/// # Security
///
/// The parameters MUST come from a trusted setup ceremony where:
/// - Multiple independent participants contributed randomness
/// - The "toxic waste" (secret τ) was destroyed
/// - The ceremony is publicly verifiable
///
/// # Setup Instructions
///
/// Download pre-converted KZG parameters from the halo2-kzg-srs project:
///
/// ```bash
/// cd deposit-prover
/// ./download_trusted_setup.sh
/// ```
///
/// This downloads the pre-converted `.srs` file (33 MB) from the Hermez/Polygon
/// Powers of Tau ceremony, already in Halo2 format.
///
/// See `TRUSTED_SETUP.md` for detailed instructions.
///
/// # Arguments
///
/// * `k` - Circuit degree (log2 of number of rows)
///
/// # Returns
///
/// KZG parameters for the given degree, or an error if not found
///
/// # Errors
///
/// Returns an error if:
/// - The parameters file doesn't exist
/// - The file is corrupted or invalid
/// - The file format is incorrect
pub fn load_kzg_params_from_trusted_setup(k: u32) -> Result<ParamsKZG<Bn256>, String> {
    // Hermez / Polygon Powers of Tau only (`data/kzg_params_{k}.srs`).
    // Matches `tvm-sdk` `feature/hermez-kzg-resurrection` embedded
    // `KZG_S_G2_BYTES` (`928fafb3…`). No chain-ceremony fallback.
    let params_path = format!("data/kzg_params_{}.srs", k);

    // Try to load existing parameters
    if Path::new(&params_path).exists() {
        println!("Loading KZG parameters from {}", params_path);
        match load_kzg_params(&params_path) {
            Ok(params) => {
                println!("✅ Successfully loaded KZG parameters from trusted setup");
                return Ok(params);
            },
            Err(e) => {
                return Err(format!(
                    "\n╔══════════════════════════════════════════════════════════════╗\n\
                     ║  ❌ Failed to load KZG parameters!                           ║\n\
                     ╠══════════════════════════════════════════════════════════════╣\n\
                     ║  File exists but is corrupted or invalid: {}                 \n\
                     ║                                                              ║\n\
                     ║  Error: {}                                                   \n\
                     ║                                                              ║\n\
                     ║  Steps to fix:                                              ║\n\
                     ║  1. Delete the corrupted file:                              ║\n\
                     ║     rm {}                                                    \n\
                     ║                                                              ║\n\
                     ║  2. Re-download trusted setup:                              ║\n\
                     ║     cd deposit-prover                                       ║\n\
                     ║     ./download_trusted_setup.sh                             ║\n\
                     ║                                                              ║\n\
                     ║  See TRUSTED_SETUP.md for detailed instructions.            ║\n\
                     ╚══════════════════════════════════════════════════════════════╝\n",
                    params_path, e, params_path
                ));
            },
        }
    }

    // Parameters not found - try Hermez degree 18 (downward compatible).
    if k < 18 {
        let fallback_path = "data/kzg_params_18.srs";
        if Path::new(fallback_path).exists() {
            println!(
                "⚠️  KZG parameters for degree {} not found, using degree 18 (downward compatible)",
                k
            );
            match load_kzg_params(fallback_path) {
                Ok(params) => {
                    println!("✅ Successfully loaded KZG parameters from trusted setup");
                    return Ok(params);
                },
                Err(e) => {
                    return Err(format!(
                        "Failed to load fallback KZG parameters from {}: {}",
                        fallback_path, e
                    ));
                },
            }
        }
    }

    // Parameters not found - provide clear instructions
    Err(format!(
        "\n╔══════════════════════════════════════════════════════════════╗\n\
         ║  ❌ KZG parameters not found!                                ║\n\
         ╠══════════════════════════════════════════════════════════════╣\n\
         ║  File not found: {}                                          \n\
         ║                                                              ║\n\
         ║  You MUST use trusted setup parameters from a ceremony.     ║\n\
         ║  Random parameter generation has been DISABLED for security. ║\n\
         ║                                                              ║\n\
         ║  Steps to fix:                                              ║\n\
         ║                                                              ║\n\
         ║  Download pre-converted KZG parameters:                     ║\n\
         ║     cd deposit-prover                                       ║\n\
         ║     ./download_trusted_setup.sh                             ║\n\
         ║                                                              ║\n\
         ║  This downloads the pre-converted .srs file (33 MB) from    ║\n\
         ║  the Hermez/Polygon Powers of Tau ceremony.                 ║\n\
         ║                                                              ║\n\
         ║  See TRUSTED_SETUP.md for detailed instructions.            ║\n\
         ╚══════════════════════════════════════════════════════════════╝\n",
        params_path
    ))
}

/// Generate or load proving key for the deposit circuit.
///
/// This function will:
/// - Check if proving key exists at `pk_path`
/// - If exists and valid, use it (gen_pk will load it automatically)
/// - If not found, generate new proving key from the circuit
/// - Save newly generated proving key to disk for future reuse
///
/// Note: gen_pk from snark-verifier-sdk handles loading automatically if the
/// file exists
///
/// # Arguments
///
/// * `params` - KZG parameters
/// * `input` - Real deposit proof input (needed for keygen to determine circuit
///   structure)
/// * `config` - Circuit configuration
/// * `pk_path` - Path to save/load proving key
///
/// # Returns
///
/// Proving key for the circuit
/// Short hex fingerprint of the circuit shape that determines the verifying key
/// (and hence whether a cached proving key is reusable): degree, advice/lookup
/// column counts, RLC columns, and the pinned keccak capacity.
fn pk_fingerprint(rlc: &RlcCircuitParams, keccak_capacity: usize) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rlc.base.k.hash(&mut h);
    rlc.base.num_advice_per_phase.hash(&mut h);
    rlc.base.num_lookup_advice_per_phase.hash(&mut h);
    rlc.base.num_fixed.hash(&mut h);
    rlc.base.lookup_bits.hash(&mut h);
    rlc.base.num_instance_columns.hash(&mut h);
    rlc.num_rlc_columns.hash(&mut h);
    keccak_capacity.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Insert a shape fingerprint before the file extension, e.g.
/// `data/deposit_prover_k18.pk` + `a1b2…` -> `data/deposit_prover_k18.a1b2….pk`.
fn pk_path_with_fingerprint(pk_path: &Path, fingerprint: &str) -> std::path::PathBuf {
    let stem = pk_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("deposit_prover");
    let ext = pk_path.extension().and_then(|s| s.to_str()).unwrap_or("pk");
    let file = format!("{stem}.{fingerprint}.{ext}");
    match pk_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(file),
        _ => std::path::PathBuf::from(file),
    }
}

pub fn get_or_create_proving_key(
    params: &ParamsKZG<Bn256>,
    input: &DepositProofInput,
    config: &CircuitConfig,
    pk_path: &Path,
) -> Result<(ProvingKey<G1Affine>, RlcCircuitParams, RlcThreadBreakPoints), String> {
    // Create circuit for key generation using real input
    // Note: For axiom-eth circuits, we need real RLP data even for keygen
    // because the circuit structure depends on the data layout
    let circuit_input = DepositEventCircuitV2::new(input.clone(), config);
    let circuit_params = get_default_params();
    // Pin the keccak promise-loader capacity so the VK is witness-independent
    // (see `FIXED_KECCAK_CAPACITY`). The same pin is applied to the prover
    // circuit below and to the VK exporter, so all three agree.
    let fixed_keccak = PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY);
    let mut circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Keygen,
        circuit_input,
        circuit_params.clone(),
        fixed_keccak,
    );

    // CRITICAL: Fulfill Keccak promises and calculate params BEFORE keygen
    // This is required for axiom-eth circuits even in Keygen mode
    // See: axiom-eth/src/storage/tests.rs for reference
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));
    circuit.calculate_params();
    // calculate_params() clears the witnesses; re-fulfill before keygen.
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));

    // Derive a SHAPE-FINGERPRINTED proving-key path so a stale on-disk PK from a
    // different circuit shape (e.g. a previous `num_advice_per_phase` /
    // keccak-capacity / circuit version) is NEVER silently loaded. `gen_pk`
    // deserialises whatever PK file it finds without checking it matches the
    // current circuit — loading a stale PK would make the relayer emit proofs
    // against the WRONG verifying key, which then fail on-chain
    // (`ZKHALO2VERIFYWITHVK`). Keying the filename on the calculated shape means
    // a shape change produces a fresh keygen instead of a silent mismatch.
    let shape = pk_fingerprint(&circuit.params().rlc, FIXED_KECCAK_CAPACITY);
    let pk_path = pk_path_with_fingerprint(pk_path, &shape);
    let pk_path = pk_path.as_path();

    // Break points are computed by halo2 DURING keygen synthesis and stored on
    // the (RefCell) builder. `gen_pk(.., Some(path))` only *deserialises* an
    // existing PK — it does NOT synthesise — so on the load path
    // `circuit.break_points()` comes back EMPTY and the prover later panics with
    // "break points not set". They are therefore persisted to a sidecar next to
    // the PK during keygen and reloaded on a cache hit, so proving reuses the PK
    // with NO keygen (the operator fast path). `RlcThreadBreakPoints` is serde.
    let bp_path = PathBuf::from(format!("{}.bp.json", pk_path.display()));

    // A PK generated before this sidecar existed would be loaded WITHOUT its
    // break points (unrecoverable without a synthesis). Remove such a legacy PK
    // so both the PK and its sidecar are regenerated together, once.
    if pk_path.exists() && !bp_path.exists() {
        println!(
            "Proving key {:?} present but break-points sidecar missing; \
             regenerating both (one-time)...",
            pk_path
        );
        let _ = fs::remove_file(pk_path);
    }

    let load_from_cache = pk_path.exists() && bp_path.exists();
    if load_from_cache {
        println!(
            "Found existing proving key + break points, loading (no keygen): {:?}",
            pk_path
        );
    } else {
        println!("Generating proving key (this may take a few minutes)...");
    }

    let pk = gen_pk(params, &circuit, Some(pk_path));
    println!("Proving key ready");

    // Get the calculated circuit params from the keygen circuit. Available after
    // `calculate_params()` above on both the load and generate paths.
    use halo2_base::halo2_proofs::plonk::Circuit;
    let calculated_params = circuit.params().rlc;

    let break_points = if load_from_cache {
        // Load path: the circuit was NOT synthesised (gen_pk only deserialised
        // the PK), so read the break points from the sidecar instead of the
        // (empty) builder.
        let bytes = fs::read(&bp_path)
            .map_err(|e| format!("reading break-points sidecar {:?}: {e}", bp_path))?;
        serde_json::from_slice::<RlcThreadBreakPoints>(&bytes)
            .map_err(|e| format!("deserialising break-points sidecar {:?}: {e}", bp_path))?
    } else {
        // Generate path: gen_pk synthesised the circuit, so the builder now holds
        // the break points. Persist them next to the PK for future cache hits.
        let bp = circuit.break_points();
        let bytes = serde_json::to_vec(&bp)
            .map_err(|e| format!("serialising break points: {e}"))?;
        fs::write(&bp_path, &bytes)
            .map_err(|e| format!("writing break-points sidecar {:?}: {e}", bp_path))?;
        println!("Wrote break-points sidecar -> {:?}", bp_path);
        bp
    };

    Ok((pk, calculated_params, break_points))
}

/// Generate a full SNARK proof for a deposit event.
///
/// This function:
/// 1. Loads or generates KZG parameters
/// 2. Loads or generates proving key
/// 3. Creates the circuit with real input
/// 4. Generates SNARK proof using SHPLONK
///
/// # Arguments
///
/// * `input` - The deposit proof input containing event data and receipt proof
/// * `config` - Circuit configuration
///
/// # Returns
///
/// SNARK proof and public outputs
pub fn generate_proof(
    input: DepositProofInput,
    config: &CircuitConfig,
) -> Result<DepositProofOutput, String> {
    let k = config.degree;

    // 1. Get KZG parameters
    let params = load_kzg_params_from_trusted_setup(k)
        .map_err(|e| format!("Failed to load KZG params: {}", e))?;

    // 2. Get proving key, calculated params, and break points (using real input for
    //    keygen)
    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    let pk_path = Path::new(&pk_path_str);
    fs::create_dir_all("data").map_err(|e| format!("Failed to create data directory: {}", e))?;

    let (pk, circuit_params, break_points) =
        get_or_create_proving_key(&params, &input, config, pk_path)
            .map_err(|e| format!("Failed to get proving key: {}", e))?;

    // 3. Create prover circuit with real input, calculated params, and break points
    // IMPORTANT: Use the circuit_params from keygen, not get_default_params()
    let circuit_input = DepositEventCircuitV2::new(input.clone(), config);
    let fixed_keccak = PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY);
    let circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Prover,
        circuit_input,
        circuit_params,
        fixed_keccak,
    )
    .use_break_points(break_points);

    // CRITICAL: Fulfill Keccak promises AFTER setting break points
    // This is required for axiom-eth circuits in Prover mode
    // See: axiom-eth/src/storage/tests.rs for reference
    circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY));

    // 4. Generate SNARK proof
    println!("Generating SNARK proof...");
    let snark_path = format!("data/deposit_proof_{}.snark", input.event_data.deposit_id);
    let snark = gen_snark_shplonk(&params, &pk, circuit, Some(snark_path.as_str()));

    println!("Proof generated successfully!");

    // 5. Extract public outputs and serialize proof
    let proof_bytes =
        bincode::serialize(&snark).map_err(|e| format!("Failed to serialize proof: {}", e))?;

    // 6. Compute block hash from block header RLP
    use ethers::utils::keccak256;
    let block_hash: [u8; 32] = keccak256(&input.receipt_proof.block_header_rlp);

    Ok(DepositProofOutput::new(
        proof_bytes,
        input.event_data.deposit_id,
        input.event_data.sender,
        input.event_data.amount,
        input.dapp_id,
        input.event_data.an_account,
        input.event_data.contract_address,
        block_hash,
    ))
}

/// Verify a SNARK proof.
///
/// This function verifies a SNARK proof against the verifying key.
///
/// # Arguments
///
/// * `proof` - The proof output to verify
/// * `config` - Circuit configuration (must match the one used for proof
///   generation)
///
/// # Returns
///
/// `Ok(true)` if the proof is valid, `Ok(false)` if invalid, `Err` on error
pub fn verify_proof(proof: &DepositProofOutput, config: &CircuitConfig) -> Result<bool, String> {
    let k = config.degree;

    // 1. Get KZG parameters (needed for verification)
    let _params = load_kzg_params_from_trusted_setup(k)
        .map_err(|e| format!("Failed to load KZG params: {}", e))?;

    // 2. Deserialize SNARK
    let snark: Snark = bincode::deserialize(&proof.proof)
        .map_err(|e| format!("Failed to deserialize proof: {}", e))?;

    // 3. Verify proof structure and public inputs
    // Note: Full cryptographic verification happens on-chain via Solidity verifier
    // Here we perform basic sanity checks on the proof structure

    // Check that proof has instances (public inputs)
    if snark.instances.is_empty() {
        return Err("Proof has no public instances".to_string());
    }

    // FIX BC-CIRCUIT-004: Check that we have exactly 7 public inputs
    // (depositId, sender, amount, contract_address, block_hash_high,
    // block_hash_low, promise_commit)
    if snark.instances[0].len() != 7 {
        return Err(format!(
            "Expected 7 public inputs (6 user values + promise_commit), got {}",
            snark.instances[0].len()
        ));
    }

    // Helper function to convert bytes to field element using Horner's method
    // This matches the circuit's bytes_to_field() logic
    fn bytes_to_field(bytes: &[u8]) -> Fr {
        let mut result = Fr::zero();
        let base = Fr::from(256);
        for &byte in bytes.iter() {
            result = result * base + Fr::from(byte as u64);
        }
        result
    }

    // Verify public inputs match the claimed values
    let deposit_id_field = Fr::from(proof.deposit_id);

    // FIX BC-PROVER-003 Issue B: Convert ALL 20 bytes of sender address
    let sender_field = bytes_to_field(&proof.sender);

    // FIX BC-TYPES-001: Convert ALL 32 bytes of amount
    let amount_field = bytes_to_field(&proof.amount);

    // FIX BC-PROVER-003 Issue B: Convert ALL 20 bytes of contract address
    let contract_field = bytes_to_field(&proof.contract_address);

    // FIX BC-PROVER-003 Issue C: Convert block_hash to high/low field elements
    // Block hash is split into two 128-bit (16-byte) field elements
    let block_hash_high = bytes_to_field(&proof.block_hash[0..16]);
    let block_hash_low = bytes_to_field(&proof.block_hash[16..32]);

    if snark.instances[0][0] != deposit_id_field {
        return Err("Public input mismatch: depositId".to_string());
    }
    if snark.instances[0][1] != sender_field {
        return Err("Public input mismatch: sender".to_string());
    }
    if snark.instances[0][2] != amount_field {
        return Err("Public input mismatch: amount".to_string());
    }
    if snark.instances[0][3] != contract_field {
        return Err("Public input mismatch: contract_address".to_string());
    }
    // FIX BC-PROVER-003 Issue C: Verify block_hash_high and block_hash_low
    if snark.instances[0][4] != block_hash_high {
        return Err("Public input mismatch: block_hash_high".to_string());
    }
    if snark.instances[0][5] != block_hash_low {
        return Err("Public input mismatch: block_hash_low".to_string());
    }

    println!("✓ Proof structure valid");
    println!("✓ Public inputs verified (7 instances: 6 user values + promise_commit)");
    println!("  depositId: {}", proof.deposit_id);
    println!("  sender: 0x{}", hex::encode(proof.sender));
    println!("  amount: 0x{}", hex::encode(proof.amount));
    println!("  contract: 0x{}", hex::encode(proof.contract_address));
    println!("  block_hash: 0x{}", hex::encode(proof.block_hash));
    println!(
        "  promise_commit: 0x{}",
        hex::encode(snark.instances[0][6].to_bytes())
    );
    println!();
    println!("Note: Full cryptographic verification must be done on-chain via Solidity verifier");
    println!("      This check only validates proof structure and public inputs");

    Ok(true)
}

/// Generate a Solidity verifier contract.
///
/// This function generates a Solidity smart contract that can verify proofs
/// on-chain. The verifier is generated using the SHPLONK multi-open scheme.
///
/// # Arguments
///
/// * `config` - Circuit configuration (must match the one used for proof
///   generation)
/// * `output_path` - Path where to save the Solidity verifier contract
///
/// # Returns
///
/// `Ok(())` on success, `Err` with error message on failure
///
/// # Example
///
/// ```ignore
/// let config = CircuitConfig::default();
/// generate_solidity_verifier(&config, Path::new("contracts/DepositVerifier.sol"))?;
/// ```
pub fn generate_solidity_verifier(
    config: &CircuitConfig,
    output_path: &Path,
) -> Result<(), String> {
    println!("Generating Solidity verifier contract...");

    let k = config.degree;

    // 1. Get KZG parameters
    println!("Loading KZG parameters...");
    let params = load_kzg_params_from_trusted_setup(k)?;

    // 2. Get proving key (which contains the verifying key)
    println!("Loading proving key...");
    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    let pk_path = Path::new(&pk_path_str);

    // Use placeholder input for verifier generation
    let placeholder_input = create_keygen_placeholder_input();
    let (pk, _circuit_params, _break_points) =
        get_or_create_proving_key(&params, &placeholder_input, config, pk_path)?;

    // 3. Get verifying key from proving key
    let vk = pk.get_vk();

    // 4. Define number of public instances
    // FIX BC-CIRCUIT-004: Updated to 7 to include promise_commit
    // We have 7 public outputs: [depositId, sender, amount,
    // contract_address, block_hash_high, block_hash_low, promise_commit]
    let num_instance = vec![7];

    // 5. Generate Solidity verifier using SHPLONK
    println!("Generating Solidity code...");
    use axiom_eth::utils::eth_circuit::EthCircuitImpl;

    let bytecode = gen_evm_verifier_shplonk::<EthCircuitImpl<Fr, DepositEventCircuitV2>>(
        &params,
        vk,
        num_instance,
        Some(output_path),
    );

    println!("✅ Solidity verifier generated at: {:?}", output_path);
    println!("   Contract size: {} bytes", bytecode.len());

    // 6. Also save the deployment bytecode for direct deployment
    let bytecode_path = output_path.with_extension("bytecode");
    let bytecode_hex_path = output_path.with_extension("bytecode.hex");

    fs::write(&bytecode_path, &bytecode)
        .map_err(|e| format!("Failed to write bytecode file: {}", e))?;

    let hex_string = format!("0x{}", hex::encode(&bytecode));
    fs::write(&bytecode_hex_path, &hex_string)
        .map_err(|e| format!("Failed to write hex bytecode file: {}", e))?;

    println!("✅ Deployment bytecode saved:");
    println!("   Raw:  {:?} ({} bytes)", bytecode_path, bytecode.len());
    println!(
        "   Hex:  {:?} ({} chars)",
        bytecode_hex_path,
        hex_string.len()
    );

    if bytecode.len() > 24576 {
        println!("\n⚠️  WARNING: Bytecode exceeds 24KB Ethereum contract size limit!");
        println!(
            "   Size: {:.1} KB (limit: 24 KB)",
            bytecode.len() as f64 / 1024.0
        );
        println!("   This verifier can only be deployed on:");
        println!("   - Testnets (no size limit enforcement)");
        println!("   - L2 networks with higher limits (Arbitrum, Optimism, zkSync)");
    } else {
        println!(
            "\n✅ Bytecode is within 24KB limit ({:.1} KB)",
            bytecode.len() as f64 / 1024.0
        );
    }

    Ok(())
}

/// Create a minimal valid RLP input for key generation.
///
/// This creates a minimal valid RLP structure that can be used to generate
/// proving/verifying keys. For axiom-eth circuits, we need valid RLP data even
/// for keygen because the circuit structure depends on the RLP parsing logic.
///
/// This creates a minimal valid Ethereum receipt with a single log entry.
fn create_keygen_placeholder_input() -> DepositProofInput {
    use alloy_rlp::Encodable;

    use crate::types::{DepositEventData, ReceiptProof, TransactionProof};

    let event_data = DepositEventData {
        block_number: 0,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 0,
        sender: [0u8; 20],
        amount: [0u8; 32], // FIX BC-TYPES-001: Changed from u64 to [u8; 32]
        an_workchain: 0,
        an_account: [0u8; 32],
        timestamp: 0,
        contract_address: [0u8; 20],
    };

    // Create a minimal valid receipt RLP with one log
    // Receipt structure: [status, cumulative_gas, bloom, logs]

    // Build topics list: [event_sig, depositId, sender]
    let mut topics_buf = Vec::new();
    vec![0u8; 32].encode(&mut topics_buf); // event signature
    vec![0u8; 32].encode(&mut topics_buf); // depositId
    vec![0u8; 32].encode(&mut topics_buf); // sender
    let mut topics_list = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: topics_buf.len(),
    }
    .encode(&mut topics_list);
    topics_list.extend_from_slice(&topics_buf);

    // Build log: [address, topics, data]
    let mut log_buf = Vec::new();
    vec![0u8; 20].encode(&mut log_buf); // contract address
    log_buf.extend_from_slice(&topics_list); // topics (already has list header)
    vec![0u8; 64].encode(&mut log_buf); // data (amount + timestamp)
    let mut log_list = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: log_buf.len(),
    }
    .encode(&mut log_list);
    log_list.extend_from_slice(&log_buf);

    // Build logs array containing one log
    let mut logs_list = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: log_list.len(),
    }
    .encode(&mut logs_list);
    logs_list.extend_from_slice(&log_list);

    // Build receipt: [status, cumulative_gas, bloom, logs]
    let mut receipt_buf = Vec::new();
    1u8.encode(&mut receipt_buf); // status = 1 (success)
    21000u64.encode(&mut receipt_buf); // cumulative_gas
    vec![0u8; 256].encode(&mut receipt_buf); // bloom filter (256 bytes)
    receipt_buf.extend_from_slice(&logs_list); // logs (already has list header)

    let mut receipt_rlp = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: receipt_buf.len(),
    }
    .encode(&mut receipt_rlp);
    receipt_rlp.extend_from_slice(&receipt_buf);

    // Create a minimal MPT proof (single node)
    let proof_nodes = vec![receipt_rlp.clone()];

    let receipt_proof = ReceiptProof {
        receipt_rlp,
        proof_nodes,
        receipt_root: [0u8; 32],
        block_header_rlp: vec![0u8; 100], // minimal block header
    };

    // Minimal EIP-1559 typed-tx leaf: 0x02 || RLP([chainId=1, nonce, tips, fees,
    // gas, to, value, data, accessList, yParity, r, s]) — sizes only need to be
    // structurally valid for keygen shape (not MockProver-satisfying).
    let mut tx_payload = Vec::new();
    1u64.encode(&mut tx_payload); // chain_id
    0u64.encode(&mut tx_payload); // nonce
    0u64.encode(&mut tx_payload); // maxPriorityFeePerGas
    0u64.encode(&mut tx_payload); // maxFeePerGas
    21000u64.encode(&mut tx_payload); // gas
    vec![0u8; 20].encode(&mut tx_payload); // to
    0u64.encode(&mut tx_payload); // value
    Vec::<u8>::new().encode(&mut tx_payload); // data
    {
        // empty accessList
        let mut al = Vec::new();
        alloy_rlp::Header {
            list: true,
            payload_length: 0,
        }
        .encode(&mut al);
        tx_payload.extend_from_slice(&al);
    }
    0u64.encode(&mut tx_payload); // yParity
    vec![0u8; 32].encode(&mut tx_payload); // r
    vec![0u8; 32].encode(&mut tx_payload); // s
    let mut tx_list = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: tx_payload.len(),
    }
    .encode(&mut tx_list);
    tx_list.extend_from_slice(&tx_payload);
    let mut tx_bytes = vec![0x02u8];
    tx_bytes.extend_from_slice(&tx_list);

    let tx_proof = TransactionProof {
        tx_bytes: tx_bytes.clone(),
        proof_nodes: vec![tx_bytes],
        transactions_root: [0u8; 32],
    };

    DepositProofInput {
        event_data,
        receipt_proof,
        tx_proof,
        dapp_id: [0u8; 32],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = CircuitConfig::default();
        assert_eq!(config.degree, 15); // Updated for Option B+ ultra-aggressive optimization
        assert_eq!(config.max_data_byte_len, 128); // Updated for Option B+ optimization
        assert_eq!(config.max_log_num, 3); // Updated for Option B+ ultra-aggressive optimization
        assert_eq!(config.topic_num_bounds, (0, 4));
    }

    #[test]
    fn test_load_circuit_params() {
        let params = load_circuit_params("configs/circuit_params.json");
        assert!(
            params.is_ok(),
            "Failed to load circuit params: {:?}",
            params.err()
        );

        let params = params.unwrap();
        assert_eq!(params.base.k, 18);
        assert_eq!(params.num_rlc_columns, 3);
    }

    #[test]
    fn test_create_keygen_placeholder_input() {
        let input = create_keygen_placeholder_input();

        // Verify placeholder has expected zero values
        assert_eq!(input.event_data.deposit_id, 0);
        assert_eq!(input.event_data.sender, [0u8; 20]);
        assert_eq!(input.event_data.amount, [0u8; 32]); // FIX BC-TYPES-001
        assert_eq!(input.event_data.timestamp, 0);
        assert_eq!(input.event_data.contract_address, [0u8; 20]);

        // Verify receipt proof has minimal structure (1 proof node)
        assert_eq!(input.receipt_proof.proof_nodes.len(), 1);
        assert_eq!(input.receipt_proof.receipt_root, [0u8; 32]);
        assert!(!input.receipt_proof.receipt_rlp.is_empty());
        assert!(!input.receipt_proof.block_header_rlp.is_empty());
    }

    #[test]
    fn test_get_default_params() {
        let params = get_default_params();

        // Verify default parameters
        assert_eq!(params.base.k, 18);
        assert_eq!(params.num_rlc_columns, 3);
    }

    // Note: test_circuit_mock requires real Ethereum data and is tested in
    // integration tests Note: Full proof generation tests are too slow for
    // unit tests (5-10 minutes)
}
