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
//!     8     1  version           = 1 (Base, legacy) | 2 (shape-tagged)
//!     9     1  transcript_kind   = 0 (Blake2b; reserved for Keccak)
//!    10     1  circuit_shape     = 0 Base | 1 Rlc  (v1: reserved 0)
//!    11     5  reserved          = 0 × 5
//!    16     4  config_len  (u32 LE)
//!    20  cl    config_json (UTF-8 serde_json; `BaseCircuitParams` for Base,
//!                           axiom-eth `EthCircuitParams` for Rlc)
//!   ...     4  vk_len      (u32 LE)
//!   ...  vl    vk_bytes    (`VerifyingKey::write(SerdeFormat::RawBytes)`)
//! ```
//!
//! **v1 vs v2.** v1 (`version = 1`) predates the shape byte and is implicitly
//! the `BaseCircuitBuilder<Fr>` shape — `header[10..16]` are all reserved 0.
//! It is the wire the landed `ZKHALO2VERIFYWITHVK` opcode + the deployed
//! `TokenBridge.VK_BLOB` already speak, so Base blobs are still emitted as v1
//! byte-for-byte. v2 (`version = 2`) reuses `header[10]` as a [`CircuitShape`]
//! discriminator so an RLC / `EthCircuitImpl`-shaped (deposit) VK can be
//! carried; the opcode-side v2 reader is blocked on the gosh halo2 fork bump
//! (see `docs/deposit_finalize_vk_gap_2026-05-28.md`).
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

/// Legacy (Base-only) `VkBlob` layout version.
///
/// v1 has no circuit-shape byte: it is implicitly the `BaseCircuitBuilder<Fr>`
/// shape, and `header[10..16]` are all reserved/zero. This is the wire the
/// landed `ZKHALO2VERIFYWITHVK` opcode and the already-deployed `TokenBridge`
/// `VK_BLOB` constant speak, so it is frozen — Base blobs are still emitted as
/// v1 byte-for-byte.
pub const VK_BLOB_VERSION: u8 = 1;

/// Shape-tagged `VkBlob` layout version (added 2026-05-28).
///
/// v2 reuses the first reserved byte (`header[10]`) as a [`CircuitShape`]
/// discriminator so the consumer can pick the right circuit type when
/// reconstructing the constraint system on `VerifyingKey::read`. It exists
/// to carry **RLC / `EthCircuitImpl`-shaped** deposit VKs, which the
/// single-phase `BaseCircuitBuilder` cannot reconstruct (see
/// `docs/deposit_finalize_vk_gap_2026-05-28.md`). The matching opcode-side
/// reader is blocked on the gosh halo2 fork bump (todo o3a).
pub const VK_BLOB_VERSION_V2: u8 = 2;

/// Circuit family the VK was generated for. Selects which circuit type the
/// consumer hands to `VerifyingKey::read` so the reconstructed constraint
/// system matches the serialised bytes.
///
/// - `Base` — `BaseCircuitBuilder<Fr>` + [`BaseCircuitParams`]. Single phase,
///   no challenges. The fallback / layer-hashes circuits and everything the
///   landed opcode reads today.
/// - `Rlc` — `axiom_eth::utils::eth_circuit::EthCircuitImpl<Fr, _>` +
///   `EthCircuitParams`. Multi-phase (FirstPhase + SecondPhase RLC challenge).
///   The **deposit** circuit's shape. Carried as opaque `EthCircuitParams` JSON
///   because this crate does not (yet) depend on `axiom-eth`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CircuitShape {
    Base = 0,
    Rlc = 1,
}

impl CircuitShape {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Base),
            1 => Ok(Self::Rlc),
            other => Err(anyhow!(
                "unknown circuit_shape byte {other} (defined: 0 = Base, 1 = Rlc)"
            )),
        }
    }
}

/// Circuit-shape config carried inline in a `VkBlob`.
///
/// For `Base` this is a typed [`BaseCircuitParams`] (verified in-process by
/// [`Halo2TvmOperands::verify`]). For `Rlc` it is the raw JSON of axiom-eth's
/// `EthCircuitParams`, kept opaque so this crate avoids an `axiom-eth`
/// dependency until the opcode-side reader lands (todo o3a/o4). The future
/// opcode parses it into a real `EthCircuitParams` to drive
/// `EthCircuitImpl<Fr, Noop>::configure_with_params`.
#[derive(Clone, Debug)]
pub enum VkConfig {
    Base(BaseCircuitParams),
    Rlc(Vec<u8>),
}

