# AN audit — direction (subagent synthesis, 2026-07-21)

Entry point for subagents and humans. Update as BC/QC close.

**Author responses:** Pruvendo QA Stage II — Bridge § (recorded 2026-07-21). See `questions-an.md`, `questions-cross-chain.md`.

---

## Priority queue

| P | ID | Area | Action |
|---|-----|------|--------|
| **P0** | BC-AN-01 | `dappId` in replay key, not event-bound | ✅ **closed** (Stage II: «dapp_id = 0»; regression green) |
| **P1** | BC-AN-02 | No L1 `contractAddr` allowlist | ⏳ **open** — Stage II: no answer |
| **P1** | DEP-AN-11/12 | MessagePipeline | ✅ `integration/test_finalize_deposit_pipeline.py` |
| **P2** | WD-AN-01/02, ADM-AN-01 | withdraw + admin negatives | ✅ `unit/test_usdcbridge_withdraw_admin_negative.py` |
| **P2** | Joint ETH↔AN | cap, pause, anWorkchain | ✅ `questions-cross-chain.md` |
| **P2** | F2 manual + admin tests | withdraw/admin/TIP-3 | ✅ F2 + `test_usdcbridge_admin.py` |
| **P3** | QC-AN-07 | Voucher brick | Research TVM replay when voucher deploy fails mid-flight |
| **P3** | QC-AN-08 | PI length | ✅ `unit/test_usdcbridge_deposit_edge.py` |
| **P3** | QC-AN-10 | anAccount == 0 | ✅ pre-ZK QC (no ETH-style revert) |
| **P3** | DEP-AN-04 | Voucher hash | ✅ exit 219 |
| **defer** | E-AN-01 | shellnet | `acki-nacki/tests/exchange/test_usdcbridge_finalize.py` |
| **next** | F8-F / closeout | BC/QC regressions + `closeout-an.md` | ✅ draft closeout + `test_bc_f8f_regressions.py` |
| **core** | F8 fuzz | MessagePipeline order (AN) | ✅ `test_pipeline_order_fuzz.py` |
| **core** | F9 fuzz | ETH invariant gaps + CI night | ✅ 53/53 audit + ci profile |
| **done** | F10 | Off-chain pipeline + relayer threat model | relayer F10 ✅ (59 tests + proptest); prover F10-A ✅ (pin, binding, padding PoC) |

---

## BC / QC register (AN)

| ID | Class | Status | PoC |
|----|-------|--------|-----|
| BC-AN-01 | dappId double-mint | **closed** | dual-proof regression tests |
| BC-AN-02 | L1 bridge allowlist | **open** (Stage II: no answer) | ✅ pre-ZK integration |
| QC-AN-01 | amount ≤ uint64 / ETH cap | **partial ack** | ✅ unit |
| QC-AN-02 | owner mint centralization | **open** | partial unit |
| QC-AN-03 | voucher code rotation | **closed (ack)** | F2 manual |
| QC-AN-04 | no AN pause | **open** | code review |
| QC-AN-05 | dual supply counters | **partial ack** | fuzz |
| QC-AN-06 | VK blob | **partial ack** | fixtures |
| QC-AN-07 | voucher brick | **closed (ack)** | ✅ integration |
| QC-AN-08 | PI length | **closed (test)** | ✅ unit |
| QC-AN-09 | accept before ZK | **open** | griefing only |
| QC-AN-10 | anAccount == 0 | **partial ack** | AN require + drop ETH guard |
| QC-AN-J4 | workchain unused | **closed (ack)** | — |

**BC count:** 1 open (BC-AN-02). Non-deposit surfaces: **BC=0** per withdraw agent.

---

## Test gate

```bash
./scripts/sync_an_contracts.sh
make audit-an-test    # 74 passed (F7 + F8 fuzz + F8-F BC regressions)
make pre-push-an      # unit only (28 deterministic; no Hypothesis integration)
make audit-deposit-relayer-test   # F10 relayer: 59 passed + proptest (2026-07-17)
```

---

## Subagent briefing

Always read `audit/spec/an/AGENT_CONTEXT.md` + this file before new tests.
