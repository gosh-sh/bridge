//! Persistent PK cache for the outer SHPLONK aggregator keygen.
//!
//! `aggregate_and_prove` / `aggregate_inner` / `generate_yul_verifier` all
//! previously called `gen_pk(agg_params, &circuit, None)` on every
//! invocation, forcing a full outer keygen (~3-5 min at K=21 on M1-class
//! hardware, ~1 GB peak RAM). Every rerun of `export-inner-aggregator` and
//! every runtime call to `aggregate-proof` paid that cost from scratch.
//!
//! This module memoises the outer
//! `(pk, break_points, calculated, num_instance)` bundle on disk, keyed by
//! `(base_name, k_outer, lookup_bits, universality, inner-shape hash)`.
//!
//! Cache slot layout under `<cache_dir>`:
//! - `<stem>.pk`         -- proving key (SDK's `RawBytes` format, ~800 MB @ K=21)
//! - `<stem>.meta.json`  -- break_points + calculated params + num_instance
//!
//! The inner-shape hash is a 64-bit `SipHash` of `bincode::serialize(&snark.protocol)`.
//! `PlonkProtocol` bytes are a stable function of the inner VK, so a different
//! inner circuit shape allocates a fresh slot even if the caller reuses
//! `base_name`. Collision safety is not cryptographic here -- if two
//! shape-different snarks ever collide on the 64-bit hash and get pointed at
//! the same slot, the byte-drift check in `bin/aggregate_proof.rs:82-104`
//! still catches it before calldata is emitted on-chain.

use std::{
    collections::hash_map::DefaultHasher,
    hash::Hasher,
    path::{Path, PathBuf},
};

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::bn256::{Bn256, G1Affine},
        plonk::ProvingKey,
        poly::kzg::commitment::ParamsKZG,
    },
};
use serde::{Deserialize, Serialize};
use snark_verifier_sdk::{
    gen_pk,
    halo2::aggregation::{
        AggregationCircuit, AggregationConfigParams, VerifierUniversality,
    },
    CircuitExt, Snark, SHPLONK,
};

use crate::aggregator::AggregatorConfig;

/// Outer-aggregator keygen artefacts, either freshly generated or reloaded
/// from disk. The caller uses these to build the prover-stage circuit
/// (`calculated` + `break_points`) and to invoke `gen_evm_verifier_shplonk`
/// (`pk.get_vk()` + `num_instance`).
pub struct CachedKeygen {
    pub pk: ProvingKey<G1Affine>,
    pub break_points: Vec<Vec<usize>>,
    pub calculated: AggregationConfigParams,
    pub num_instance: Vec<usize>,
}

/// Slot paths (`.pk` + `.meta.json`) inside `cache_dir`.
struct SlotPaths {
    pk: PathBuf,
    meta: PathBuf,
}

/// On-disk mirror of everything the SDK's `gen_pk(Some(path))` does *not*
/// persist. Uses local mirror structs instead of relying on
/// `AggregationConfigParams: Serialize` -- serde derive status is
/// version-dependent, and mirroring costs 20 lines.
#[derive(Serialize, Deserialize)]
struct CachedMeta {
    break_points: Vec<Vec<usize>>,
    calculated: MirrorConfigParams,
    num_instance: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct MirrorConfigParams {
    degree: u32,
    num_advice: usize,
    num_lookup_advice: usize,
    num_fixed: usize,
    lookup_bits: usize,
}

impl From<AggregationConfigParams> for MirrorConfigParams {
    fn from(p: AggregationConfigParams) -> Self {
        Self {
            degree: p.degree,
            num_advice: p.num_advice,
            num_lookup_advice: p.num_lookup_advice,
            num_fixed: p.num_fixed,
            lookup_bits: p.lookup_bits,
        }
    }
}

impl From<MirrorConfigParams> for AggregationConfigParams {
    fn from(p: MirrorConfigParams) -> Self {
        Self {
            degree: p.degree,
            num_advice: p.num_advice,
            num_lookup_advice: p.num_lookup_advice,
            num_fixed: p.num_fixed,
            lookup_bits: p.lookup_bits,
        }
    }
}

fn universality_tag(u: VerifierUniversality) -> &'static str {
    match u {
        VerifierUniversality::None => "none",
        VerifierUniversality::PreprocessedAsWitness => "preprocessed",
        VerifierUniversality::Full => "full",
    }
}

fn inner_shape_hash(inner_snark: &Snark) -> u64 {
    let protocol_bytes = bincode::serialize(&inner_snark.protocol)
        .expect("PlonkProtocol serialization is infallible for well-formed snarks");
    let mut hasher = DefaultHasher::new();
    hasher.write(&protocol_bytes);
    hasher.finish()
}

