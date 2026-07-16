# Questions (QC)

**Policy:** QC = question **after PoC**. We reproduced the behaviour; we need product/protocol intent to decide bug vs feature.  
**Not** «skipped testing». See `closeout-eth.md` for full register + PoC links.

| ID | Topic | PoC / evidence | Open question | Status |
|----|-------|----------------|---------------|--------|
| QC-A1-1 | Per-tx deposit cap | `DepositWhaleCap.t.sol` | 100 USDC per call only — global TVL cap intended? | open |
| QC-A1-2 | USDC trust model | `FuzzDepositToken.t.sol`; `deposit` L587–592 | Document upgradeable proxy / blacklist / fee-on-transfer assumption? | open |
| QC-A1-3 | Emergency AAVE yield | `EmergencyYield.t.sol` | Yield trapped after `emergencyWithdrawAll` — accepted? | open |
| QC-A1-4 | AC-6 docs vs code | Code review + `nonReentrant` | Reword AC-6 in `bridge_verification.md` (pull-then-account)? | open |
| QC-A2-1 | Anchor docs vs code | `AckiNackiBridgeLayerAnchor.t.sol` | Update LH-3/CC-6/L6 monitoring for per-layer `_expectedPrevAnchor`? | open |
| QC-A2-2 | BK update liveness | `BkSetUpdateReplay.t.sol`; `applyBkSetUpdate` cursors | Prover `lastSeen` if `verifyBlock` advances before submit? | open |
| QC-A2-3 | Zero active layer hash | `VerifyBlockZeroLayer.t.sol` | Circuit 2 can emit zero? On-chain reject vs document assumption? | open |
| QC-A2-4 | `_highestActiveLayer` monotonicity | Code `_highestActiveLayer()` L880–888 | Partner semantics when layer count permanently shrinks? | open |
| WD-Q1 | Anchor window eviction | `WithdrawAnchorEviction.t.sol` | Re-prove withdrawal against fresher anchor? | open |
| WD-Q2 | Zero recipient | `WithdrawRecipientZero.t.sol`; main `zeroRecipientWorks` | Add `InvalidRecipient` or AN-side guarantee? | open |
| WD-Q3 | Groth16 stub deploy | `DeployWithdrawVerifier.t.sol`, `DeployShplonkSmoke.t.sol` | Deploy invariant: stub never on mainnet? | open |
| WD-Q4 | Hard-coded anchor layer | `AckiNackiBridge.sol` L75/L1042 | Layer 1 fixed vs future PI `anchorLayer`? | open |
| QC-A4-1 | Empty Yul verifier | `ShplonkEmptyCode.t.sol` | Add `extcodesize` guard (flip U-SHL-02 after fix)? | open |
| A4-Q2 | Pause governance | `FuzzPauseMatrix.t.sol` | Timelock / max-pause before mainnet? | open |

Resolve → update `closeout-eth.md` row to **OK (accepted)** or escalate to **BC** if team agrees behaviour is wrong.
