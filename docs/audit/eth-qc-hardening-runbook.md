# ETH QC hardening — operator / relayer runbook

**Branch / PR:** `pruvendo/eth-audit-qc-hardening`  
**Source audit:** `github/audit` → `questions-eth.md` / `closeout-eth.md`  
**Scope:** Author dispositions implemented as code + ops notes (QC-A1/A2/A3/A4).

---

## Code hardenings (landed)

| ID | Change |
|----|--------|
| **WD-Q2** | `withdrawByProof` reverts `InvalidRecipient` if reconstructed recipient is `address(0)` |
| **QC-A4-1** | `ShplonkHalo2Verifier` ctor requires `extcodesize(yul) > 0` |
| **WD-Q3** | NatSpec + `scripts/check_withdrawal_verifier_not_stub.sh` — production must use `BridgeWithdrawalAggregatorVerifier` only |
| **QC-A2-3** | `verifyBlock` reverts `LayerHashActiveZero` if any active `layerHashes[i]==0` |
| **QC-A1-1** | `MAX_DEPOSIT_AMOUNT = type(uint64).max` (AN mint-path width; not a TVL cap) |
| **QC-A1-3** | `excessUsdc()` + `skimExcessUsdc` — recover post-`emergencyWithdrawAll` liquid yield |

Gate: `cd contracts/ethereum && forge test --match-contract EthAuditQcHardening`  
Deploy gate: `./scripts/check_withdrawal_verifier_not_stub.sh`

---

## Ops notes (no further code)

### QC-A2-1 — per-layer chain anchor (AB-Q4)

Do **not** monitor `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`.  
Use the view `expectedPrevAnchor(numLayers)` (or the relayer helper that mirrors `_expectedPrevAnchor`). Flat-anchor alerts fire false positives when `numLayers` shrinks.

### QC-A2-2 — dual cursors

| Cursor | Updated by | Used as |
|--------|------------|---------|
| `storedLastSeenBlockSeqNo` | `verifyBlock` | Attestation PI `lastSeen` for **both** verifyBlock and `applyBkSetUpdate` |
| `storedLastBkSetUpdateSeqNo` | `applyBkSetUpdate` | BK-update monotonicity only |

**Rule:** prove `applyBkSetUpdate` against the **live** on-chain `storedLastSeenBlockSeqNo`. If `verifyBlock` advances between prove and submit, re-prove — do not treat revert as a consensus bug.

### QC-A2-4 — permanent layer shrink

`_highestActiveLayer()` does not decrease when AN permanently drops layers (windows retain history). Partner must keep `prev_max_level_layer_hash_for` aligned. Fail-closed stalls are preferred over wrong anchors.

### WD-Q1 — 128-window eviction

`HISTORY_PROOF_WINDOW = 128`. A Circuit-4 `finalRoot` older than the last 128 L1 anchors reverts `UnknownAnchor`. Funds stay in treasury.

**SLA:** submit `withdrawByProof` (or re-prove against a fresher L1 root) before eviction. Confirm partner Circuit-4 re-prove path before mainnet.

### WD-Q4 — withdraw anchor layer

NB-Q1 (2026-08-04): the `WITHDRAW_ANCHOR_LAYER = 1` pin was removed. `withdrawByProof` now scans every layer window via `_isKnownAnchor`, so partner L≥2 witnesses are accepted without a contract upgrade. Every window entry was written by a verified `verifyBlock`, so the layer index adds specificity, not security. Option A (Circuit 4 PI slot `anchorLayer` + range-checked scan of the specific window) remains the ultimate target once the Circuit 4 re-keygen lands.

### A4-Q2 — pause

Owner may leave `pause()` on indefinitely (blocks `deposit` / `verifyBlock` / `withdrawByProof`). AAVE owner paths stay available. Mainnet: use a documented pause procedure / multisig; optional timelock is a governance choice, not a fund-safety bug.

### QC-A1-2 — USDC trust

Bridge assumes standard ERC-20 semantics (no fee-on-transfer). Circle blacklist/pause on the bridge address freezes flows — operational risk.

---

## Deposit pipeline — mock vs SHPLONK (TD-43)

Off-chain deposit proofs must pass the **Blake2b SHPLONK opcode triple**, not only MockProver / `verify_proof`.

| When | Run |
|------|-----|
| CI / pre-push deposit audit | `scripts/check_deposit_audit_gates.sh` (includes TD-43 smoke after VkBlob pin) |
| Local opcode reproduction | `deposit-prover/examples/verify_opcode_triple.rs` or `verify_deposit_opcode_triple` |
| Witness / constraint only | `test_circuit_mock` — **insufficient** for AN finalize |
| Struct PI match only | `verify_proof` — **insufficient** (bincode Snark ≠ Blake2b wire) |

Fixture: `deposit-prover/fixtures/deposit_10proofs/proof_00/` (384 B `public_inputs.bin`). Without fixture, smoke needs `deposit-prover/data/kzg_params_18.srs` or `DEPOSIT_KZG_SRS`. See `audit/reports/td-43-mock-vs-shplonk-notes.md`.

---

## Deploy checklist (mainnet)

1. `./scripts/check_withdrawal_verifier_not_stub.sh`
2. `./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers`
3. Confirm `WIRE_WITHDRAW_BY_PROOF=true` and withdrawal adapter is `BridgeWithdrawalAggregatorVerifier`
4. Confirm `MAX_DEPOSIT_AMOUNT` / product policy matches AN `uint64` mint path
5. Relayer uses `expectedPrevAnchor(numLayers)` for verifyBlock
6. Withdraw relayer monitors window age vs `HISTORY_PROOF_WINDOW`
