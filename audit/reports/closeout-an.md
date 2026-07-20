# AN contracts audit — closeout (branch `audit`)

**Date:** 2026-07-17  
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

**This pass:** BC = **1 candidate** open (BC-AN-02). BC-AN-01 **closed** (author ack 2026-07-20, `f.dappId=0` on `contracts/dex_bridge`). QC = **10** AN-only + **5** joint (QC-AN-J*) + **13** off-chain (QC-OFF*) + **4** prover (QC-PROV*). Withdraw/admin surface: **0 BC**.

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
| Author BC/QC disposition | ⏳ open |
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
| **BC-AN-02** | Medium | `test_bc_an_02_no_l1_bridge_allowlist_pre_zk` + F8-F source regression | **open** |

Analysis: `audit/findings/BC-AN-01/analysis.md`, `audit/findings/BC-AN-02/analysis.md`.  
HANDOFF (RU): `HANDOFF-an-cross-chain-ru.txt`.

---

## QC register — summary (detail in sibling docs)

### AN-only (`questions-an.md`)

| ID | PoC | Disposition |
|----|-----|-------------|
| QC-AN-01 | `unit/test_usdcbridge_finalize_negative.py` | open |
| QC-AN-02…06 | admin / pause / VK / counters | open (partial unit + F2) |
| QC-AN-07 | `integration/test_finalize_deposit_voucher_brick.py` | **closed (test)** — confirm prod deploy |
| QC-AN-08 | `unit/test_usdcbridge_deposit_edge.py` | **closed (test)** |
| QC-AN-09 | code review (accept before ZK) | accepted griefing |
| QC-AN-10 | `unit/test_usdcbridge_deposit_edge.py` | open (QC) |

### Cross-chain (`questions-cross-chain.md`)

| ID | Topic |
|----|-------|
| QC-AN-J1…J5 | caps, pause asymmetry, zero recipient, workchain, custody model |

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
| QC-AN-10 | `require(anAccount != 0)` before accept (ETH parity) |
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
- [x] **Author confirms** BC-AN-01 (2026-07-20)
- [ ] **Author confirms** remaining BC/QC rows
- [ ] E-AN-01 shellnet — **deferred**
- [ ] `closeout-an.md` final signoff after disposition

**Auditor note:** Deliverable is test-backed + documented. BC/QC rows stay open until author ack; we do not delete or downgrade without explicit team confirmation.

**Out of scope (this pass):** full `tvm-sdk` GraphQL audit, Halo2 circuit soundness, shellnet E2E, production ops SLA.