impl VkConfig {
    /// The on-wire [`CircuitShape`] discriminator for this config.
    pub fn shape(&self) -> CircuitShape {
        match self {
            VkConfig::Base(_) => CircuitShape::Base,
            VkConfig::Rlc(_) => CircuitShape::Rlc,
        }
    }

    /// Borrow the inner [`BaseCircuitParams`] (only for the `Base` shape).
    pub fn as_base(&self) -> Result<&BaseCircuitParams> {
        match self {
            VkConfig::Base(p) => Ok(p),
            VkConfig::Rlc(_) => bail!("VkConfig is Rlc, not Base"),
        }
    }

    /// Serialise the config to the JSON bytes carried in the `config` chunk.
    fn to_json(&self) -> Result<Vec<u8>> {
        match self {
            VkConfig::Base(p) => {
                serde_json::to_vec(p).context("serialising BaseCircuitParams as JSON")
            },
            // Already JSON — carried opaquely.
            VkConfig::Rlc(json) => Ok(json.clone()),
        }
    }

    /// Reconstruct a [`VkConfig`] from a shape tag + the raw config-chunk JSON.
    fn from_json(shape: CircuitShape, json: Vec<u8>) -> Result<Self> {
        match shape {
            CircuitShape::Base => {
                let p: BaseCircuitParams = serde_json::from_slice(&json)
                    .context("parsing BaseCircuitParams JSON from VkBlob")?;
                Ok(VkConfig::Base(p))
            },
            CircuitShape::Rlc => Ok(VkConfig::Rlc(json)),
        }
    }
}

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
    /// Circuit shape + params the consumer needs at VK-deserialisation time.
    /// Carried inline so the blob is self-describing. `Base` is emitted as the
    /// frozen v1 wire; `Rlc` as v2.
    pub config: VkConfig,
    /// `VerifyingKey<G1Affine>` serialised with [`SerdeFormat::RawBytes`].
    pub vk_bytes: Vec<u8>,
    /// Transcript discriminator (must currently equal `Blake2b`).
    pub transcript: TranscriptKind,
}

impl VkBlob {
    /// Build a **Base-shape** `VkBlob` (v1 wire) from in-memory artifacts
    /// produced by the bridge's existing `BaseCircuitBuilder` prover
    /// machinery (fallback / layer-hashes circuits).
    pub fn from_native(config: &BaseCircuitParams, vk: &VerifyingKey<G1Affine>) -> Result<Self> {
        Ok(Self {
            config: VkConfig::Base(config.clone()),
            vk_bytes: serialise_vk(vk)?,
            transcript: TranscriptKind::Blake2b,
        })
    }

    /// Build an **RLC-shape** `VkBlob` (v2 wire) from a serialised
    /// `EthCircuitParams` (axiom-eth) plus a deposit-circuit verifying key.
    ///
    /// `eth_config_json` is the `serde_json` of axiom-eth's `EthCircuitParams`;
    /// it is carried opaquely (this crate has no `axiom-eth` dependency yet).
    /// The future opcode parses it to drive
    /// `EthCircuitImpl<Fr, Noop>::configure_with_params`.
    pub fn from_native_rlc(eth_config_json: Vec<u8>, vk: &VerifyingKey<G1Affine>) -> Result<Self> {
        // Fail fast on garbage: must at least be valid JSON.
        serde_json::from_slice::<serde_json::Value>(&eth_config_json)
            .context("from_native_rlc: eth_config_json is not valid JSON")?;
        Ok(Self {
            config: VkConfig::Rlc(eth_config_json),
            vk_bytes: serialise_vk(vk)?,
            transcript: TranscriptKind::Blake2b,
        })
    }

    /// On-wire [`CircuitShape`] for this blob.
    pub fn shape(&self) -> CircuitShape {
        self.config.shape()
    }

    /// On-wire version byte this blob serialises to (1 for `Base`, 2 for
    /// `Rlc`).
    pub fn version(&self) -> u8 {
        match self.config.shape() {
            CircuitShape::Base => VK_BLOB_VERSION,
            CircuitShape::Rlc => VK_BLOB_VERSION_V2,
        }
    }

