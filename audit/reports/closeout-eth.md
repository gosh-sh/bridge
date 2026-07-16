# ETH contracts audit — closeout (branch `audit`)

**Date:** 2026-07-16  
**Scope:** Ethereum bridge — wiring, accounting, replay, pause, deploy. ZK soundness out of scope.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; **auditor view stated**; **author must confirm** bug vs feature |
| **BC** | **Required** | Bug **candidate** — likely wrong under bridge assumptions; not confirmed until team agrees |

QC ≠ «не проверяли». QC = «проверили, просим автора подтвердить intent».

**This pass:** BC = 0. QC = 13 (all PoC-linked). No QC row deleted without author ack.

---

## Phase completion

| Phase | Status |
|-------|--------|
| A–E tests | ✅ 47/47 audit overlay |
| Docs fixes (QC-A1-4, QC-A2-1) | ✅ applied — **author ack pending** |
| Author QC disposition | ⏳ 13 items open |

---

## QC register — PoC, auditor view, ask author

### A1 — Deposits & treasury

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A1-1 | `DepositWhaleCap.t.sol` | Per-tx cap likely intentional; not global TVL limit. NatSpec clarified. | Per-tx-only cap OK for deployment target? |
| QC-A1-2 | `FuzzDepositToken.t.sol` | Trust assumption; draft in `PROJECT_FACTS.md`. | USDC proxy/blacklist risk accepted? |
| QC-A1-3 | `EmergencyYield.t.sol` | Yield trapped after emergency — tradeoff, not theft. | Accept vs emergency should harvest yield? |
| QC-A1-4 | Code review | **Docs bug fixed** (`bridge_verification.md` AC-6). Code safe. | AC-6 wording OK? |

### A2 — verifyBlock

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A2-1 | `AckiNackiBridgeLayerAnchor.t.sol` | **Docs bug fixed** (LH-3/CC-6/L6). AB-Q4 code intentional. | Monitoring/relayer runbook aligned? |
| QC-A2-2 | `BkSetUpdateReplay.t.sol` | Dual cursor likely by design; stale prover = liveness. | Prover reads live cursor at prove time? |
| QC-A2-3 | `VerifyBlockZeroLayer.t.sol` | Partner-dependent: zero active hash — document or reject on-chain. | Circuit 2 can emit zero active hash? |
| QC-A2-4 | `_highestActiveLayer()` | Edge on permanent layer shrink. | Partner semantics on shrink? |

### A3 — withdrawByProof

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| WD-Q1 | `WithdrawAnchorEviction.t.sol` | 128-window by design; liveness if no re-prove. | Re-prove against fresher anchor supported? |
| WD-Q2 | `WithdrawRecipientZero.t.sol` | Lean hardening: `InvalidRecipient`. Stuck event, not theft. | On-chain reject vs AN guarantee? |
| WD-Q3 | Deploy smoke tests | Misdeploy risk only; prod uses SHPLONK. | Deploy/CI guard on mainnet? |
| WD-Q4 | Code comments | Layer 1 coupling intentional. | Fixed vs future PI slot? |

### A4 — Verifiers / governance

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A4-1 | `ShplonkEmptyCode.t.sol` | Lean hardening: `extcodesize` guard. | Add before mainnet? |
| A4-Q2 | `FuzzPauseMatrix.t.sol` | Centralization; not fund loss. | Timelock / max-pause? |

---

## Recommended hardening (auditor — still ask before implementing)

| ID | Suggested action | Severity if author agrees |
|----|------------------|---------------------------|
| WD-Q2 | `recipient != address(0)` revert | Low / UX |
| QC-A4-1 | `extcodesize(yul) > 0` in `ShplonkHalo2Verifier` ctor | Low / defense-in-depth |
| QC-A2-3 | Reject `layerHashes[i]==0` for `i<numLayers` **if** Circuit 2 can emit zero | Medium |

---

## Docs updated by audit (ack pending)

- `docs/operations/bridge_verification.md` — LH-3, LH-8, CC-6, L6 monitoring, AC-6
- `docs/architecture/four_circuit_architecture.md` — CC-6
- `contracts/ethereum/src/AckiNackiBridge.sol` — `MAX_DEPOSIT_AMOUNT` NatSpec
- `audit/PROJECT_FACTS.md` — USDC trust draft

---

## Signoff checklist

- [x] Phases A–E; 47/47 audit tests
- [x] QC register with PoC + auditor view
- [ ] **Author confirms** each QC row (or escalates to BC)
- [ ] E-07 post-deploy immutables (manual)
- [ ] `make pre-push` — blocked on pre-existing `forge fmt` drift in main tree

**Auditor note:** We lowered the bar for *our* opinion (see auditor view column) but **did not close** QC items without author. Two docs bugs were fixed proactively; please ack QC-A1-4 and QC-A2-1.
