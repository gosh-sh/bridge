//! Runtime guard against silent toxic-waste SRS.
//!
//! `halo2_base::utils::fs::gen_srs(k)` silently generates a fresh unsafe SRS
//! (local RNG for tau) if `$PARAMS_DIR/kzg_bn254_{k}.srs` is missing. Any
//! aggregator proof produced under that SRS is trivially forgeable by whoever
//! ran the process — they know tau and can craft valid outer proofs for
//! arbitrary public instances, bypassing the whole inner-Snark chain.
//!
//! This module fingerprints the SRS's `s_g2` point and refuses to proceed
//! unless the head matches the **Hermez Perpetual Powers of Tau** ceremony
//! (`928fafb3d0cc…`). Same anchor bytes as the sibling check in
//! `bridge-prover-lib::keys::common::assert_hermez_srs`, so both sides
//! of the bridge trust the same ceremony.

use anyhow::{bail, Result};
use halo2_base::halo2_proofs::{
    halo2curves::{bn256::Bn256, serde::SerdeObject},
    poly::{commitment::Params, kzg::commitment::ParamsKZG},
};

/// Hermez `s_g2` head (`928fafb3d0cc…`). Rejects Acki Nacki chain-ceremony
/// (`c6028acf…`) and any synthetic `gen_srs` trapdoor.
pub const HERMEZ_S_G2_HEAD: [u8; 6] = [0x92, 0x8f, 0xaf, 0xb3, 0xd0, 0xcc];

/// Verify the outer aggregator SRS came from the Hermez PPoT ceremony.
///
/// Bails with a loud error if the head bytes of `s_g2` don't match — that
/// signals either (a) `PARAMS_DIR` is missing `kzg_bn254_{k}.srs` and
/// `gen_srs` fell back to toxic-waste generation, or (b) an operator
/// substituted a different ceremony (e.g. the Acki Nacki chain-ceremony
/// file). Either way, the aggregator refuses to emit a proof whose
/// soundness cannot be traced to a public multi-party ceremony.
pub fn assert_hermez_ceremony(srs: &ParamsKZG<Bn256>) -> Result<()> {
    let mut buf = Vec::with_capacity(128);
    srs.s_g2()
        .write_raw(&mut buf)
        .expect("write to Vec cannot fail");
    if buf.len() < HERMEZ_S_G2_HEAD.len() {
        bail!(
            "SRS s_g2 encoding too small ({} bytes) — SRS at k={} is malformed",
            buf.len(),
            srs.k(),
        );
    }
    let head = &buf[..HERMEZ_S_G2_HEAD.len()];
    if head != HERMEZ_S_G2_HEAD {
        bail!(
            "outer aggregator SRS (k={}) is NOT Hermez Perpetual Powers of Tau \
             (s_g2 head {:02x?}, expected {:02x?}). \
             PARAMS_DIR likely lacks kzg_bn254_{}.srs and `gen_srs` silently \
             generated a toxic-waste SRS whose tau is known to the local \
             process — every proof produced with it is forgeable. \
             Bootstrap via `scripts/bootstrap_hermez_srs.sh`. REFUSING to proceed.",
            srs.k(),
            head,
            HERMEZ_S_G2_HEAD,
            srs.k(),
        );
    }
    Ok(())
}
