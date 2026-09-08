# Delta — ETH deposit audit pass

**База:** `baseline-deposit-eth.md` (open only)  
**Locked:** `baseline-deposit-eth-locked.md`  
**Milestone digest:** `phase-2-deposit-eth-milestone.md`

---

## contracts/bridge migration (2026-08-14)

Post-Phase 2 alignment with upstream **`../acki-nacki` @ `contracts/bridge`** (`d9f7dc9b1`). Documentation + test suite; **no** shellnet live.

### What changed

| Before | After |
|--------|-------|
| Sync default `contracts/dex_bridge` | **`contracts/bridge`** |
| Audit overlay `USDCBridge.sol` | **`eccUSDCBridge` v1.3.x** (upstream source) |
| Allowlist `setExpectedBridge` / `_expectedBridgeFr` | **`setTrustedL1Bridge(chainId, l1Bridge, true)`** SET per chain |
| DepositVoucher 5-arg / `srcChainId` overlay naming | **6-arg** constructor; identity hash with **`chainId`** |
| Public inputs | **12 × 32 B (384 B)**; `chainId` at fr[4] |
| On-chain dappId | **Pinned to 0** (PI dapp limbs ignored) |

### Test impact

| Item | Result |
|------|--------|
| Stale `build/USDCBridge.tvc` | **Removed** — suite uses `eccUSDCBridge.tvc` only |
| AN pytest full (`AN_AUDIT_INTEGRATION=1`) | **74 passed**, 0 skipped, 0 failed |
| BC-AN-01 dual-proof fixtures | Regenerated **384 B** PI; integration **4 passed** |
| Handoff | v2 `td-04-an-deploy-owner-handoff.md`; gates + `check_td04_deploy_readiness.sh` green |

### Ops obsolete (handoff §7)

Legacy overlay-only steps **not** on `eccUSDCBridge@contracts/bridge`: `setExpectedBridge`, `setExpectedAnDappId`, block anchors, `_mintCapByChain`, M-of-N attesters.

### Open (unchanged — ops)

| ID | Blocker |
|----|---------|
| TD-04 | Live deploy `eccUSDCBridge.tvc` + **`setTrustedL1Bridge`** seed per L1 chain |
| TD-53 | Shellnet E-AN-01 (live daemon E2E) |
| TD-65 | Live G3 on shellnet |
| — | Formal author sign-off on open baseline rows |

**Verdict:** META — documentation + mock/CI green on correct upstream; ops blockers unchanged.

---

## Phase 2 pass (2026-08-14)

Autopilot cycle: mock/CI milestone closed; **no new BC**; documentation + CI gate wiring. Full rollup → `phase-2-deposit-eth-milestone.md`.

### Rollup

| Item | Detail |
|------|--------|
| TD-36–68 | **33** P2 directions — partial PoC + notes on all rows (catalog §3) |
| META / RELAYER CI gates | TD-68 → TD-42 → TD-43 → TD-49 → TD-53 → TD-65 → TD-04 overlay (`check_deposit_audit_gates.sh`) |
| Ops artifacts | `docs/operations/td-04-an-deploy-owner-handoff.md` (v2 **eccUSDCBridge**), `scripts/check_td04_deploy_readiness.sh` |
| Suite refs (2026-08-14) | relayer **~263**, prover **~170+**, overlay Deposit **~69**, AN unit **42** |

### CI gate chain (wired this pass)

| Script | TD |
|--------|-----|
| `check_pi_count_docs.sh` | TD-68 |
| `check_vk_srs_pin.sh` | TD-42 |
| `check_mock_vs_shplonk_smoke.sh` | TD-43 |
| `check_mutation_kill_smoke.sh` | TD-49 |
| `td_53_dry_run_recovery_smoke.sh` | TD-53 |
| `check_td65_g3_smoke.sh` | TD-65 |
| `check_an_overlay_patch_matrix.sh` | TD-04 |

### New findings (this pass)

**None new BC.** QC rows already tracked in `baseline-deposit-eth.md`; Phase 2 added evidence cross-ref + recommended author defaults (not formal sign-off).

### Closed this pass (partial-ack in baseline)

Baseline rows annotated with Phase 2 evidence + **Author default (recommended)**:

| ID | Notes |
|----|-------|
| QC-A1-2 | TD-23/24/56 USDC trust |
| QC-AN-J1 | TD-36 cap policy |
| QC-AN-J2 | TD-58 pause asymmetry |
| QC-OFF-05 | TD-65 `is_finalized` stub documented |
| QC-OFF-06 | TD-65 mock CI (exit 51 vs HOL) |
| QC-PROV-05 | TD-43 opcode triple CI smoke |
| QC-PROV-06 | TD-49 mutation **13/13** |

