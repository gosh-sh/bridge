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
//!
//! # positive: re-shape the header to another chain's field count. Only
//! # `transactionsRoot` / `receiptsRoot` (fields 4/5) and the keccak bind the
//! # deposit, so fields 0–15 are kept and the tail is cut or extended:
//! # `amsterdam` (23 fields), `spare` (24, the table's full width) and
//! # `arbitrum` (16). Each must still satisfy every constraint.
//! cargo run --release --example mock_fixture -- <input.json> --reshape amsterdam
//! ```
//!
//! Each run costs ~2 min at k=18, which is why this is an example and not a
//! `#[test]`.

use std::{env, fs};

use anyhow::{bail, Context, Result};
use deposit_prover::{
    prover::{test_circuit_mock_pinned, CircuitConfig},
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

    if let Some(shape) = args
        .iter()
        .position(|a| a == "--reshape")
        .and_then(|i| args.get(i + 1))
    {
        let header = &mut input.receipt_proof.block_header_rlp;
        *header = reshape_header(header, shape)?;
        println!(
            "  RESHAPED    = {shape}: {} fields, {} B",
            rlp::Rlp::new(header).item_count()?,
            header.len()
        );
    }

    let expect_reject = match mutate.as_deref() {
        None => false,
        Some("header-pad") => {
            input.receipt_proof.block_header_rlp.push(0u8);
            println!(
                "  MUTATED     = header padded to {} B (BC-D03)",
                input.receipt_proof.block_header_rlp.len()
            );
            true
        },
        Some(other) => bail!("unknown mutation {other:?}; known: header-pad"),
    };

    let outcome = test_circuit_mock_pinned(input, &CircuitConfig::production());

    match (expect_reject, outcome) {
        (false, Ok(())) => {
            println!("\n✓ all constraints satisfied");
            Ok(())
        },
        (false, Err(e)) => bail!("MockProver rejected an unmutated witness: {e}"),
        (true, Err(e)) => {
            let head = e.lines().take(4).collect::<Vec<_>>().join("\n");
            println!("\n✓ rejected as expected:\n{head}");
            Ok(())
        },
        (true, Ok(())) => {
            bail!("MockProver ACCEPTED the mutated witness — the constraint is not doing its job")
        },
    }
}

/// Re-encode `header` with fields 0–15 kept and the post-London tail replaced
/// by the one `shape` names.
fn reshape_header(header: &[u8], shape: &str) -> Result<Vec<u8>> {
    let fields: Vec<Vec<u8>> = rlp::Rlp::new(header)
        .iter()
        .map(|f| f.as_raw().to_vec())
        .collect();
    if fields.len() < 21 {
        bail!("--reshape needs a Prague (21-field) witness, got {} fields", fields.len());
    }
    // Amsterdam values from fixtures/headers/sepolia_amsterdam.json.
    let block_access_list_hash =
        hex::decode("7770d6dfe7d3dd2ba81c5fb1455d1e19d68a56978485d279117b126299dcb8e2")?;
    let slot_number = vec![0xac, 0x82, 0x13];
    let mut tail: Vec<Vec<u8>> = Vec::new();
    let keep = match shape {
        "arbitrum" => 16,
        "amsterdam" | "spare" => {
            tail.push(rlp::encode(&block_access_list_hash).to_vec());
            tail.push(rlp::encode(&slot_number).to_vec());
            if shape == "spare" {
                tail.push(rlp::encode(&vec![0xffu8; 32]).to_vec());
            }
            21
        }
        other => bail!("unknown shape {other:?}; known: amsterdam, spare, arbitrum"),
    };
    let mut s = rlp::RlpStream::new_list(keep + tail.len());
    for f in fields.iter().take(keep).chain(tail.iter()) {
        s.append_raw(f, 1);
    }
    Ok(s.out().to_vec())
}
