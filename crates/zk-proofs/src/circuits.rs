//! Circuit utilities and common gadgets for Halo2

use halo2_proofs::{
    circuit::{AssignedCell, Layouter, SimpleFloorPlanner, Value},
    plonk::{Advice, Circuit, Column, ConstraintSystem, Error, Fixed, Instance},
    poly::Rotation,
};
use ff::PrimeField;

/// Configuration for a simple hash circuit
#[derive(Debug, Clone)]
pub struct HashConfig {
    /// Advice columns for inputs
    pub advice: [Column<Advice>; 2],
    /// Instance column for public inputs
    pub instance: Column<Instance>,
    /// Fixed column for selectors
    pub selector: Column<Fixed>,
}

/// Chip for Poseidon hash operations
///
/// Note: This is a simplified placeholder. In production, use halo2_gadgets::poseidon
pub struct PoseidonChip<F: PrimeField> {
    config: HashConfig,
    _marker: std::marker::PhantomData<F>,
}

impl<F: PrimeField> PoseidonChip<F> {
    /// Construct a new chip
    pub fn construct(config: HashConfig) -> Self {
        Self {
            config,
            _marker: std::marker::PhantomData,
        }
    }

    /// Configure the chip
    pub fn configure(
        meta: &mut ConstraintSystem<F>,
        advice: [Column<Advice>; 2],
        instance: Column<Instance>,
    ) -> HashConfig {
        let selector = meta.fixed_column();

        // Enable equality constraints
        meta.enable_equality(advice[0]);
        meta.enable_equality(advice[1]);
        meta.enable_equality(instance);

        // Create a simple gate (placeholder for actual Poseidon)
        meta.create_gate("poseidon_hash", |meta| {
            let s = meta.query_fixed(selector, Rotation::cur());
            let a = meta.query_advice(advice[0], Rotation::cur());
            let b = meta.query_advice(advice[1], Rotation::cur());
            let c = meta.query_advice(advice[0], Rotation::next());

            // Simplified constraint: c = a + b (placeholder)
            // In production, this would be the actual Poseidon permutation
            vec![s * (a + b - c)]
        });

        HashConfig {
            advice,
            instance,
            selector,
        }
    }

    /// Hash two field elements (simplified)
    pub fn hash_two(
        &self,
        mut layouter: impl Layouter<F>,
        a: AssignedCell<F, F>,
        b: AssignedCell<F, F>,
    ) -> Result<AssignedCell<F, F>, Error> {
        layouter.assign_region(
            || "hash_two",
            |mut region| {
                // Enable selector
                self.config.selector.enable(&mut region, 0)?;

                // Copy inputs
                a.copy_advice(|| "a", &mut region, self.config.advice[0], 0)?;
                b.copy_advice(|| "b", &mut region, self.config.advice[1], 0)?;

                // Compute hash (simplified: just add them)
                let hash_value = a.value().copied() + b.value().copied();

                // Assign output
                region.assign_advice(
                    || "hash",
                    self.config.advice[0],
                    1,
                    || hash_value,
                )
            },
        )
    }
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
}