/// Deterministic slot stem: `<base_name>__k<k>__lb<lb>__<univ>__<shape:016x>`.
pub fn cache_stem(base_name: &str, config: &AggregatorConfig, inner_snark: &Snark) -> String {
    format!(
        "{base_name}__k{}__lb{}__{}__{:016x}",
        config.k_outer,
        config.lookup_bits_outer,
        universality_tag(config.universality),
        inner_shape_hash(inner_snark),
    )
}

fn slot_paths(cache_dir: &Path, stem: &str) -> SlotPaths {
    SlotPaths {
        pk: cache_dir.join(format!("{stem}.pk")),
        meta: cache_dir.join(format!("{stem}.meta.json")),
    }
}

/// Load `(pk, break_points, calculated, num_instance)` from `cache_dir` if
/// both slot files exist. On cache miss, run the real outer keygen and
/// (if `cache_dir` is `Some`) persist everything for subsequent runs.
///
/// Cost:
/// - cache **miss** (K=21): ~3-5 min wall-clock, ~1 GB peak RAM
/// - cache **hit** (K=21): ~15-60 s (PK read + circuit construct)
///
/// Passing `cache_dir = None` is equivalent to the pre-cache behaviour: full
/// keygen every call, nothing persisted.
pub fn keygen_or_load(
    agg_params: &ParamsKZG<Bn256>,
    base_name: &str,
    config: AggregatorConfig,
    inner_snark: &Snark,
    cache_dir: Option<&Path>,
) -> anyhow::Result<CachedKeygen> {
    let slots = match cache_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            let stem = cache_stem(base_name, &config, inner_snark);
            Some(slot_paths(dir, &stem))
        }
        None => None,
    };

    // ------------------------------------------------------------------
    // Cache hit: skip real keygen, just read PK + meta from disk.
    // ------------------------------------------------------------------
    if let Some(s) = slots.as_ref() {
        if s.pk.exists() && s.meta.exists() {
            let meta_text = std::fs::read_to_string(&s.meta)?;
            let meta: CachedMeta = serde_json::from_str(&meta_text)?;
            let calculated: AggregationConfigParams = meta.calculated.into();

            // Build a "post-keygen shape" circuit so `circuit.params()`
            // returns `calculated`. Only `params()` is consulted by
            // `gen_pk`'s PK-file read path -- no synthesis runs. We omit
            // `expose_previous_instances` / `calculate_params` deliberately:
            // both `num_instance` and `break_points` are recovered from
            // `meta` so re-deriving them here would be pure overhead.
            let circuit = AggregationCircuit::new::<SHPLONK>(
                CircuitBuilderStage::Keygen,
                calculated,
                agg_params,
                vec![inner_snark.clone()],
                config.universality,
            );
            let pk = gen_pk(agg_params, &circuit, Some(&s.pk));
            tracing::info!(
                target: "bridge_evm_aggregator::cache",
                stem = %s.pk.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
                "aggregator PK cache HIT"
            );
            return Ok(CachedKeygen {
                pk,
                break_points: meta.break_points,
                calculated,
                num_instance: meta.num_instance,
            });
        }
    }

    // ------------------------------------------------------------------
    // Cache miss: real keygen. Persist afterwards if `cache_dir = Some`.
    // ------------------------------------------------------------------
    let agg_config = AggregationConfigParams {
        degree: config.k_outer,
        lookup_bits: config.lookup_bits_outer,
        ..Default::default()
    };
    let mut keygen_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config,
        agg_params,
        vec![inner_snark.clone()],
        config.universality,
    );
    keygen_circuit.expose_previous_instances(false);
    let calculated = keygen_circuit.calculate_params(Some(10));
    let num_instance = keygen_circuit.num_instance();

    let pk = gen_pk(
        agg_params,
        &keygen_circuit,
        slots.as_ref().map(|s| s.pk.as_path()),
    );

    let break_points = keygen_circuit.break_points();
    drop(keygen_circuit);

    if let Some(s) = slots.as_ref() {
        let meta = CachedMeta {
            break_points: break_points.clone(),
            calculated: calculated.into(),
            num_instance: num_instance.clone(),
        };
        std::fs::write(&s.meta, serde_json::to_string_pretty(&meta)?)?;
        tracing::info!(
            target: "bridge_evm_aggregator::cache",
            stem = %s.pk.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
            "aggregator PK cache MISS -> persisted new slot"
        );
    }

    Ok(CachedKeygen {
        pk,
        break_points,
        calculated,
        num_instance,
    })
}
