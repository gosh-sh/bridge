use std::cell::RefCell;

use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams},
        GateInstructions, RangeInstructions,
    },
    halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        halo2curves::bn256::Fr,
        plonk::{Circuit, ConstraintSystem, Error},
    },
    poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher},
    AssignedValue, QuantumCell,
};
use halo2_base::halo2_proofs::halo2curves::group::ff::Field;
use gosh_sha256_chip::Sha256Chip;
use gosh_dense_balanced_tree::{
    verify_chain_of_dense_proofs, DenseChainLink, MAX_CHAIN_LEN,
};

use crate::{
    decompose_fr_to_bytes, LayerHashesCircuitParams, LayerHashesConfig,
    LAYER_PREIMAGE_SIZE, MAX_LAYERS, NUM_MERKLE_SIBLINGS,
    POSEIDON_R_F, POSEIDON_R_P, POSEIDON_RATE, POSEIDON_T,
};

// ---------------------------------------------------------------------------
// Core constraint builder
// ---------------------------------------------------------------------------

/// Build all circuit constraints for Layer Hashes Prover.
///
/// Returns the public instance values in order:
/// [0]     = block_id
/// [1]     = bk_set_poseidon_hash  (passthrough)
/// [2]     = num_layers
/// [3..13] = layer_hash_frs[0..10]
/// [13]    = prev_max_level_layer_hash
fn build_layer_hashes_constraints(
    builder: &mut BaseCircuitBuilder<Fr>,
    layer_hashes_preimage: &[u8; LAYER_PREIMAGE_SIZE],
    merkle_siblings: &[[u8; 32]; NUM_MERKLE_SIBLINGS],
    prev_max_level_layer_hash: Fr,
    num_prev_chain_steps: u8,
    prev_chain_proofs: &[DenseChainLink],
    bk_set_poseidon_hash: Fr,
) -> Vec<AssignedValue<Fr>> {
    let range = builder.range_chip();

    // -----------------------------------------------------------------------
    // A. Load preimage bytes, range-check, Poseidon hash -> L0
    // -----------------------------------------------------------------------
    let preimage_cells: Vec<AssignedValue<Fr>> = {
        let ctx = builder.main(0);
        layer_hashes_preimage
            .iter()
            .map(|&b| {
                let cell = ctx.load_witness(Fr::from(b as u64));
                range.range_check(ctx, cell, 8);
                cell
            })
            .collect()
    };

    // Split 331 bytes into 11 Fr elements (31 bytes each, LE packing).
    let l0_fr = {
        let ctx = builder.main(0);
        let gate = range.gate();

        let mut poseidon_input = Vec::with_capacity(11);
        for chunk_idx in 0..11 {
            let start = chunk_idx * 31;
            let end = std::cmp::min(start + 31, LAYER_PREIMAGE_SIZE);
            let chunk = &preimage_cells[start..end];
            let fr_elem = gate.inner_product(
                ctx,
                chunk.iter().map(|&b| QuantumCell::Existing(b)),
                (0..chunk.len()).map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64]))),
            );
            poseidon_input.push(fr_elem);
        }

        // Poseidon hash of the 11 elements -> L0
        let spec = OptimizedPoseidonSpec::<Fr, POSEIDON_T, POSEIDON_RATE>::new::<
            POSEIDON_R_F,
            POSEIDON_R_P,
            0,
        >();
        let mut poseidon = PoseidonHasher::<Fr, POSEIDON_T, POSEIDON_RATE>::new(spec);
        poseidon.initialize_consts(ctx, gate);
        poseidon.hash_fix_len_array(ctx, gate, &poseidon_input)
    };

    // Convert L0 Fr to 32 LE bytes for SHA-256 input.
    let l0_bytes = {
        let ctx = builder.main(0);
        decompose_fr_to_bytes(ctx, &range, l0_fr)
    };

    // -----------------------------------------------------------------------
    // B. SHA-256 Merkle path: open L0 up the depth-4 block-id tree.
    //
    //    block_id tree layout (poseidon_profile_new canonical shape):
    //        L0 = Poseidon(layer_hashes_preimage)        <- opened here
    //        L1..L7 = per-block content leaves
    //        L8      = tracked_ext_out_messages_root
    //        L9..L15 = [0u8; 32] protocol-fixed padding
    //    Combine rule: SHA-256(left_32B || right_32B) at every internal node.
    //
    //    L0's opening path has 4 opaque 32-byte siblings:
    //        sibling[0] = L1                (level 0)
    //        sibling[1] = sha_pair(L2, L3)  (level 1)
    //        sibling[2] = subtree(L4..L7)   (level 2)
    //        sibling[3] = subtree(L8..L15)  (level 3)
    //    → 4 SHA-256 compressions total to reconstruct block_id.
    // -----------------------------------------------------------------------
    let root_hash_bytes = {
        let ctx = builder.main(0);
        let sha256_chip = Sha256Chip::new(&range);

        // Load merkle siblings as witness bytes.
        let sibling_cells: Vec<Vec<AssignedValue<Fr>>> = merkle_siblings
            .iter()
            .map(|sib| {
                sib.iter()
                    .map(|&b| {
                        let cell = ctx.load_witness(Fr::from(b as u64));
                        range.range_check(ctx, cell, 8);
                        cell
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        // H_0 = SHA-256(L0_bytes || sibling[0])            [pair with L1]
        let mut h0_input = l0_bytes.clone();
        h0_input.extend_from_slice(&sibling_cells[0]);
        let h0 = sha256_chip.digest_bytes(ctx, &h0_input);

        // H_01 = SHA-256(H_0 || sibling[1])                [pair with h23]
        let mut h01_input = h0;
        h01_input.extend_from_slice(&sibling_cells[1]);
        let h01 = sha256_chip.digest_bytes(ctx, &h01_input);

        // H_012 = SHA-256(H_01 || sibling[2])              [pair with h4_7]
        let mut h012_input = h01;
        h012_input.extend_from_slice(&sibling_cells[2]);
        let h012 = sha256_chip.digest_bytes(ctx, &h012_input);

        // Root = SHA-256(H_012 || sibling[3])              [pair with h8_15]
        let mut root_input = h012;
        root_input.extend_from_slice(&sibling_cells[3]);
        sha256_chip.digest_bytes(ctx, &root_input)
    };

    // -----------------------------------------------------------------------
    // C. Convert root hash (32 BE bytes from SHA-256) to Fr and constrain
    //    as block_id (public[0]).
    //    SHA-256 outputs big-endian bytes. The block_id Fr is computed
    //    from 32 LE bytes. So we reverse the byte order before packing.
    // -----------------------------------------------------------------------
    let block_id_fr = {
        let ctx = builder.main(0);
        let gate = range.gate();

        // SHA-256 output is big-endian; reverse to get LE bytes.
        let root_le: Vec<AssignedValue<Fr>> = root_hash_bytes.iter().rev().cloned().collect();

        gate.inner_product(
            ctx,
            root_le.iter().map(|&b| QuantumCell::Existing(b)),
            (0..32).map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64]))),
        )
    };

    // -----------------------------------------------------------------------
    // D. Parse preimage: extract num_layers, layer numbers, root hashes.
    // -----------------------------------------------------------------------
    let num_layers_cell;
    let layer_hash_frs;
    {
        let ctx = builder.main(0);
        let gate = range.gate();

        // num_layers = preimage[0], constrain 1 <= num_layers <= 10.
        num_layers_cell = preimage_cells[0];
        // num_layers >= 1: (num_layers - 1) fits in 4 bits [0..9]
        let one = ctx.load_constant(Fr::from(1u64));
        let nl_minus_1 = gate.sub(ctx, num_layers_cell, one);
        range.range_check(ctx, nl_minus_1, 4); // [0, 15], but values > 9 will fail layer constraints

        // num_layers <= 10: (10 - num_layers) fits in 4 bits [0..9]
        let ten = ctx.load_constant(Fr::from(10u64));
        let ten_minus_nl = gate.sub(ctx, ten, num_layers_cell);
        range.range_check(ctx, ten_minus_nl, 4);

        let mut frs = Vec::with_capacity(MAX_LAYERS);

        for i in 0..MAX_LAYERS {
            // Constrain layer_number == i+1.
            let layer_number_cell = preimage_cells[1 + i * 33];
            let expected_layer_num = ctx.load_constant(Fr::from((i + 1) as u64));
            ctx.constrain_equal(&layer_number_cell, &expected_layer_num);

            // Extract root_hash bytes [2+i*33 .. 2+i*33+32], convert to Fr (LE).
            let hash_start = 2 + i * 33;
            let hash_bytes = &preimage_cells[hash_start..hash_start + 32];
            let hash_fr = gate.inner_product(
                ctx,
                hash_bytes.iter().map(|&b| QuantumCell::Existing(b)),
                (0..32).map(|j| QuantumCell::Constant(Fr::from(256u64).pow([j as u64]))),
            );

            // If i >= num_layers, constrain hash_fr == 0.
            // is_active = (i < num_layers) = is_less_than(i_const, num_layers_cell)
            let i_const = ctx.load_constant(Fr::from(i as u64));
            let is_active = range.is_less_than(ctx, i_const, num_layers_cell, 4);
            let zero = ctx.load_constant(Fr::zero());
            // When inactive (is_active=0), hash_fr must be zero.
            // Enforce: (1 - is_active) * hash_fr == 0
            let one_minus_active = gate.sub(ctx, one, is_active);
            let check = gate.mul(ctx, one_minus_active, hash_fr);
            ctx.constrain_equal(&check, &zero);

            frs.push(hash_fr);
        }

        layer_hash_frs = frs;
    }

    // -----------------------------------------------------------------------
    // E. Poseidon Merkle chain: verify prev_max_level_layer_hash ->
    //    layer_hash_frs[num_layers - 1] using verify_chain_of_dense_proofs.
    // -----------------------------------------------------------------------
    let prev_hash_cell = {
        let ctx = builder.main(0);
        ctx.load_witness(prev_max_level_layer_hash)
    };

    let num_steps_cell = {
        let ctx = builder.main(0);
        let gate = range.gate();
        let cell = ctx.load_witness(Fr::from(num_prev_chain_steps as u64));
        // Range check: num_prev_chain_steps in [1, MAX_CHAIN_LEN].
        let one = ctx.load_constant(Fr::from(1u64));
        let s_minus_1 = gate.sub(ctx, cell, one);
        range.range_check(ctx, s_minus_1, 4); // [0, 15]
        let max = ctx.load_constant(Fr::from(MAX_CHAIN_LEN as u64));
        let max_minus_s = gate.sub(ctx, max, cell);
        range.range_check(ctx, max_minus_s, 4);
        cell
    };

    // Run the chain verification.
    let chain_result = {
        let ctx = builder.main(0);
        let gate = range.gate();

        let spec = OptimizedPoseidonSpec::<Fr, POSEIDON_T, POSEIDON_RATE>::new::<
            POSEIDON_R_F,
            POSEIDON_R_P,
            0,
        >();
        let mut poseidon = PoseidonHasher::<Fr, POSEIDON_T, POSEIDON_RATE>::new(spec);
        poseidon.initialize_consts(ctx, gate);

        verify_chain_of_dense_proofs(
            ctx, &range, &poseidon, prev_hash_cell, prev_chain_proofs, num_steps_cell,
        )
    };

    // The chain result should equal layer_hash_frs[num_layers - 1].
    // We compute the target as: select layer_hash_frs[i] where i == num_layers - 1.
    let target_layer_hash = {
        let ctx = builder.main(0);
        let gate = range.gate();
        let one = ctx.load_constant(Fr::from(1u64));
        let target_idx = gate.sub(ctx, num_layers_cell, one);

        // Multiplexer: sum over i of (is_eq(i, target_idx) * layer_hash_frs[i])
        let mut result = ctx.load_constant(Fr::zero());
        for i in 0..MAX_LAYERS {
            let i_const = ctx.load_constant(Fr::from(i as u64));
            let is_eq = gate.is_equal(ctx, i_const, target_idx);
            let contribution = gate.mul(ctx, is_eq, layer_hash_frs[i]);
            result = gate.add(ctx, result, contribution);
        }
        result
    };

    {
        let ctx = builder.main(0);
        ctx.constrain_equal(&chain_result, &target_layer_hash);
    }

    // -----------------------------------------------------------------------
    // F. bk_set_poseidon_hash: passthrough witness.
    // -----------------------------------------------------------------------
    let bk_set_hash_cell = {
        let ctx = builder.main(0);
        ctx.load_witness(bk_set_poseidon_hash)
    };

    // -----------------------------------------------------------------------
    // Collect public instances.
    // -----------------------------------------------------------------------
    let mut instances = Vec::with_capacity(14);
    instances.push(block_id_fr);       // [0]
    instances.push(bk_set_hash_cell);       // [1]
    instances.push(num_layers_cell);        // [2]
    for i in 0..MAX_LAYERS {
        instances.push(layer_hash_frs[i]);  // [3..12]
    }
    instances.push(prev_hash_cell);         // [13]

    instances
}

