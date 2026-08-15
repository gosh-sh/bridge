//! TD-57 — `parse_and_validate_dapp_id` hex fuzz / width boundary (no silent truncate).
//!
//! Catalog `TD-57`: CLI/env `AN_DAPP_ID` parsing is fail-closed; max 32-byte hex;
//! canonical lowercase `0x` prefix; `allow_zero` gate (QC-OFF-09).

use deposit_relayer_daemon::{error::RelayerError, parse_and_validate_dapp_id};
use proptest::prelude::*;

const MAX_U256_HEX: &str =
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const MAX_U256_CANONICAL: &str =
    "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn err_string(err: &RelayerError) -> String {
    err.to_string()
}

fn assert_qc_off_09_reject(raw: &str) {
    let err = parse_and_validate_dapp_id(raw, false)
        .expect_err("expected live-path reject for zero dappId");
    let msg = err_string(&err);
    assert!(
        msg.contains("non-zero") && msg.contains("dry-run"),
        "QC-OFF-09 snippet missing in: {msg}"
    );
}

#[test]
fn td_57_accept_matrix_allow_zero_true() {
    assert_eq!(
        parse_and_validate_dapp_id("0x1a1a1a1a1a", true).unwrap(),
        "0x1a1a1a1a1a"
    );
    assert_eq!(
        parse_and_validate_dapp_id("1A1A1A1A1A", true).unwrap(),
        "0x1a1a1a1a1a"
    );
    assert_eq!(
        parse_and_validate_dapp_id("0x0001", true).unwrap(),
        "0x1"
    );
    assert_eq!(
        parse_and_validate_dapp_id(MAX_U256_HEX, true).unwrap(),
        MAX_U256_CANONICAL
    );
    let prefixed_max = format!("0x{MAX_U256_HEX}");
    assert_eq!(
        parse_and_validate_dapp_id(&prefixed_max, true).unwrap(),
        MAX_U256_CANONICAL
    );
}

#[test]
fn td_57_reject_matrix_garbage_overwidth_and_nul() {
    let rejects: &[&str] = &[
        "",
        "zz",
        "0xzz",
        &"f".repeat(65),
        &format!("0x{}", "f".repeat(65)),
        &"ab".repeat(33),
        "   ",
        "0x1\0a",
        "a\0b",
    ];

    for raw in rejects {
        let err = parse_and_validate_dapp_id(raw, true)
            .expect_err(&format!("expected reject for {raw:?}"));
        let msg = err_string(&err);
        assert!(
            msg.contains("empty")
                || msg.contains("not valid hex")
                || msg.contains("exceeds 32 bytes"),
            "unexpected reject reason for {raw:?}: {msg}"
        );
    }
}

#[test]
fn td_57_zero_gate_qc_off_09_when_allow_zero_false() {
    for raw in ["0", "0x0", "0x00"] {
        assert_qc_off_09_reject(raw);
    }
    assert_eq!(parse_and_validate_dapp_id("0", true).unwrap(), "0x0");
}

#[test]
fn td_57_max_u256_width_one_char_wider_rejects_no_truncate() {
    let max = parse_and_validate_dapp_id(MAX_U256_HEX, true).unwrap();
    assert_eq!(max, MAX_U256_CANONICAL);

    let one_wider = format!("1{}", MAX_U256_HEX);
    let err = parse_and_validate_dapp_id(&one_wider, true).expect_err("65 hex digits");
    assert!(
        err_string(&err).contains("exceeds 32 bytes"),
        "expected width reject, got {}",
        err_string(&err)
    );

    let prefixed_wider = format!("0x1{}", MAX_U256_HEX);
    let err2 = parse_and_validate_dapp_id(&prefixed_wider, true).expect_err("0x + 65 hex");
    assert!(
        err_string(&err2).contains("exceeds 32 bytes"),
        "expected width reject for prefixed wider input"
    );
}

proptest! {
    #[test]
    fn td_57_random_ascii_never_panics(s in "\\PC*") {
        let _ = parse_and_validate_dapp_id(&s, true);
    }

    #[test]
    fn td_57_hex_only_acceptance_bounded_by_width(
        body in prop::collection::vec(
            prop::sample::select(vec!['0','1','a','b','c','d','e','f']),
            0usize..=70
        )
    ) {
        let raw: String = body.iter().collect();
        let result = parse_and_validate_dapp_id(&raw, true);
        if raw.is_empty() {
            prop_assert!(result.is_err());
        } else if raw.len() > 64 {
            prop_assert!(result.is_err());
            let msg = err_string(&result.unwrap_err());
            prop_assert!(msg.contains("exceeds 32 bytes"));
        } else {
            prop_assert!(result.is_ok());
        }
    }
}
