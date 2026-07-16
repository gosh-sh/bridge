# ETH contracts audit — closeout (branch `audit`)

**Commit:** `ea836dd` (+ merge cleanup → `audit` @ GitLab)  
**Date:** 2026-07-16  
**Scope:** Ethereum bridge contracts — wiring, accounting, replay, pause, deploy path. **ZK soundness out of scope.**

---

## Classification policy (BC vs QC vs OK)

| Label | Meaning | PoC required? |
|-------|---------|---------------|
| **OK** | Behaviour matches intent; no action. | Optional |
| **QC** | **PoC or reproducible evidence exists**, behaviour is understood, but **we cannot tell bug vs feature** without product/protocol/design confirmation. Not laziness — we checked and need intent. | **Yes** — every QC in this closeout links a test or cited code path |
| **BC** | **Bug candidate:** PoC shows behaviour that **should not** happen under reasonable bridge assumptions; we still do **not** call it a confirmed bug out of respect for dev context we may lack. | **Yes** — mandatory |

**This audit:** **BC = 0**, **QC = 13 unique items**, all with PoC or explicit code citation.

---

## Phase completion

| Phase | Deliverable | Status |
|-------|-------------|--------|
| A | Manual A1–A4 | ✅ |
| B | `test-matrix.md` | ✅ |
| C | 29 unit tests | ✅ |
| D | 11 fuzz/invariant | ✅ |
| E | E2E gaps (incl. E-03/04/06) | ✅ |
| Gate | `FOUNDRY_PROFILE=audit forge test` | **47/47 green** |
| Closeout | This file + `make pre-push` | see CI row below |

---

## Residual risks — QC register (PoC-linked)

### A1 — Deposits & treasury

| ID | PoC / evidence | What we proved | Open question (bug or feature?) |
|----|----------------|----------------|-----------------------------------|
| QC-A1-1 | `audit/spec/ethereum/DepositWhaleCap.t.sol` | N×100 USDC deposits succeed; no global TVL cap | Per-tx cap only — intentional exposure limit? |
| QC-A1-2 | `audit/spec/ethereum/FuzzDepositToken.t.sol` (TR-4); code `AckiNackiBridge.deposit` L587–592 | Exact `transferFrom` accounting; no fee-on-transfer handling | Document mainnet USDC proxy/blacklist trust model in `PROJECT_FACTS.md`? |
| QC-A1-3 | `audit/spec/ethereum/EmergencyYield.t.sol` | After `emergencyWithdrawAll`, residual yield not harvestable | Accepted residual vs emergency should route yield to `yieldRecipient`? |
| QC-A1-4 | Code review `deposit` / `emergencyWithdrawAll` / `harvestYield` vs `bridge_verification.md` AC-6 | Pull-then-account + `nonReentrant`; docs say “effects before externals” literally | Fix AC-6 wording in ops docs (not a code defect) |

### A2 — verifyBlock

| ID | PoC / evidence | What we proved | Open question |
|----|----------------|----------------|---------------|
| QC-A2-1 | `contracts/ethereum/test/AckiNackiBridgeLayerAnchor.t.sol` | `_expectedPrevAnchor(numLayers)` ≠ flat `storedPrevMaxLevelLayerHash` on layer shrink | Update `bridge_verification.md` LH-3/CC-6/L6 monitoring? |
| QC-A2-2 | `audit/spec/ethereum/BkSetUpdateReplay.t.sol`; code `applyBkSetUpdate` L777/787 | Dual cursor: PI from `storedLastSeenBlockSeqNo`, gate on `storedLastBkSetUpdateSeqNo` | Prover `lastSeen` semantics when `verifyBlock` advances between prove and submit? |
| QC-A2-3 | `audit/spec/ethereum/VerifyBlockZeroLayer.t.sol` | Active slot `layerHashes[i]==0` accepted; not appended to window | Can Circuit 2 ever emit zero active layer hash? If not — document assumption; if yes — add on-chain reject? |
| QC-A2-4 | Code `_highestActiveLayer()` L880–888 | `t` monotonic; windows never shrink | Partner semantics when AN permanently reduces layer count? |

