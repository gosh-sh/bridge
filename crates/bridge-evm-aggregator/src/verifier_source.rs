//! The verifier self-check without a compiler.
//!
//! `snark_verifier_sdk::evm::gen_evm_verifier` builds the Solidity source of a
//! verifier from `(params, vk, num_instance)` and then compiles it with `solc`.
//! The source is already fully determined by the key, so comparing it with the
//! committed `<name>.sol` catches the same verifying-key drift the bytecode
//! comparison did, and the compiler is needed only where bytecode is actually
//! produced: when a verifier is regenerated. That is what lets the withdrawal
//! CLI and the relayer run without `solc`.
//!
//! A source comparison is also sensitive to the generator itself: a
//! `snark-verifier` upgrade that reformats the output fails it with an
//! unchanged key. That is deliberate — generator drift is worth catching — but
//! the refusal has to say so, or a dependency bump reads as a compromised key.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fq, Fr, G1Affine},
    plonk::VerifyingKey,
    poly::{commitment::ParamsProver, kzg::commitment::ParamsKZG},
};
use snark_verifier::{
    loader::evm::EvmLoader,
    system::halo2::{compile, transcript::evm::EvmTranscript, Config},
    verifier::SnarkVerifier,
};
use snark_verifier_sdk::{CircuitExt, PlonkVerifier, SHPLONK};

/// Generate the Solidity source of the SHPLONK EVM verifier for `vk`.
///
/// The body of `snark_verifier_sdk::evm::gen_evm_verifier` up to its
/// `compile_solidity` call, specialised to SHPLONK. Keep the two in step when
/// the SDK pin moves: the ignored test
/// `source_is_what_the_sdk_writes_and_compiles` is what proves they still
/// agree.
pub fn gen_evm_verifier_sol_shplonk<C: CircuitExt<Fr>>(
    params: &ParamsKZG<Bn256>,
    vk: &VerifyingKey<G1Affine>,
    num_instance: Vec<usize>,
) -> String {
    let protocol = compile(
        params,
        vk,
        Config::kzg()
            .with_num_instance(num_instance.clone())
            .with_accumulator_indices(C::accumulator_indices()),
    );
    let dk: snark_verifier::pcs::kzg::KzgDecidingKey<Bn256> =
        (params.get_g()[0], params.g2(), params.s_g2()).into();

    let loader = EvmLoader::new::<Fq, Fr>();
    let protocol = protocol.loaded(&loader);
    let mut transcript = EvmTranscript::<_, Rc<EvmLoader>, _, _>::new(&loader);

    let instances = transcript.load_instances(num_instance);
    let proof = PlonkVerifier::<SHPLONK>::read_proof(&dk, &protocol, &instances, &mut transcript)
        .expect("the EVM loader records the verifier program; reading a proof cannot fail");
    PlonkVerifier::<SHPLONK>::verify(&dk, &protocol, &instances, &proof)
        .expect("the EVM loader records the verifier program; verifying cannot fail");

    loader.solidity_code()
}

/// Outcome of comparing a generated verifier source with the committed one.
#[derive(Debug)]
pub enum SourceCheck {
    /// Byte-identical to the committed `<name>.sol`; carries its length.
    Match(usize),
    /// The committed file differs. The string is the refusal to show.
    Drift(String),
    /// No committed `<name>.sol` at this path.
    Missing(PathBuf),
}

/// Compare `generated` with `<verifiers_dir>/<name>.sol`.
///
/// Only an I/O failure other than "not found" is an `Err`; a missing file is
/// [`SourceCheck::Missing`], because whether that is fatal is the caller's
/// decision (`aggregate-proof --allow-source-drift`).
pub fn check_committed_source(
    verifiers_dir: &Path,
    name: &str,
    generated: &str,
) -> std::io::Result<SourceCheck> {
    let path = verifiers_dir.join(format!("{name}.sol"));
    let committed = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceCheck::Missing(path))
        },
        Err(e) => return Err(e),
    };
    if committed == generated.as_bytes() {
        return Ok(SourceCheck::Match(committed.len()));
    }
    let committed = String::from_utf8_lossy(&committed);
    Ok(SourceCheck::Drift(format!(
        "regenerated {name}.sol ({} B) != committed {} ({} B), first difference at line {}: \
         aggregator VK drift. Either the verifying key changed (inner-snark shape, \
         AggregatorConfig or SRS) and the deployed verifier would REJECT this calldata, or the \
         snark-verifier dependency changed the generated source and the committed .sol and .bin \
         have to be regenerated together (contracts/ethereum/verifiers/README.md). Diff the two \
         sources before concluding either.",
        generated.len(),
        path.display(),
        committed.len(),
        first_differing_line(&committed, generated),
    )))
}

