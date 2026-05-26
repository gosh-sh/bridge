//! Producer-side wire format for the AN `ZKHALO2VERIFYWITHVK` opcode.
//!
//! This module is the **producer counterpart** of the
//! `ZKHALO2VERIFYWITHVK` TVM opcode landed in `tvm-sdk` `main` (see PR
//! #243). The opcode takes **three stack operands** — not one bundle —
//! and this module emits them.
//!
//! ## Stack ABI (Variant A, frozen 2026-05-25)
//!
//! ```text
//!   top      proof_cell           raw SHPLONK proof bytes (no header)
//!   ↑        public_inputs_cell   raw Fr × N (strict 32-byte LE, no header)
//!   bottom   vk_cell              VkBlob (magic "VKBLOB\0\0" + version
//!                                  + transcript + config_json + vk_bytes)
//! ```
//!
//! Assembly snippet:
//!
//! ```text
//!   PUSHREF vk_cell
//!   PUSHREF public_inputs_cell
//!   PUSHREF proof_cell
//!   ZKHALO2VERIFYWITHVK
//! ```
//!
//! The producer side builds:
//!
//! 1. **`VkBlob` payload** — magic-tagged, versioned, self-describing (carries
//!    `BaseCircuitParams` JSON inline so the consumer doesn't need any
//!    out-of-band schema). One blob per circuit, expected to be deployed once
//!    into the verifier contract's `c4`/storage.
//! 2. **`public_inputs` payload** — bare `N × 32` LE `Fr::to_repr()`, no
//!    header. The contract assembles this O(1) on the hot path from the call
//!    arguments.
//! 3. **`proof` payload** — bare SHPLONK proof bytes (Blake2b transcript), no
//!    header. Comes straight from `Blake2bWrite::finalize()`.
//!
//! ## `VkBlob` byte layout
//!
//! ```text
//!   off  size  field
//!   ───  ────  ─────────────────────────────────────────────────────────
//!     0     8  magic = b"VKBLOB\x00\x00"
//!     8     1  version           = 1
//!     9     1  transcript_kind   = 0 (Blake2b; reserved for Keccak)
//!    10     6  reserved          = 0 × 6
//!    16     4  config_len  (u32 LE)
//!    20  cl    config_json (UTF-8 serde_json of `BaseCircuitParams`)
//!   ...     4  vk_len      (u32 LE)
//!   ...  vl    vk_bytes    (`VerifyingKey::write(SerdeFormat::RawBytes)`)
//! ```
//!
//! All length prefixes are `u32` LE because (a) VKs are always well
//! under 4 GB and (b) it keeps the parser branch-free vs varints.
//!
//! ## Strict 32-byte LE `Fr`
//!
//! Per Q-WIRE-3, the `public_inputs` payload mandates **strict 32-byte
//! LE `Fr`** encoding for every public input. There is no u64
//! shortcut. This makes the wire format unambiguous when an address
//! (160 bits) happens to have a 24-zero-byte prefix.
//!
//! ## Safety of VK deserialisation
//!
//! VK is written with [`SerdeFormat::RawBytes`] (NOT `RawBytesUnchecked`).
//! `RawBytes` runs the curve membership check on every group element
//! on read, which is required for soundness when consuming a
//! caller-supplied VK on-chain. The producer side intentionally
//! trades a few hundred ms of serialise time for safety on the
//! consumer side.

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

/// 8-byte ASCII magic at offset 0 of every `VkBlob` payload.
pub const VK_BLOB_MAGIC: &[u8; 8] = b"VKBLOB\x00\x00";

/// Current `VkBlob` layout version. Bump on any breaking change.
pub const VK_BLOB_VERSION: u8 = 1;

