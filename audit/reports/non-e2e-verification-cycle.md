# Non-E2E verification cycle (audit branch)

**Goal:** maximum contract-logic coverage without shellnet / live cluster E2E.  
**ZK circuits:** out of audit scope — use pre-generated fixtures only; clone prover repos on demand.

---

## Layer model

| Layer | ETH (Phases A–E) | AN (Phase F) | Tooling |
|-------|------------------|--------------|---------|
| **Manual** | `audit/reports/manual-audit/A*.md` | `F1-usdcbridge-deposit.md` + F2 withdraw/admin | code walk + invariant map |
| **Unit (U)** | `audit/spec/ethereum/` Foundry | `audit/spec/an/unit/` pytest | mocks for oracles/verifiers |
| **Integration (I)** | bound Groth16 in `CrossCircuitNegative` | real `deposit_10proofs` + MessagePipeline | real ZK bytes, not circuit audit |
| **E2E (E)** | fork AAVE, shellnet relayer | `test_usdcbridge_finalize.py` | **deferred** |

---

## Gates (run order)

```bash
# ETH contract overlay (closed)
make pre-push-audit          # main forge + audit/spec/ethereum (47 tests)

# AN contracts (active)
./scripts/setup_an_audit_tools.sh
./scripts/sync_an_contracts.sh    # acki-nacki @ dev → an-contracts + fixtures
cd audit/spec/an-contracts && ./build.sh
make audit-an-test           # full pytest (fixtures)
make pre-push-an             # unit only, no fixtures

# Optional local vendor tree (gitignored)
./scripts/setup_audit_vendors.sh
SETUP_AUDIT_VENDORS_PROVERS=1 ./scripts/setup_audit_vendors.sh   # when PoC needs partner prover
```

---

## What is real vs mock

| Flow | Contract logic under test | ZK prover in CI | ZK verifier in tests |
|------|---------------------------|-----------------|----------------------|
| ETH→AN deposit | `USDCBridge` / `DepositVoucher` | not run | **real** opcode + fixture bytes |
| ETH `deposit()` | `AckiNackiBridge` | not in audit overlay | N/A (no ETH verifier) |
| AN `initiateWithdrawal` | burn + event shape | — | — |
| ETH `verifyBlock` | state machine | not in AN pass | mock in audit overlay; real in main `VerifyBlock.t.sol` |
| ETH `withdrawByProof` | nullifier/treasury | not in AN pass | mock verifier |

---

## Phase F — remaining work (no E2E)

| P | Item | Deliverable |
|---|------|-------------|
| **P0** | BC-AN-01 dual `dappId` mint | `bootstrap_hermez_srs_k18.sh` + `generate_bc_an_01_dual_proofs.sh` | ✅ |
| **P1** | BC-AN-02 L1 bridge allowlist | integration pre-ZK test | ✅ |
| **P1** | Joint ETH↔AN QC | cap / pause / workchain matrix in `questions-cross-chain.md` | ✅ |
| **P2** | F2 manual withdraw/admin | `manual-audit/F2-usdcbridge-withdraw-admin.md` | ✅ |
| **P2** | Admin + TIP-3 unit tests | `unit/test_usdcbridge_admin.py` | ✅ |
| **P2** | Withdraw happy path (no ZK) | `unit/test_usdcbridge_withdraw_happy.py` | ✅ |
| **P3** | QC-AN-07 voucher brick | `integration/test_finalize_deposit_voucher_brick.py` | ✅ |
| **P3** | CI job `test:an:audit` | `.gitlab-ci.yml` (skip w/o `.tools/`) | ✅ |
| **defer** | F6 shellnet | `acki-nacki/tests/exchange/` |

---

## BC / QC logging

- Register: `audit/reports/an-audit-direction.md`
- ETH closed register: `audit/reports/closeout-eth.md`
- New finding: `audit/findings/<ID>/analysis.md` + test or PoC path
- Test matrix: `audit/reports/test-matrix-an.md`

---

## Vendor layout

See `audit/vendors/README.md`. Synced contract workspace remains `audit/spec/an-contracts/` (rsync, not vendor path).
