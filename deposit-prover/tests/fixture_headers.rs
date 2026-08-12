//! Offline guard for the fixture witnesses' block headers.
//!
//! Every witness carries `receipt_proof.block_header_rlp`, and the circuit turns
//! its keccak hash into the `blockHash` public input. If that header is missing
//! a consensus field — which is exactly what happened when ethers-core 2.0.14
//! silently dropped the Prague `requestsHash` — the proof still verifies, but it
//! commits to a block that does not exist. Nothing downstream can catch that.
//!
//! So the canonical hashes live in `fixtures/canonical_block_hashes.json`
//! (read from the chain, refreshed by
//! `scripts/recanonicalize_fixture_headers.py`) and this test pins every witness
//! against them without needing network access.

use std::{collections::BTreeMap, fs, path::Path};

use ethers::utils::keccak256;
use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    blocks: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Witness {
    event_data: EventData,
    receipt_proof: ReceiptProof,
}

#[derive(Deserialize)]
struct EventData {
    block_number: u64,
}

#[derive(Deserialize)]
struct ReceiptProof {
    block_header_rlp: Vec<u8>,
}

fn collect_witnesses(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("fixtures dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_witnesses(&path, out);
        } else if path.file_name().is_some_and(|n| n == "input.json") {
            out.push(path);
        }
    }
}

#[test]
fn every_fixture_header_hashes_to_its_canonical_block() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(fixtures.join("canonical_block_hashes.json")).unwrap())
            .unwrap();

    let mut witnesses = Vec::new();
    collect_witnesses(&fixtures, &mut witnesses);
    assert!(!witnesses.is_empty(), "no fixture witnesses found");

    for path in witnesses {
        let witness: Witness =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let block = witness.event_data.block_number;
        let expected = manifest.blocks.get(&block.to_string()).unwrap_or_else(|| {
            panic!(
                "{}: block {block} is missing from canonical_block_hashes.json — add it \
                 (see scripts/recanonicalize_fixture_headers.py)",
                path.display()
            )
        });
        let actual = format!("0x{}", hex::encode(keccak256(&witness.receipt_proof.block_header_rlp)));
        assert_eq!(
            &actual,
            expected,
            "{}: header RLP ({} B) hashes to a non-canonical block",
            path.display(),
            witness.receipt_proof.block_header_rlp.len()
        );
    }
}
