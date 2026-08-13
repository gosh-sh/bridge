# AN contracts audit — closeout (branch `audit`)

**Date:** 2026-07-21 (partial author ack — Pruvendo QA Stage II Bridge §)  
**Scope:** `USDCBridge` / `DepositVoucher` on Acki Nacki — deposit finalize, withdraw initiation, admin/TIP-3. ZK opcode internals (`tvm-sdk`) out of scope; verified via fixture bytes only.

**Registers:** `questions-an.md` (AN-only), `questions-cross-chain.md` (joint + off-chain F10).  
**Direction:** `an-audit-direction.md`. **Matrix:** `test-matrix-an.md`.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; **auditor view stated**; **author must confirm** bug vs feature |
| **BC** | **Required** | Bug **candidate** — fund loss or broken binding under stated assumptions |

**This pass:** BC = **1** open (BC-AN-02). BC-AN-01 **closed**. QC AN-only: **4 closed**, **4 partial ack**, **3 open**. QC joint (J*): **1 closed**, **4 open**. F10 QC-OFF/PROV unchanged. Withdraw/admin surface: **0 BC**.

---

## Phase completion

| Phase | Status |
|-------|--------|
| F0–F3 manual (F1/F2) | ✅ |
| F4 unit negatives + admin | ✅ |
| F5 integration (fixtures + pipeline) | ✅ |
| F7 property tests | ✅ |
| F8 fuzz / state machines | ✅ |
| F9 ETH fuzz (cross-repo gate) | ✅ 53/53 audit + ci |
| F10 off-chain relayer + prover overlay | ✅ Rust gate (relayer 59 tests); prover pin + padding PoC |
| BC PoC (BC-AN-01 dual proof) | ✅ |
| Author BC/QC disposition | ⏳ partial (Stage II Bridge §, 2026-07-21) |
| E-AN-01 shellnet E2E | **deferred** |

---

## Test gates

```bash
./scripts/sync_an_contracts.sh
make audit-an-test              # 74+ pytest (unit + integration + F8-F BC regressions)
make pre-push-an                # unit only (~28 deterministic)
make audit-deposit-relayer-test # F10 relayer Rust
make audit-solidity-test        # ETH audit overlay 53/53
```

CI: `test:an:audit` (skip w/o `.tools/`), `test:solidity:audit`, `test:solidity:audit:ci` (night, allow_failure), `test:deposit-relayer:audit`.

---

## BC register — bug candidates (author confirm)

| ID | Severity | PoC | Status |
|----|----------|-----|--------|
| **BC-AN-01** | High | `integration/test_bc_an_01_dapp_id_double_mint.py` + F8-F regressions | **closed** (author ack 2026-07-20; `f.dappId=0` on `contracts/dex_bridge`) |
| **BC-AN-02** | Medium | `test_bc_an_02_no_l1_bridge_allowlist_pre_zk` + F8-F source regression | **open** (Stage II: no answer) |

Analysis: `audit/findings/BC-AN-01/analysis.md`, `audit/findings/BC-AN-02/analysis.md`.  
HANDOFF (RU): `HANDOFF-an-cross-chain-ru.txt`.

---

## QC register — summary (detail in sibling docs)

### AN-only (`questions-an.md`)

| ID | PoC | Disposition |
|----|-----|-------------|
| QC-AN-01 | `unit/test_usdcbridge_finalize_negative.py` | **partial ack** — raise ETH cap toward u64; min deposit TBD |
| QC-AN-02 | `unit/test_usdcbridge_admin.py` | **open** |
| QC-AN-03 | F2 manual + code review | **closed (ack)** — voucher code immutable |
| QC-AN-04 | code review | **open** |
| QC-AN-05 | `integration/test_cross_counter_fuzz.py` | **partial ack** — separate counters; exceed case under review |
| QC-AN-06 | integration fixtures | **partial ack** — fixture smoke for VK |
| QC-AN-07 | `integration/test_finalize_deposit_voucher_brick.py` | **closed (ack)** — voucher at deploy |
| QC-AN-08 | `unit/test_usdcbridge_deposit_edge.py` | **closed (test)** |
| QC-AN-09 | code review (accept before ZK) | **open** |
| QC-AN-10 | `unit/test_usdcbridge_deposit_edge.py` | **partial ack** — AN require planned; ETH guard to drop |

