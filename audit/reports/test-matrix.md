# Test matrix — фаза B→E (ETH contracts)

**Gate:** `cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test` — **53/53 green** (2026-07-17).

**Night gate:** `make audit-solidity-ci` or `./scripts/ci_eth_audit_night.sh` — **53/53 @ profile ci** (5000 fuzz / 1000 inv).

---

## Progress tracker

| Phase | Tests | Status |
|-------|-------|--------|
| C — unit | 29 | ✅ |
| D — fuzz/invariant | 17 (128 inv / 256 fuzz @ audit; 1000 inv @ ci) | ✅ |
| E — E2E gaps | 7 new + covered | ✅ |

---

## Фаза C — Unit tests ✅

| ID | File | Status |
|----|------|--------|
| U-DEP-01..03 | `DepositNegative.t.sol` | done |
| U-DEP-04 | `DepositCounter.t.sol` | done |
| U-DEP-05 | `DepositIsolation.t.sol` | done |
| U-DEP-06 | `DepositWhaleCap.t.sol` | done |
| U-DEP-07 | `AckiNackiBridgePause.t.sol` | covered |
| U-VB-01 | `VerifyBlockCEI.t.sol` | done |
| U-VB-02 | `VerifyBlockZeroLayer.t.sol` | done |
| U-VB-03 | `BkSetUpdateReplay.t.sol` | done |
| U-VB-04 | `AckiNackiBridgeLayerAnchor.t.sol` | covered |
| U-WD-01 | `WithdrawAnchorEviction.t.sol` | done |
| U-WD-02 | `WithdrawRecipientZero.t.sol` | done |
| U-WD-03 | `DeployWithdrawVerifier.t.sol` | done |
| U-WD-04 | `AckiNackiBridgeWithdrawByProof.t.sol` | covered |
| U-AAVE-01 | `HarvestYieldBound.t.sol` | done |
| U-AAVE-02 | `EmergencyYield.t.sol` | done |
| U-AAVE-03 | `OwnerPrincipal.t.sol` | done |
| U-SHL-01 | `ShplonkEmptyCode.t.sol` | done (documents QC-A4-1) |
| U-SHL-02 | flip after extcodesize fix | blocked |
| U-ADP-01 | main tree forgery suites | covered |
| U-ADP-02 | `VerifierCtor.t.sol` | done |

---

## Фаза D — Fuzz / invariant ✅

| ID | Invariant | Handler | File | Status |
|----|-----------|---------|------|--------|
| F-TR-1 | TR-1 solvency | `TreasuryHandler` | `InvariantsTreasury.t.sol` | **done** |
| F-TR-2 | TR-2 conservation | ghost deposits/withdraws | `InvariantsTreasury.t.sol` | **done** |
| F-TR-3 | TR-3 principal ≤ aUSDC | `AaveHandler` | `InvariantsAave.t.sol` | **done** |
| F-TR-4 | TR-4 exact transferFrom | fuzz | `FuzzDepositToken.t.sol` | **done** |
| F-PS-1 | PS-1 / A4-INV-4 pause | fuzz matrix | `FuzzPauseMatrix.t.sol` | **done** |
| F-VB-1 | LH-6 seq monotonic | `VerifyBlockHandler` | `InvariantsVerifyBlock.t.sol` | **done** |
| F-CC-3 | CC-3 bkSet stable | `VerifyBlockHandler` | `InvariantsVerifyBlock.t.sol` | **done** |
| F-WD-1 | WD-7 nullifier replay | `WithdrawReplayHandler` | `InvariantsWithdraw.t.sol` | **done** |
| F-DEP-5 | DEP-5 counter monotonic | `DepositHandler` | `InvariantsDeposit.t.sol` | **done** |
| F-DEP-6 | DEP-6 deposit isolation from VB state | `DepositHandler` | `InvariantsDeposit.t.sol` | **done** |
| F-A4-1 | A4-INV-1 owner treasury | `OwnerOpsHandler` | `InvariantsOwner.t.sol` | **done** |
| F-A4-5 | instance tampering | — | `ShplonkAggregatorForgery.t.sol` | covered |

Handlers live in `audit/spec/ethereum/handlers/AuditHandlers.sol`.

Profile: `[profile.audit]` in `foundry.toml` — `invariant.runs=128`, `fuzz.runs=256`.

---

## Фаза E — E2E ✅

| ID | Scenario | File | Status |
|----|----------|------|--------|
| E-01 | Bound 1A+2 verifyBlock | main `AckiNackiBridgeVerifyBlock.t.sol` | covered |
| E-02 | Production SHPLONK paths | main `AckiNackiBridgeProduction*.t.sol` | covered |
| E-03 | Cross-circuit negative (CC-1/3/7 + CEI) | `CrossCircuitNegative.t.sol` | **done** |
| E-04 | 50+ block relayer loop | `E2eRelayerLoop50.t.sol` | **done** |
| E-05 | AAVE fork | main `AckiNackiBridgeAaveFork.t.sol` | covered (opt-in) |
| E-06 | Deploy script SHPLONK smoke | `DeployShplonkSmoke.t.sol` | **done** |
| E-07 | Post-deploy immutables | — | manual |

---

## QC → test mapping

| QC | Action | Status |
|----|--------|--------|
| QC-A1-1 | U-DEP-06 | ✅ |
| QC-A1-2 | F-TR-4 | ✅ |
| QC-A1-3 | U-AAVE-02 | ✅ |
| QC-A2-3 | U-VB-02 | ✅ |
| QC-A4-1 | U-SHL-01 | ✅ |
| WD-Q1 | U-WD-01 | ✅ |
| WD-Q3 | U-WD-03 + E-06 | ✅ |

---

## Closeout (non-E2E / non-AN)

| Item | Status |
|------|--------|
| `closeout-eth.md` | done |
| CI `test:solidity:audit` | done |
| CI `test:solidity:audit:ci` (night, allow_failure) | done |
| `make pre-push-audit` | done |
| `forge fmt` main tree | done |
| QC author ack | open |
| E-07 immutables | deferred |
| Phase F AN | deferred |
