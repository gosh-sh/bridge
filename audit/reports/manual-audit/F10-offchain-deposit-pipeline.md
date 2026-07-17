# F10 — Off-chain deposit pipeline (manual + threat model)

**Date:** 2026-07-17  
**Scope:** ETH `Deposit` event → `deposit-prover` → `deposit-relayer-daemon` → AN `USDCBridge.finalizeDeposit`.  
**Out of scope (separate):** AN/TVM contracts (F0–F8 ✅), ETH `AckiNackiBridge` (ETH closeout), opcode internals (smoke).

---

## Pipeline map

```text
[1] AckiNackiBridge.deposit()          — Ethereum L1
[2] Deposit event in block log        — Ethereum L1 (canonical)
[3] EthLogSource / fetch_deposit_data — Ethereum JSON-RPC
[4] deposit-prover (Halo2 SHPLONK)    — off-chain binary
[5] deposit-relayer-daemon            — off-chain service (optional operator)
[6] finalizeDeposit(proof, publicInputs) — AN external msg (permissionless)
[7] USDCBridge + ZKHALO2VERIFYWITHVK  — AN on-chain (audited F0–F8)
```

| Step | Component | Repo path |
|------|-----------|-----------|
| 1–2 | L1 bridge + event | `contracts/ethereum/` |
| 3 | Witness fetch | `deposit-prover/examples/fetch_deposit_data`, `crates/deposit-relayer-daemon/src/source.rs` |
| 4 | Proving | `deposit-prover/` (`circuit_v2.rs`, `export_blake2b_proof`) |
| 5 | Orchestration | `crates/deposit-relayer-daemon/` |
| 5b | Manual submit | `deposit-relayer finalize-one` (bundle dir → AN) |
| 6 | AN client | `crates/acki-nacki-interface/`, tvm-sdk GraphQL |
| 7 | On-chain | `acki-nacki/contracts/exchange/USDCBridge.sol` |

**Trust boundary:** steps 1–2 and 7 are authoritative for *safety* of binding (amount, recipient from L1 event in circuit). Steps 3–6 affect **liveness** and **configuration** (`dappId`, RPC endpoints). A compromised relayer must **not** be able to redirect payout if the proof is honest — but a **malicious prover config** can break binding (BC-AN-01).

---

## Relayer is a third-party service — what if it fails?

Design principle (from `state.rs`, `submitter.rs`, `questions-an.md`): **`finalizeDeposit` on AN is permissionless**. The relayer is **not** a trusted party for credit destination; it is an **availability** layer.

| Scenario | Safety (funds) | Liveness (credit) | Mitigation / audit action |
|----------|----------------|-------------------|---------------------------|
| Relayer **dies** | ETH USDC stays in bridge; no double-mint on AN | Deposit not credited until someone else proves + submits | Operator `finalize-one`; competing relayer; document runbook (**QC-OFF-02**) |
| Relayer **slow** / backoff | Same | Delay until finalize | Exponential backoff in `daemon.rs`; user/ops manual path |
| Relayer **bug** (wrong PI encode) | AN rejects (`AnRejected`); no mint | Stuck until fix | Unit tests `submitter.rs`; F10-B negative tests |
| Relayer **bug** (wrong `dappId` in prover config) | **BC-AN-01** — wrong tag → extra mint path | May mint under wrong namespace | Pin `AN_DAPP_ID`; on-chain `EXPECTED_DAPP_ID` |
| **Two relayers** race | AN nullifier — second submit no-op | First success wins | ✅ by design; F10-C test |
| Relayer **crash** mid-tick | AN nullifier authoritative; `state.json` is cache | Resume from `state.json` on restart | `restart_resumes_from_persisted_state` test |
| **Stuck** on one `depositId` | No loss; later ids blocked | **Head-of-line blocking** — `next_target = last+1` | **QC-OFF-01** — intentional? skip policy? |
| **Phishing** (fake relayer UI) | User ETH deposit still valid on real L1 | Victim may wait forever if they don't use real chain | User education; ops monitor `depositId` lag |
| **MITM** on ETH RPC | Witness from wrong fork/reorg → proof fails or wrong block hash | Prove/submit fail or reject | Confirmation depth in `EthLogSource`; F10-D reorg test |
| **MITM** on AN GraphQL | Operator keys / submit to wrong contract | No user ETH loss; failed finalize | TLS, endpoint pinning (**QC-OFF-04**) |
| **Griefing** bad proofs | No theft; AN gas wasted | N/A | QC-AN-09 (accepted) |
| Malicious relayer **censors** one user | Cannot change L1 event | That deposit not finalized by *this* relayer | Another party can submit |

