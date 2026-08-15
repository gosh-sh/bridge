# TD-42 — VK/SRS pin + downgrade detection (DEP-VK-SRS-PIN)

PoC: `scripts/check_vk_srs_pin.sh`, `deposit-prover/tests/td_42_vk_srs_pin.rs`.  
Cross-ref: `scripts/preserve_audit_vk_blob.sh`, `scripts/embed_deposit_vk_blob.py`, TD-04 deploy readiness.

## Pinned VkBlob

| Field | Value |
|-------|-------|
| Path | `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin` |
| Size | **5006** bytes |
| SHA-256 | `9dacd998af5fd03af8097cb80a571df098c925bba235af61d920cc808360fae3` |
| Pin file | `deposit_vk_blob.bin.sha256` |
| USDCBridge | `audit/spec/an-contracts/exchange/USDCBridge.sol` `VK_BLOB` constant |
| Relayer opcode | `(vk_blob, public_inputs, proof)` → `ZKHALO2VERIFYWITHVK` |

Fixture ↔ embedded `VK_BLOB` must be byte-identical (CI gate).

## Downgrade scenarios

| Probe | Expected | Script result |
|-------|----------|---------------|
| Truncated (256 B) | hash ≠ pin | reject |
| Empty file | hash ≠ pin | reject |
| Upstream prefix `304c1c4e…` | hash ≠ pin | reject |
| Audit overlay `724687a4…` | hash ≠ pin | reject |
| Fixture vs USDCBridge drift | embed `--check` fail | exit 1 |

Legacy pins (not current): upstream tvm-sdk `304c1c4e…` (3982 B); audit overlay `724687a4…` (3982 B, `max_key_byte_len=3` era). Current fixture is **12 PI** `9dacd998…` row (`docs/deposit_max_key_byte_len.md`).

## CI hooks (TD-42 gate wired)

| Hook | When | Gates |
|------|------|-------|
| `scripts/check_vk_srs_pin.sh` | standalone / aggregator | fixture pin, USDCBridge embed, downgrade probes |
| `scripts/check_deposit_audit_gates.sh` | deposit-relayer CI | PI docs (TD-68) + VkBlob pin + overlay matrix (TD-04) |
| `scripts/ci_an_audit.sh` | after `audit/spec/an-contracts/build.sh` | `check_vk_srs_pin.sh` + `check_an_overlay_patch_matrix.sh` (runs with `AN_AUDIT_INTEGRATION=0`) |
| `make audit-deposit-relayer-test` | pre `cargo test` | full `check_deposit_audit_gates.sh` |
| `.gitlab-ci.yml` `test:an:audit` | AN audit job | via `ci_an_audit.sh` (unit-only path still runs VkBlob gate) |
| `.gitlab-ci.yml` `test:deposit-relayer:audit` | F10 job | `check_deposit_audit_gates.sh` before `cargo test` |

VkBlob rotation: regenerate fixture → `embed_deposit_vk_blob.py` → overlay `build.sh` → **AN `.tvc` redeploy** (ops; see TD-04 deploy checklist).

`ci_an_audit.sh` skips `sync_an_contracts.sh` when patch-shaped `USDCBridge.sol` is present (`_expectedBridgeFr` marker) — `dex_bridge` rsync would delete overlay `USDCBridge.sol` (eccUSDCBridge-only upstream).

## Verdict: **partial META / QC — CI gate closed**

CI hash gate closes downgrade drift (fixture ↔ overlay ↔ downgrade probes). Remaining gap: live AN `.tvc` redeploy after VkBlob rotation (ops, TD-04).

## Commands

    bash scripts/check_vk_srs_pin.sh
    bash scripts/check_deposit_audit_gates.sh
    bash scripts/ci_an_audit.sh
    cd deposit-prover && cargo test td_42 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_04 -- --nocapture
