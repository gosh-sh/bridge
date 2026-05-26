//! R15 / M2 feasibility spike for the snark-verifier aggregator pipeline.
//!
//! This crate is **not** the real Circuit 4 verifier yet — it is the
//! standalone "does the snark-verifier-sdk → Yul EVM verifier pipeline work
//! in our build environment?" experiment. When the partner's Circuit 4 lands
//! (M4 in `docs/r15_snark_verifier_roadmap.md`) the trivial multiplication
//! circuit exercised here is swapped for the real one (M5), the Yul output
//! becomes `BridgeWithdrawalAggregatorVerifier.sol`, and the Foundry harness
//! moves out of `contracts/ethereum/test/spike/` into the real test suite
//! (M6 + M7).
//!
//! ## Pipeline implemented here (M2)
//!
//! 1. [`multiply::build_multiply_circuit`] — minimal halo2 circuit proving
//!    knowledge of `(a, b)` such that `a * b = c` where `c` is the only public
//!    input. Uses [`halo2_base::BaseCircuitBuilder`] with
//!    [`halo2_base::gates::GateChip`].
//!
//! 2. [`aggregator::prove_inner`] — generates a SHPLONK SNARK of the inner
//!    multiply circuit (Poseidon transcript — what snark-verifier-sdk expects).
//!
//! 3. [`aggregator::aggregate`] — wraps the inner SNARK in
//!    `snark_verifier_sdk::halo2::aggregation::AggregationCircuit` and proves
//!    it (also SHPLONK, also Poseidon transcript). Aggregator's public
//!    instances are the inner instances (the multiplication result) plus the
//!    KZG accumulator (4 limbs).
//!
//! 4. [`aggregator::generate_yul_verifier`] — calls
//!    `snark_verifier_sdk::evm::gen_evm_verifier_shplonk` to emit a Yul
//!    Solidity verifier contract for the aggregator. Output target size is the
//!    EIP-170 24 576-byte runtime limit; deposit-prover empirics suggest ~10–15
//!    KB at `k_outer = 21`.
//!
//! See `examples/run_spike.rs` for the end-to-end driver and
//! `tests/round_trip.rs` for the Rust-only acceptance check.

pub mod aggregator;
pub mod multiply;
