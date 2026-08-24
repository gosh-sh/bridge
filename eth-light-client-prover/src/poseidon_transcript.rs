//! Native Poseidon-based Fiat–Shamir transcript for Halo2 SHPLONK proofs.
//!
//! **Vendored** (verbatim port) from
//! `crates/an-bridge-prover/bridge-prover-lib/src/transcript/poseidon.rs`. It is
//! the prover-side counterpart to `snark-verifier-sdk`'s
//! `PoseidonTranscript<NativeLoader, _>`, and produces the SAME proof bytes for
//! the same circuit + witness because it shares:
//!
//! - the exact `OptimizedPoseidonSpec` (`T=3, RATE=2, R_F=8, R_P=57,
//!   SECURE_MDS=0`) that lives in `halo2-base`'s `poseidon::hasher::spec` (the
//!   gosh fork and the axiom fork carry it verbatim, both from PSE upstream);
//! - the absorption order — scalars as one field element, EC points as
//!   `[fe_to_fe(x), fe_to_fe(y)]`, challenges squeezed by a buffered permutation;
//! - the on-wire encoding — scalars `PrimeField::to_repr`/`from_repr` (32 LE
//!   bytes), EC points `CurveAffine::to_bytes`/`from_bytes`.
//!
//! ## Why vendored (not a dependency)
//!
//! `eth-light-client-prover` pins the gosh `halo2-base` fork (bls12_381 chips).
//! `snark-verifier` declares axiom `halo2-base` as a non-optional dependency, so
//! it cannot coexist in this cargo unit — same reason
//! `bridge-prover-lib::transcript::poseidon` re-implements the ~80 lines of
//! native Poseidon permutation instead of depending on `snark-verifier`.
//!
//! ## What this is for
//!
//! The recursive rotate (M6) proves the committee SHA root in **shards** and
//! combines them in a snark-verifier `AggregationCircuit` living in
//! `crates/bridge-evm-aggregator/` (axiom `halo2-lib`). A shard proof produced
//! here with `PoseidonWrite` can be wrapped by
//! `bridge-snark-utils::export_poseidon_snark` into a `snark_verifier_sdk::Snark`
//! and fed to that aggregator without re-proving inside its workspace — the exact
//! R15 bridge that already carries Circuit-1A/1B/2/4 across the same gosh↔axiom
//! boundary. See `docs/m6_rotate_recursive.md`.
//!
//! Byte-for-byte compatibility with `snark-verifier-sdk` is what makes this work;
//! the constants below MUST equal snark-verifier-sdk's `T/RATE/R_F/R_P/SECURE_MDS`
//! or the aggregator's in-circuit verifier derives different challenges and the
//! inner SNARK fails to verify.

use std::{
    io::{self, Read, Write},
    mem,
};

use halo2_base::{
    halo2_proofs::{
        halo2curves::{
            bn256::{Fr, G1Affine},
            ff::{FromUniformBytes, PrimeField},
            group::GroupEncoding,
            CurveAffine,
        },
        transcript::{
            EncodedChallenge, Transcript, TranscriptRead, TranscriptReadBuffer, TranscriptWrite,
            TranscriptWriterBuffer,
        },
    },
    poseidon::hasher::{mds::SparseMDSMatrix, spec::OptimizedPoseidonSpec},
};
use num_bigint::BigUint;

/// Width of the Poseidon state.
pub const POSEIDON_T: usize = 3;
/// Rate (number of absorbed elements per permutation).
pub const POSEIDON_RATE: usize = 2;
/// Number of full rounds (half before, half after the partial-round chunk).
pub const POSEIDON_R_F: usize = 8;
/// Number of partial rounds.
pub const POSEIDON_R_P: usize = 57;
/// `SECURE_MDS = 0` — matches snark-verifier-sdk.
pub const POSEIDON_SECURE_MDS: usize = 0;

/// Native Poseidon-128 sponge state used to derive Fiat–Shamir challenges.
/// Mirrors `snark_verifier::util::hash::poseidon::Poseidon<F, F, T, RATE>`.
#[derive(Clone, Debug)]
struct NativePoseidon {
    spec: OptimizedPoseidonSpec<Fr, POSEIDON_T, POSEIDON_RATE>,
    state: [Fr; POSEIDON_T],
    buf: Vec<Fr>,
}

