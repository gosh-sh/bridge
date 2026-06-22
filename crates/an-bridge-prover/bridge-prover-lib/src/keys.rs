use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::Context;
use halo2_base::gates::circuit::builder::BaseCircuitBuilder;
use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::{keygen_pk, keygen_vk, ProvingKey, VerifyingKey},
    poly::kzg::commitment::ParamsKZG,
    SerdeFormat,
};
use halo2_base::utils::fs::gen_srs;
use tracing::info;

use attestation_bls_checker_circuit::primary_circuit::PrimaryAttestationBlsCheckerCircuit;
use attestation_bls_checker_circuit::fallback_circuit::FallbackAttestationBlsCheckerCircuit;
use historical_layer_hashes_movement_checker_circuit::circuit::LayerHashesMovementCheckerCircuit;
use bridge_event_prove_circuit::test_helpers::build_synthetic_event_keygen_inputs;

// ---- Circuit 1a (Primary Attestation) and 1b (Fallback Attestation) constants ----
// Both circuits share K and shape; only the constraint system / VK differ.
const K: u32 = 20;
const NUM_UNUSABLE_ROWS: usize = 109;
const LOOKUP_BITS: usize = 19;
const LIMB_BITS: usize = 104;
const NUM_LIMBS: usize = 5;
const MAX_SIGNERS: usize = 300;

// ---- Circuit 2 (Layer Hashes Movement) constants ----
const LAYER_K: u32 = 17;
const LAYER_NUM_UNUSABLE_ROWS: usize = 109;
const LAYER_LOOKUP_BITS: usize = 16;

// ---- Circuit 4 (Event Prove — WithdrawalInitiated) constants ----
// EVENT_K matches `bridge-event-prove-circuit::test_helpers::K` and the
// `k` field of `event_prover::default_event_circuit_params`. SRS at K=20
// covers this smaller K too.
const EVENT_K: u32 = 19;
// Deterministic seed for the synthetic-witness keygen path. Any seed
// produces the same VK/PK shape since the circuit's constraint system
// is independent of witness values — we just need *some* constraint-
// satisfying input to drive `keygen_vk` / `keygen_pk`.
const EVENT_KEYGEN_SEED: u64 = 0xE5E5_E5E5_E5E5_E5E5;

const SERDE_FMT: SerdeFormat = SerdeFormat::RawBytesUnchecked;

/// Manages SRS, VK, and PK for both circuits with disk caching.
pub struct KeyManager {
    pub params_dir: PathBuf,
    pub srs: ParamsKZG<Bn256>,
    // Circuit 1a (Primary Attestation)
    pub primary_vk: Option<VerifyingKey<G1Affine>>,
    pub primary_pk: Option<ProvingKey<G1Affine>>,
    pub primary_config: Option<BaseCircuitParams>,
    // Circuit 1b (Fallback Attestation) — same K=20 shape, different VK/PK
    pub fallback_vk: Option<VerifyingKey<G1Affine>>,
    pub fallback_pk: Option<ProvingKey<G1Affine>>,
    pub fallback_config: Option<BaseCircuitParams>,
    // Circuit 2 (Layer Hashes Movement)
    pub layer_vk: Option<VerifyingKey<G1Affine>>,
    pub layer_pk: Option<ProvingKey<G1Affine>>,
    pub layer_config: Option<BaseCircuitParams>,
    // Circuit 4 (Event Prove — WithdrawalInitiated)
    pub event_vk: Option<VerifyingKey<G1Affine>>,
    pub event_pk: Option<ProvingKey<G1Affine>>,
    pub event_config: Option<BaseCircuitParams>,
}

