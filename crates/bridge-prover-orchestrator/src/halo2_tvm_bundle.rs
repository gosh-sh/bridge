//! Wire format for AN-side `ZKHALO2VERIFYWITHVK` opcode (Phase A artifact).
//!
//! This module is the **producer-side counterpart** of the proposed
//! `ZKHALO2VERIFYWITHVK` TVM opcode skeletoned in `tvm-sdk` on branch
//! `serhii/verhalo2shplonk-skeleton` (see
//! `docs/zk_halo2_an_side_design.md` for the full design memo and the
//! Q-WIRE-1..5 open questions).
//!
//! ## Scope
//!
//! - **Locks in the wire format** the producer (`bridge-prover-orchestrator`)
//!   and the consumer (`tvm_vm` Halo2 opcode) will agree on, so the AN partner
//!   team has a concrete byte layout to review during Phase A.
//! - **Round-trip verifiable** in [`tests/halo2_tvm_bundle_round_trip.rs`]:
//!   prove with `FallbackKeyManager`, serialise via [`Halo2TvmBundle`],
//!   deserialise via [`Halo2TvmBundle::read`], verify against the reconstructed
//!   `(vk, instances, proof)` triple. If the test passes the wire format is
//!   self-consistent.
//! - **Resolves Q-WIRE-4 Option B**: the VK envelope is self-describing
//!   (carries the `BaseCircuitParams` JSON inline) so the consumer doesn't need
//!   any out-of-band schema.
//!
//! ## Out of scope
//!
//! - **KZG SRS** is NOT carried in the bundle (Q-WIRE-2). The consumer is
//!   expected to source `ParamsKZG<Bn256>` from a chain-wide shared trusted
//!   setup keyed by `k = vk.cs.degree`. For the round-trip test we just re-run
//!   `gen_srs(K)`.
//! - **Cell layout** for the actual TVM stack (which TVM cells carry which byte
//!   ranges) — that's part of the opcode-side wiring and will be defined when
//!   Phase B of the roadmap lands.
//! - **Transcript discriminator** (Q-WIRE-1). The bundle commits to **Blake2b**
//!   SHPLONK transcript exclusively (matches the producer-side pattern in
//!   [`crate::verifier::verify_fallback_proof`] and the
//!   `gosh-zk-snark-halo2-utils` AN-side machinery). A `transcript_kind` byte
//!   in the header is reserved for a future Keccak variant if the AN team
//!   prefers a different default.
//!
//! ## Byte layout
//!
//! ```text
//!   off  size  field
//!   ───  ────  ─────────────────────────────────────────────────────────
//!     0     8  magic = b"HALO2TVM" (ASCII, no NUL)
//!     8     1  version           = 1
//!     9     1  transcript_kind   = 0 (Blake2b)
//!    10     6  reserved          = 0 × 6
//!    16     4  config_len  (u32 LE)
//!    20  cl    config_json (UTF-8 serde_json of `BaseCircuitParams`)
//!   ...     4  vk_len      (u32 LE)
//!   ...  vl    vk_bytes    (`VerifyingKey::write(SerdeFormat::RawBytes)`)
//!   ...     4  instances_len  (u32 LE; must be a multiple of 32)
//!   ...  il    instances_bytes (N × 32-byte LE `Fr::to_repr()`, strict)
//!   ...     4  proof_len   (u32 LE)
//!   ...  pl    proof_bytes (SHPLONK proof with Blake2b transcript)
//! ```
//!
//! All length prefixes are `u32` LE because (a) bundles are always < 4 GB
//! and (b) it keeps the parser branch-free vs varints.
//!
//! ## Strictness vs `ZKHALO2VERIFY`'s u64 shortcut
//!
//! Per Q-WIRE-3, this format mandates **strict 32-byte LE `Fr`** encoding
//! for every public input. There is no u64 shortcut. This makes the bundle
//! unambiguous when an address (160 bits) happens to have a 24-zero-byte
//! prefix.
//!
//! ## Safety of VK deserialisation
//!
//! VK is written with [`SerdeFormat::RawBytes`] (NOT `RawBytesUnchecked`).
//! `RawBytes` runs the curve membership check on every group element on
//! read, which is required for soundness when consuming a caller-supplied
//! VK on-chain. The producer side intentionally trades a few hundred ms of
//! serialise time for safety on the consumer side.

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Context, Result};
use halo2_base::{
    gates::circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
    halo2_proofs::{
        halo2curves::{
            bn256::{Bn256, Fr, G1Affine},
            ff::PrimeField,
        },
        plonk::{verify_proof, VerifyingKey},
        poly::{
            commitment::ParamsProver,
            kzg::{
                commitment::{KZGCommitmentScheme, ParamsKZG},
                multiopen::VerifierSHPLONK,
                strategy::SingleStrategy,
            },
        },
        transcript::{Blake2bRead, Challenge255, TranscriptReadBuffer},
        SerdeFormat,
    },
};

