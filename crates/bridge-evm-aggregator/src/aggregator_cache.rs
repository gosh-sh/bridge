//! Persistent PK cache for the outer SHPLONK aggregator keygen.
//!
//! `aggregate_and_prove` / `aggregate_inner` / `generate_yul_verifier` all
//! previously called `gen_pk(agg_params, &circuit, None)` on every
//! invocation, forcing a full outer keygen (~3-5 min at K=21 on M1-class
//! hardware, ~1 GB peak RAM). Every rerun of `export-inner-aggregator` and
//! every runtime call to `aggregate-proof` paid that cost from scratch.
//!
//! This module memoises the outer
//! `(pk, break_points, calculated, num_instance)` bundle on disk, keyed by a
//! **content hash** over the inputs that actually determine the outer PK:
//! `AggregatorConfig`, the SRS `s_g2` head, and `bincode(inner_snark.protocol)`
//! (a stable function of the inner VK).
//!
//! Cache slot layout under `<cache_dir>`:
//! - `<stem>.pk`         -- proving key (SDK's `RawBytes` format, ~800 MB @ K=21)
//! - `<stem>.meta.json`  -- break_points + calculated params + num_instance
//!
//! Slot stem: `<base_name>__v3__<content_hash:hex[..32]>`. `base_name` is
//! a human-readable label only — it is **not** trusted for correctness. Two
//! callers using the same `base_name` with distinct inner VKs, distinct
//! aggregator configs, or a distinct SRS produce distinct content hashes, so
//! they can never share a slot. Conversely, two callers producing identical
//! content share a slot even under different names (deemed acceptable — this
//! is the classical cache-dedup property).
//!
//! Format tag `v3` in the stem gates against silent breakage if the hash
//! preimage layout ever changes; bump to `v4` to invalidate all v3 slots.
//!
//! History:
//!
//! - v1 used a 64-bit `SipHash` of the protocol bytes alone plus trusted
//!   `base_name`; the SRS was **not** in the key. That meant a
//!   re-bootstrapped SRS or an unlucky hash collision under the same
//!   `base_name` could silently serve stale keys. The `bin/aggregate_proof.rs`
//!   byte-drift check would catch it before on-chain use, but the cache-side
//!   precondition is now enforced properly.
//! - v1→v2 widened the content hash to include the SRS `s_g2` head so a
//!   re-bootstrapped SRS never shares a slot with the old one.
//! - v2→v3 was bumped when `expose_vk_digest` was added inside the keygen
//!   circuit. The hash preimage layout itself did not change, but the outer
//!   PK, `calculated` params, and `num_instance` all did — so pre-v3 cache
//!   slots produce a Yul verifier that no longer matches the on-chain
//!   adapters and must not be reused. The content hash cannot notice this
//!   because it is computed from the inner-snark protocol, not the outer
//!   circuit synthesis; the stem tag is the guard.

use std::{
    path::{Path, PathBuf},
};

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        halo2curves::{bn256::{Bn256, G1Affine}, serde::SerdeObject},
        plonk::ProvingKey,
        poly::kzg::commitment::ParamsKZG,
    },
};
use sha2::{Digest, Sha256};
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

fn universality_byte(u: VerifierUniversality) -> u8 {
    match u {
        VerifierUniversality::None => 0,
        VerifierUniversality::PreprocessedAsWitness => 1,
        VerifierUniversality::Full => 2,
    }
}

/// SRS `s_g2` bytes — the public MPC-ceremony fingerprint. Fresh SRS ⇒ fresh
/// bytes ⇒ fresh cache slot. Same encoding path as `srs_guard::assert_hermez_ceremony`.
fn srs_s_g2_bytes(params: &ParamsKZG<Bn256>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(128);
    params
        .s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    buf
}

fn snark_protocol_bytes(inner_snark: &Snark) -> Vec<u8> {
    bincode::serialize(&inner_snark.protocol)
        .expect("PlonkProtocol serialization is infallible for well-formed snarks")
}

/// Content hash — SHA-256 over the domain-tagged, length-prefixed concatenation
/// of every input that determines the outer PK. Two calls collide iff the
/// caller passed identical config + SRS + inner-snark protocol bytes.
///
/// Preimage layout (all little-endian):
/// ```text
///   b"bridge-evm-aggregator-cache-v3"
///   u32(k_outer) || u32(lookup_bits_outer) || u8(universality)
///   u32(s_g2_len)     || s_g2_bytes
///   u32(protocol_len) || protocol_bytes
/// ```
fn content_hash(
    config: &AggregatorConfig,
    s_g2_bytes: &[u8],
    protocol_bytes: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"bridge-evm-aggregator-cache-v3");
    h.update((config.k_outer as u32).to_le_bytes());
    h.update((config.lookup_bits_outer as u32).to_le_bytes());
    h.update([universality_byte(config.universality)]);
    h.update((s_g2_bytes.len() as u32).to_le_bytes());
    h.update(s_g2_bytes);
    h.update((protocol_bytes.len() as u32).to_le_bytes());
    h.update(protocol_bytes);
    h.finalize().into()
}