---

## Safety vs liveness split (for developers)

**Safety (must hold even if relayer is evil or absent):**

- Recipient `anAccount` and `amount` come from **proof public inputs** bound to L1 receipt in circuit — relayer cannot pass scalar args to `finalizeDeposit` (only `proof`, `publicInputs` bytes).
- Double finalize of same `(depositId, contractAddr, dappId)` blocked on AN (voucher replay).
- **Exceptions (on-chain / config, not relayer UI):** `dappId` not in L1 event (BC-AN-01); `contractAddr` not allowlisted (BC-AN-02).

**Liveness (relayer or operator matters):**

- Someone must run `deposit-prover` + submit `finalizeDeposit` after L1 deposit.
- Default daemon processes **`depositId` strictly in order** — one poisoned/stuck id blocks the queue (**QC-OFF-01**).
- `is_finalized` pre-check on live AN may be a no-op until read API ships (`submitter.rs` comment) — wastes prove gas on replay, not a safety bug.

---

## F10 audit workstreams

| ID | Target | Deliverable | Status |
|----|--------|-------------|--------|
| F10-M | This doc + plan § F10 | Threat model | ✅ |
| F10-A | `deposit-prover/` | PI layout ↔ L1 event; MPT witness; `dappId` injection point; VK/SRS pins | todo |
| F10-B | `deposit-relayer-daemon/` | Fault injection: `ProofFailed`, `AnRejected`, restart, `DepositIdMismatch` | partial (unit tests exist) |
| F10-C | Competing submitters | Two mocks finalize same id — one mint | todo |
| F10-D | Reorg / RPC | Confirmation depth; stale block hash in witness | todo |
| F10-E | `acki-nacki-interface` | `finalizeDeposit` ABI encode; no extra fields | todo |
| F10-F | Head-of-line blocking | Document + test: stuck id N blocks N+1 | todo |
| F10-G | E2E shellnet | `E-AN-01` + live relayer tick | deferred |
| F10-H | Operator runbook | `finalize-one` recovery path documented for authors | todo |

---

## Suggested tests (audit overlay)

| Test | File (proposed) | Assert |
|------|-----------------|--------|
| F10-C | `crates/deposit-relayer-daemon/tests/competing_submit.rs` | Second submit `AlreadyFinalized`; one mint |
| F10-F | `crates/deposit-relayer-daemon/tests/head_of_line_blocking.rs` | `failing_on(0)` blocks id 1 in daemon loop |
| F10-A | `deposit-prover` audit tests or `audit/spec/deposit-prover/` | Mutate L1 field → proof fails or PI mismatch |
| F10-B | extend `relayer.rs` tests | Corrupt `public_inputs.bin` → `AnRejected`, cursor not advanced |

Existing coverage: `proof_failure_is_recoverable`, `restart_resumes_from_persisted_state`, `log_index_mapping`, `MockAnSubmitter` nullifier, 32+ crate tests per `PROJECT_FACTS.md`.

---

## Questions for authors (off-chain register)

See `questions-cross-chain.md` § Off-chain / relayer (QC-OFF-01..05).

---

## Related BC/QC

| ID | Link to off-chain |
|----|-------------------|
| BC-AN-01 | Prover config `AN_DAPP_ID` — relayer-owned |
| BC-AN-02 | Witness `contractAddr` — prover + optional on-chain allowlist |
| QC-AN-09 | Bad proofs from any submitter (not only relayer) |
| QC-AN-J2 | ETH pause does not stop permissionless finalize on AN |