/// 8-byte ASCII magic at offset 0 of every bundle.
pub const BUNDLE_MAGIC: &[u8; 8] = b"HALO2TVM";

/// Current bundle layout version. Bump on any breaking change.
pub const BUNDLE_VERSION: u8 = 1;

/// Transcript flavour for the proof bytes.
///
/// Current opcode design (Phase A) commits to Blake2b. The discriminator
/// byte exists so a future Keccak variant could be added without breaking
/// already-emitted bundles.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    Blake2b = 0,
    // Reserved for Q-WIRE-1 resolution if the AN team prefers Keccak:
    // Keccak = 1,
}

impl TranscriptKind {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Blake2b),
            other => Err(anyhow!(
                "unknown transcript_kind byte {other} (only 0 = Blake2b is currently defined)"
            )),
        }
    }
}

/// VK + public inputs + proof in the wire format consumed by the proposed
/// `ZKHALO2VERIFYWITHVK` opcode.
///
/// Construct via [`Halo2TvmBundle::from_native`] (producer side), serialise
/// via [`Halo2TvmBundle::write`], read back via [`Halo2TvmBundle::read`],
/// and run the actual SHPLONK verification with [`Halo2TvmBundle::verify`].
#[derive(Clone, Debug)]
pub struct Halo2TvmBundle {
    /// Circuit shape that `BaseCircuitBuilder` needs at VK-deserialisation
    /// time. Carried inline so the bundle is self-describing (Q-WIRE-4 / B).
    pub config: BaseCircuitParams,
    /// `VerifyingKey<G1Affine>` serialised with [`SerdeFormat::RawBytes`].
    pub vk_bytes: Vec<u8>,
    /// `N × 32` flat little-endian `Fr::to_repr()`. Strictly LE, no u64
    /// shortcut.
    pub instances_bytes: Vec<u8>,
    /// SHPLONK proof bytes from `Blake2bWrite`.
    pub proof_bytes: Vec<u8>,
    /// Transcript discriminator (must currently equal `Blake2b`).
    pub transcript: TranscriptKind,
}

impl Halo2TvmBundle {
    /// Build a bundle from in-memory artifacts produced by the bridge's
    /// existing prover machinery.
    pub fn from_native(
        config: &BaseCircuitParams,
        vk: &VerifyingKey<G1Affine>,
        instances: &[Fr],
        proof_bytes: Vec<u8>,
    ) -> Result<Self> {
        let mut vk_bytes = Vec::new();
        vk.write(&mut vk_bytes, SerdeFormat::RawBytes)
            .context("serialising VerifyingKey<G1Affine> with SerdeFormat::RawBytes")?;

        let instances_bytes = encode_instances(instances);

        Ok(Self {
            config: config.clone(),
            vk_bytes,
            instances_bytes,
            proof_bytes,
            transcript: TranscriptKind::Blake2b,
        })
    }