/// 1-based number of the first line where `a` and `b` differ.
fn first_differing_line(a: &str, b: &str) -> usize {
    let mut left = a.lines();
    let mut right = b.lines();
    let mut line = 1;
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            _ => return line,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use halo2_base::{
        gates::circuit::builder::BaseCircuitBuilder,
        halo2_proofs::{
            halo2curves::bn256::{Bn256, Fr, G1Affine},
            plonk::ProvingKey,
            poly::kzg::commitment::ParamsKZG,
        },
    };
    use rand::{rngs::StdRng, SeedableRng};
    use snark_verifier_sdk::gen_pk;

    use super::*;
    use crate::{
        aggregator::{K_INNER_SPIKE, LOOKUP_BITS_INNER_SPIKE},
        multiply::build_multiply_circuit,
    };

    /// A directory under the system temp dir, unique per test and process.
    /// The crate has no `tempfile` dependency, and this is all the tests need.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bridge-evm-aggregator-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The multiply spike circuit under a seeded test SRS. Not a production
    /// verifier: only the generator is under test, and it takes the key as
    /// given.
    fn spike_key() -> (ParamsKZG<Bn256>, ProvingKey<G1Affine>) {
        let params = ParamsKZG::<Bn256>::setup(K_INNER_SPIKE, StdRng::seed_from_u64(1));
        let (builder, _) = build_multiply_circuit(
            false,
            K_INNER_SPIKE as usize,
            LOOKUP_BITS_INNER_SPIKE,
            Fr::from(7u64),
            Fr::from(11u64),
        );
        let pk = gen_pk(&params, &builder, None);
        (params, pk)
    }

    #[test]
    fn source_is_a_solidity_verifier_and_is_deterministic() {
        let (params, pk) = spike_key();
        let first =
            gen_evm_verifier_sol_shplonk::<BaseCircuitBuilder<Fr>>(&params, pk.get_vk(), vec![1]);
        let second =
            gen_evm_verifier_sol_shplonk::<BaseCircuitBuilder<Fr>>(&params, pk.get_vk(), vec![1]);
        assert!(
            first.contains("contract Halo2Verifier"),
            "source head:\n{}",
            &first[..first.len().min(400)]
        );
        assert_eq!(first, second, "the same key must yield the same source");
    }

    /// The claim the whole change rests on: the source this crate generates
    /// is byte for byte what the SDK writes before compiling it, and it
    /// compiles to exactly the SDK's bytecode.
    #[test]
    #[ignore = "needs solc 0.8.19 on PATH"]
    fn source_is_what_the_sdk_writes_and_compiles() {
        let (params, pk) = spike_key();
        let ours =
            gen_evm_verifier_sol_shplonk::<BaseCircuitBuilder<Fr>>(&params, pk.get_vk(), vec![1]);

        let dir = scratch_dir("sdk-parity");
        let sdk_path = dir.join("Sdk.sol");
        let sdk_bytecode = snark_verifier_sdk::evm::gen_evm_verifier_shplonk::<
            BaseCircuitBuilder<Fr>,
        >(&params, pk.get_vk(), vec![1], Some(&sdk_path));
        let sdk_source = std::fs::read_to_string(&sdk_path).unwrap();

        assert_eq!(ours, sdk_source);
        assert_eq!(
            snark_verifier::loader::evm::compile_solidity(&ours),
            sdk_bytecode
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_identical_committed_source_matches() {
        let dir = scratch_dir("match");
        std::fs::write(dir.join("Foo.sol"), "a\nb\n").unwrap();
        let check = check_committed_source(&dir, "Foo", "a\nb\n").unwrap();
        assert!(matches!(check, SourceCheck::Match(4)), "got {check:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drift_names_the_line_and_both_possible_causes() {
        let dir = scratch_dir("drift");
        std::fs::write(dir.join("Foo.sol"), "a\nb\nc\n").unwrap();
        let check = check_committed_source(&dir, "Foo", "a\nX\nc\n").unwrap();
        let SourceCheck::Drift(msg) = check else {
            panic!("expected drift, got {check:?}");
        };
        assert!(msg.contains("aggregator VK drift"), "{msg}");
        assert!(msg.contains("line 2"), "{msg}");
        assert!(
            msg.contains("snark-verifier"),
            "an upgrade must be named as a cause: {msg}"
        );
        assert!(msg.contains("Foo.sol"), "{msg}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_committed_source_is_reported_with_its_path() {
        let dir = scratch_dir("missing");
        let check = check_committed_source(&dir, "Foo", "a\n").unwrap();
        let SourceCheck::Missing(path) = check else {
            panic!("expected missing, got {check:?}");
        };
        assert_eq!(path, dir.join("Foo.sol"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
