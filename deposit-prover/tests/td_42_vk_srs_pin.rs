//! TD-42 / DEP-VK-SRS-PIN — VkBlob fixture pin + USDCBridge embedded VK hash gate.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

/// Pinned SHA-256 of `fixtures/deposit_10proofs/deposit_vk_blob.bin`.
pub const EXPECTED_VK_SHA256: &str =
    "9dacd998af5fd03af8097cb80a571df098c925bba235af61d920cc808360fae3";
pub const EXPECTED_VK_SIZE: usize = 5006;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fixture_vk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/deposit_10proofs/deposit_vk_blob.bin")
}

fn usdc_bridge_sol_path() -> PathBuf {
    repo_root().join("audit/spec/an-contracts/exchange/USDCBridge.sol")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// Decode concatenated `hex"…"` chunks from `bytes constant VK_BLOB = … ;`.
pub fn extract_vk_blob_from_usdc_bridge(sol: &str) -> Vec<u8> {
    let marker = "bytes constant VK_BLOB";
    let start = sol.find(marker).expect("VK_BLOB constant");
    let after_eq = sol[start..]
        .find('=')
        .map(|i| start + i + 1)
        .expect("VK_BLOB assignment");
    let end = sol[after_eq..]
        .find(';')
        .map(|i| after_eq + i)
        .expect("VK_BLOB terminator");
    let block = &sol[after_eq..end];
    let mut out = Vec::new();
    for part in block.split("hex\"") {
        if let Some((hex, _)) = part.split_once('"') {
            if hex.is_empty() {
                continue;
            }
            if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                out.extend(hex::decode(hex).expect("valid vk hex"));
            }
        }
    }
    out
}

#[test]
fn td_42_fixture_vk_sha256_matches_pin() {
    let bytes = fs::read(fixture_vk_path()).expect("fixture vk blob");
    assert_eq!(bytes.len(), EXPECTED_VK_SIZE);
    assert_eq!(sha256_hex(&bytes), EXPECTED_VK_SHA256);

    let pin_path = fixture_vk_path().with_extension("bin.sha256");
    let pin_line = fs::read_to_string(pin_path).expect("pin file");
    let pinned = pin_line.split_whitespace().next().expect("pin hash");
    assert_eq!(pinned, EXPECTED_VK_SHA256);
}

#[test]
fn td_42_usdcbridge_embedded_vk_matches_fixture() {
    let fixture = fs::read(fixture_vk_path()).expect("fixture");
    let sol = fs::read_to_string(usdc_bridge_sol_path()).expect("USDCBridge.sol");
    let embedded = extract_vk_blob_from_usdc_bridge(&sol);
    assert_eq!(embedded.len(), EXPECTED_VK_SIZE);
    assert_eq!(embedded, fixture);
    assert_eq!(sha256_hex(&embedded), EXPECTED_VK_SHA256);
}

#[test]
fn td_42_downgrade_truncated_vk_detected() {
    let fixture = fs::read(fixture_vk_path()).expect("fixture");
    let truncated = &fixture[..256];
    assert_ne!(sha256_hex(truncated), EXPECTED_VK_SHA256);
    assert_ne!(truncated.len(), EXPECTED_VK_SIZE);
}

#[test]
fn td_42_downgrade_empty_and_wrong_blob_detected() {
    assert_ne!(sha256_hex(&[]), EXPECTED_VK_SHA256);
    let wrong = b"304c1c4ed1e4cf09a00fb1d83a0ae2ba";
    assert_ne!(sha256_hex(wrong), EXPECTED_VK_SHA256);
    let audit_old = b"724687a4db00b11afd24500715e6b0ab";
    assert_ne!(sha256_hex(audit_old), EXPECTED_VK_SHA256);
}
