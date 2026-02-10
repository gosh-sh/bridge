//! Groth16 wrapper module for wrapping Halo2 proofs in Groth16 for Ethereum
//! mainnet deployment.
//!
//! This module provides functionality to:
//! 1. Parse Halo2 Snark proofs into a format suitable for gnark
//! 2. Export proof data to JSON for consumption by the Go gnark circuit
//! 3. Interface with the Go gnark-wrapper via FFI (future)

pub mod proof_parser;

pub use proof_parser::{
    load_and_parse_snark, parse_snark_for_gnark, save_proof_data_json, Halo2ProofData,
};