// ---------------------------------------------------------------------------
// LayerHashesMovementCheckerCircuit
// ---------------------------------------------------------------------------

pub struct LayerHashesMovementCheckerCircuit {
    pub layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
    pub merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
    pub prev_max_level_layer_hash: Fr,
    pub num_prev_chain_steps: u8,
    pub prev_chain_proofs: Vec<DenseChainLink>,
    pub bk_set_poseidon_hash: Fr,
    pub params: LayerHashesCircuitParams,
    base_circuit_builder: RefCell<BaseCircuitBuilder<Fr>>,
}

impl LayerHashesMovementCheckerCircuit {
    pub fn new(
        layer_hashes_preimage: [u8; LAYER_PREIMAGE_SIZE],
        merkle_siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS],
        prev_max_level_layer_hash: Fr,
        num_prev_chain_steps: u8,
        prev_chain_proofs: Vec<DenseChainLink>,
        bk_set_poseidon_hash: Fr,
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
    ) -> Self {
        // Simulate witness generation to determine BaseCircuitParams.
        let base_circuit_params = Self::calculate_base_circuit_params(
            k,
            num_unusable_rows,
            lookup_bits,
            &layer_hashes_preimage,
            &merkle_siblings,
            prev_max_level_layer_hash,
            num_prev_chain_steps,
            &prev_chain_proofs,
            bk_set_poseidon_hash,
        );

        let params = LayerHashesCircuitParams {
            k,
            num_unusable_rows,
            base_circuit_params: base_circuit_params.clone(),
        };

        let mut base_circuit_builder = BaseCircuitBuilder::new(false);
        base_circuit_builder.set_params(base_circuit_params);

        Self {
            layer_hashes_preimage,
            merkle_siblings,
            prev_max_level_layer_hash,
            num_prev_chain_steps,
            prev_chain_proofs,
            bk_set_poseidon_hash,
            params,
            base_circuit_builder: RefCell::new(base_circuit_builder),
        }
    }

    /// Override the base circuit params (for reusing cached VK/PK across different inputs).
    pub fn override_base_circuit_params(&mut self, params: BaseCircuitParams) {
        self.params.base_circuit_params = params.clone();
        self.base_circuit_builder.borrow_mut().set_params(params);
    }

    pub fn base_circuit_params(&self) -> &BaseCircuitParams {
        &self.params.base_circuit_params
    }

    fn calculate_base_circuit_params(
        k: usize,
        num_unusable_rows: usize,
        lookup_bits: usize,
        layer_hashes_preimage: &[u8; LAYER_PREIMAGE_SIZE],
        merkle_siblings: &[[u8; 32]; NUM_MERKLE_SIBLINGS],
        prev_max_level_layer_hash: Fr,
        num_prev_chain_steps: u8,
        prev_chain_proofs: &[DenseChainLink],
        bk_set_poseidon_hash: Fr,
    ) -> BaseCircuitParams {
        let mut builder = BaseCircuitBuilder::<Fr>::new(false);
        builder.set_params(BaseCircuitParams {
            k,
            num_instance_columns: 1,
            lookup_bits: Some(lookup_bits),
            ..Default::default()
        });

        build_layer_hashes_constraints(
            &mut builder,
            layer_hashes_preimage,
            merkle_siblings,
            prev_max_level_layer_hash,
            num_prev_chain_steps,
            prev_chain_proofs,
            bk_set_poseidon_hash,
        );

        let params = builder.calculate_params(Some(num_unusable_rows));
        builder.clear();
        params
    }

    fn generate_witnesses(&self) {
        let mut builder = self.base_circuit_builder.borrow_mut();

        while builder.assigned_instances.len()
            < self.params.base_circuit_params.num_instance_columns
        {
            builder.assigned_instances.push(vec![]);
        }

        let instances = build_layer_hashes_constraints(
            &mut builder,
            &self.layer_hashes_preimage,
            &self.merkle_siblings,
            self.prev_max_level_layer_hash,
            self.num_prev_chain_steps,
            &self.prev_chain_proofs,
            self.bk_set_poseidon_hash,
        );

        for inst in instances {
            builder.assigned_instances[0].push(inst);
        }
    }
}

