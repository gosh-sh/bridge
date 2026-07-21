# F10 Deposit Pipeline — Audit Remediation Plan

**Source:** `HANDOFF-f10-prover-relayer-ru.txt` (2026-07-17, audit branch)  
**Scope:** `deposit-prover` + `deposit-relayer-daemon` off-chain pipeline  
**Threat model:** `finalizeDeposit` on AN is permissionless — relayer is not a trust root; failures affect **liveness**, not fund safety (nullifier prevents double-mint).

**Gate (from audit branch):** `make audit-deposit-relayer-test`; PoC tests in `deposit-prover/tests/f10a_binding.rs`, `padding_mutation_poc.rs`, `crates/deposit-relayer-daemon/tests/f10_*.rs`.

---

## Verdict summary

| ID | Priority | Verdict | Phase |
|----|----------|---------|-------|
| QC-OFF-13 | P4 | QC acknowledged | **B** (done) |
| QC-OFF-06 | P0 | QC acknowledged | **B** (done; maps exit 51) |
| QC-OFF-09 | P1 | QC acknowledged | **C** (off-chain done; on-chain after BC-AN-01) |
| QC-PROV-01 | P2 | QC acknowledged | **A** |
| QC-OFF-01 | P3 | QC acknowledged | D |
| QC-OFF-07 | P4 | OK to extend | **A** |
| QC-OFF-08 | P4 | QC acknowledged | **A** |
| QC-OFF-11 | P4 | QC acknowledged | **A** (partial), B (Pending) |
| QC-OFF-12 | P4 | OK to fix | **A** |
| QC-OFF-10 | — | QC acknowledged | D |
| QC-OFF-02 | — | OK + doc | D |
| QC-OFF-03 | — | OK by design | — |
| QC-OFF-04 | — | QC acknowledged | D |
| QC-OFF-05 | — | QC acknowledged | B/C |
| QC-PROV-02 | — | **CLOSED** (2026-07-17) | — |
| QC-PROV-03 | — | **CLOSED** (2026-07-17) | — |
| QC-PROV-04 | — | QC acknowledged, accept | D (upstream) |
| BC-AN-01 | cross | BC confirmed | C (on-chain, sibling handoff) |
| BC-AN-02 | cross | BC confirmed | C (on-chain, sibling handoff) |

---

## Phase A — local fixes (no external deps)

Shippable with unit tests only; target branch `pruvendo/f10-phase-a`.

### QC-PROV-01 — verify/generator on 7 instances vs prod 11

**Problem:** `deposit-prover/src/prover.rs::verify_proof` and `generate_solidity_verifier` still use 7 public instances; circuit + AN opcode use 11.

**Plan:**
- Update `verify_proof` to expect 11 instances and compare all fields (add dappIdHigh/Low, anAccountHigh/Low).
- Change `generate_solidity_verifier` to `num_instance = vec![11]`.
- Share constant with relayer `NUM_PUBLIC_INPUTS = 11`.

### QC-OFF-07 — extend `check_binds_to`

**Problem:** Relayer only checks depositId, sender, anAccount before prove.

**Plan:** Add amount, contractAddress (vs `event.source_contract`), blockHashHigh/Low checks in `types.rs::check_binds_to`.

### QC-OFF-08 — deployment-bound state + single-writer lock

**Problem:** `state.json` has no chain/bridge/dappId binding; no lock against two processes.

**Plan:**
- Extend `RelayerState` with `chain_id`, `bridge_address`, `dapp_id_hash` (or full fields).
- On load: refuse mismatch unless `--force-state`.
- Advisory `flock` on state file path at daemon start.

### QC-OFF-11 — partial (CLI + durability)

**Problem:** `backoff_multiplier=0` hot-loops; no fsync; Pending→Rejected (Pending deferred to Phase B).

**Plan:**
- CLI: reject `backoff_multiplier == 0` and `backoff_initial_secs == 0`.
- `state.save`: fsync temp file + parent directory before rename.

### QC-OFF-12 — stale 7-arg python ABI

