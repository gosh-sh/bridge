# ETH contracts audit — closeout (`audit-new`)

**Date:** 2026-08-13 (Phase G1 sync on `audit-new`)  
**Scope:** Ethereum bridge — wiring, accounting, replay, deploy. ZK soundness out of scope.  
**Code base:** `origin/main` @ `c9d5412` + overlay `audit/spec/ethereum/` (56 tests).

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; auditor view stated; author must confirm bug vs feature |
| **BC** | **Required** | Bug candidate — likely wrong under bridge assumptions |

**This pass:** BC = 0. QC = 13 registered; **4 closed by main code** (see below); **9 open** for author ack.

---

## Phase completion

| Phase | Status |
|-------|--------|
| A–E overlay tests | ✅ **56/56** on `audit-new` (was 47; pause suite removed) |
| Main tree tests | ✅ **126/126** (`contracts/ethereum`) |
| Docs fixes (QC-A1-4, QC-A2-1) | ✅ applied — author ack pending |
| Merge main → audit overlay | ✅ `audit-new` branch |
| Author QC disposition | ⏳ 9 items open |

---

## Resolved in main (overlay updated — ack pending)

| ID | Main change | Overlay |
|----|-------------|---------|
| QC-A1-1 | `MAX_DEPOSIT_AMOUNT = type(uint64).max` (#20) | `DepositWhaleCap.t.sol` |
| QC-A2-3 | `LayerHashActiveZero` revert (#16) | `VerifyBlockZeroLayer.t.sol` |
| QC-A4-1 | `EmptyYulVerifierCode` in ctor (#16) | `ShplonkEmptyCode.t.sol` |
| WD-Q2 | `InvalidRecipient` on zero recipient (#16) | `WithdrawRecipientZero.t.sol` |
| A4-Q2 | EVM `pause()` **removed** (#20) | `FuzzPauseMatrix.t.sol` **removed** |

---

## QC register — open (author ack)

### A1 — Deposits & treasury

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A1-2 | `FuzzDepositToken.t.sol` | USDC proxy/blacklist trust assumption | Accepted for target deployment? |
| QC-A1-3 | `EmergencyYield.t.sol` | Yield trapped after emergency — tradeoff | Accept vs harvest on emergency? |
| QC-A1-4 | Code review | Docs AC-6 fixed; code safe | AC-6 wording OK? |

### A2 — verifyBlock

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A2-1 | Main `AckiNackiBridgeLayerAnchor.t.sol` + overlay | AB-Q4 per-layer anchor intentional | Runbook aligned? |
| QC-A2-2 | `BkSetUpdateReplay.t.sol` | Dual BK cursor by design | Prover reads live cursor? |
| QC-A2-4 | Invariants / fuzz | Edge on permanent layer shrink | Partner semantics? |

### A3 — withdrawByProof

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| WD-Q1 | `WithdrawAnchorEviction.t.sol` | 128-window by design | Re-prove fresher anchor supported? |
| WD-Q3 | `DeployWithdrawVerifier.t.sol` | SHPLONK-only adapters | Deploy/CI guard on mainnet? |
| WD-Q4 | Code comments | Layer-1 PI coupling intentional | Fixed vs future slot? |

---

## Recommended hardening — status

| ID | Action | Status |
|----|--------|--------|
| WD-Q2 | `InvalidRecipient` | ✅ **implemented** (#16) |
| QC-A4-1 | `extcodesize` in Shplonk ctor | ✅ **implemented** (#16) |
| QC-A2-3 | Reject zero active layer hash | ✅ **implemented** (#16) |

---

## Test gates

```bash
cd contracts/ethereum && forge test          # main suite
cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test  # overlay 56
make pre-push-audit
```

---

## Signoff checklist

- [x] Overlay 56/56 on `audit-new`
- [x] Main forge 126/126
- [x] QC register + resolved-in-main table
- [x] CI `test:solidity:audit`
- [ ] Author ack on 9 open QC rows
- [ ] E-07 post-deploy immutables — deferred
- [ ] Merge `audit-new` → `main` — **after** disposition (not this pass)

**Out of scope:** fork E2E extensions, AN contracts (see `closeout-an.md`).
