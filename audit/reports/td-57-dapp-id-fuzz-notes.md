# TD-57 — `parse_and_validate_dapp_id` hex fuzz / modulus wrap

PoC: `crates/deposit-relayer-daemon/tests/td_57_dapp_id_hex_fuzz.rs`.

Implementation: `crates/deposit-relayer-daemon/src/types.rs` (`parse_and_validate_dapp_id`).

## Input → canonical output (accept, `allow_zero=true`)

| Input | Canonical output |
|-------|------------------|
| `0x1a1a1a1a1a` | `0x1a1a1a1a1a` |
| `1A1A1A1A1A` | `0x1a1a1a1a1a` |
| `0x0001` | `0x1` |
| 64× `f` (no prefix) | `0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff` |
| `0x` + 64× `f` | same max U256 |

## Reject matrix

| Input class | Revert / error snippet |
|-------------|------------------------|
| `""` | `AN_DAPP_ID is empty` |
| `"zz"`, `"0xzz"` | `is not valid hex` |
| 65+ hex chars | `exceeds 32 bytes` |
| `"ab".repeat(33)` | `exceeds 32 bytes` |
| whitespace-only | `is empty` (after trim) |
| embedded NUL | `is not valid hex` |
| `0`, `0x0`, `0x00` with `allow_zero=false` | `non-zero` + `dry-run` (QC-OFF-09) |

## Width / no silent truncate

| Case | Result |
|------|--------|
| 64 hex digits = max U256 | accept |
| 65 hex digits (`1` + 64× `f`) | reject width |
| `0x1` + 64× `f` (65 hex after prefix strip) | reject width |

Parse layer uses `U256::from_str_radix` on ≤64 hex digits — no Fr modulus reduction; values above U256 max are rejected by width, not wrapped.

## Cross-refs

| ID | Link |
|----|------|
| TD-05 | Wrong dappId in PI → AN `ERR_WRONG_DAPP` (223); parse is upstream of PI stamp |
| TD-50 | BN254 Fr modulus binding in PI decode — separate from CLI hex parse (U256 string) |
| QC-OFF-09 | Live daemon rejects zero dappId unless `--dry-run` |

## Verdict: **META/QC**

Fail-closed parse matrix + proptest (random ASCII never panics; hex acceptance bounded by 64-char width).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_57 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test dapp_id_validation -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_05 -- --nocapture