impl Circuit<Fr> for LayerHashesMovementCheckerCircuit {
    type Config = LayerHashesConfig;
    type FloorPlanner = SimpleFloorPlanner;
    type Params = LayerHashesCircuitParams;

    fn params(&self) -> Self::Params {
        self.params.clone()
    }

    fn without_witnesses(&self) -> Self {
        unimplemented!()
    }

    fn configure_with_params(meta: &mut ConstraintSystem<Fr>, params: Self::Params) -> Self::Config {
        LayerHashesConfig::configure_with_params(meta, params.base_circuit_params)
    }

    fn configure(_: &mut ConstraintSystem<Fr>) -> Self::Config {
        unreachable!("Use configure_with_params")
    }

    fn synthesize(
        &self,
        config: Self::Config,
        layouter: impl Layouter<Fr>,
    ) -> Result<(), Error> {
        self.generate_witnesses();
        self.base_circuit_builder
            .borrow()
            .synthesize(config.base_config, layouter)?;
        self.base_circuit_builder.borrow_mut().clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;
    use gosh_dense_balanced_tree::{
        bytes_to_fr, fr_to_bytes,
        preprocess_dense_proof, DenseChainLink, MAX_CHAIN_LEN,
    };
    use halo2_base::halo2_proofs::{dev::MockProver, halo2curves::bn256::Fr};
    use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
    use sha2::{Digest, Sha256};

    /// Build a synthetic layer_hashes preimage with `num_layers` active layers.
    /// Returns (preimage, layer_hash_frs) where layer_hash_frs has 10 entries
    /// (inactive ones are Fr::zero()).
    fn build_test_preimage(num_layers: u8) -> ([u8; LAYER_PREIMAGE_SIZE], Vec<Fr>) {
        assert!(num_layers >= 1 && num_layers <= 10);
        let mut preimage = [0u8; LAYER_PREIMAGE_SIZE];
        preimage[0] = num_layers;

        let mut layer_hash_frs = Vec::with_capacity(MAX_LAYERS);

        for i in 0..MAX_LAYERS {
            let offset = 1 + i * 33;
            preimage[offset] = (i + 1) as u8; // layer_number

            if (i as u8) < num_layers {
                // Active layer: fill with deterministic non-zero hash.
                let mut hash = [0u8; 32];
                for j in 0..32 {
                    hash[j] = ((i * 32 + j + 1) & 0xFF) as u8;
                }
                // Zero out high byte to ensure it fits in Fr.
                hash[31] = 0;
                preimage[offset + 1..offset + 1 + 32].copy_from_slice(&hash);
                layer_hash_frs.push(bytes_le_to_fr(&hash));
            } else {
                // Inactive: all zeros (already zeroed).
                layer_hash_frs.push(Fr::zero());
            }
        }

        (preimage, layer_hash_frs)
    }

    /// Compute the Poseidon hash of a 331-byte preimage using bridge_poseidon::poseidon_hash_bytes.
    fn compute_preimage_poseidon(preimage: &[u8; LAYER_PREIMAGE_SIZE]) -> Fr {
        let hash_bytes = bridge_poseidon::poseidon_hash_bytes(preimage);
        Fr::from_repr(hash_bytes).unwrap()
    }

    /// Native mirror of the in-circuit L0 opening walk up the depth-4 block-id
    /// tree. `l0_le_bytes` is 32 bytes (LE representation of `L0 = Fr`); the
    /// four siblings are opaque 32-byte digests taken from the block-id tree
    /// (see the circuit-side comment in section B for the sibling roles).
    /// SHA-256 output is big-endian.
    fn compute_sha256_merkle_root(
        l0_le_bytes: &[u8; 32],
        siblings: &[[u8; 32]; NUM_MERKLE_SIBLINGS],
    ) -> [u8; 32] {
        let mut acc: [u8; 32] = *l0_le_bytes;
        for sib in siblings.iter() {
            let mut buf = Vec::with_capacity(64);
            buf.extend_from_slice(&acc);
            buf.extend_from_slice(sib);
            acc = Sha256::digest(&buf).into();
        }
        acc
    }

    /// Build a test Poseidon dense Merkle chain.
    ///
    /// Creates a chain where each step uses a depth-1 tree (2 leaves, 1 sibling).
    /// Returns (chain_links, prev_hash_fr).
    /// The caller must set layer_hash_frs[num_layers-1] to the chain result.
    fn build_test_chain(
        num_steps: usize,
    ) -> (Vec<DenseChainLink>, Fr) {
        assert!(num_steps >= 1 && num_steps <= MAX_CHAIN_LEN);

        // Start with deterministic leaf bytes.
        let mut cur_bytes: [u8; 32] = [0u8; 32];
        cur_bytes[0] = 0x42;
        cur_bytes[1] = 0x13;
        cur_bytes[31] = 0; // Ensure valid Fr

        let prev_hash_fr = bytes_to_fr(&cur_bytes);

        let mut links = Vec::with_capacity(MAX_CHAIN_LEN);

        for step in 0..num_steps {
            let mut sibling = [0u8; 32];
            sibling[0] = (step as u8).wrapping_add(0xAA);
            sibling[1] = (step as u8).wrapping_add(0xBB);
            sibling[31] = 0;

            links.push(DenseChainLink {
                active: true,
                siblings: vec![sibling],
                position: 0,
                leaf_native: cur_bytes,
            });

            let proof = preprocess_dense_proof(cur_bytes, &[sibling], 0);
            let root_fr = gosh_dense_balanced_tree::compute_root_native(&proof);
            cur_bytes = fr_to_bytes(root_fr);
        }

        // Pad remaining links with inactive entries.
        let final_bytes = cur_bytes;
        for _ in num_steps..MAX_CHAIN_LEN {
            links.push(DenseChainLink::inactive(final_bytes, 1));
        }

        (links, prev_hash_fr)
    }

    /// Compute the chain result natively (same logic as in-circuit).
    fn compute_chain_result_native(
        prev_hash_fr: Fr,
        chain: &[DenseChainLink],
        num_steps: usize,
    ) -> Fr {
        let mut cur_bytes = fr_to_bytes(prev_hash_fr);
        for (j, link) in chain.iter().enumerate() {
            if j >= num_steps {
                break;
            }
            let proof = preprocess_dense_proof(
                cur_bytes,
                &link.siblings,
                link.position,
            );
            let root_fr = gosh_dense_balanced_tree::compute_root_native(&proof);
            cur_bytes = fr_to_bytes(root_fr);
        }
        bytes_to_fr(&cur_bytes)
    }

    #[test]
    fn test_layer_hashes_circuit_mock() {
        use std::time::Instant;
        let t_total = Instant::now();

        let num_layers: u8 = 3;
        let num_chain_steps: u8 = 2;

        // 1. Build preimage.
        let (mut preimage, mut layer_hash_frs) = build_test_preimage(num_layers);

        // 2. Build chain.
        let (chain_links, prev_hash_fr) = build_test_chain(num_chain_steps as usize);

        let chain_result = compute_chain_result_native(
            prev_hash_fr, &chain_links, num_chain_steps as usize,
        );

        // Update the preimage so that layer_hash_frs[num_layers-1] == chain_result.
        let target_idx = (num_layers - 1) as usize;
        let chain_result_bytes: [u8; 32] = chain_result.to_repr();
        let offset = 1 + target_idx * 33 + 1; // skip num_layers byte, layer_number byte
        preimage[offset..offset + 32].copy_from_slice(&chain_result_bytes);
        layer_hash_frs[target_idx] = bytes_le_to_fr(&chain_result_bytes);

        // 3. Compute L0 = Poseidon(preimage_chunks).
        let l0_fr = compute_preimage_poseidon(&preimage);
        let l0_bytes: [u8; 32] = l0_fr.to_repr();

        // 4. Generate deterministic merkle siblings for the L0 opening path
        //    up the depth-4 block-id tree, and compute the SHA-256 root.
        let siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS] = {
            let mut s = [[0u8; 32]; NUM_MERKLE_SIBLINGS];
            for i in 0..NUM_MERKLE_SIBLINGS {
                for j in 0..32 {
                    s[i][j] = ((i * 32 + j + 0x10) & 0xFF) as u8;
                }
            }
            s
        };

        let root_be = compute_sha256_merkle_root(&l0_bytes, &siblings);
        // Convert BE root to LE for Fr.
        let mut root_le = root_be;
        root_le.reverse();
        let block_id_fr = bytes_le_to_fr(&root_le);

        // 5. Random bk_set_poseidon_hash (passthrough).
        let bk_set_poseidon_hash = Fr::from(0xDEADBEEFu64);

        // 6. Compute expected public instances.
        let num_layers_fr = Fr::from(num_layers as u64);

        let mut expected_instances = vec![
            block_id_fr,
            bk_set_poseidon_hash,
            num_layers_fr,
        ];
        for i in 0..MAX_LAYERS {
            expected_instances.push(layer_hash_frs[i]);
        }
        expected_instances.push(prev_hash_fr); // prev_max_level_layer_hash

        println!("block_id_fr = {:?}", block_id_fr);
        println!("bk_set_poseidon_hash = {:?}", bk_set_poseidon_hash);
        println!("num_layers = {}", num_layers);
        println!("chain_result = {:?}", chain_result);
        println!("prev_hash_fr = {:?}", prev_hash_fr);
        println!("Total public instances: {}", expected_instances.len());

        // 7. Build circuit.
        let t = Instant::now();
        let circuit = LayerHashesMovementCheckerCircuit::new(
            preimage,
            siblings,
            prev_hash_fr,
            num_chain_steps,
            chain_links,
            bk_set_poseidon_hash,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
        );
        println!("[timing] circuit construction: {:?}", t.elapsed());
        println!(
            "base_circuit_params: {:?}",
            circuit.params.base_circuit_params
        );

        // 8. Run MockProver.
        println!("Running MockProver at K={}...", K);
        let t = Instant::now();
        let prover = MockProver::run(
            K,
            &circuit,
            vec![expected_instances],
        )
        .unwrap();
        println!("[timing] MockProver::run (witness gen): {:?}", t.elapsed());

        let t = Instant::now();
        prover.assert_satisfied();
        println!(
            "[timing] MockProver::assert_satisfied (constraint check): {:?}",
            t.elapsed()
        );

        println!("[timing] TOTAL: {:?}", t_total.elapsed());
        println!("MockProver passed ({} layers, {} chain steps)!", num_layers, num_chain_steps);
    }

