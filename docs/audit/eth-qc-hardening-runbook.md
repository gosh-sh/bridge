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

### WD-Q1 — 128-window eviction (ETH-3 / A3-01)

`HISTORY_PROOF_WINDOW = 128` is a count of **successful `verifyBlock` appends per layer**, not a `blockSeqNo` span. After 128 newer appends on that layer the oldest hash is evicted; `withdrawByProof` with that `finalRoot` reverts `UnknownAnchor`. **Funds stay in the treasury** (no burn, no second spend).

A `blockSeqNo` jump (strictly greater than `storedLastSeenBlockSeqNo`) is **permitted** so the relayer can catch up with a later valid proof. One call still writes **one** window slot — a jump does not mass-evict. Sequential `last_seen+1` stays an **off-chain** relayer policy (`test_relayerLoop_seqNoFastForward_isPermittedByContract`, `test_seqNoFastForward_doesNotEvictEarlierAnchor`). No on-chain jump cap: a cap would brick catch-up after downtime.

**SLA (withdraw relayer):**

1. **Primary:** submit `withdrawByProof` against the original Circuit 4 `finalRoot` before that hash is evicted (128 subsequent `verifyBlock`s on that layer).
2. **Fallback:** re-prove Circuit 4 against a **still-in-window** descendant (`test_reproveAgainstLaterInWindowAnchor_succeeds` pins the contract path; `reprove_against_later_layer_hash_keeps_nullifier` pins the daemon translation). Partner dense chain is at most `MAX_CHAIN_LEN = 11` rungs (`gosh-dense-balanced-tree`); pick a remaining window entry within that hop bound. If no such root remains, that withdrawal event is stranded until/unless the circuit hop bound is raised — funds still sit in treasury.

**Alert (ETH-03):** poll `layerWindowLen(L)` for each active layer. When occupancy ≥ 100 (of 128) and a pending withdrawal is still bound to an older hash in that ring, page the withdraw relayer. After wrap (`layerWindowLen == 128` and `isKnownLayerAnchor(L, finalRoot) == false`) only the re-prove path remains.

Gate: `cd audit/spec/ethereum && forge test --match-contract WithdrawAnchorEviction -vv`

### WD-Q4 — withdraw anchor layer

NB-Q1 (2026-08-04): the `WITHDRAW_ANCHOR_LAYER = 1` pin was removed. `withdrawByProof` now scans every layer window via `_isKnownAnchor`, so partner L≥2 witnesses are accepted without a contract upgrade. Every window entry was written by a verified `verifyBlock`. Layer *identity* is Circuit 4's dense-chain climb, written in `docs/audit/circuit4-anchor-binding.md` (ETH-15). Option A (Circuit 4 PI slot `anchorLayer` + range-checked scan of the specific window) remains the ultimate target once the Circuit 4 re-keygen lands.

### A4-Q2 / ETH-4 — pause (keep #20; incident plan)

`AckiNackiBridge` has **no** `pause()` / `whenNotPaused` / `BridgePaused` (#20 / TD-58). Stage II PDF asked for a guardian pause so ETH-1 could be stopped; ETH-1/ETH-2 are now gated (`FieldElementOutOfRange`). Restoring pause would reverse #20 (deposit HOL / pause asymmetry). A scoped pause (withdraw + `verifyBlock` only, deposits open) stays a future product question, not this change.

**Incident plan (ETH-04, risk accepted):**

| Step | Who | Action | Time box |
|------|-----|--------|----------|
| 1 | On-call relayer | Stop `verifyBlock` / `withdrawByProof` submission off-chain. Do **not** pause deposits on-chain. | minutes |
| 2 | Bridge owner (prod multisig) | `emergencyWithdrawAll` + `setAaveEnabled(false)` if AAVE must be isolated. User principal stays in `treasuryBalance`. | minutes |
| 3 | Owner / Circle liaison | Ask Circle to pause or blacklist the **bridge address** (USDC), per TD-24/56. Channel: Circle account team / issuer ops — not an on-chain switch. | hours (third party) |
| 4 | Deploy | Replacement `AckiNackiBridge` with `bridgeWithdrawalVerifier = address(0)` (withdraws off) or a patched verifier. Migrate treasury by owner ops; old contract cannot be upgraded (no proxy). | hours–day |
| 5 | Users | Unwithdrawn AN→ETH events stay claimable on the **new** contract only after a new Circuit 4 proof against its windows — communicate the cut-over. Funds on the old contract remain in its USDC/aUSDC until migrated. | announced |

Do **not** restore an on-chain pause without an explicit product reversal of #20. See `audit/findings/BRIDGE-ETH-04/`.

### QC-A1-2 — USDC trust

Bridge assumes standard ERC-20 semantics (no fee-on-transfer). Circle blacklist/pause on the bridge address freezes flows — operational risk.

### QC-OFF-01 — deposit-relayer head-of-line skip

The CLI default is `--skip-after-attempts 0` (strict sequential). **Production systemd** (`scripts/ursus/deposit-relayer.service`) sets `SKIP_AFTER_ATTEMPTS=64` before `EnvironmentFile` (the env file may override) and passes `--skip-after-attempts ${SKIP_AFTER_ATTEMPTS}`. Parked ids land in `state.json` → `parked_deposit_ids`; run `finalize-one` for each. See `docs/audit/deposit-relayer-operator-runbook.md`.

Do not ship a live daemon with skip disabled unless operators accept that one stuck `depositId` blocks the queue.

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
2. `./scripts/check_shplonk_artefacts.sh` (SHA-256 pin + EIP-170)
3. `cd contracts/ethereum && forge test --match-contract ShplonkArtefactPairing` — Circuit 4 pairing green. 1A/1B/C2 live in `ShplonkArtefactPairingPendingN14` until n14 regen; do not deploy `WIRE_VERIFY_BLOCK` until that contract is green.
4. Confirm `WIRE_VERIFY_BLOCK=true` and `USE_AXIOM_ORACLE=true` on mainnet (`DeployRealBridge` `envBool`; ETH-5)
5. Confirm Circuit 4 is wired together with the verifyBlock triple (constructor `WithdrawRequiresVerifyBlock`)
6. Confirm `MAX_DEPOSIT_AMOUNT` / product policy matches AN `uint64` mint path
7. Relayer uses `expectedPrevAnchor(numLayers)` for verifyBlock
8. Withdraw relayer monitors `layerWindowLen(L)` vs `HISTORY_PROOF_WINDOW` (alert at occupancy ≥ 100)
9. Production `deposit-relayer` runs with `--skip-after-attempts` / `SKIP_AFTER_ATTEMPTS=64` (QC-OFF-01; `scripts/ursus/deposit-relayer.service`)
