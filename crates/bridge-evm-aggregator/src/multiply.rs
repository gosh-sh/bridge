//! Minimal inner circuit for the M2 spike: prove `a * b == c` where `c` is
//! the single public input. Built with [`BaseCircuitBuilder`] so that
//! snark-verifier-sdk can wrap it in `AggregationCircuit` without any
//! adapter glue.
//!
//! Witness layout (private): `a`, `b`.
//! Public instance layout: `[c]` where `c = a * b mod r`.
//!
//! This is intentionally trivial — the spike's purpose is to validate the
//! plumbing (halo2-base → SHPLONK proof → AggregationCircuit → Yul
//! verifier), not to demonstrate the cryptography of the partner's actual
//! bridge circuits.

use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
        GateChip, GateInstructions,
    },
    halo2_proofs::halo2curves::bn256::Fr,
};

/// Canonical [`BaseCircuitParams`] used by both keygen and prove sides of the
/// spike. Mirrors the layout used in
/// `snark-verifier-sdk/examples/range_check.rs`.
pub fn multiply_params(k: usize, lookup_bits: usize) -> BaseCircuitParams {
    BaseCircuitParams {
        k,
        num_advice_per_phase: vec![1],
        num_fixed: 1,
        num_lookup_advice_per_phase: vec![0],
        lookup_bits: Some(lookup_bits),
        num_instance_columns: 1,
    }
}

/// Build a fresh [`BaseCircuitBuilder`] that proves `a * b == c` and exposes
/// `c` as a single public instance.
///
/// `witness_gen_only` should be `false` for keygen + prove (so break-points
/// are populated by `gen_pk` and reused by `gen_snark_shplonk` on the same
/// builder).
///
/// Returns `(builder, vec![c])` where the vec is the expected public-instance
/// column (the caller passes it to `MockProver`/`gen_snark_shplonk` as
/// reference).
pub fn build_multiply_circuit(
    witness_gen_only: bool,
    k: usize,
    lookup_bits: usize,
    a: Fr,
    b: Fr,
) -> (BaseCircuitBuilder<Fr>, Vec<Fr>) {
    let mut builder =
        BaseCircuitBuilder::<Fr>::new(witness_gen_only).use_params(multiply_params(k, lookup_bits));

    let gate = GateChip::<Fr>::default();
    let ctx = builder.main(0);
    let a_w = ctx.load_witness(a);
    let b_w = ctx.load_witness(b);
    let c_w = gate.mul(ctx, a_w, b_w);
    let c_val = *c_w.value();

    builder.assigned_instances[0] = vec![c_w];

    (builder, vec![c_val])
}

#[cfg(test)]
mod tests {
    use halo2_base::halo2_proofs::dev::MockProver;

    use super::*;

    /// Mock-prover sanity: `2 * 3 == 6` satisfies the circuit.
    #[test]
    fn multiply_circuit_mock_prover_ok() {
        let k = 8;
        let lookup_bits = 7;
        let a = Fr::from(2u64);
        let b = Fr::from(3u64);
        let (builder, public) =
            build_multiply_circuit(/* witness_gen_only = */ false, k, lookup_bits, a, b);
        assert_eq!(public, vec![Fr::from(6u64)]);

        let prover = MockProver::run(k as u32, &builder, vec![public]).expect("mock prover");
        prover.assert_satisfied();
    }
}
