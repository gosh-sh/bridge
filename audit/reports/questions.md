# Questions (QC)

**Policy:** QC = question **after PoC**. Behaviour reproduced; we state an **auditor view** but **ask the author to confirm** — we do not unilaterally close items as OK.

| ID | PoC | Auditor view | Ask author |
|----|-----|--------------|------------|
| QC-A1-1 | `DepositWhaleCap.t.sol` | Likely **intentional per-tx limit**; misleading “whale” wording (NatSpec fixed in `AckiNackiBridge.sol`). Not a fund-loss bug. | Confirm per-tx-only cap is acceptable for milestone / mainnet? |
| QC-A1-2 | `FuzzDepositToken.t.sol` | **Trust assumption**, not code bug. Draft in `PROJECT_FACTS.md`. | Confirm USDC proxy/blacklist/pause risk is accepted? |
| QC-A1-3 | `EmergencyYield.t.sol` | **Tradeoff**, not theft: yield stays over-collateral after emergency. | Accept residual vs route yield to `yieldRecipient` on emergency? |
| QC-A1-4 | Code + `nonReentrant` | **Docs bug** (fixed `bridge_verification.md` AC-6). Code uses safe pull-then-account. | Confirm AC-6 rewording matches your intent? |
| QC-A2-1 | `AckiNackiBridgeLayerAnchor.t.sol` | **Docs bug** (fixed LH-3/CC-6/L6 in `bridge_verification.md`). Code AB-Q4 is intentional. | Confirm monitoring/relayer docs match your operational runbook? |
| QC-A2-2 | `BkSetUpdateReplay.t.sol` | Likely **by design** — rotation proof must use fresh `lastSeen`. Liveness risk if prover stale. | Confirm prover/relayer always reads live cursor before prove? |
| QC-A2-3 | `VerifyBlockZeroLayer.t.sol` | **Unclear without partner:** if Circuit 2 cannot emit zero active hash → document; if can → lean **hardening/BC**. | Can Circuit 2 bind a zero hash in an active slot? |
| QC-A2-4 | `_highestActiveLayer()` code | **Edge case** if AN permanently shrinks layers; likely rare in production schedule. | Confirm partner semantics on permanent layer-count shrink? |
| WD-Q1 | `WithdrawAnchorEviction.t.sol` | **By design** 128-slot window. **Liveness** if re-prove impossible (funds stay in treasury). | Confirm withdrawal events can be re-proven against fresher anchor? |
| WD-Q2 | `WithdrawRecipientZero.t.sol` | **Lean hardening:** early `InvalidRecipient` cleaner than stuck nullifier. Not fund loss. | Prefer on-chain reject vs AN-side guarantee never emit recipient=0? |
| WD-Q3 | `DeployWithdrawVerifier.t.sol`, `DeployShplonkSmoke.t.sol` | **Process risk** if stub misdeployed; prod scripts use SHPLONK. | Confirm deploy checklist / CI guard for mainnet? |
| WD-Q4 | `AckiNackiBridge.sol` L75/L1042 | **Intentional coupling** to witness builder layer 1; coordinated upgrade if changed. | Layer 1 fixed forever vs future PI `anchorLayer`? |
| QC-A4-1 | `ShplonkEmptyCode.t.sol` | **Lean hardening:** add `extcodesize` in ctor; deploy mitigates today. | Accept on-chain guard before mainnet? |
| A4-Q2 | `FuzzPauseMatrix.t.sol` | **Governance / centralization**, not fund theft. | Timelock / max-pause / guardian before mainnet? |

**Docs fixed by audit (pending author ack):** QC-A1-4, QC-A2-1 — see `docs/operations/bridge_verification.md`, `docs/architecture/four_circuit_architecture.md`.

Full register: `closeout-eth.md`.