### Cross-chain (`questions-cross-chain.md`)

| ID | Topic |
|----|-------|
| QC-AN-J1 | caps — **open** |
| QC-AN-J2 | pause asymmetry — **open** |
| QC-AN-J3 | zero recipient — **open** (linked QC-AN-10 partial) |
| QC-AN-J4 | workchain — **closed (ack)** |
| QC-AN-J5 | custody model — **open** |

### Off-chain F10 (`questions-cross-chain.md` § QC-OFF)

| ID | Priority note |
|----|----------------|
| QC-OFF-06 | **Critical liveness** — live submitter never `AlreadyFinalized` + sequential cursor |
| QC-OFF-01 | Head-of-line blocking |
| QC-OFF-07…13 | state, ABI drift, backoff, interface status |
| QC-PROV-01 | 7 vs 11 instances in `verify_proof` |
| QC-PROV-02 | axiom-eth pin `@1d61be0` — **closed (pin)** |
| QC-PROV-03 | `max_key_byte_len = 3` — **closed (dev ack 2026-07-17)**; audit VkBlob `724687a4…` |
| QC-PROV-04 | MPT `key_bytes` padding witness malleability (`padding_mutation_poc.rs`) |

---

## F8-F — BC regression gate

Keeps BC-AN-01 regression + BC-AN-02 gate visible. BC-AN-01 tests are **regression** (post-fix); BC-AN-02 still documents open behaviour.

| Test file | What |
|-----------|------|
| `integration/test_bc_f8f_regressions.py` | Source-level guards absent + re-run BC-AN-02 pre-ZK |
| `integration/test_bc_an_01_dapp_id_double_mint.py` | Full BC-AN-01 dual-proof PoC (Hermez fixtures) |
| `unit/test_replay_key_properties.py` | Replay anchor splits on `dappId` (F7-B / AN-VCH-1) |

---

## Recommended mitigations (auditor — confirm before implementing)

| ID | Suggested action |
|----|------------------|
| BC-AN-02 | `immutable EXPECTED_L1_BRIDGE` |
| QC-AN-10 | `require(anAccount != 0)` on AN; dev to remove ETH guard (Stage II partial) |
| QC-OFF-06 | Map AN revert → `AlreadyFinalized`; nullifier read API |

---

## Signoff checklist

- [x] Phases F0–F8; pytest gate + property/fuzz overlay
- [x] BC-AN-01 dual-proof PoC + BC-AN-02 pre-ZK
- [x] F8-F BC regression file
- [x] F9 ETH ci night profile green
- [x] F10 relayer + prover audit overlay (QC-PROV-02 pin; QC-PROV-03/04 documented)
- [x] QC/BC registers with PoC links (`questions-an.md`, HANDOFF)
- [x] CI jobs (`test:an:audit`, `test:solidity:audit`, `test:deposit-relayer:audit`)
- [x] **Author confirms** BC-AN-01 (2026-07-20) + Stage II
- [x] **Partial author ack** QC-AN-03, QC-AN-07, QC-AN-J4 (Stage II 2026-07-21)
- [ ] **Author confirms** remaining BC/QC rows
- [ ] E-AN-01 shellnet — **deferred**
- [ ] `closeout-an.md` final signoff after disposition

**Auditor note:** Deliverable is test-backed + documented. BC/QC rows stay open until author ack; we do not delete or downgrade without explicit team confirmation.

**Out of scope (this pass):** full `tvm-sdk` GraphQL audit, Halo2 circuit soundness, shellnet E2E, production ops SLA.
