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
| DEP-AN-10 | integration | real proof_00 finalize | `integration/test_finalize_deposit_fixture.py` | ✅ |
| DEP-AN-11 | integration | pipeline voucher→confirm | — | ⏳ F4 |
| DEP-AN-12 | integration | replay proof_00 | — | ⏳ F4 |
| WD-AN-01 | unit | `initiateWithdrawal` no ECC | — | ⏳ |
| WD-AN-02 | unit | unsupported tokenId | — | ⏳ |
| ADM-AN-01 | unit | `mintAndSend` wrong nonce | — | ⏳ |
| E-AN-01 | e2e | shellnet finalize | acki-nacki `test_usdcbridge_finalize.py` | deferred |

**Gate:** `cd audit/spec/an && python3 -m pytest -q` → **11/11** (with fixtures synced).

Fixtures: `./scripts/sync_an_contracts.sh` → `audit/spec/an/fixtures/deposit_10proofs/`.
