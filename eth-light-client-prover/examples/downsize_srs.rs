//! Downsize a KZG SRS to a smaller `k`, preserving the ceremony (same tau).
//!
//! Twin of `deposit-prover/examples/downsize_srs.rs`, but built against the
//! **gosh** halo2-base this crate (and `bridge-snark-utils`) use, so the written
//! `kzg_bn254_{k}.srs` round-trips byte-for-byte through the `gen_srs` reader
//! that `bridge-snark-utils::export_poseidon_snark` invokes (it sets
//! `PARAMS_DIR = vk_path.parent()` and reads `kzg_bn254_{config.k}.srs`).
//!
//! For the M6 recursive-rotate shard wrap we need a **Hermez** k=20 SRS next to
//! the shard VK. We have Hermez k=21 in `params/`; downsizing preserves the
//! Hermez `[s]·G2`, so `export_poseidon_snark`'s `assert_hermez_srs` passes.
//!
//! ```bash
//! cd eth-light-client-prover
//! cargo run --release --example downsize_srs -- \
//!   --input ../params/kzg_bn254_21.srs \
//!   --output out/shard_snark/kzg_bn254_20.srs \
//!   --k 20
//! ```

use std::fs;

use halo2_base::halo2_proofs::halo2curves::bn256::Bn256;
use halo2_base::halo2_proofs::poly::commitment::Params;
use halo2_base::halo2_proofs::poly::kzg::commitment::ParamsKZG;

fn main() -> anyhow::Result<()> {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut k: Option<u32> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" => input = args.next(),
            "--output" => output = args.next(),
            "--k" => k = args.next().and_then(|s| s.parse().ok()),
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }
    let input = input.ok_or_else(|| anyhow::anyhow!("--input required"))?;
    let output = output.ok_or_else(|| anyhow::anyhow!("--output required"))?;
    let k = k.ok_or_else(|| anyhow::anyhow!("--k required"))?;

    println!("Reading SRS {input} ...");
    let mut f = fs::File::open(&input)?;
    let mut params = ParamsKZG::<Bn256>::read(&mut f)?;
    println!("  loaded k={}", params.k());
    if params.k() < k {
        anyhow::bail!("cannot upsize: input k={} < target k={k}", params.k());
    }
    if params.k() > k {
        println!("Downsizing {} -> {k} (tau preserved) ...", params.k());
        params.downsize(k);
    }
    if let Some(parent) = std::path::Path::new(&output).parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = fs::File::create(&output)?;
    params.write(&mut out)?;
    println!("Wrote downsized SRS (k={}) -> {output}", params.k());
    Ok(())
}