    /// Write the bundle to any [`Write`] sink in the layout documented at
    /// the module level.
    pub fn write<W: Write>(&self, mut w: W) -> Result<()> {
        w.write_all(BUNDLE_MAGIC)?;
        w.write_all(&[BUNDLE_VERSION])?;
        w.write_all(&[self.transcript as u8])?;
        w.write_all(&[0u8; 6])?;

        let config_json =
            serde_json::to_vec(&self.config).context("serialising BaseCircuitParams as JSON")?;
        write_chunk(&mut w, &config_json)?;
        write_chunk(&mut w, &self.vk_bytes)?;
        write_chunk(&mut w, &self.instances_bytes)?;
        write_chunk(&mut w, &self.proof_bytes)?;
        Ok(())
    }

    /// Read a bundle back from any [`Read`] source.
    ///
    /// Validates the magic, version, transcript discriminator, and that
    /// `instances_bytes.len() % 32 == 0`. Does **not** validate the
    /// `vk_bytes` cryptographically — that happens inside [`Self::verify`]
    /// because deserialisation needs `config`, which is read here.
    pub fn read<R: Read>(mut r: R) -> Result<Self> {
        let mut header = [0u8; 16];
        r.read_exact(&mut header)
            .context("reading 16-byte bundle header")?;
        if &header[0..8] != BUNDLE_MAGIC {
            bail!("bundle magic mismatch: expected b\"HALO2TVM\"");
        }
        let version = header[8];
        if version != BUNDLE_VERSION {
            bail!(
                "bundle version mismatch: expected {BUNDLE_VERSION}, got {version}; producer / \
                 consumer have drifted"
            );
        }
        let transcript = TranscriptKind::from_u8(header[9])?;
        // header[10..16] reserved, ignored.

        let config_json = read_chunk(&mut r).context("reading config chunk")?;
        let config: BaseCircuitParams = serde_json::from_slice(&config_json)
            .context("parsing BaseCircuitParams JSON from bundle")?;
        let vk_bytes = read_chunk(&mut r).context("reading vk chunk")?;
        let instances_bytes = read_chunk(&mut r).context("reading instances chunk")?;
        if !instances_bytes.len().is_multiple_of(32) {
            bail!(
                "instances chunk length {} is not a multiple of 32 bytes (each public input must \
                 be a 32-byte LE Fr)",
                instances_bytes.len()
            );
        }
        let proof_bytes = read_chunk(&mut r).context("reading proof chunk")?;

        Ok(Self {
            config,
            vk_bytes,
            instances_bytes,
            proof_bytes,
            transcript,
        })
    }

    /// Reassemble the bundle into a live `(vk, instances)` pair and run
    /// `verify_proof::<KZG, VerifierSHPLONK, _, Blake2bRead, SingleStrategy>`
    /// against `srs.verifier_params()`.
    ///
    /// The `srs` argument represents the chain-wide shared trusted setup
    /// (Q-WIRE-2). In the TVM opcode this would be a static loaded once at
    /// VM startup and indexed by `k`; here the test scaffold passes it in
    /// directly.
    ///
    /// Returns `Ok(true)` on a valid proof, `Ok(false)` on a well-formed
    /// but invalid proof, and `Err(_)` on a structural failure (malformed
    /// VK bytes, malformed proof bytes, malformed Fr in `instances_bytes`).
    pub fn verify(&self, srs: &ParamsKZG<Bn256>) -> Result<bool> {
        if self.transcript != TranscriptKind::Blake2b {
            bail!(
                "bundle transcript {:?} is not supported by this verifier (Blake2b only in v1)",
                self.transcript
            );
        }

        let vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut self.vk_bytes.as_slice(),
            SerdeFormat::RawBytes,
            self.config.clone(),
        )
        .context("deserialising VerifyingKey<G1Affine> from bundle vk chunk")?;

        let instances = decode_instances(&self.instances_bytes)?;
        let instance_refs: &[&[Fr]] = &[&instances];

