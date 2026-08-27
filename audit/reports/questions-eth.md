# ETH contracts & proofs — questions for authors

**Date:** 2026-07-17  
**Audience:** Ethereum bridge team (`AckiNackiBridge.sol`, verifiers, deploy).  
**Scope:** On-chain ETH contracts, Groth16/SHPLONK adapters, `verifyBlock` / `withdrawByProof` / `deposit`.  
**Out of scope here:** AN `USDCBridge`, cross-chain alignment — see sibling docs below.

**Status:** Questions sent to authors (2026-07-16). Awaiting disposition.

**Related:** Full closeout with test counts → `closeout-eth.md`. Test matrix → `test-matrix.md`.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | Usually yes | Matches intent (after author confirms) |
| **QC** | **Required** | Reproduced; auditor view stated; **author must confirm** bug vs feature |
| **BC** | **Required** | Bug candidate under bridge assumptions; not closed until team agrees |

**This pass:** BC = 0. QC = 13 (all PoC-linked).

---

## A1 — Deposits & treasury

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A1-1 | `DepositWhaleCap.t.sol` | Per-tx cap likely intentional; not global TVL limit. NatSpec clarified. | Per-tx-only cap OK for deployment target? |
| QC-A1-2 | `FuzzDepositToken.t.sol` | Trust assumption; draft in `PROJECT_FACTS.md`. | USDC proxy/blacklist risk accepted? |
| QC-A1-3 | `EmergencyYield.t.sol` | Yield trapped after emergency — tradeoff, not theft. | Accept vs emergency should harvest yield? |
| QC-A1-4 | Code review | **Docs bug fixed** (`bridge_verification.md` AC-6). Code safe. | AC-6 wording OK? |

---

## A2 — verifyBlock (AN state proofs on ETH)

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A2-1 | `AckiNackiBridgeLayerAnchor.t.sol` | **Docs bug fixed** (LH-3/CC-6/L6). AB-Q4 code intentional. | Monitoring/relayer runbook aligned? |
| QC-A2-2 | `BkSetUpdateReplay.t.sol` | Dual cursor likely by design; stale prover = liveness. | Prover reads live cursor at prove time? |
| QC-A2-3 | `VerifyBlockZeroLayer.t.sol` | Partner-dependent: zero active hash — document or reject on-chain. | Circuit 2 can emit zero active hash? |
| QC-A2-4 | `_highestActiveLayer()` | Edge on permanent layer shrink. | Partner semantics on shrink? |

---

## A3 — withdrawByProof (AN withdrawal proofs verified on ETH)

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| WD-Q1 | `WithdrawAnchorEviction.t.sol` | 128-window by design; liveness if no re-prove. | **closed Stage II ETH-3** — SLA + re-prove path in `docs/audit/eth-qc-hardening-runbook.md`; seq_no jump does not mass-evict; no on-chain cap. |
| WD-Q2 | `WithdrawRecipientZero.t.sol` | Lean hardening: `InvalidRecipient`. Stuck event, not theft. | **closed Stage II** — ETH keeps `InvalidRecipient`; AN `initiateWithdrawal` rejects empty recipient (`ERR_ZERO_RECIPIENT`, `BRIDGE-AN-10`). |
| WD-Q3 | Deploy smoke tests | Misdeploy risk only; prod uses SHPLONK. | Deploy/CI guard on mainnet? |
| WD-Q4 | Code comments | Layer 1 coupling intentional. | Fixed vs future PI slot? |

---

## A4 — Verifiers / governance

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A4-1 | `ShplonkEmptyCode.t.sol` | Lean hardening: `extcodesize` guard. | Add before mainnet? |
| A4-Q2 | `DepositPauseAsymmetry.t.sol` | Centralization; not fund loss. | **closed Stage II ETH-4** — keep #20 (no `pause()`). ETH-1/ETH-2 gated; do not restore pause without reversing #20. |

Stage II deploy/CI (ETH-5 / ETH-6 / ETH-10): constructor `WithdrawRequiresVerifyBlock`; `verifiers/SHA256SUMS` + `ShplonkArtefactPairing.t.sol` (1A/1B/C2 pairing still red until n14); Foundry `solc_version = "0.8.21"`. Findings `BRIDGE-ETH-05` / `06` / `10`.

Stage II Wave 5 (QC-AN-10 / QC-OFF-01 / cap docs): AN zero-recipient require; ETH `InvalidAnAccount` / `InvalidRecipient` kept; production `SKIP_AFTER_ATTEMPTS=64`; live docs no longer claim 100 USDC as the contract cap. Finding `BRIDGE-AN-10`.

---

## Docs updated by audit (ack pending)

- QC-A1-4, QC-A2-1 — `docs/operations/bridge_verification.md`, `docs/architecture/four_circuit_architecture.md`

---

## Sibling registers

| Doc | Topic |
|-----|-------|
| `questions-an.md` | AN `USDCBridge` / deposit finalization |
| `questions-cross-chain.md` | ETH ↔ AN alignment |
