//! M6 recursive rotate — **KZG accumulator DECIDER reference check.**
//!
//! Part (a) established that `ZKHALO2VERIFYWITHVK` does only a **plain** SHPLONK
//! `verify_proof` (`SingleStrategy`, Blake2b) — it never runs the accumulation
//! **decider** pairing. So the N=8 tree-root proof (12 accumulator limbs + 3 rotate
//! PIs) is *accepted* by the opcode today but **unsound**: nothing forces the inner
//! shard proofs, whose validity the in-circuit verifier only *deferred* into those
//! 12 limbs. A BN254-only "decider-in-circuit" is impossible (the check is a BN254
//! pairing, uncomputable inside a BN254 circuit), so the fix is a **verifier-side**
//! decider: exactly the pairing an EVM Yul verifier bakes into its tail, and what
//! the opcode must add.
//!
//! This example is that decider, run natively on the REAL root snark
//! (`out/rotate_tree/root.snark` from `examples/rotate_tree_n8.rs`):
//!
//! 1. decode the accumulator `(lhs, rhs) ∈ G1²` from instances `[0..12]`
//!    (snark-verifier layout: `lhs.x ‖ lhs.y ‖ rhs.x ‖ rhs.y`, each coordinate =
//!    `LIMBS=3` little-endian limbs of `BITS=88`);
//! 2. run the decider `e(lhs, g2) == e(rhs, s_g2)` (snark-verifier
//!    `pcs/kzg/decider.rs`: `terms=[(lhs,g2),(rhs,−s_g2)]`, product = 𝟙) with the
//!    outer SRS's `g2` / `s_g2`;
//! 3. **negative controls**: a tampered limb and swapped `lhs/rhs` must both be
//!    rejected — proving the pairing is the real soundness gate, not a formality.
//!
//! The exact Rust the opcode needs is `decode_accumulator` + `decide` below (the
//! opcode already embeds `[s]G2` as `KZG_S_G2_BYTES`).
//!
//! ## Run (n14, fast — no proving)
//!
//! ```bash
//! cd eth-light-client-prover
//! env ROOT_SNARK=out/rotate_tree/root.snark OUTER_SRS=../params/kzg_bn254_21.srs \
//!   cargo run --release --features aggregation --example rotate_decider_check
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use halo2_base::halo2_proofs::halo2curves::bn256::{pairing, Bn256, Fq, Fr, G1Affine, G2Affine};
use halo2_base::halo2_proofs::halo2curves::ff::Field;
use halo2_base::halo2_proofs::halo2curves::CurveAffine;
use halo2_base::halo2_proofs::poly::commitment::Params;
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;

use snark_verifier_sdk::Snark;

/// snark-verifier's accumulator encoding constants (`snark-verifier-sdk/src/lib.rs`).
const LIMBS: usize = 3;
const BITS: usize = 88;
const NUM_ACCUMULATOR_INSTANCES: usize = 4 * LIMBS; // lhs.x, lhs.y, rhs.x, rhs.y

/// `2^BITS` as an `Fq` (little-endian: bit 88 ⇒ byte 11).
fn limb_base() -> Fq {
    let mut b = [0u8; 32];
    b[BITS / 8] = 1;
    Fq::from_bytes(&b).expect("2^88 < q")
}

/// One G1 coordinate from `LIMBS` little-endian limbs (each an `Fr` holding a value
/// `< 2^BITS`): `Σ limb_i · (2^BITS)^i` in `Fq`.
fn coord_from_limbs(limbs: &[Fr]) -> Fq {
    let base = limb_base();
    let mut acc = Fq::ZERO;
    let mut p = Fq::ONE;
    for l in limbs {
        // limb value < 2^88 < q, so the 32-byte LE repr is a valid Fq.
        let l_fq = Fq::from_bytes(&l.to_bytes()).expect("limb < 2^88 < q");
        acc += l_fq * p;
        p *= base;
    }
    acc
}

/// Decode the KZG accumulator `(lhs, rhs)` from the 12 accumulator instances
/// (`lhs.x ‖ lhs.y ‖ rhs.x ‖ rhs.y`, each 3 LE limbs). `None` if a decoded pair is
/// not a valid curve point (a tamper signal in itself).
fn decode_accumulator(acc: &[Fr]) -> Option<(G1Affine, G1Affine)> {
    assert_eq!(acc.len(), NUM_ACCUMULATOR_INSTANCES);
    let lhs_x = coord_from_limbs(&acc[0..3]);
    let lhs_y = coord_from_limbs(&acc[3..6]);
    let rhs_x = coord_from_limbs(&acc[6..9]);
    let rhs_y = coord_from_limbs(&acc[9..12]);
    let lhs = Option::<G1Affine>::from(G1Affine::from_xy(lhs_x, lhs_y))?;
    let rhs = Option::<G1Affine>::from(G1Affine::from_xy(rhs_x, rhs_y))?;
    Some((lhs, rhs))
}