impl KeyManager {
    /// Create a new KeyManager. Loads SRS (cached by halo2-base).
    /// Loads VKs and configs from disk. PKs are NOT loaded here — they are
    /// loaded on-demand via `load_primary_pk()` / `load_layer_pk()` to avoid
    /// holding both ~3.7 GB + ~2.8 GB proving keys in memory simultaneously.
    pub fn new(params_dir: &Path) -> Self {
        std::fs::create_dir_all(params_dir).ok();

        // gen_srs caches to ./params/kzg_bn254_{K}.srs
        // Use the larger K (Circuit 1a) since SRS can be used for smaller circuits too.
        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_var("PARAMS_DIR", params_dir.to_str().unwrap());
        let srs = gen_srs(K);
        std::env::set_current_dir(&prev_dir).ok();

        let mut mgr = Self {
            params_dir: params_dir.to_path_buf(),
            srs,
            primary_vk: None,
            primary_pk: None,
            primary_config: None,
            fallback_vk: None,
            fallback_pk: None,
            fallback_config: None,
            layer_vk: None,
            layer_pk: None,
            layer_config: None,
            event_vk: None,
            event_pk: None,
            event_config: None,
        };

        // Try loading cached primary VK and config (PK loaded on demand).
        if let Ok(config) = mgr.load_config("primary") {
            info!("found primary config: {:?}", config);
            if let Some(vk) = mgr.try_load_vk("primary", &config) {
                info!("loaded primary VK from cache");
                mgr.primary_vk = Some(vk);
            }
            if mgr.pk_path("primary").exists() {
                info!("primary PK found on disk (will load on demand)");
            }
            mgr.primary_config = Some(config);
        }

        // Try loading cached fallback VK and config (PK loaded on demand).
        if let Ok(config) = mgr.load_config("fallback") {
            info!("found fallback config: {:?}", config);
            if let Some(vk) = mgr.try_load_vk("fallback", &config) {
                info!("loaded fallback VK from cache");
                mgr.fallback_vk = Some(vk);
            }
            if mgr.pk_path("fallback").exists() {
                info!("fallback PK found on disk (will load on demand)");
            }
            mgr.fallback_config = Some(config);
        }

        // Try loading cached layer VK and config (PK loaded on demand).
        if let Ok(config) = mgr.load_config("layer") {
            info!("found layer config: {:?}", config);
            if let Some(vk) = mgr.try_load_vk("layer", &config) {
                info!("loaded layer VK from cache");
                mgr.layer_vk = Some(vk);
            }
            if mgr.pk_path("layer").exists() {
                info!("layer PK found on disk (will load on demand)");
            }
            mgr.layer_config = Some(config);
        }

        // Try loading cached event VK and config (PK loaded on demand).
        if let Ok(config) = mgr.load_config("event") {
            info!("found event config: {:?}", config);
            if let Some(vk) = mgr.try_load_vk("event", &config) {
                info!("loaded event VK from cache");
                mgr.event_vk = Some(vk);
            }
            if mgr.pk_path("event").exists() {
                info!("event PK found on disk (will load on demand)");
            }
            mgr.event_config = Some(config);
        }

        mgr
    }

    // ---- Circuit 1a (Primary Attestation) ----

