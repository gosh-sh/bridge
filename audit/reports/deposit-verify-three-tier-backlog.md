# Deposit verify — 3-tier test backlog (levels 1–3)

**Date:** 2026-08-15  
**Context:** QC-PROV-05 closed — MockProver ≠ production acceptance; extend coverage without live shellnet.  
**Level 4 (live shellnet / E-AN-01)** — ops defer; not in this backlog.

## Target model (four levels, implement 1–3 in-repo)

| Tier | Layer | What it proves | Tooling | Status |
|------|-------|----------------|---------|--------|
| **1** | Mock | Witness satisfies circuit constraints | MockProver, `test_circuit_mock` | **CI** — fast |
| **2** | Standalone copy | Blake2b SHPLONK + RLC VkBlob = AN opcode handler | `verify_deposit_opcode_triple`, TD-43 | **CI** — `check_mock_vs_shplonk_smoke.sh` (~6 min) |
| **3** | Real SDK + debugger | tvm-sdk opcode / contract path on real proof bytes | `tvm-debugger`, sibling SDK (`../tvm-sdk` or `../pruvendo-tvm-sdk`), `.tools/` | **Partial** — AN pytest 74 passed; opcode tier explicit backlog below |
| **4** | Live shellnet | Full Sepolia → prove → finalize on cluster | ops runbooks | **Defer** — devs green Jul 2026 |

Trust boundary: tier 2 mirrors handler in Rust; tier 3 closes «SDK eats same bytes» without cluster.

**Build:** `audit/spec/an/BUILD.md`, `audit/spec/an/AGENT_CONTEXT.md` (`.tools/tvm-debugger` → sibling SDK).

---

## Backlog — tier 1 (Mock)

| ID | Task | Acceptance | Gate |
|----|------|------------|------|
| T1-1 | Keep MockProver on PI/layout regressions | `f10a_binding`, `circuit_v2` layout pins | existing `cargo test -p deposit-prover` |
| T1-2 | Padding QC documented (not tier-1 gate) | TD-20 / PROV-04 survivors byte-identical PI | `td_20`, `padding_mutation_poc` |

No expansion required for sign-off; tier 1 is regression anchor only.

---

## Backlog — tier 2 (standalone SHPLONK)

| ID | Task | Acceptance | Gate |
|----|------|------------|------|
| T2-1 | **All** `deposit_10proofs/*` pass opcode triple | each dir: `vk_blob` + 384 B `public_inputs.bin` + `proof.bin` → `verify_deposit_opcode_triple` OK | extend `td_43_mock_vs_shplonk.rs` or new `td_43_all_fixtures_opcode.rs` |
| T2-2 | Corruption matrix on `proof_00` | 1-byte flip proof/pi/vk → fail; real triple → pass | extend TD-43 (partial today) |
| T2-3 | Proptest: random garbage triple → always fail | proptest on `verify_deposit_opcode_triple` | `deposit-prover/tests/` new file |
| T2-4 | Full prove → export Blake2b → opcode verify (slow) | one synthetic + one fixture path | optional nightly; needs `data/kzg_params_18.srs` |
| T2-5 | Wire T2-1 into aggregator | `check_mock_vs_shplonk_smoke.sh` or sibling `check_opcode_all_fixtures.sh` | `check_deposit_audit_gates.sh` |

**Commands (today):**

    bash scripts/check_mock_vs_shplonk_smoke.sh
    cd deposit-prover && cargo run --release --example verify_opcode_triple -- ...

---

## Backlog — tier 3 (real SDK + debugger)

| ID | Task | Acceptance | Gate |
|----|------|------------|------|
| T3-1 | Document sibling SDK path in BUILD | `PRUVENDO_TVM_SDK` or `../tvm-sdk` symlink to `.tools/tvm-debugger` | `audit/spec/an/BUILD.md` |
| T3-2 | Opcode smoke: real bytes through debugger | `ZKHALO2VERIFYWITHVK` or contract `finalizeDeposit` with pinned VkBlob + `proof_00` bundle | new `audit/spec/an/integration/test_opcode_triple_debugger.py` or extend F8-F |
| T3-3 | All 10 fixtures → `finalizeDeposit` on `eccUSDCBridge` emulator | extend beyond BC-AN-01 dual proofs; seed `setTrustedL1Bridge` | `AN_AUDIT_INTEGRATION=1 scripts/ci_an_audit.sh` |
| T3-4 | Negative: corrupt proof bytes in debugger path | `ERR_INVALID_ZKPROOF` / reject before mint | pytest negative |
| T3-5 | Optional: Hypothesis byte-flip on proof prefix | same as T2-3 but through MessagePipeline | slow integration mark |
| T3-6 | CI hook for tier 3 smoke | subset ≤ 2 min on MR; full 10 fixtures on `audit-deposit` job | `.gitlab-ci.yml` / `ci_an_audit.sh` flag |

**Prereq:** `cd ../tvm-sdk && cargo build --release` (or pruvendo sibling); `audit/spec/an-contracts && bash build.sh`.

**Commands (today):**

    AN_AUDIT_INTEGRATION=1 bash scripts/ci_an_audit.sh
    # 74 passed — tier 3 contract path; T3-2 opcode-only smoke = backlog

---

## Mapping to open QC / defer items

| Item | Tier that closes gap |
|------|----------------------|
| QC-PROV-05 | Tier 2 = acceptance; tier 3 = SDK parity |
| QC-OFF-13 receipt parse | Tier 3 debugger + tier 4 live matrix |
| E-AN-01 / TD-53 shellnet | Tier 4 only |

---

## Suggested implementation order

1. T2-1 + T2-5 — low effort, high signal (all fixtures opcode green).
2. T3-3 — widen AN integration fixtures (already have harness).
3. T3-2 — explicit opcode/debugger smoke (closes Rust↔SDK gap).
4. T2-3 / T3-5 — fuzz (property layer).
5. T2-4 — nightly full prove path (cost).

---

## Regression bundle (after backlog lands)

    make pre-push-audit
    bash scripts/check_deposit_audit_gates.sh   # TD-68 → TD-42 → TD-43 → TD-49 → TD-53 → TD-65 → TD-04
    AN_AUDIT_INTEGRATION=1 bash scripts/ci_an_audit.sh

Cross-ref: `audit/reports/td-43-mock-vs-shplonk-notes.md`, `phase-2-deposit-eth-milestone.md`, `PROJECT_FACTS.md` § verification layers.