/// The decider: `e(lhs, g2) == e(rhs, s_g2)`.
fn decide(lhs: &G1Affine, rhs: &G1Affine, g2: &G2Affine, s_g2: &G2Affine) -> bool {
    pairing(lhs, g2) == pairing(rhs, s_g2)
}

fn load_srs(path: &Path) -> anyhow::Result<ParamsKZG<Bn256>> {
    anyhow::ensure!(path.exists(), "SRS not found at {}", path.display());
    let mut f = fs::File::open(path)?;
    Ok(ParamsKZG::<Bn256>::read(&mut f)?)
}

fn main() -> anyhow::Result<()> {
    let snark_path = PathBuf::from(
        std::env::var("ROOT_SNARK").unwrap_or_else(|_| "out/rotate_tree/root.snark".to_string()),
    );
    let outer_srs = PathBuf::from(
        std::env::var("OUTER_SRS").unwrap_or_else(|_| "../params/kzg_bn254_21.srs".to_string()),
    );

    println!("=== M6: KZG accumulator DECIDER reference check ===");
    println!("root snark : {}", snark_path.display());
    println!("outer SRS  : {}\n", outer_srs.display());

    let snark: Snark = bincode::deserialize(&fs::read(&snark_path)?)?;
    anyhow::ensure!(snark.instances.len() == 1, "expected 1 instance column");
    let inst = &snark.instances[0];
    anyhow::ensure!(
        inst.len() >= NUM_ACCUMULATOR_INSTANCES,
        "root has {} instances (< {NUM_ACCUMULATOR_INSTANCES} accumulator limbs)",
        inst.len()
    );
    println!("root instances: {} (12 accumulator + {} rotate PIs)", inst.len(), inst.len() - 12);
    let rotate_pis = &inst[NUM_ACCUMULATOR_INSTANCES..];
    println!("rotate PIs [current, next, period] = {rotate_pis:?}\n");

    let params = load_srs(&outer_srs)?;
    let g2 = params.g2();
    let s_g2 = params.s_g2();

    // ---- 1. honest accumulator: decider MUST pass -------------------------
    let (lhs, rhs) = decode_accumulator(&inst[0..NUM_ACCUMULATOR_INSTANCES])
        .ok_or_else(|| anyhow::anyhow!("honest accumulator did not decode to curve points"))?;
    let ok = decide(&lhs, &rhs, &g2, &s_g2);
    println!("decider on honest root proof: {}", if ok { "PASS ✅" } else { "FAIL ❌" });
    anyhow::ensure!(ok, "decider REJECTED the honest proof — decode/pairing convention is wrong");

    // ---- 2. tampered limb: MUST be rejected -------------------------------
    let mut tampered = inst[0..NUM_ACCUMULATOR_INSTANCES].to_vec();
    tampered[0] += Fr::ONE;
    let tamper_ok = decode_accumulator(&tampered).map(|(l, r)| decide(&l, &r, &g2, &s_g2)).unwrap_or(false);
    println!("decider on tampered limb[0]:   {}", if tamper_ok { "PASS (BAD!)" } else { "REJECTED ✅" });
    anyhow::ensure!(!tamper_ok, "decider ACCEPTED a tampered accumulator");

    // ---- 3. swapped lhs/rhs (both on-curve, wrong pairing): MUST reject ----
    let swap_ok = decide(&rhs, &lhs, &g2, &s_g2);
    println!("decider on swapped lhs/rhs:    {}", if swap_ok { "PASS (BAD!)" } else { "REJECTED ✅" });
    anyhow::ensure!(!swap_ok, "decider ACCEPTED swapped lhs/rhs");

    println!("\n=== RESULT: PASS ===");
    println!(
        "Honest root proof decides; tampered + swapped accumulators are rejected — the\n\
         pairing e(lhs,g2)==e(rhs,s_g2) is the genuine soundness gate. `decode_accumulator`\n\
         + `decide` above are the exact reference the opcode (or a final wrapper) must run\n\
         over instances[0..12] with the embedded [s]G2."
    );
    Ok(())
}
