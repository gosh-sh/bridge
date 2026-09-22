//! Poseidon constants used both natively (for instance computation in tests)
//! and inside the circuit (via halo2-base's `PoseidonHasher`). Native hashing
//! delegates to `bridge-poseidon` for consistency across all bridge circuits.

use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

pub const T: usize = 3;
pub const RATE: usize = 2;
pub const R_F: usize = 8;
pub const R_P: usize = 57;

/// Native Poseidon hash matching the in-circuit halo2-base hasher
/// (T=3, RATE=2, R_F=8, R_P=57). Wraps `bridge_poseidon::poseidon_hash_fr`.
pub fn poseidon_hash(message: &[Fr]) -> Fr {
    bridge_poseidon::poseidon_hash_fr(message)
}
