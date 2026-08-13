# Cross-chain (ETH ↔ AN) — questions for authors

**Date:** 2026-08-13 (Phase G1 — `audit-new` / main `c9d5412`)  
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
| Max amount | `MAX_DEPOSIT_AMOUNT` = `uint64.max` per tx (#20) | `uint64` bound on `fr[2]` (QC-AN-01) | QC-AN-J1 — **ETH side raised** |
| Zero recipient | `InvalidRecipient` on withdraw (#16); deposit `InvalidAnAccount` | No pre-ZK guard (QC-AN-10) | QC-AN-J3 |
| Emergency pause | **No EVM pause** (#20) | No pause on `finalizeDeposit` | QC-AN-J2 — **ETH pause removed** |
| Workchain | Emitted in `Deposit` | `makeAddrStd(0, anAccount)` always WC 0 | QC-AN-J4 **closed** |
| Custody model | USDC in bridge / AAVE | ECC mint (currency #3) | QC-AN-J5 |

---

## QC register — cross-chain

| ID | ETH side | AN side | Auditor view | Author (Stage II) | Disposition |
|----|----------|---------|--------------|-------------------|-------------|
| QC-AN-J1 | `MAX_DEPOSIT_AMOUNT` = `uint64.max` (#20) | `uint64` cap on mint path | ETH cap raised; confirm joint min/max policy. | QC-AN-01 partial | **open** — confirm deployment caps |
| QC-AN-J2 | **No EVM pause** (#20) | no pause on AN | ETH pause removed; AN finalize still permissionless for prior L1 events. | *No answer* | **partial** — asymmetry reduced |
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

## 12 public inputs alignment (deposit)

ETH `deposit-prover` + relayer (`NUM_PUBLIC_INPUTS = 12`):

```
[0] depositId  [1] sender  [2] amount  [3] contractAddress
[4] chainId    [5] dappIdHigh [6] dappIdLow
[7] anAccountHigh [8] anAccountLow
[9] blockHashHigh [10] blockHashLow [11] promiseCommit
```

`chainId` added on main (#20). Relayer binds `parsed.chain_id` to `event.source_chain_id` (#15).

AN opcode layout: verify against synced `USDCBridge` after `sync_an_contracts.sh` — overlay assumes 12-instance operand blob.

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
| QC-OFF-06 | **Partial (#15):** exit `0x1000` → `AlreadyFinalized`; mock path OK (`f10_competing_submit.rs`). Live Revert → `Rejected` (`f10_interface_reverted.rs`). | Map all «already finalized» reverts? Nullifier read API? **G3** |
| QC-OFF-07 | ~~Partial local bind~~ **closed in main (#15):** full `check_binds_to` including amount/contract/block/chainId. | — |
| QC-OFF-08 | `state.json` not tagged with chain/bridge/dappId; no lock file — reuse across deploys skips deposits. | Deployment binding + single-writer lock? |
| QC-OFF-09 | CLI `AN_DAPP_ID` defaults to `"0"` without hex/EXPECTED preflight. | Startup validation vs on-chain dappId? |
| QC-OFF-10 | `EthLogSource::fetch` rescans full `[from_block, safe_head]` every tick (10-block chunks) — O(head) RPC. | Incremental scan cursor? |
| QC-OFF-11 | `--backoff-multiplier 0` → zero delay hot loop; `state.json` save without fsync; `Pending` after timeout → `Rejected`. | CLI validation + durable persist? |
| QC-OFF-12 | ~~Stale 7-arg ABI~~ **closed (#15):** `an-bridge-prover/python` ABI matches 2-arg finalize. | — |
| QC-OFF-13 | `TvmAckiNacki` status parsing — live path coverage. | Real receipt parsing from tvm-sdk? |
| QC-PROV-01 | `deposit-prover/prover.rs`: legacy verify may use old instance count; circuit uses **12** PI. | Update off-chain verify + generator to 12? |
| QC-PROV-02 | ~~`test_circuit_mock` panic (`left:4 right:3`)~~ **closed (pin, 2026-07-17):** axiom-eth `@1d61be0`. | — |
| QC-PROV-03 | ~~`max_key_byte_len: 4` vs reference `3`~~ **closed (2026-07-17, dev ack):** circuit uses `3`; regen `deposit_10proofs/` + audit `VK_BLOB` → `724687a4…` (overlay; compare with tvm-sdk after deploy). | — |
| QC-PROV-04 | MPT `key_bytes` padding slots beyond `key_byte_len` are **not** zero-constrained; PoC `deposit-prover/tests/padding_mutation_poc.rs` (`poc_padding_slot_garbage_accepted_*`). Active prefix corruption still fails (`poc_active_key_byte_corruption_rejected`). No false-deposit path found — witness malleability only. | Upstream axiom-eth hardening or accept? |

**Linked BC:** BC-AN-01 **closed** (2026-07-20), BC-AN-02 (witness `contractAddr`). **Linked QC:** QC-AN-09 (griefing bad proofs from any submitter, not only relayer).
