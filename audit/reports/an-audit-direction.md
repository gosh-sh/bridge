# AN audit — direction (subagent synthesis, 2026-07-17)

Entry point for subagents and humans. Update as BC/QC close.

**Sources:** deposit-path, withdraw/admin, MessagePipeline, ETH-cross agents on `audit` @ `77449b1`.

---

## Priority queue

| P | ID | Area | Action |
|---|-----|------|--------|
| **P0** | BC-AN-01 | `dappId` in replay key, not event-bound | PoC: two proofs same receipt, different `dappId` → double mint. Red/xfail test when prover artifacts ready. Fix: on-chain `EXPECTED_DAPP_ID` or drop dappId from hash. |
| **P1** | BC-AN-02 | No L1 `contractAddr` allowlist | QC/BC: document + optional `require(f.contractAddr == ETH_BRIDGE)`. |
| **P1** | DEP-AN-11/12 | MessagePipeline | ✅ `integration/test_finalize_deposit_pipeline.py` |
| **P2** | WD-AN-01/02, ADM-AN-01 | withdraw + admin negatives | ✅ `unit/test_usdcbridge_withdraw_admin_negative.py` |
| **P2** | Joint ETH↔AN | cap, pause, anWorkchain | QC: L1 `MAX_DEPOSIT_AMOUNT` / pause not mirrored on AN; workchain dropped in PI |
| **P3** | QC-AN-07 | Voucher brick | Research TVM replay when voucher deploy fails mid-flight |
| **P3** | QC-AN-08 | PI length | `test_finalize_deposit_public_inputs_too_short` |
| **P3** | Voucher hash | DepositVoucher ctor | `test_deposit_voucher_hash_mismatch` (exit 219) |
| **defer** | E-AN-01 | shellnet | `acki-nacki/tests/exchange/test_usdcbridge_finalize.py` |

---

## BC / QC register (AN)

| ID | Class | Status | PoC |
|----|-------|--------|-----|
| BC-AN-01 | dappId double-mint | **open** | needs dual-proof prover run |
| BC-AN-02 | L1 bridge allowlist | **open** | doc + optional guard test |
| QC-AN-01 | amount ≤ uint64 | open | ✅ unit |
| QC-AN-02…06 | admin/pause/VK | open | partial |
| QC-AN-07 | voucher brick | open | — |
| QC-AN-08 | PI length | open | — |
| QC-AN-09 | accept before ZK | accepted | griefing only |
| QC-AN-10 | anAccount == 0 | open | — |

**BC count:** 2 candidates (author confirm). Non-deposit surfaces: **BC=0** per withdraw agent.

---

## Test gate

```bash
./scripts/sync_an_contracts.sh
make audit-an-test    # target: 19/19
make pre-push-an      # unit only (16)
```

---

## Subagent briefing

Always read `audit/spec/an/AGENT_CONTEXT.md` + this file before new tests.