impl NativePoseidon {
    fn new() -> Self {
        // Variable-Input-Length Hashing capacity: `2^64 + (o-1)`, output len `o=1`.
        let mut state = [Fr::zero(); POSEIDON_T];
        state[0] = Fr::from_u128(1u128 << 64);
        Self {
            spec: OptimizedPoseidonSpec::new::<POSEIDON_R_F, POSEIDON_R_P, POSEIDON_SECURE_MDS>(),
            state,
            buf: Vec::new(),
        }
    }

    fn update(&mut self, elements: &[Fr]) {
        self.buf.extend_from_slice(elements);
    }

    fn squeeze(&mut self) -> Fr {
        let buf = mem::take(&mut self.buf);
        let exact = buf.len() % POSEIDON_RATE == 0;

        for chunk in buf.chunks(POSEIDON_RATE) {
            self.permutation(chunk);
        }
        if exact {
            self.permutation(&[]);
        }

        // Same convention as snark-verifier: output `state[1]`.
        self.state[1]
    }

    fn permutation(&mut self, inputs: &[Fr]) {
        let r_f = self.spec.r_f() / 2;
        let mds = self.spec.mds_matrices().mds().as_ref();
        let pre_sparse_mds = self.spec.mds_matrices().pre_sparse_mds().as_ref();
        let sparse_matrices = self.spec.mds_matrices().sparse_matrices();

        let constants_start = self.spec.constants().start();
        absorb_with_pre_constants(&mut self.state, inputs, &constants_start[0]);
        for constants in constants_start.iter().skip(1).take(r_f - 1) {
            sbox_full(&mut self.state, constants);
            apply_mds(&mut self.state, mds);
        }
        sbox_full(&mut self.state, constants_start.last().unwrap());
        apply_mds(&mut self.state, pre_sparse_mds);

        let constants_partial = self.spec.constants().partial();
        for (constant, sparse_mds) in constants_partial.iter().zip(sparse_matrices.iter()) {
            sbox_part(&mut self.state, constant);
            apply_sparse_mds(&mut self.state, sparse_mds);
        }

        let constants_end = self.spec.constants().end();
        for constants in constants_end.iter() {
            sbox_full(&mut self.state, constants);
            apply_mds(&mut self.state, mds);
        }
        sbox_full(&mut self.state, &[Fr::zero(); POSEIDON_T]);
        apply_mds(&mut self.state, mds);
    }
}

fn sbox_full(state: &mut [Fr; POSEIDON_T], constants: &[Fr; POSEIDON_T]) {
    for (x, c) in state.iter_mut().zip(constants.iter()) {
        let x2 = *x * *x;
        let x4 = x2 * x2;
        *x = *x * x4 + *c;
    }
}

fn sbox_part(state: &mut [Fr; POSEIDON_T], constant: &Fr) {
    let x = &mut state[0];
    let x2 = *x * *x;
    let x4 = x2 * x2;
    *x = *x * x4 + *constant;
}

fn absorb_with_pre_constants(
    state: &mut [Fr; POSEIDON_T],
    inputs: &[Fr],
    pre_constants: &[Fr; POSEIDON_T],
) {
    assert!(inputs.len() < POSEIDON_T, "inputs must fit in RATE slots");

    state[0] += pre_constants[0];

    for ((x, c), input) in state
        .iter_mut()
        .zip(pre_constants.iter())
        .skip(1)
        .zip(inputs.iter())
    {
        *x += *input + *c;
    }

    let offset = inputs.len() + 1;
    for (i, (x, c)) in state
        .iter_mut()
        .zip(pre_constants.iter())
        .skip(offset)
        .enumerate()
    {
        *x += if i == 0 { Fr::one() + *c } else { *c };
    }
}

fn apply_mds(state: &mut [Fr; POSEIDON_T], mds: &[[Fr; POSEIDON_T]; POSEIDON_T]) {
    let mut out = [Fr::zero(); POSEIDON_T];
    for i in 0..POSEIDON_T {
        for j in 0..POSEIDON_T {
            out[i] += mds[i][j] * state[j];
        }
    }
    *state = out;
}

fn apply_sparse_mds(
    state: &mut [Fr; POSEIDON_T],
    mds: &SparseMDSMatrix<Fr, POSEIDON_T, POSEIDON_RATE>,
) {
    let mut out = [Fr::zero(); POSEIDON_T];

    let row = mds.row();
    for j in 0..POSEIDON_T {
        out[0] += row[j] * state[j];
    }

    let col_hat = mds.col_hat();
    for i in 1..POSEIDON_T {
        out[i] = state[i] + col_hat[i - 1] * state[0];
    }

    *state = out;
}