    #[test]
    fn test_layer_hashes_circuit_max_layers() {
        use std::time::Instant;
        let t_total = Instant::now();

        let num_layers: u8 = 10;
        let num_chain_steps: u8 = 1;

        let (mut preimage, mut layer_hash_frs) = build_test_preimage(num_layers);

        let (chain_links, prev_hash_fr) = build_test_chain(num_chain_steps as usize);

        let chain_result = compute_chain_result_native(
            prev_hash_fr, &chain_links, num_chain_steps as usize,
        );

        let target_idx = (num_layers - 1) as usize;
        let chain_result_bytes: [u8; 32] = chain_result.to_repr();
        let offset = 1 + target_idx * 33 + 1;
        preimage[offset..offset + 32].copy_from_slice(&chain_result_bytes);
        layer_hash_frs[target_idx] = bytes_le_to_fr(&chain_result_bytes);

        let l0_fr = compute_preimage_poseidon(&preimage);
        let l0_bytes: [u8; 32] = l0_fr.to_repr();

        let siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS] = {
            let mut s = [[0u8; 32]; NUM_MERKLE_SIBLINGS];
            for i in 0..NUM_MERKLE_SIBLINGS {
                for j in 0..32 {
                    s[i][j] = ((i * 32 + j + 0x20) & 0xFF) as u8;
                }
            }
            s
        };