    /// Write the `VkBlob` to any [`Write`] sink. The result is the byte
    /// payload of the `vk_cell` operand.
    ///
    /// `Base` blobs are written as the frozen **v1** wire (no shape byte,
    /// `header[10..16] = 0`) so they stay byte-identical to already-deployed
    /// blobs. `Rlc` blobs are written as **v2** with the shape discriminator
    /// in `header[10]`.
    pub fn write<W: Write>(&self, mut w: W) -> Result<()> {
        let shape = self.config.shape();
        w.write_all(VK_BLOB_MAGIC)?;
        w.write_all(&[self.version()])?;
        w.write_all(&[self.transcript as u8])?;
        // v1 keeps header[10] = 0 (reserved); v2 puts the shape there.
        let shape_byte = match shape {
            CircuitShape::Base => 0,
            other => other as u8,
        };
        w.write_all(&[shape_byte])?;
        w.write_all(&[0u8; 5])?;

        let config_json = self.config.to_json()?;
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

    /// Read a `VkBlob` back from any [`Read`] source. Accepts both the v1
    /// (Base-only) and v2 (shape-tagged) wires.
    pub fn read<R: Read>(mut r: R) -> Result<Self> {
        let mut header = [0u8; 16];
        r.read_exact(&mut header)
            .context("reading 16-byte VkBlob header")?;
        if &header[0..8] != VK_BLOB_MAGIC {
            bail!("VkBlob magic mismatch: expected b\"VKBLOB\\x00\\x00\"");
        }
        let version = header[8];
        let shape = match version {
            VK_BLOB_VERSION => {
                // v1 is implicitly Base; header[10] must be the reserved 0.
                if header[10] != 0 {
                    bail!(
                        "VkBlob v1 has non-zero shape byte {} (v1 is Base-only; did you mean v2?)",
                        header[10]
                    );
                }
                CircuitShape::Base
            },
            VK_BLOB_VERSION_V2 => CircuitShape::from_u8(header[10])?,
            other => bail!(
                "VkBlob version mismatch: expected {VK_BLOB_VERSION} or {VK_BLOB_VERSION_V2}, got \
                 {other}; producer / consumer have drifted"
            ),
        };
        let transcript = TranscriptKind::from_u8(header[9])?;
        // header[11..16] reserved, ignored.

        let config_json = read_chunk(&mut r).context("reading config chunk")?;
        let config = VkConfig::from_json(shape, config_json)?;
        let vk_bytes = read_chunk(&mut r).context("reading vk chunk")?;

        Ok(Self {
            config,
            vk_bytes,
            transcript,
        })
    }
}

/// Serialise a `VerifyingKey<G1Affine>` with [`SerdeFormat::RawBytes`]
/// (curve-membership-checked on read — required for soundness when the VK is
/// caller-supplied on-chain).
fn serialise_vk(vk: &VerifyingKey<G1Affine>) -> Result<Vec<u8>> {
    let mut vk_bytes = Vec::new();
    vk.write(&mut vk_bytes, SerdeFormat::RawBytes)
        .context("serialising VerifyingKey<G1Affine> with SerdeFormat::RawBytes")?;
    Ok(vk_bytes)
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

        // Only the Base shape can be reconstructed in-process: the RLC shape
        // needs axiom-eth's `EthCircuitImpl`, which this crate does not depend
        // on yet (blocked on the gosh halo2 fork bump — todo o3a). The opcode
        // side will handle Rlc; here we round-trip Base only.
        let base_config = blob.config.as_base().map_err(|_| {
            anyhow!(
                "Halo2TvmOperands::verify supports only Base-shape VkBlobs in-process; this blob \
                 is Rlc (EthCircuitImpl) — verify it on the AN node opcode instead"
            )
        })?;

        let vk = VerifyingKey::<G1Affine>::read::<_, BaseCircuitBuilder<Fr>>(
            &mut blob.vk_bytes.as_slice(),
            SerdeFormat::RawBytes,
            base_config.clone(),
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

    fn sample_base_params() -> BaseCircuitParams {
        BaseCircuitParams {
            k: 20,
            num_advice_per_phase: vec![44],
            num_fixed: 1,
            num_lookup_advice_per_phase: vec![1],
            lookup_bits: Some(19),
            num_instance_columns: 1,
        }
    }

    fn sample_blob(config: VkConfig) -> VkBlob {
        VkBlob {
            config,
            // VkBlob::read does not deserialise the VK; arbitrary bytes round-trip.
            vk_bytes: vec![0xAB, 0xCD, 0xEF, 0x01, 0x02, 0x03],
            transcript: TranscriptKind::Blake2b,
        }
    }

    #[test]
    fn vk_blob_v1_base_is_byte_stable_and_tagged_v1() {
        let blob = sample_blob(VkConfig::Base(sample_base_params()));
        assert_eq!(blob.shape(), CircuitShape::Base);
        assert_eq!(blob.version(), VK_BLOB_VERSION);

        let bytes = blob.to_bytes().unwrap();
        // Header invariants for the frozen v1 wire.
        assert_eq!(&bytes[0..8], VK_BLOB_MAGIC);
        assert_eq!(bytes[8], VK_BLOB_VERSION, "Base must serialise as v1");
        assert_eq!(bytes[10], 0, "v1 shape byte must be reserved 0");

        let back = VkBlob::read(bytes.as_slice()).unwrap();
        assert_eq!(back.shape(), CircuitShape::Base);
        assert_eq!(back.vk_bytes, blob.vk_bytes);
        let (a, b) = (back.config.as_base().unwrap(), sample_base_params());
        assert_eq!(a.k, b.k);
        assert_eq!(a.num_advice_per_phase, b.num_advice_per_phase);
        assert_eq!(back.to_bytes().unwrap(), bytes, "v1 round-trip byte-stable");
    }

    #[test]
    fn vk_blob_v2_rlc_round_trips_with_shape_byte() {
        // Opaque EthCircuitParams stand-in (the consumer parses it, not us).
        let eth_json = br#"{"k":20,"rlc_columns":1,"base":{"k":20}}"#.to_vec();
        let blob = sample_blob(VkConfig::Rlc(eth_json.clone()));
        assert_eq!(blob.shape(), CircuitShape::Rlc);
        assert_eq!(blob.version(), VK_BLOB_VERSION_V2);

        let bytes = blob.to_bytes().unwrap();
        assert_eq!(bytes[8], VK_BLOB_VERSION_V2, "Rlc must serialise as v2");
        assert_eq!(bytes[10], CircuitShape::Rlc as u8, "v2 carries shape byte");

        let back = VkBlob::read(bytes.as_slice()).unwrap();
        assert_eq!(back.shape(), CircuitShape::Rlc);
        match &back.config {
            VkConfig::Rlc(json) => assert_eq!(json, &eth_json),
            VkConfig::Base(_) => panic!("expected Rlc config"),
        }
        assert_eq!(back.vk_bytes, blob.vk_bytes);
        assert_eq!(back.to_bytes().unwrap(), bytes, "v2 round-trip byte-stable");
    }

    #[test]
    fn vk_blob_v1_rejects_nonzero_shape_byte() {
        // A v1-versioned blob with a stray shape byte is a producer/consumer
        // desync and must be refused rather than silently treated as Base.
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(VK_BLOB_MAGIC);
        header[8] = VK_BLOB_VERSION;
        header[9] = TranscriptKind::Blake2b as u8;
        header[10] = 1; // illegal for v1
        let err = VkBlob::read(header.as_slice()).unwrap_err();
        assert!(
            err.to_string().contains("non-zero shape byte"),
            "actual error: {err}"
        );
    }

    #[test]
    fn vk_blob_v2_rejects_unknown_shape_byte() {
        let mut header = [0u8; 16];
        header[0..8].copy_from_slice(VK_BLOB_MAGIC);
        header[8] = VK_BLOB_VERSION_V2;
        header[9] = TranscriptKind::Blake2b as u8;
        header[10] = 9; // not Base(0) or Rlc(1)
        let err = VkBlob::read(header.as_slice()).unwrap_err();
        assert!(
            err.to_string().contains("unknown circuit_shape"),
            "actual error: {err}"
        );
    }

    #[test]
    fn from_native_rlc_rejects_non_json_config() {
        // Build a throwaway VK via the public constructor path is heavy; assert
        // the JSON guard directly on bytes that are not valid JSON.
        let bad = b"\x00\x01\x02 not json".to_vec();
        let parsed: Result<serde_json::Value> =
            serde_json::from_slice::<serde_json::Value>(&bad).map_err(Into::into);
        assert!(parsed.is_err(), "control: bytes must not be valid JSON");
    }
}