/// Deterministic slot stem: `<base_name>__v3__<content_hash[..32]>`.
///
/// `base_name` is a human-readable label only. Correctness is enforced by the
/// content hash — see module-level docs.
pub fn cache_stem(
    base_name: &str,
    config: &AggregatorConfig,
    srs: &ParamsKZG<Bn256>,
    inner_snark: &Snark,
) -> String {
    let hash = content_hash(
        config,
        &srs_s_g2_bytes(srs),
        &snark_protocol_bytes(inner_snark),
    );
    // 128 bits of the 256-bit digest keeps filenames short; a full collision
    // there is still infeasible and the byte-drift check in aggregate_proof
    // provides defence in depth.
    format!("{base_name}__v3__{}", hex::encode(&hash[..16]))
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
            let stem = cache_stem(base_name, &config, agg_params, inner_snark);
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
    // Bind the inner-circuit VK. Must run before `calculate_params` /
    // `num_instance` so the extra Poseidon gates are counted in the
    // auto-config and the persisted `num_instance` reflects the +1
    // exposed instance.
    crate::vk_binding::expose_vk_digest(&mut keygen_circuit);
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

#[cfg(test)]
mod tests {
    //! Regression tests for the content-addressed cache key. These exercise
    //! `content_hash` directly with hand-crafted bytes — building a real
    //! `Snark` + `ParamsKZG` is prohibitively expensive for a unit test, and
    //! the correctness properties Sergey flagged live entirely at the
    //! hash-preimage layer.
    use super::*;

    fn base_cfg() -> AggregatorConfig {
        AggregatorConfig::default()
    }

    #[test]
    fn content_hash_is_deterministic() {
        let cfg = base_cfg();
        let s_g2 = b"srs-s-g2-bytes";
        let proto = b"protocol-bytes";
        assert_eq!(content_hash(&cfg, s_g2, proto), content_hash(&cfg, s_g2, proto));
    }

    #[test]
    fn content_hash_diverges_on_protocol_change() {
        let cfg = base_cfg();
        let s_g2 = b"srs-s-g2-bytes";
        let a = content_hash(&cfg, s_g2, b"protocol-A");
        let b = content_hash(&cfg, s_g2, b"protocol-B");
        assert_ne!(a, b, "distinct inner protocols must not share a cache slot");
    }

    #[test]
    fn content_hash_diverges_on_srs_change() {
        // The v1 SipHash-only key omitted SRS entirely. A re-bootstrapped SRS
        // under an unchanged base_name would silently serve stale keys. The
        // v2 key must catch this.
        let cfg = base_cfg();
        let proto = b"protocol-bytes";
        let hermez_head = content_hash(&cfg, b"hermez-srs", proto);
        let toxic_head = content_hash(&cfg, b"toxic-waste-srs", proto);
        assert_ne!(
            hermez_head, toxic_head,
            "distinct SRS ceremonies must not share a cache slot"
        );
    }

    #[test]
    fn content_hash_diverges_on_config_change() {
        let s_g2 = b"srs-s-g2-bytes";
        let proto = b"protocol-bytes";
        let base = content_hash(&base_cfg(), s_g2, proto);

        let mut c = base_cfg();
        c.k_outer = base_cfg().k_outer + 1;
        assert_ne!(base, content_hash(&c, s_g2, proto), "k_outer must be in key");

        let mut c = base_cfg();
        c.lookup_bits_outer = base_cfg().lookup_bits_outer + 1;
        assert_ne!(base, content_hash(&c, s_g2, proto), "lookup_bits_outer must be in key");

        let mut c = base_cfg();
        c.universality = match c.universality {
            VerifierUniversality::None => VerifierUniversality::Full,
            _ => VerifierUniversality::None,
        };
        assert_ne!(base, content_hash(&c, s_g2, proto), "universality must be in key");
    }

    #[test]
    fn content_hash_length_prefix_prevents_boundary_ambiguity() {
        // Without length prefixes, ("ab", "cd") and ("a", "bcd") would hash
        // identically. The v2 preimage encodes u32-le length before each
        // variable-length field.
        let cfg = base_cfg();
        let a = content_hash(&cfg, b"ab", b"cd");
        let b = content_hash(&cfg, b"a", b"bcd");
        assert_ne!(a, b, "field lengths must be encoded to disambiguate boundaries");
    }
}
