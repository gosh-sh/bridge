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
//! # deposit. `amsterdam` / `spare` keep the Prague 21 and append the tail;
//! # `arbitrum` keeps the first 16. Each must still satisfy every constraint.
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
    let mutate = flag_value(&args, "--mutate")?;
    let reshape = flag_value(&args, "--reshape")?;

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

    if let Some(shape) = reshape.as_deref() {
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

fn flag_value(args: &[String], name: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    match args.get(i + 1).filter(|v| !v.starts_with("--")) {
        Some(v) => Ok(Some(v.clone())),
        None => bail!("{name} needs a value"),
    }
}

/// Re-encode `header`, keeping the prefix `shape` needs and replacing the tail.
fn reshape_header(header: &[u8], shape: &str) -> Result<Vec<u8>> {
    let fields: Vec<Vec<u8>> = rlp::Rlp::new(header)
        .iter()
        .map(|f| f.as_raw().to_vec())
        .collect();
    // `blockAccessListHash` / `slotNumber` from
    // fixtures/headers/sepolia_amsterdam.json.
    let block_access_list_hash =
        hex::decode("14987b93fe0eb2981c574f7de2a7a2cf704803b24eb3f6eeb378e9f6cd2d7aef")?;
    let slot_number = vec![0xac, 0x82, 0x24];
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
        },
        other => bail!("unknown shape {other:?}; known: amsterdam, spare, arbitrum"),
    };
    if fields.len() < keep {
        bail!(
            "--reshape {shape} needs a {keep}-field witness, got {} fields",
            fields.len()
        );
    }
    let mut s = rlp::RlpStream::new_list(keep + tail.len());
    for f in fields.iter().take(keep).chain(tail.iter()) {
        s.append_raw(f, 1);
    }
    Ok(s.out().to_vec())
}
