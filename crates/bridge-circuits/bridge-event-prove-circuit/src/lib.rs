pub mod boc_helper;
pub mod bridge_event_prove_circuit;
pub mod event_data_helper;
pub mod poseidon;

// `test_helpers` was originally `#[cfg(test)]`-only. It's now a regular
// public module so downstream crates (e.g. `bridge-prover-lib::keys` for
// `ensure_event_keys`) can reuse the same native-Poseidon-based synthetic
// witness builders without re-implementing them. The dependency on
// `dense-balanced-tree` was promoted to a regular dep accordingly.
pub mod test_helpers;