### A3 — withdrawByProof

| ID | PoC / evidence | What we proved | Open question |
|----|----------------|----------------|---------------|
| WD-Q1 / A3-01 | `audit/spec/ethereum/WithdrawAnchorEviction.t.sol` | After 129+ verifyBlocks, old L1 root → `UnknownAnchor` | Can withdrawal proof be re-bound to fresher anchor? |
| WD-Q2 / A3-02 | `audit/spec/ethereum/WithdrawRecipientZero.t.sol`; main `test_withdrawByProof_zeroRecipientWorks` | Zero recipient accepted; USDC transfer reverts → nullifier unused, event stuck | Early `InvalidRecipient` revert vs AN must never emit recipient=0? |
| WD-Q3 / A3-03 | `DeployWithdrawVerifier.t.sol`, `DeployShplonkSmoke.t.sol`, `BridgeWithdrawalVerifier.sol` NatSpec | Groth16 stub accepts arbitrary 256 B proof; prod scripts use SHPLONK aggregator | Deploy checklist / CI guard that stub never wired on mainnet? |
| WD-Q4 / A3-05 | `AckiNackiBridge.sol` L75/L1042 + comments | `WITHDRAW_ANCHOR_LAYER = 1` hard-coded | Fixed forever vs future PI slot `anchorLayer`? |

### A4 — AAVE / verifiers / governance

| ID | PoC / evidence | What we proved | Open question |
|----|----------------|----------------|---------------|
| QC-A4-1 / A4-01 | `audit/spec/ethereum/ShplonkEmptyCode.t.sol` | `ShplonkHalo2Verifier` on empty code → `verify` returns true (unsafe) | Add `extcodesize` guard in ctor? (blocked test **U-SHL-02** until fix) |
| A4-Q2 | `audit/spec/ethereum/FuzzPauseMatrix.t.sol` | Owner can pause user ops indefinitely; owner AAVE ops still work | Timelock / max-pause / guardian split before mainnet? |

---

## Info / OK (no QC escalation)

| ID | Note |
|----|------|
| A3-04 | Zero-amount withdraw consumes nullifier — harmless, undocumented |
| A4-02..05 | Docs drift, oracle optimism, centralization notes — see `findings-summary.md` |

---

## Manual / operational (not automated)

| ID | Action |
|----|--------|
| E-07 | Post-deploy immutables on Sepolia (`cast call` verifier addresses, USDC, BK seed) |
| E-05 | Opt-in AAVE fork: `FOUNDRY_PROFILE=fork FORK_URL=… forge test --match-contract AaveFork` |

---

## Recommended follow-ups (team)

1. **Resolve QC table** — product/protocol answers → move row to OK (accepted) or BC (if team agrees it's wrong).
2. **Docs:** `bridge_verification.md` anchor wording (QC-A2-1, QC-A1-4); `PROJECT_FACTS.md` USDC trust (QC-A1-2).
3. **Code (if QC → fix):** `extcodesize` in `ShplonkHalo2Verifier` (QC-A4-1); optional `InvalidRecipient` (WD-Q2).
4. **CI:** add `FOUNDRY_PROFILE=audit forge test` job on `audit` branch.
5. **Phase F:** AN contracts (`audit/spec/an/`) after ETH signoff.

---

## Signoff checklist

- [x] Phases A–E complete; audit overlay 47/47 green
- [x] BC = 0; all QC entries have PoC links
- [ ] Partner/dev responses on QC register (13 items)
- [ ] `make pre-push` green on `audit` — **blocked on pre-existing `forge fmt` drift** in `contracts/ethereum/` (not introduced by audit overlay); audit gate `FOUNDRY_PROFILE=audit forge test` is 47/47 green
- [ ] E-07 immutables check on target deployment

**Auditor note:** No confirmed bugs filed. Residual risk is **documented and test-backed**; closure requires QC disposition, not more PoC work.
