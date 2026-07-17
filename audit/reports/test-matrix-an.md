# AN test matrix (Phase F)

| ID | Layer | Target | Spec test | Status |
|----|-------|--------|-----------|--------|
| F-TC-01 | unit | toolchain symlinks | `unit/test_toolchain_smoke.py` | ✅ |
| F-BR-01 | unit | `getVersion` after ctor | `unit/test_usdcbridge_getters.py` | ✅ |
| F-BR-02 | unit | voucher code hash set | `unit/test_usdcbridge_getters.py` | ✅ |
| F-BR-03 | unit | `getTotalBridged` zero | `unit/test_usdcbridge_getters.py` | ✅ |
| DEP-AN-01 | unit | zero amount → 204 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-02 | unit | bad ZK proof → 220 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-03 | unit | `confirmDeposit` wrong sender → 207 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| QC-AN-01 | unit | amount Fr overflow → 214 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-10 | integration | real proof_00/01 finalize | `integration/test_finalize_deposit_fixture.py` | ✅ |
| DEP-AN-11 | integration | pipeline voucher→confirm | `integration/test_finalize_deposit_pipeline.py` | ✅ |
| DEP-AN-12 | integration | replay proof_00 | `integration/test_finalize_deposit_pipeline.py` | ✅ |
| WD-AN-01 | unit | `initiateWithdrawal` no ECC | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-02 | unit | unsupported tokenId | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-03 | unit | zero ECC amount | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-04 | unit | recipient too long | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-05 | unit | multiple ECC | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-06 | unit | withdraw happy burn+event | `unit/test_usdcbridge_withdraw_happy.py` | ✅ |
| QC-AN-08 | unit | PI too short | `unit/test_usdcbridge_deposit_edge.py` | ✅ |
| QC-AN-10 | unit | anAccount==0 pre-ZK | `unit/test_usdcbridge_deposit_edge.py` | ✅ (QC) |
| DEP-AN-04 | unit | voucher hash mismatch 219 | `unit/test_usdcbridge_deposit_edge.py` | ✅ |
| ADM-AN-01 | unit | `mintAndSend` wrong nonce | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| TIP-AN-01 | unit | onTransferReceived wrong sender | `unit/test_usdcbridge_admin.py` | ✅ |
| TIP-AN-02 | unit | onTransferReceived happy | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-02 | unit | mintAndSend not owner | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-03 | unit | mintAndSend happy | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-04 | unit | mintAndSend zero | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-05 | unit | accumulator not whole USDC | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-06 | unit | mintAndSend overflow | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-07 | unit | setPubkey rotation | `unit/test_usdcbridge_admin.py` | ✅ |
| BC-AN-01 | integration | dual dappId double mint | `integration/test_bc_an_01_*.py` | ✅ dual-proof PoC (Hermez SRS) |
| BC-AN-02 | integration | no L1 bridge allowlist | `integration/test_bc_an_01_*.py` | ✅ pre-ZK |
| QC-AN-07 | integration | voucher brick w/o code | `integration/test_finalize_deposit_voucher_brick.py` | ✅ |
| E-AN-01 | e2e | shellnet finalize | acki-nacki `test_usdcbridge_finalize.py` | deferred |

**Gate:** `make audit-an-test` → **37 passed** (2026-07-17).

Direction: `audit/reports/an-audit-direction.md`.