**Problem:** `crates/an-bridge-prover/python/contracts/USDCBridge.abi.json` has legacy 7-arg `finalizeDeposit`; production uses 2-arg `(bytes proof, bytes publicInputs)`.

**Plan:** Replace with canonical ABI from `scripts/ursus/USDCBridge.abi.json`; add README pointer.

---

## Phase B — receipt parsing (enables P0)

### QC-OFF-13 — real transaction status

**Plan:** Parse tvm-sdk receipt (compute exit_code, abort) in `TvmAckiNacki::get_transaction_status`; map to Confirmed/Reverted/Pending.

### QC-OFF-06 — competing relayers (P0)

**Plan:** Map known nullifier-already-consumed exit code → `SubmitOutcome::AlreadyFinalized`; depends on QC-OFF-13.

### QC-OFF-05 — nullifier pre-check

**Plan:** Implement `is_finalized` when AN ships read API for `usedDepositIds`.

### QC-OFF-11 — Pending policy

**Plan:** Keep Pending distinct from Rejected (retry without advancing cursor).

---

## Phase C — config + on-chain gates

### QC-OFF-09 — AN_DAPP_ID validation

**Plan:** Remove silent `"0"` default for live paths; parse/validate hex U256 at startup; preflight vs on-chain `EXPECTED_DAPP_ID` after BC-AN-01.

**Done (off-chain):** `parse_and_validate_dapp_id` rejects empty/non-hex/over-width; live `daemon` rejects zero unless `--dry-run`; configured dappId is logged at start. On-chain `EXPECTED_DAPP_ID` getter remains BC-AN-01 (acki-nacki).

### BC-AN-01 / BC-AN-02

See sibling handoff — USDCBridge Solidity changes in acki-nacki repo (out of scope here).

---

## Phase D — larger / optional

- **QC-OFF-01:** skip-after-N-attempts + optional out-of-order finalize.
- **QC-OFF-10:** incremental `scanned_through_block` in `EthLogSource`.
- **QC-OFF-02:** operator runbook (manual `finalize-one`, backup relayer SLA).
- **QC-OFF-04:** TLS/endpoint allowlist, key custody docs.
- **QC-PROV-04:** upstream axiom-eth padding zero-constraint (accept for testnet).

**Done (Phase D):**
- `--skip-after-attempts N` parks stuck ids in `state.json` → `parked_deposit_ids`.
- `scanned_through_block` + shared scan cursor — incremental `eth_getLogs` tail scans.
- `SubmitOutcome::Pending` distinct from `Rejected` (QC-OFF-11 Pending policy).
- Live daemon rejects non-HTTPS GraphQL unless loopback / `--allow-insecure-graphql`.
- Runbook: `docs/audit/deposit-relayer-operator-runbook.md`.
- QC-PROV-04 documented as accepted upstream limitation in runbook.

---

## Implementation tracking

| Task | Status | Branch / PR |
|------|--------|-------------|
| Plan doc (this file) | done | `pruvendo/f10-phase-a` |
| QC-PROV-01 | done | `pruvendo/f10-phase-a` |
| QC-OFF-07 | done | `pruvendo/f10-phase-a` |
| QC-OFF-08 | done | `pruvendo/f10-phase-a` |
| QC-OFF-11 (partial) | done | `pruvendo/f10-phase-a` |
| QC-OFF-12 | done | `pruvendo/f10-phase-a` |
| Phase B (QC-OFF-13, 06) | done | `pruvendo/f10-phase-a` |
| Phase C (QC-OFF-09 off-chain) | done | `pruvendo/f10-phase-a` |
| Phase D (QC-OFF-01,10,02,04,11 Pending) | done | `pruvendo/f10-phase-a` |
| BC-AN-01 / BC-AN-02 (on-chain) | pending | acki-nacki sibling |

---

## How to respond to audit (format)

For each ID: `OK` / `QC acknowledged` / `BC confirmed` + brief plan + target phase.

Example: `QC-OFF-06 → QC acknowledged; Phase B: map nullifier revert to AlreadyFinalized after QC-OFF-13 lands.`