Open baseline rows **не удалены** — formal author ack still pending.

### Open carry-over (ops, не BC)

| ID | Blocker |
|----|---------|
| TD-04 | Live AN `.tvc` deploy + owner seed (`setTrustedL1Bridge` per chain — see migration delta) |
| TD-53 | Shellnet E-AN-01 (live daemon E2E) |
| TD-65 | Live G3 / `is_finalized` API on shellnet |
| — | Formal author sign-off (`baseline-deposit-eth.md` recommended defaults → partner ack) |

### Next step

1. **Owner:** `bash scripts/check_td04_deploy_readiness.sh` → deploy per handoff v2 (`eccUSDCBridge`, `setTrustedL1Bridge` per chain).
2. **Autopilot maintenance:** corpus re-pin on header upgrade (TD-64); keep `check_deposit_audit_gates.sh` green on MR.
3. **Worker pause OK** after this delta if no owner-independent maintenance remains.

---

## Phase G pass (2026-08-13)

**Сравнение с:** июльский аудит + Phase G1 closeout

### Новые тесты (этот проход)

| File | Tests added | Covers |
|------|-------------|--------|
| `DepositEdgeCases.t.sol` | 11 | max boundary, reentrancy, donation, fee-on-transfer, returnless token, allowance/balance, no pause, event timestamp, sender binding |
| `DepositWorkchain.t.sol` | 2 + fuzz | `anWorkchain` emission, multi-account deposits |
| `FeeOnTransferERC20.sol` | mock | QC-A1-2 concrete solvency gap |
| `ReturnlessERC20.sol` | mock | fail-closed non-standard ERC20 |
| `ReentrantERC20.sol` | mock | DEP-4 |
| `f10a_binding.rs` | updated | 12 PI (fixes stale 11-PI assert) |

### Новые / уточнённые замечания

| ID | Class | PoC | Auditor view |
|----|-------|-----|--------------|
| **QC-A1-5** | QC | `DepositEdgeCases.t.sol::test_directUsdcTransfer_doesNotCreditTreasury` | Прямой USDC transfer на bridge — донат; `treasuryBalance` не растёт. Ожидаемо; задокументировать для ops (не баг). |
| **QC-A1-6** | QC | `DepositEdgeCases.t.sol::test_feeOnTransferToken_reverts` | Fee-on-transfer fails closed (`TransferAmountMismatch`); ETH-11. |
| **QC-A1-7** | OK | `DepositEdgeCases.t.sol::test_returnlessToken_revertsOnDeposit` | Returnless `transferFrom` → revert при decode bool. Fail-closed для USDT-style; не баг. |
| **QC-A1-8** | OK | `DepositWorkchain.t.sol` | L1 не валидирует `anWorkchain`; любой `int8` в event. Согласовано с QC-AN-J4 (AN игнорирует). |
| **QC-ETH-DEP-01** | OK | `DepositEdgeCases.t.sol` | `sender` = `msg.sender`; third party cannot pull approved USDC from another address. |

### Закрыто тестами в Phase G (не в open baseline)

| ID | Как закрыто |
|----|-------------|
| QC-PROV-01 (partial) | `f10a_binding.rs` — 12 PI assert; circuit tests в `circuit_v2.rs` |
| QC-OFF-08 (partial) | `state.rs`; `baseline-deposit-eth-locked.md` |

### Phase G — не изменилось

Строки open baseline без новых закрытий в Phase G (см. Phase 2 pass выше для partial-ack).

---

## Wave 2 kickoff (2026-08-15)

| Item | Detail |
|------|--------|
| Commit | `7bd7366` — Phase 2 deposit-ETH handoff pushed to `origin/audit-new` |
| `github/main` | @ `a7a1130` — **already merged into** `audit-new` (no delta merge this sprint) |
| `origin/main` | @ `c9d5412` — lags github; track **`github/main`** for dev commits |
| `acki-nacki` sync | `contracts/bridge` @ `d9f7dc9b1` — no drift |
| T2-1 | `td_43_all_fixture_triples_pass_opcode_triple` — all 10 Blake2b fixtures pass opcode verify |
| Docs | `wave-2-security-plan.md`, `phase-g-status.md` refresh |

Next: W2-1 withdraw security baseline; tier 3 tvm-debugger smoke (T3-2).

---

## Следующий шаг (aggregate)

Owner ops TD-04 deploy (вне worker). Autopilot: `git fetch github` each sprint; gates green. Wave 2 security focus: withdraw egress (W2-1).
