//! Circuit utilities and common gadgets for Halo2

use halo2_proofs::{
    circuit::{AssignedCell, Layouter},
    halo2curves::bn256::Fr,
    plonk::Error,
};
use ff::PrimeField;

// Re-export Poseidon types from poseidon-circuit (scroll-tech)
pub use poseidon_circuit::poseidon::{Pow5Chip as PoseidonChip, Pow5Config as PoseidonConfig};
use poseidon_circuit::poseidon::Hash;
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3, P128Pow5T3Compact};

/// Type alias for Poseidon spec with T=3, RATE=2
pub type P128Pow5T3Fr = P128Pow5T3<Fr>;

/// Hash two field elements using Poseidon in-circuit
///
/// This is the production implementation using scroll-tech/poseidon-circuit
pub fn poseidon_hash_two(
    config: PoseidonConfig<Fr, 3, 2>,
    mut layouter: impl Layouter<Fr>,
    a: AssignedCell<Fr, Fr>,
    b: AssignedCell<Fr, Fr>,
) -> Result<AssignedCell<Fr, Fr>, Error> {
    let chip = PoseidonChip::construct(config);
    let hasher = Hash::<_, _, P128Pow5T3<Fr>, ConstantLength<2>, 3, 2>::init(
        chip,
        layouter.namespace(|| "init poseidon hasher"),
    )?;

    hasher.hash(layouter.namespace(|| "hash two"), [a, b])
}

/// Hash a variable number of field elements using Poseidon in-circuit
///
/// Generic over the number of inputs L
pub fn poseidon_hash_gadget<const L: usize>(
    config: PoseidonConfig<Fr, 3, 2>,
    mut layouter: impl Layouter<Fr>,
    messages: [AssignedCell<Fr, Fr>; L],
) -> Result<AssignedCell<Fr, Fr>, Error> {
    let chip = PoseidonChip::construct(config);
    let hasher = Hash::<_, _, P128Pow5T3<Fr>, ConstantLength<L>, 3, 2>::init(
        chip,
        layouter.namespace(|| "init poseidon hasher"),
    )?;

    hasher.hash(layouter.namespace(|| "hash"), messages)
}

/// Hash field elements using Poseidon (native, outside circuit)
///
/// This is used for computing expected values in tests and for witness generation
pub fn poseidon_hash<const L: usize>(message: [Fr; L]) -> Fr {
    PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<L>, 3, 2>::init().hash(message)
}

/// Utility to convert bytes to field element
pub fn bytes_to_field<F: PrimeField>(bytes: &[u8]) -> F {
    let mut repr = F::Repr::default();
    let repr_bytes = repr.as_mut();
    let len = std::cmp::min(bytes.len(), repr_bytes.len());
    repr_bytes[..len].copy_from_slice(&bytes[..len]);
    F::from_repr(repr).unwrap_or(F::ZERO)
}

/// Utility to convert field element to bytes
pub fn field_to_bytes<F: PrimeField>(field: &F) -> Vec<u8> {
    field.to_repr().as_ref().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use halo2_proofs::halo2curves::bn256::Fr;

    #[test]
    fn test_bytes_to_field_conversion() {
        let bytes = vec![1u8, 2, 3, 4];
        let field: Fr = bytes_to_field(&bytes);
        let back = field_to_bytes(&field);

        // Should preserve the data (with padding)
        assert_eq!(&back[..4], &bytes[..]);
    }

    #[test]
    fn test_field_roundtrip() {
        let original = Fr::from(12345u64);
        let bytes = field_to_bytes(&original);
        let recovered: Fr = bytes_to_field(&bytes);
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_poseidon_hash_native() {
        // Test the native (outside-circuit) Poseidon hash
        let a = Fr::from(12345u64);
        let b = Fr::from(67890u64);

        let hash = poseidon_hash([a, b]);

        // The hash should be deterministic
        let hash2 = poseidon_hash([a, b]);
        assert_eq!(hash, hash2);

        // Different inputs should produce different hashes
        let hash3 = poseidon_hash([b, a]);
        assert_ne!(hash, hash3);

        println!("Poseidon([{:?}, {:?}]) = {:?}", a, b, hash);
    }
}