/// Transcript flavour for the proof bytes.
///
/// The on-chain Acki Nacki opcode (`ZKHALO2VERIFYWITHVK`, Variant A) commits
/// to Blake2b. The discriminator byte exists so a future Keccak variant
/// could be added without breaking already-emitted blobs.
///
/// `Poseidon = 2` was added in 2026-05-27 (R15 milestone M3) for the
/// ETH-side aggregator pipeline (`crates/bridge-evm-aggregator/`):
/// inner SNARKs that feed `snark-verifier-sdk::AggregationCircuit` MUST use
/// Poseidon. This variant is intentionally NOT accepted by the
/// `ZKHALO2VERIFYWITHVK` opcode — see [`VkBlob::write`] / verifier checks.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    Blake2b = 0,
    // 1 is reserved for Keccak (EvmTranscript).
    Poseidon = 2,
}

impl TranscriptKind {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Blake2b),
            2 => Ok(Self::Poseidon),
            other => Err(anyhow!(
                "unknown transcript_kind byte {other} (defined: 0 = Blake2b, 2 = Poseidon)"
            )),
        }
    }
}

/// Payload of the `vk_cell` stack operand — circuit shape + verifying
/// key, magic-tagged and versioned so the consumer can refuse a
/// drifted producer loudly.
#[derive(Clone, Debug)]
pub struct VkBlob {
    /// Circuit shape that `BaseCircuitBuilder` needs at VK-deserialisation
    /// time. Carried inline so the blob is self-describing.
    pub config: BaseCircuitParams,
    /// `VerifyingKey<G1Affine>` serialised with [`SerdeFormat::RawBytes`].
    pub vk_bytes: Vec<u8>,
    /// Transcript discriminator (must currently equal `Blake2b`).
    pub transcript: TranscriptKind,
}

impl VkBlob {
    /// Build a `VkBlob` from in-memory artifacts produced by the bridge's
    /// existing prover machinery.
    pub fn from_native(config: &BaseCircuitParams, vk: &VerifyingKey<G1Affine>) -> Result<Self> {
        let mut vk_bytes = Vec::new();
        vk.write(&mut vk_bytes, SerdeFormat::RawBytes)
            .context("serialising VerifyingKey<G1Affine> with SerdeFormat::RawBytes")?;
        Ok(Self {
            config: config.clone(),
            vk_bytes,
            transcript: TranscriptKind::Blake2b,
        })
    }

    /// Write the `VkBlob` to any [`Write`] sink. The result is the byte
    /// payload of the `vk_cell` operand.
    pub fn write<W: Write>(&self, mut w: W) -> Result<()> {
        w.write_all(VK_BLOB_MAGIC)?;
        w.write_all(&[VK_BLOB_VERSION])?;
        w.write_all(&[self.transcript as u8])?;
        w.write_all(&[0u8; 6])?;

        let config_json =
            serde_json::to_vec(&self.config).context("serialising BaseCircuitParams as JSON")?;
        write_chunk(&mut w, &config_json)?;
        write_chunk(&mut w, &self.vk_bytes)?;
        Ok(())
    }

    /// Serialise to an owned byte vector. Convenience wrapper around
    /// [`Self::write`].
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write(&mut out)?;
        Ok(out)
    }

    /// Read a `VkBlob` back from any [`Read`] source.
    pub fn read<R: Read>(mut r: R) -> Result<Self> {
        let mut header = [0u8; 16];
        r.read_exact(&mut header)
            .context("reading 16-byte VkBlob header")?;
        if &header[0..8] != VK_BLOB_MAGIC {
            bail!("VkBlob magic mismatch: expected b\"VKBLOB\\x00\\x00\"");
        }
        let version = header[8];
        if version != VK_BLOB_VERSION {
            bail!(
                "VkBlob version mismatch: expected {VK_BLOB_VERSION}, got {version}; producer / \
                 consumer have drifted"
            );
        }
        let transcript = TranscriptKind::from_u8(header[9])?;
        // header[10..16] reserved, ignored.

        let config_json = read_chunk(&mut r).context("reading config chunk")?;
        let config: BaseCircuitParams = serde_json::from_slice(&config_json)
            .context("parsing BaseCircuitParams JSON from VkBlob")?;
        let vk_bytes = read_chunk(&mut r).context("reading vk chunk")?;

        Ok(Self {
            config,
            vk_bytes,
            transcript,
        })
    }
}

