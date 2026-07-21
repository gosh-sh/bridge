# Cross-chain (ETH ↔ AN) — questions for authors

**Date:** 2026-07-21 (author responses from Pruvendo QA Stage II — Bridge §)  
**Audience:** Bridge integrators — **both** ETH and AN teams (joint disposition).  
**Scope:** Policy and binding alignment between `AckiNackiBridge` (Sepolia/mainnet) and `USDCBridge` (shellnet/production AN). Not a substitute for per-side registers.

**Related:**

| Side | Register |
|------|----------|
| Ethereum only | `questions-eth.md`, `closeout-eth.md` |
| Acki Nacki only | `questions-an.md`, `an-audit-direction.md` |

---

## Classification policy

Items here are **QC** (design / policy questions). They may reference BC rows on one side (e.g. BC-AN-01) but require **joint** author agreement because both deployments are involved.

---

## Binding matrix (auditor view)

| Concern | Ethereum (`AckiNackiBridge`) | Acki Nacki (`USDCBridge`) | Gap? |
|---------|------------------------------|---------------------------|------|
| Deposit event fields | `depositId`, `sender`, `amount`, `anWorkchain`, `anAccount` | PI fr[0..7]; `f.dappId=0` on-chain (BC-AN-01 **closed**) | ~~BC-AN-01~~ resolved |
| L1 bridge identity | Fixed deploy address in proof via receipt | `contractAddr` in PI, no allowlist | **BC-AN-02** |
| Max amount | `MAX_DEPOSIT_AMOUNT` (100 USDC per tx) | `uint64` bound on `fr[2]` (QC-AN-01) | QC-AN-J1 |
| Zero recipient | `InvalidAnAccount` if `anAccount==0` | No pre-ZK guard (QC-AN-10) | QC-AN-J3 |
| Emergency pause | `pause()` blocks `deposit` | No pause on `finalizeDeposit` | QC-AN-J2 |
| Workchain | Emitted in `Deposit` | `makeAddrStd(0, anAccount)` always WC 0 | QC-AN-J4 **closed** |
| Custody model | USDC in bridge / AAVE | ECC mint (currency #3) | QC-AN-J5 |

---

## QC register — cross-chain

| ID | ETH side | AN side | Auditor view | Author (Stage II) | Disposition |
|----|----------|---------|--------------|-------------------|-------------|
| QC-AN-J1 | `MAX_DEPOSIT_AMOUNT` = 100 USDC | `uint64` cap on mint path (QC-AN-01) | Asymmetric caps if circuit > ETH limit. | *No answer* (see QC-AN-01 partial: raise ETH to u64) | **open** — in progress |
| QC-AN-J2 | `pause()` on user entrypoints | no pause on AN | ETH pause does not stop AN finalize for prior L1 events. | *No answer* | **open** — in progress |
| QC-AN-J3 | `InvalidAnAccount` | no pre-ZK `anAccount==0` check (QC-AN-10) | ETH stricter than AN on zero recipient. | *No answer* (QC-AN-10: add AN require, drop ETH guard) | **open** — in progress; dev chose opposite harmonization |
| QC-AN-J4 | `anWorkchain` in `Deposit` event | payout `makeAddrStd(0, account)` | Workchain from L1 not enforced on AN. | «Не используется» | **closed (ack)** — workchain in event ignored on AN |
| QC-AN-J5 | USDC trust (QC-A1-2) | ECC mint, no USDC pause hook | Two-domain custody model. | *No answer* | **open** — in progress |

---

## BC items with cross-chain impact

These are filed on the **AN** register but ETH team should ack binding assumptions:

| ID | Why cross-chain |
|----|-----------------|
| **BC-AN-01** | **Closed (2026-07-20):** `f.dappId=0` on AN — PI dapp limbs ignored; no multi-dapp double-mint. |
| **BC-AN-02** | L1 `contractAddr` in PI comes from receipt; ETH team confirms canonical bridge address(es) to pin on AN. *Stage II: no answer.* |

Details: `questions-an.md` § BC register.

---

## 11 public inputs alignment (deposit)

Auditor verified ETH `deposit-prover` layout matches AN `_parsePublicInputs`:

```
[0] depositId  [1] sender  [2] amount  [3] contractAddress
[4] dappIdHigh [5] dappIdLow [6] anAccountHigh [7] anAccountLow
[8] blockHashHigh [9] blockHashLow [10] promiseCommit
```

Recipient on AN: `(anAccountHigh << 128) | anAccountLow` — matches post-#2271 256-bit binding.

**Note:** PI limbs [4–5] still carry `dappId` in the circuit; on-chain `USDCBridge` pins `f.dappId=0` (BC-AN-01 **closed** 2026-07-20). Relayer `AN_DAPP_ID` no longer affects voucher identity on AN.

---

## AN → ETH direction (note for integrators)

Withdrawal: user burns ECC on AN via `initiateWithdrawal` → off-chain Circuit 4 proof → `AckiNackiBridge.withdrawByProof` on ETH. That path is audited on **ETH** (`questions-eth.md` § A3), not in the AN contract QC table above.

---

## Off-chain / relayer (deposit path) — QC register

Items for **deposit-relayer-daemon** + **deposit-prover** + operator playbook. Safety baseline: `finalizeDeposit` on AN is **permissionless** — relayer outage must not enable theft, only delay credit. Detail: `manual-audit/F10-offchain-deposit-pipeline.md`.

| ID | Auditor view | Ask author (joint: ETH ops + AN ops) |
|----|--------------|----------------------------------------|
| QC-OFF-01 | Daemon uses **strict sequential** `depositId` (`next_target = last_processed + 1`). One deposit that never finalizes (persistent `AnRejected`, bad config) **blocks all later depositIds**. | Intentional? Need skip / out-of-order finalize / alert on `attempts_since_progress`? |
| QC-OFF-02 | If the **only** relayer dies, credit stops until another operator runs `deposit-relayer finalize-one` or their own daemon. | Documented runbook? Is a backup relayer required for mainnet SLA? |
| QC-OFF-03 | Any party with ETH RPC + `deposit-prover` + AN access can finalize — relayer is not whitelisted. | Encourage competing relayers / watchdogs? Incentives for permissionless submitters? |
| QC-OFF-04 | Relayer holds **AN operator keys** and talks to GraphQL endpoint — MITM or DNS hijack risks wrong submit destination. | TLS pinning, allowlisted AN endpoints, key custody model? |
| QC-OFF-05 | Live `is_finalized` pre-check may be unavailable (`submitter.rs` — mock returns false until read API). Restarts may **re-prove** already-finalized ids (waste gas, not double-mint). | When will on-chain nullifier read ship? Is redundant prove acceptable? |
| QC-OFF-06 | **Live submitter never returns `AlreadyFinalized`:** duplicate `finalizeDeposit` → `Reverted` → `Rejected`; with sequential cursor, **competing relayer permanently stalls** our daemon on that id (mock hides this). | Map revert reason to `AlreadyFinalized`? Read API before mainnet? |
| QC-OFF-07 | `check_binds_to` compares only depositId/sender/anAccount — not amount/contract/block. PoC test: `binding_check_ignores_amount_and_contract_poc`. | Extend local bind check or rely on AN only? |
| QC-OFF-08 | `state.json` not tagged with chain/bridge/dappId; no lock file — reuse across deploys skips deposits. | Deployment binding + single-writer lock? |
| QC-OFF-09 | CLI `AN_DAPP_ID` defaults to `"0"` without hex/EXPECTED preflight. | Startup validation vs on-chain dappId? |
| QC-OFF-10 | `EthLogSource::fetch` rescans full `[from_block, safe_head]` every tick (10-block chunks) — O(head) RPC. | Incremental scan cursor? |
| QC-OFF-11 | `--backoff-multiplier 0` → zero delay hot loop; `state.json` save without fsync; `Pending` after timeout → `Rejected`. | CLI validation + durable persist? |
| QC-OFF-12 | Stale 7-arg `finalizeDeposit` ABI in `an-bridge-prover/python/` vs 2-arg deployed ABI. | Pin/canonical ABI path for operators? |
| QC-OFF-13 | `TvmAckiNacki` status always `Confirmed` — `Reverted`/`Pending` branches in submitter untested on live path. | Real receipt parsing from tvm-sdk? |
| QC-PROV-01 | `deposit-prover/prover.rs`: `verify_proof` / Solidity generator use **7** instances; `circuit_v2` uses **11**. | Update off-chain verify + generator to 11? |
| QC-PROV-02 | ~~`test_circuit_mock` panic (`left:4 right:3`)~~ **closed (pin, 2026-07-17):** axiom-eth `@1d61be0`. | — |
| QC-PROV-03 | ~~`max_key_byte_len: 4` vs reference `3`~~ **closed (2026-07-17, dev ack):** circuit uses `3`; regen `deposit_10proofs/` + audit `VK_BLOB` → `724687a4…` (overlay; compare with tvm-sdk after deploy). | — |
| QC-PROV-04 | MPT `key_bytes` padding slots beyond `key_byte_len` are **not** zero-constrained; PoC `deposit-prover/tests/padding_mutation_poc.rs` (`poc_padding_slot_garbage_accepted_*`). Active prefix corruption still fails (`poc_active_key_byte_corruption_rejected`). No false-deposit path found — witness malleability only. | Upstream axiom-eth hardening or accept? |

**Linked BC:** BC-AN-01 **closed** (2026-07-20), BC-AN-02 (witness `contractAddr`). **Linked QC:** QC-AN-09 (griefing bad proofs from any submitter, not only relayer).