fn fq_to_fr(fe: <G1Affine as CurveAffine>::Base) -> Fr {
    let repr = fe.to_repr();
    let bytes = repr.as_ref();
    let big = BigUint::from_bytes_le(bytes);
    let modulus = fr_modulus();
    let reduced = big % modulus;
    let reduced_bytes = reduced.to_bytes_le();

    let mut out_repr = <Fr as PrimeField>::Repr::default();
    let len = reduced_bytes.len().min(out_repr.as_ref().len());
    out_repr.as_mut()[..len].copy_from_slice(&reduced_bytes[..len]);
    Fr::from_repr(out_repr).expect("reduced bytes fit in Fr by construction")
}

fn fr_modulus() -> BigUint {
    let neg_one = -Fr::one();
    let repr = neg_one.to_repr();
    let bytes = repr.as_ref();
    BigUint::from_bytes_le(bytes) + 1u32
}

/// `EncodedChallenge` whose carrier is a native scalar field element.
#[derive(Clone, Copy, Debug)]
pub struct PoseidonChallenge(Fr);

impl EncodedChallenge<G1Affine> for PoseidonChallenge {
    type Input = Fr;

    fn new(input: &Fr) -> Self {
        PoseidonChallenge(*input)
    }

    fn get_scalar(&self) -> Fr {
        self.0
    }
}

/// Poseidon transcript reader. Reads a proof produced by [`PoseidonWrite`] or by
/// snark-verifier-sdk's `PoseidonTranscript<NativeLoader, _>`.
pub struct PoseidonRead<R: Read> {
    hasher: NativePoseidon,
    stream: R,
}

impl<R: Read> PoseidonRead<R> {
    pub fn init(stream: R) -> Self {
        Self { hasher: NativePoseidon::new(), stream }
    }
}

impl<R: Read> Transcript<G1Affine, PoseidonChallenge> for PoseidonRead<R> {
    fn squeeze_challenge(&mut self) -> PoseidonChallenge {
        PoseidonChallenge::new(&self.hasher.squeeze())
    }

    fn common_point(&mut self, ec_point: G1Affine) -> io::Result<()> {
        common_ec_point(&mut self.hasher, ec_point)
    }

    fn common_scalar(&mut self, scalar: Fr) -> io::Result<()> {
        self.hasher.update(&[scalar]);
        Ok(())
    }
}

impl<R: Read> TranscriptRead<G1Affine, PoseidonChallenge> for PoseidonRead<R> {
    fn read_point(&mut self) -> io::Result<G1Affine> {
        let mut data = <G1Affine as GroupEncoding>::Repr::default();
        self.stream.read_exact(data.as_mut())?;
        let ec_point: G1Affine = Option::<G1Affine>::from(G1Affine::from_bytes(&data))
            .ok_or_else(|| io::Error::other("invalid EC point encoding in Poseidon transcript"))?;
        Transcript::<G1Affine, PoseidonChallenge>::common_point(self, ec_point)?;
        Ok(ec_point)
    }

    fn read_scalar(&mut self) -> io::Result<Fr> {
        let mut data = <Fr as PrimeField>::Repr::default();
        self.stream.read_exact(data.as_mut())?;
        let scalar: Fr = Fr::from_repr(data)
            .into_option()
            .ok_or_else(|| io::Error::other("invalid scalar encoding in Poseidon transcript"))?;
        Transcript::<G1Affine, PoseidonChallenge>::common_scalar(self, scalar)?;
        Ok(scalar)
    }
}

impl<R: Read> TranscriptReadBuffer<R, G1Affine, PoseidonChallenge> for PoseidonRead<R>
where
    Fr: FromUniformBytes<64>,
{
    fn init(reader: R) -> Self {
        Self::init(reader)
    }
}

/// Poseidon transcript writer. `finalize()` returns the proof-byte sink.
pub struct PoseidonWrite<W: Write> {
    hasher: NativePoseidon,
    stream: W,
}

impl<W: Write> PoseidonWrite<W> {
    pub fn init(stream: W) -> Self {
        Self { hasher: NativePoseidon::new(), stream }
    }

    pub fn finalize(self) -> W {
        self.stream
    }
}

impl<W: Write> Transcript<G1Affine, PoseidonChallenge> for PoseidonWrite<W> {
    fn squeeze_challenge(&mut self) -> PoseidonChallenge {
        PoseidonChallenge::new(&self.hasher.squeeze())
    }

    fn common_point(&mut self, ec_point: G1Affine) -> io::Result<()> {
        common_ec_point(&mut self.hasher, ec_point)
    }

    fn common_scalar(&mut self, scalar: Fr) -> io::Result<()> {
        self.hasher.update(&[scalar]);
        Ok(())
    }
}