/// All three stack operands of the `ZKHALO2VERIFYWITHVK` opcode, ready
/// to be loaded into TVM cells.
///
/// Construct via [`Halo2TvmOperands::from_native`] (producer side), and
/// verify the same `(vk, instances, proof)` triple in-process with
/// [`Halo2TvmOperands::verify`] before shipping to the AN VM.
#[derive(Clone, Debug)]
pub struct Halo2TvmOperands {
    /// Byte payload of the `vk_cell` operand — a serialised [`VkBlob`].
    pub vk_blob: Vec<u8>,
    /// Byte payload of the `public_inputs_cell` operand — raw `N × 32`
    /// LE `Fr::to_repr()` (strict, no header).
    pub public_inputs: Vec<u8>,
    /// Byte payload of the `proof_cell` operand — raw SHPLONK proof
    /// bytes from `Blake2bWrite::finalize()` (no header).
    pub proof: Vec<u8>,
}

impl Halo2TvmOperands {
    /// Build the three operand byte streams from in-memory artifacts.
    pub fn from_native(
        config: &BaseCircuitParams,
        vk: &VerifyingKey<G1Affine>,
        instances: &[Fr],
        proof_bytes: Vec<u8>,
    ) -> Result<Self> {
        let blob = VkBlob::from_native(config, vk)?;
        let vk_blob = blob.to_bytes()?;
        let public_inputs = encode_instances(instances);
        Ok(Self {
            vk_blob,
            public_inputs,
            proof: proof_bytes,
        })
    }

    /// Reassemble the three byte streams into a live `(vk, instances)`
    /// pair and run `verify_proof::<KZG, VerifierSHPLONK, _, Blake2bRead,
    /// SingleStrategy>` against `srs.verifier_params()`.
    ///
    /// Mirrors the on-chain handler so the producer can round-trip a
    /// proof end-to-end without booting the AN VM.
    pub fn verify(&self, srs: &ParamsKZG<Bn256>) -> Result<bool> {
        let blob = VkBlob::read(self.vk_blob.as_slice())?;
        if blob.transcript != TranscriptKind::Blake2b {
            bail!(
                "VkBlob transcript {:?} is not supported by this verifier (Blake2b only in v1)",
                blob.transcript
            );
        }

        let vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut blob.vk_bytes.as_slice(),
            SerdeFormat::RawBytes,
            blob.config.clone(),
        )
        .context("deserialising VerifyingKey<G1Affine> from VkBlob")?;

        let instances = decode_instances(&self.public_inputs)?;
        let instance_refs: &[&[Fr]] = &[&instances];

        let verifier_params = srs.verifier_params();
        let strategy = SingleStrategy::new(srs);
        let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(self.proof.as_slice());
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

    /// Number of public inputs encoded in the operand bundle.
    pub fn num_instances(&self) -> usize {
        self.public_inputs.len() / 32
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
        let bytes = vec![0xFFu8; 32];
        let err = decode_instances(&bytes).unwrap_err();
        assert!(
            err.to_string().contains(">= modulus"),
            "actual error: {err}"
        );
    }

    #[test]
    fn vk_blob_header_magic_mismatch_rejected() {
        let bogus = vec![0u8; 16 + 4 + 4];
        let err = VkBlob::read(bogus.as_slice()).unwrap_err();
        assert!(err.to_string().contains("magic mismatch"));
    }

    #[test]
    fn vk_blob_version_mismatch_rejected() {
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(VK_BLOB_MAGIC);
        header[8] = 99;
        let err = VkBlob::read(header.as_slice()).unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn vk_blob_transcript_kind_unknown_rejected() {
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(VK_BLOB_MAGIC);
        header[8] = VK_BLOB_VERSION;
        header[9] = 7;
        let err = VkBlob::read(header.as_slice()).unwrap_err();
        assert!(err.to_string().contains("unknown transcript_kind"));
    }
}
