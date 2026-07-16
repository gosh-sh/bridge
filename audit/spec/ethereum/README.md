# Audit spec — Ethereum (Foundry)

Overlay tests for Pruvendo audit. Production tests remain in `contracts/ethereum/test/`.

## Setup

```bash
cd contracts/ethereum && forge install foundry-rs/forge-std --no-commit   # once
cd ../../audit/spec/ethereum && forge test
```

Profile: `[profile.audit]` — `FOUNDRY_PROFILE=audit forge test` (47 tests).

**Gate:** `make pre-push-audit` (fmt + main `forge test` + audit overlay). CI: `test:solidity:audit` on branch `audit`.

## Layout (phases C + D)

```
audit/spec/ethereum/
├── handlers/AuditHandlers.sol   # Treasury, Aave, VerifyBlock, Withdraw handlers
├── InvariantsTreasury.t.sol     # F-TR-1, F-TR-2
├── InvariantsAave.t.sol         # F-TR-3
├── InvariantsWithdraw.t.sol     # F-WD-1
├── InvariantsVerifyBlock.t.sol  # F-VB-1
├── FuzzDepositToken.t.sol       # F-TR-4
├── FuzzPauseMatrix.t.sol        # F-PS-1
├── CrossCircuitNegative.t.sol   # E-03 (CC-1/3/7)
├── E2eRelayerLoop50.t.sol       # E-04
├── DeployShplonkSmoke.t.sol     # E-06
├── DepositNegative.t.sol …      # phase C (see prior README sections)
└── …
```

## Conventions

- Header comment: `// INV: TR-1` or `// QC: QC-A4-1`
- Failing tests for known bugs: comment `// BC pending` (bug candidate) or `// QC: documents current behavior` (PoC pins behaviour; intent TBD)
- Do not weaken assertions to green without BC/QC/OK classification
- **`vm.expectRevert`:** evaluate `expectedPrevAnchor()` (and other view calls) into a local **before** `expectRevert` — otherwise Foundry applies it to the staticcall in the argument list

## Profiles

| Profile | Command | Fuzz runs |
|---------|---------|-----------|
| default | `forge test` | 256 |
| ci | `FOUNDRY_PROFILE=ci forge test` | 5000 |

See `audit/reports/test-matrix.md` for full plan.
