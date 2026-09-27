pub mod block_id_tree;
pub mod boc_helper;
pub mod bridge_event_final_proof;
pub mod bundle_verifier;
pub mod dense_merkle_bound;
pub mod event_data_helper;
pub mod event_primitives;
pub mod multi_hop_proof;
pub mod multi_hop_witness;
pub mod poseidon;

// `test_helpers` was originally `#[cfg(test)]`-only. It's now a regular
// public module so downstream crates (e.g. `bridge-prover-lib::keys` for
// `ensure_event_keys`) can reuse the same native-Poseidon-based synthetic
// witness builders without re-implementing them. The dependency on
// `dense-balanced-tree` was promoted to a regular dep accordingly.
pub mod test_helpers;