    /// Ensure primary circuit keys exist on disk. Runs keygen if not cached.
    /// Does NOT keep the PK in memory — call `load_primary_pk()` before proof generation.
    pub fn ensure_primary_keys(
        &mut self,
        bk_set: &HashMap<u16, Vec<u8>>,
    ) -> anyhow::Result<()> {
        if self.primary_vk.is_some() && self.pk_path("primary").exists() {
            info!("primary keys already available (VK in memory, PK on disk)");
            return Ok(());
        }

        info!("running keygen for primary circuit (this may take ~60s)...");

        let test_data =
            bridge_test_data_gen::generator::generate_test_data_all_sign(bk_set.len())
                .context("failed to generate reference test data for keygen")?;

        let last_seen: u32 = 0;
        let circuit = PrimaryAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            test_data.bk_set,
            last_seen,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );
        let base_params = circuit.params.base_circuit_params.clone();
        info!("primary base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("primary keygen_vk failed")?;
        info!("primary keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit).context("primary keygen_pk failed")?;
        info!("primary keygen_pk: {:?}", t.elapsed());

        self.save_vk("primary", &vk)?;
        self.save_pk("primary", &pk)?;
        self.save_config("primary", &base_params)?;

        self.primary_vk = Some(vk);
        // PK intentionally NOT kept in memory — saved to disk, will load on demand.
        // `pk` drops here, freeing ~3.7 GB.
        self.primary_config = Some(base_params);

        info!("primary keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn primary_vk(&self) -> &VerifyingKey<G1Affine> {
        self.primary_vk.as_ref().expect("primary VK not loaded")
    }

    pub fn primary_pk(&self) -> &ProvingKey<G1Affine> {
        self.primary_pk.as_ref().expect("primary PK not loaded")
    }

    pub fn primary_config(&self) -> &BaseCircuitParams {
        self.primary_config.as_ref().expect("primary config not loaded")
    }

    // ---- Circuit 1b (Fallback Attestation) ----
    //
    // Same K=20 / lookup_bits / limb shape as Circuit 1a — the constraint
    // system differs (two BLS verifications + same-block_id checks +
    // `ThresholdMode::Fallback` >N/2 threshold), so VK/PK are distinct.
    // Disk artifacts: `fallback_vk.bin`, `fallback_pk.bin`, `fallback_config_params.json`.

    /// Ensure fallback circuit keys exist on disk. Runs keygen if not cached.
    /// Does NOT keep the PK in memory — call `load_fallback_pk()` before proof
    /// generation. Keygen uses `generate_test_data_fallback_all_sign(bk_set.len())`
    /// which produces a constraint-satisfying (PRIMARY, FALLBACK) attestation
    /// pair over a random block_id; only the circuit constraint shape affects
    /// the resulting VK/PK, so any admissible witness drives keygen identically.
    pub fn ensure_fallback_keys(
        &mut self,
        bk_set: &HashMap<u16, Vec<u8>>,
    ) -> anyhow::Result<()> {
        if self.fallback_vk.is_some() && self.pk_path("fallback").exists() {
            info!("fallback keys already available (VK in memory, PK on disk)");
            return Ok(());
        }

        info!("running keygen for fallback circuit (this may take ~60s)...");

        let test_data =
            bridge_test_data_gen::generator::generate_test_data_fallback_all_sign(bk_set.len())
                .context("failed to generate reference fallback test data for keygen")?;
        let attestation_fallback_bytes = test_data
            .attestation_2_bytes
            .ok_or_else(|| anyhow::format_err!(
                "fallback test data missing attestation_2_bytes (FALLBACK target proof)"
            ))?;

        let last_seen: u32 = 0;
        let circuit = FallbackAttestationBlsCheckerCircuit::<Fr>::new(
            test_data.attestation_bytes,
            attestation_fallback_bytes,
            test_data.bk_set,
            last_seen,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
            LIMB_BITS,
            NUM_LIMBS,
            MAX_SIGNERS,
        );
        let base_params = circuit.params.base_circuit_params.clone();
        info!("fallback base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("fallback keygen_vk failed")?;
        info!("fallback keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit).context("fallback keygen_pk failed")?;
        info!("fallback keygen_pk: {:?}", t.elapsed());

        self.save_vk("fallback", &vk)?;
        self.save_pk("fallback", &pk)?;
        self.save_config("fallback", &base_params)?;

        self.fallback_vk = Some(vk);
        // PK intentionally dropped here; reload on demand via load_fallback_pk().
        self.fallback_config = Some(base_params);

        info!("fallback keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn fallback_vk(&self) -> &VerifyingKey<G1Affine> {
        self.fallback_vk.as_ref().expect("fallback VK not loaded")
    }

    pub fn fallback_pk(&self) -> &ProvingKey<G1Affine> {
        self.fallback_pk.as_ref().expect("fallback PK not loaded")
    }

    pub fn fallback_config(&self) -> &BaseCircuitParams {
        self.fallback_config.as_ref().expect("fallback config not loaded")
    }

    // ---- Circuit 2 (Layer Hashes Movement) ----

    /// Ensure layer circuit keys exist on disk. Runs keygen if not cached.
    /// Does NOT keep the PK in memory — call `load_layer_pk()` before proof generation.
    pub fn ensure_layer_keys(&mut self) -> anyhow::Result<()> {
        if self.layer_vk.is_some() && self.pk_path("layer").exists() {
            info!("layer keys already available (VK in memory, PK on disk)");
            return Ok(());
        }

        info!("running keygen for layer circuit (this may take ~30s)...");

        // Build a reference circuit with synthetic test data.
        //
        // Of the three parameters to `generate_layer_hash_chain_with_depth`,
        // only `tree_depth` affects the resulting VK/PK: it controls the
        // number of Poseidon Merkle levels inside `dense_merkle_root_circuit`
        // and therefore the constraint count. `num_layers` and
        // `num_prev_chain_steps` only populate the synthetic witness — the
        // circuit always iterates over MAX_LAYERS (=10) and MAX_CHAIN_LEN
        // (=11) and masks inactive slots in-circuit. See
        // `verify_chain_of_dense_proofs` in gosh-halo2-crypto-lib's
        // dense-balanced-tree: "the circuit shape is fixed regardless of how
        // many steps are actually active, so one verification key works for
        // any chain length in [1, MAX_CHAIN_LEN]".
        //
        // REF_NUM_LAYERS / REF_NUM_PREV_CHAIN_STEPS are mid-range values
        // chosen to exercise non-trivial paths during keygen, but any other
        // admissible pair would yield the same VK/PK.
        //
        // REF_TREE_DEPTH must match real trees from the node:
        //   WINDOW=128 →130 leaves → pad 256 → depth=8
        // Derived from the shared `HISTORY_PROOF_WINDOW_SIZE` so this stays.
        const REF_NUM_LAYERS: usize = 3;
        const REF_NUM_PREV_CHAIN_STEPS: usize = 2;
        const REF_TREE_DEPTH: usize =
            (crate::poseidon_dense::HISTORY_PROOF_WINDOW_SIZE + 2)
                .next_power_of_two()
                .trailing_zeros() as usize;
        let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(
            REF_NUM_LAYERS,
            REF_NUM_PREV_CHAIN_STEPS,
            REF_TREE_DEPTH,
        );
        let preimage = build_reference_preimage(&chain_data);
        let siblings = [[0x10u8; 32], [0x20u8; 32], [0x30u8; 32]];
        let prev_hash_fr = gosh_dense_balanced_tree::bytes_to_fr(&chain_data.prev_max_level_layer_hash);
        let chain_links = chain_data_to_dense_links(&chain_data);
        let bk_set_hash = Fr::from(0xDEADBEEFu64);

        let circuit = LayerHashesMovementCheckerCircuit::new(
            preimage,
            siblings,
            prev_hash_fr,
            (chain_data.num_prev_chain_steps + 1) as u8,
            chain_links,
            bk_set_hash,
            LAYER_K as usize,
            LAYER_NUM_UNUSABLE_ROWS,
            LAYER_LOOKUP_BITS,
        );
        let base_params = circuit.base_circuit_params().clone();
        info!("layer base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("layer keygen_vk failed")?;
        info!("layer keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit).context("layer keygen_pk failed")?;
        info!("layer keygen_pk: {:?}", t.elapsed());

        self.save_vk("layer", &vk)?;
        self.save_pk("layer", &pk)?;
        self.save_config("layer", &base_params)?;

        self.layer_vk = Some(vk);
        // PK intentionally NOT kept in memory — saved to disk, will load on demand.
        // `pk` drops here, freeing ~2.8 GB.
        self.layer_config = Some(base_params);

        info!("layer keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn layer_vk(&self) -> &VerifyingKey<G1Affine> {
        self.layer_vk.as_ref().expect("layer VK not loaded")
    }

    pub fn layer_pk(&self) -> &ProvingKey<G1Affine> {
        self.layer_pk.as_ref().expect("layer PK not loaded")
    }

    pub fn layer_config(&self) -> &BaseCircuitParams {
        self.layer_config.as_ref().expect("layer config not loaded")
    }

    pub fn layer_k(&self) -> usize {
        LAYER_K as usize
    }

    pub fn layer_num_unusable_rows(&self) -> usize {
        LAYER_NUM_UNUSABLE_ROWS
    }

    pub fn layer_lookup_bits(&self) -> usize {
        LAYER_LOOKUP_BITS
    }

    // ---- Circuit 4 (Event Prove — WithdrawalInitiated) ----

    /// Ensure event circuit keys exist on disk. Runs keygen if not cached.
    /// Does NOT keep the PK in memory — call `load_event_pk()` before proof generation.
    ///
    /// Keygen uses the deterministic synthetic-witness path exposed by
    /// `bridge-event-prove-circuit::test_helpers::build_synthetic_event_keygen_inputs`,
    /// which composes the embedded `withdrawals.txt` first record + a random
    /// two-level tree + a T=1 dense chain. The produced circuit's constraint
    /// system is independent of these witness values, so the VK/PK shape is
    /// stable across machines and CI runs.
    pub fn ensure_event_keys(&mut self) -> anyhow::Result<()> {
        if self.event_vk.is_some() && self.pk_path("event").exists() {
            info!("event keys already available (VK in memory, PK on disk)");
            return Ok(());
        }

        info!("running keygen for event circuit (this may take a while)...");

        let (circuit, _instances) = build_synthetic_event_keygen_inputs(EVENT_KEYGEN_SEED);
        let base_params = circuit.base_circuit_params.clone();
        info!("event base_circuit_params: {:?}", base_params);

        let t = std::time::Instant::now();
        let vk = keygen_vk(&self.srs, &circuit).context("event keygen_vk failed")?;
        info!("event keygen_vk: {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let pk = keygen_pk(&self.srs, vk.clone(), &circuit).context("event keygen_pk failed")?;
        info!("event keygen_pk: {:?}", t.elapsed());

        self.save_vk("event", &vk)?;
        self.save_pk("event", &pk)?;
        self.save_config("event", &base_params)?;

        self.event_vk = Some(vk);
        // PK intentionally NOT kept in memory — saved to disk, will load on demand.
        self.event_config = Some(base_params);

        info!("event keys generated and cached (PK on disk, not in memory)");
        Ok(())
    }

    pub fn event_vk(&self) -> &VerifyingKey<G1Affine> {
        self.event_vk.as_ref().expect("event VK not loaded")
    }

    pub fn event_pk(&self) -> &ProvingKey<G1Affine> {
        self.event_pk.as_ref().expect("event PK not loaded")
    }

    pub fn event_config(&self) -> &BaseCircuitParams {
        self.event_config.as_ref().expect("event config not loaded")
    }

    pub fn event_k(&self) -> usize {
        EVENT_K as usize
    }

    // ---- On-demand PK loading (memory management) ----

    /// Load primary PK from disk into memory. Call before generating Circuit 1a proofs.
    pub fn load_primary_pk(&mut self) -> anyhow::Result<()> {
        if self.primary_pk.is_some() {
            return Ok(());
        }
        let config = self.primary_config.as_ref()
            .ok_or_else(|| anyhow::format_err!("primary config not loaded — run ensure_primary_keys first"))?;
        info!("loading primary PK from disk (~3.7 GB)...");
        let t = std::time::Instant::now();
        let pk = self.try_load_pk("primary", config)
            .ok_or_else(|| anyhow::format_err!("failed to load primary PK from {}", self.pk_path("primary").display()))?;
        info!("primary PK loaded in {:?}", t.elapsed());
        self.primary_pk = Some(pk);
        Ok(())
    }

    /// Unload primary PK from memory. Call after generating Circuit 1a proofs to free ~3.7 GB.
    pub fn unload_primary_pk(&mut self) {
        if self.primary_pk.is_some() {
            self.primary_pk = None;
            info!("primary PK unloaded from memory");
        }
    }

    /// Load fallback PK from disk into memory. Call before generating Circuit 1b proofs.
    pub fn load_fallback_pk(&mut self) -> anyhow::Result<()> {
        if self.fallback_pk.is_some() {
            return Ok(());
        }
        let config = self.fallback_config.as_ref()
            .ok_or_else(|| anyhow::format_err!("fallback config not loaded — run ensure_fallback_keys first"))?;
        info!("loading fallback PK from disk (~3.7 GB)...");
        let t = std::time::Instant::now();
        let pk = self.try_load_pk("fallback", config)
            .ok_or_else(|| anyhow::format_err!("failed to load fallback PK from {}", self.pk_path("fallback").display()))?;
        info!("fallback PK loaded in {:?}", t.elapsed());
        self.fallback_pk = Some(pk);
        Ok(())
    }

    /// Unload fallback PK from memory. Call after generating Circuit 1b proofs.
    pub fn unload_fallback_pk(&mut self) {
        if self.fallback_pk.is_some() {
            self.fallback_pk = None;
            info!("fallback PK unloaded from memory");
        }
    }

    /// Load layer PK from disk into memory. Call before generating Circuit 2 proofs.
    pub fn load_layer_pk(&mut self) -> anyhow::Result<()> {
        if self.layer_pk.is_some() {
            return Ok(());
        }
        let config = self.layer_config.as_ref()
            .ok_or_else(|| anyhow::format_err!("layer config not loaded — run ensure_layer_keys first"))?;
        info!("loading layer PK from disk (~2.8 GB)...");
        let t = std::time::Instant::now();
        let pk = self.try_load_pk("layer", config)
            .ok_or_else(|| anyhow::format_err!("failed to load layer PK from {}", self.pk_path("layer").display()))?;
        info!("layer PK loaded in {:?}", t.elapsed());
        self.layer_pk = Some(pk);
        Ok(())
    }

    /// Unload layer PK from memory. Call after generating Circuit 2 proofs to free ~2.8 GB.
    pub fn unload_layer_pk(&mut self) {
        if self.layer_pk.is_some() {
            self.layer_pk = None;
            info!("layer PK unloaded from memory");
        }
    }

    /// Load event PK from disk into memory. Call before generating Circuit 4 proofs.
    pub fn load_event_pk(&mut self) -> anyhow::Result<()> {
        if self.event_pk.is_some() {
            return Ok(());
        }
        let config = self.event_config.as_ref()
            .ok_or_else(|| anyhow::format_err!("event config not loaded — run ensure_event_keys first"))?;
        info!("loading event PK from disk...");
        let t = std::time::Instant::now();
        let pk = self.try_load_pk("event", config)
            .ok_or_else(|| anyhow::format_err!("failed to load event PK from {}", self.pk_path("event").display()))?;
        info!("event PK loaded in {:?}", t.elapsed());
        self.event_pk = Some(pk);
        Ok(())
    }

    /// Unload event PK from memory. Call after generating Circuit 4 proofs.
    pub fn unload_event_pk(&mut self) {
        if self.event_pk.is_some() {
            self.event_pk = None;
            info!("event PK unloaded from memory");
        }
    }

    // ---- Internal helpers ----

    fn vk_path(&self, prefix: &str) -> PathBuf {
        self.params_dir.join(format!("{}_vk.bin", prefix))
    }

    fn pk_path(&self, prefix: &str) -> PathBuf {
        self.params_dir.join(format!("{}_pk.bin", prefix))
    }

    fn config_path(&self, prefix: &str) -> PathBuf {
        self.params_dir.join(format!("{}_config_params.json", prefix))
    }

    fn load_config(&self, prefix: &str) -> anyhow::Result<BaseCircuitParams> {
        let data = std::fs::read_to_string(self.config_path(prefix))?;
        Ok(serde_json::from_str(&data)?)
    }

    fn save_config(&self, prefix: &str, config: &BaseCircuitParams) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(config)?;
        std::fs::write(self.config_path(prefix), json)?;
        Ok(())
    }

    fn try_load_vk(&self, prefix: &str, config: &BaseCircuitParams) -> Option<VerifyingKey<G1Affine>> {
        let file = std::fs::File::open(self.vk_path(prefix)).ok()?;
        let mut reader = BufReader::new(file);
        VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }

    fn try_load_pk(&self, prefix: &str, config: &BaseCircuitParams) -> Option<ProvingKey<G1Affine>> {
        let file = std::fs::File::open(self.pk_path(prefix)).ok()?;
        let mut reader = BufReader::new(file);
        ProvingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut reader,
            SERDE_FMT,
            config.clone(),
        )
        .ok()
    }

    fn save_vk(&self, prefix: &str, vk: &VerifyingKey<G1Affine>) -> anyhow::Result<()> {
        let file = std::fs::File::create(self.vk_path(prefix))?;
        let mut writer = BufWriter::new(file);
        vk.write(&mut writer, SERDE_FMT)?;
        Ok(())
    }

    fn save_pk(&self, prefix: &str, pk: &ProvingKey<G1Affine>) -> anyhow::Result<()> {
        let file = std::fs::File::create(self.pk_path(prefix))?;
        let mut writer = BufWriter::new(file);
        pk.write(&mut writer, SERDE_FMT)?;
        Ok(())
    }
}

/// Public constants for use by other modules.
pub const fn circuit_k() -> u32 { K }
pub const fn circuit_limb_bits() -> usize { LIMB_BITS }
pub const fn circuit_num_limbs() -> usize { NUM_LIMBS }
pub const fn circuit_max_signers() -> usize { MAX_SIGNERS }
pub const fn circuit_num_unusable_rows() -> usize { NUM_UNUSABLE_ROWS }
pub const fn circuit_lookup_bits() -> usize { LOOKUP_BITS }

// ---- Helpers for layer circuit keygen ----

use gosh_dense_balanced_tree::DenseChainLink;
use historical_layer_hashes_movement_checker_circuit::LAYER_PREIMAGE_SIZE;
use bridge_test_data_gen::layer_hashes::LayerHashChainData;

/// Build a 331-byte preimage from LayerHashChainData for reference circuit keygen.
fn build_reference_preimage(chain_data: &LayerHashChainData) -> [u8; LAYER_PREIMAGE_SIZE] {
    let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
    preimage[0] = chain_data.num_layers as u8;
    for i in 0..10 {
        let offset = 1 + i * 33;
        preimage[offset] = (i + 1) as u8;
        if i < chain_data.num_layers {
            preimage[offset + 1..offset + 1 + 32].copy_from_slice(&chain_data.root_hashes[i]);
        }
    }
    preimage
}

/// Convert LayerHashChainData chain proofs to DenseChainLinks.
fn chain_data_to_dense_links(chain_data: &LayerHashChainData) -> Vec<DenseChainLink> {
    chain_data
        .chain_proofs
        .iter()
        .map(|step| DenseChainLink {
            active: step.active,
            siblings: step.siblings.clone(),
            position: step.position,
            leaf_native: step.leaf_value,
        })
        .collect()
}