impl<W: Write> TranscriptWrite<G1Affine, PoseidonChallenge> for PoseidonWrite<W> {
    fn write_point(&mut self, ec_point: G1Affine) -> io::Result<()> {
        Transcript::<G1Affine, PoseidonChallenge>::common_point(self, ec_point)?;
        let data = ec_point.to_bytes();
        self.stream.write_all(data.as_ref())
    }

    fn write_scalar(&mut self, scalar: Fr) -> io::Result<()> {
        Transcript::<G1Affine, PoseidonChallenge>::common_scalar(self, scalar)?;
        let data = scalar.to_repr();
        self.stream.write_all(data.as_ref())
    }
}

impl<W: Write> TranscriptWriterBuffer<W, G1Affine, PoseidonChallenge> for PoseidonWrite<W>
where
    Fr: FromUniformBytes<64>,
{
    fn init(writer: W) -> Self {
        Self::init(writer)
    }

    fn finalize(self) -> W {
        self.finalize()
    }
}

fn common_ec_point(hasher: &mut NativePoseidon, ec_point: G1Affine) -> io::Result<()> {
    let coords = Option::<halo2_base::halo2_proofs::halo2curves::Coordinates<G1Affine>>::from(
        ec_point.coordinates(),
    )
    .ok_or_else(|| io::Error::other("invalid EC point coordinates"))?;
    let x = fq_to_fr(*coords.x());
    let y = fq_to_fr(*coords.y());
    hasher.update(&[x, y]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_squeeze_is_deterministic() {
        let mut a = NativePoseidon::new();
        let mut b = NativePoseidon::new();
        assert_eq!(a.squeeze(), b.squeeze());
    }

    #[test]
    fn write_then_read_round_trip_scalars() {
        let xs: Vec<Fr> = (0..7).map(|i| Fr::from(i as u64 + 1)).collect();

        let mut writer = PoseidonWrite::init(Vec::<u8>::new());
        for s in &xs {
            writer.write_scalar(*s).unwrap();
        }
        let challenge_w: Fr = writer.squeeze_challenge().get_scalar();
        let bytes = writer.finalize();

        assert_eq!(bytes.len(), xs.len() * 32);

        let mut reader = PoseidonRead::init(bytes.as_slice());
        let mut read_back = Vec::with_capacity(xs.len());
        for _ in &xs {
            read_back.push(reader.read_scalar().unwrap());
        }
        let challenge_r: Fr = reader.squeeze_challenge().get_scalar();

        assert_eq!(read_back, xs);
        assert_eq!(challenge_w, challenge_r);
    }

    #[test]
    fn write_then_read_round_trip_points() {
        use halo2_base::halo2_proofs::halo2curves::{
            bn256::G1,
            group::{Curve, Group},
        };
        use rand::SeedableRng;

        let mut rng = rand::rngs::StdRng::seed_from_u64(0xC0FFEE);
        let points: Vec<G1Affine> =
            (0..5).map(|_| G1::random(&mut rng).to_affine()).collect();

        let mut writer = PoseidonWrite::init(Vec::<u8>::new());
        for p in &points {
            writer.write_point(*p).unwrap();
        }
        let challenge_w: Fr = writer.squeeze_challenge().get_scalar();
        let bytes = writer.finalize();
        assert_eq!(bytes.len(), points.len() * 32);

        let mut reader = PoseidonRead::init(bytes.as_slice());
        let mut read_back = Vec::with_capacity(points.len());
        for _ in &points {
            read_back.push(reader.read_point().unwrap());
        }
        let challenge_r: Fr = reader.squeeze_challenge().get_scalar();

        assert_eq!(read_back, points);
        assert_eq!(challenge_w, challenge_r);
    }

    #[test]
    fn tampered_proof_changes_challenge() {
        let xs: Vec<Fr> = (0..3).map(|i| Fr::from(i as u64 + 1)).collect();

        let mut writer = PoseidonWrite::init(Vec::<u8>::new());
        for s in &xs {
            writer.write_scalar(*s).unwrap();
        }
        let challenge_clean: Fr = writer.squeeze_challenge().get_scalar();
        let mut bytes = writer.finalize();

        bytes[32] ^= 0x01;

        let mut reader = PoseidonRead::init(bytes.as_slice());
        for _ in &xs {
            let _ = reader.read_scalar().unwrap();
        }
        let challenge_tampered: Fr = reader.squeeze_challenge().get_scalar();

        assert_ne!(challenge_clean, challenge_tampered);
    }
}