        let verifier_params = srs.verifier_params();
        let strategy = SingleStrategy::new(srs);
        let mut transcript =
            Blake2bRead::<_, _, Challenge255<_>>::init(self.proof_bytes.as_slice());
        Ok(verify_proof::<
            KZGCommitmentScheme<Bn256>,
            VerifierSHPLONK<'_, Bn256>,
            Challenge255<G1Affine>,
            Blake2bRead<&[u8], G1Affine, Challenge255<G1Affine>>,
            SingleStrategy<'_, Bn256>,
        >(
            verifier_params,
            &vk,
            strategy,
            &[instance_refs],
            &mut transcript,
        )
        .is_ok())
    }

    /// Number of public inputs encoded in the bundle.
    pub fn num_instances(&self) -> usize {
        self.instances_bytes.len() / 32
    }
}

/// Encode `&[Fr]` as flat little-endian bytes (32 bytes per element).
pub fn encode_instances(instances: &[Fr]) -> Vec<u8> {
    let mut out = Vec::with_capacity(instances.len() * 32);
    for fr in instances {
        out.extend_from_slice(fr.to_bytes().as_ref());
    }
    out
}

/// Decode a flat LE byte slice back into `Vec<Fr>` (strict; no u64 shortcut).
pub fn decode_instances(bytes: &[u8]) -> Result<Vec<Fr>> {
    if !bytes.len().is_multiple_of(32) {
        bail!(
            "instances byte length {} is not a multiple of 32",
            bytes.len()
        );
    }
    let mut out = Vec::with_capacity(bytes.len() / 32);
    for chunk in bytes.chunks_exact(32) {
        let mut repr = <Fr as PrimeField>::Repr::default();
        repr.as_mut().copy_from_slice(chunk);
        let fr = Fr::from_repr(repr);
        if fr.is_none().into() {
            bail!(
                "Fr::from_repr rejected a 32-byte chunk in instances (>= modulus): {:02x?}",
                chunk
            );
        }
        out.push(fr.unwrap());
    }
    Ok(out)
}

fn write_chunk<W: Write>(w: &mut W, bytes: &[u8]) -> Result<()> {
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| anyhow!("chunk length {} exceeds 4 GiB", bytes.len()))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

fn read_chunk<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut out = vec![0u8; len];
    r.read_exact(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn instances_round_trip_strict_le() {
        // Fr(1), Fr(2), Fr(3) round-trip exactly.
        let xs = vec![Fr::one(), Fr::from(2u64), Fr::from(3u64)];
        let bytes = encode_instances(&xs);
        assert_eq!(bytes.len(), 96);
        let decoded = decode_instances(&bytes).unwrap();
        assert_eq!(xs, decoded);
    }

    #[test]
    fn decode_rejects_non_multiple_of_32() {
        let bytes = vec![0u8; 31];
        let err = decode_instances(&bytes).unwrap_err();
        assert!(err.to_string().contains("not a multiple of 32"));
    }

    #[test]
    fn decode_rejects_out_of_range_fr() {
        // 0xFF × 32 is well past the Fr modulus and must be rejected.
        let bytes = vec![0xFFu8; 32];
        let err = decode_instances(&bytes).unwrap_err();
        assert!(
            err.to_string().contains(">= modulus"),
            "actual error: {err}"
        );
    }

    #[test]
    fn header_magic_mismatch_rejected() {
        let bogus = vec![0u8; 16 + 4 + 4 + 4 + 4];
        let err = Halo2TvmBundle::read(bogus.as_slice()).unwrap_err();
        assert!(err.to_string().contains("magic mismatch"));
    }

    #[test]
    fn version_mismatch_rejected() {
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(BUNDLE_MAGIC);
        header[8] = 99;
        let err = Halo2TvmBundle::read(header.as_slice()).unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn transcript_kind_unknown_rejected() {
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(BUNDLE_MAGIC);
        header[8] = BUNDLE_VERSION;
        header[9] = 7;
        let err = Halo2TvmBundle::read(header.as_slice()).unwrap_err();
        assert!(err.to_string().contains("unknown transcript_kind"));
    }
}
