//! Run one saved witness through `MockProver` — the cheap pre-flight before
//! paying for a keygen + prove cycle after a constraint change.
//!
//! ```bash
//! # positive: the witness must satisfy every constraint
//! cargo run --release --example mock_fixture -- fixtures/deposit_10proofs/proof_00/input.json
//!
//! # negative: mutate the witness so a specific constraint MUST reject it.
//! # `header-pad` is the BC-D03 reproducer — it appends a byte to the header
//! # buffer, which shifts `blockHash` while leaving the RLP list prefix (and so
//! # `receiptsRoot`) untouched. Before BC-D03 the circuit accepted this, meaning
//! # one event could carry many valid `blockHash` public inputs.
//! cargo run --release --example mock_fixture -- <input.json> --mutate header-pad
//! ```
//!
//! Each run costs ~2 min at k=18, which is why this is an example and not a
//! `#[test]`.

use std::{env, fs};

use anyhow::{bail, Context, Result};
use deposit_prover::{
    prover::{test_circuit_mock, CircuitConfig},
    types::DepositProofInput,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let path = args
        .first()
        .cloned()
        .unwrap_or_else(|| "fixtures/deposit_10proofs/proof_00/input.json".to_string());
    let mutate = args
        .iter()
        .position(|a| a == "--mutate")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let json = fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    let mut input: DepositProofInput =
        serde_json::from_str(&json).with_context(|| format!("parsing {path}"))?;

    println!("witness: {path}");
    println!("  chain_id    = {}", input.witness_chain_id()?);
    println!("  deposit_id  = {}", input.event_data.deposit_id);
    println!(
        "  header len  = {} B",
        input.receipt_proof.block_header_rlp.len()
    );

    let expect_reject = match mutate.as_deref() {
        None => false,
        Some("header-pad") => {
            input.receipt_proof.block_header_rlp.push(0u8);
            println!(
                "  MUTATED     = header padded to {} B (BC-D03)",
                input.receipt_proof.block_header_rlp.len()
            );
            true
        }
        Some(other) => bail!("unknown mutation {other:?}; known: header-pad"),
    };

    let outcome = test_circuit_mock(input, &CircuitConfig::default());

    match (expect_reject, outcome) {
        (false, Ok(())) => {
            println!("\n✓ all constraints satisfied");
            Ok(())
        }
        (false, Err(e)) => bail!("MockProver rejected an unmutated witness: {e}"),
        (true, Err(e)) => {
            let head = e.lines().take(4).collect::<Vec<_>>().join("\n");
            println!("\n✓ rejected as expected:\n{head}");
            Ok(())
        }
        (true, Ok(())) => {
            bail!("MockProver ACCEPTED the mutated witness — the constraint is not doing its job")
        }
    }
}