        let root_be = compute_sha256_merkle_root(&l0_bytes, &siblings);
        let mut root_le = root_be;
        root_le.reverse();
        let block_id_fr = bytes_le_to_fr(&root_le);

        let bk_set_poseidon_hash = Fr::from(0xCAFEBABEu64);

        let mut expected_instances = vec![
            block_id_fr,
            bk_set_poseidon_hash,
            Fr::from(num_layers as u64),
        ];
        for i in 0..MAX_LAYERS {
            expected_instances.push(layer_hash_frs[i]);
        }
        expected_instances.push(prev_hash_fr);

        let circuit = LayerHashesMovementCheckerCircuit::new(
            preimage,
            siblings,
            prev_hash_fr,
            num_chain_steps,
            chain_links,
            bk_set_poseidon_hash,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
        );
        println!(
            "base_circuit_params: {:?}",
            circuit.params.base_circuit_params
        );

        println!("Running MockProver at K={}...", K);
        let t = Instant::now();
        let prover = MockProver::run(
            K,
            &circuit,
            vec![expected_instances],
        )
        .unwrap();
        println!("[timing] MockProver::run: {:?}", t.elapsed());

        let t = Instant::now();
        prover.assert_satisfied();
        println!("[timing] MockProver::assert_satisfied: {:?}", t.elapsed());
        println!("[timing] TOTAL: {:?}", t_total.elapsed());
        println!("MockProver passed (10 layers, 1 chain step)!");
    }

    /// Wrong wrong block_id should fail.
    #[test]
    #[should_panic]
    fn test_layer_hashes_circuit_wrong_block_id_fails() {
        let num_layers: u8 = 3;
        let num_chain_steps: u8 = 1;

        let (mut preimage, mut layer_hash_frs) = build_test_preimage(num_layers);

        let (chain_links, prev_hash_fr) = build_test_chain(num_chain_steps as usize);
        let chain_result = compute_chain_result_native(
            prev_hash_fr, &chain_links, num_chain_steps as usize,
        );

        let target_idx = (num_layers - 1) as usize;
        let chain_result_bytes: [u8; 32] = chain_result.to_repr();
        let offset = 1 + target_idx * 33 + 1;
        preimage[offset..offset + 32].copy_from_slice(&chain_result_bytes);
        layer_hash_frs[target_idx] = bytes_le_to_fr(&chain_result_bytes);

        let l0_fr = compute_preimage_poseidon(&preimage);
        let l0_bytes: [u8; 32] = l0_fr.to_repr();

        let siblings: [[u8; 32]; NUM_MERKLE_SIBLINGS] = {
            let mut s = [[0u8; 32]; NUM_MERKLE_SIBLINGS];
            for i in 0..NUM_MERKLE_SIBLINGS {
                for j in 0..32 {
                    s[i][j] = ((i * 32 + j + 0x10) & 0xFF) as u8;
                }
            }
            s
        };

        let _root_be = compute_sha256_merkle_root(&l0_bytes, &siblings);
        
        let wrong_block_id = Fr::from(0x12345678u64);
        let bk_set_poseidon_hash = Fr::from(0xDEADBEEFu64);

        let mut expected_instances = vec![
            wrong_block_id,  // WRONG!
            bk_set_poseidon_hash,
            Fr::from(num_layers as u64),
        ];
        for i in 0..MAX_LAYERS {
            expected_instances.push(layer_hash_frs[i]);
        }
        expected_instances.push(prev_hash_fr);

        let circuit = LayerHashesMovementCheckerCircuit::new(
            preimage,
            siblings,
            prev_hash_fr,
            num_chain_steps,
            chain_links,
            bk_set_poseidon_hash,
            K as usize,
            NUM_UNUSABLE_ROWS,
            LOOKUP_BITS,
        );

        let prover = MockProver::run(K, &circuit, vec![expected_instances]).unwrap();
        prover.assert_satisfied(); // Should panic
    }
}
