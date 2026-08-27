# Cross-chain (ETH ↔ AN) — questions for authors

**Date:** 2026-08-13 (Phase G1 — `audit-new` / main `c9d5412`)  
**Audience:** Bridge integrators — **both** ETH and AN teams (joint disposition).  
**Scope:** Policy and binding alignment between `AckiNackiBridge` (Sepolia/mainnet) and `USDCBridge` (shellnet/production AN). Not a substitute for per-side registers.

**Update 2026-08-18 (ETH team / Pruvendo, vs `main` `a69ba36`):** the off-chain / prover
register (QC-OFF-\*/QC-PROV-\*) was re-checked against current `main`. Most items the
Stage-I doc listed as *open* are already implemented in code — the doc was written against
the stale `c9d5412`. Dispositions + code refs are filled in below; the one real code nit
(QC-PROV-01 stale comment) is fixed in this MR. Cross-chain policy rows (QC-AN-J\*, BC-AN-02)
carry ETH-side dispositions; final sign-off remains joint.

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
| Zero recipient | `InvalidRecipient` on withdraw (#16); deposit `InvalidAnAccount` | `ERR_ZERO_RECIPIENT` pre-ZK (QC-AN-10 closed Stage II) | QC-AN-J3 |
| Emergency pause | **No EVM pause** (#20) | No pause on `finalizeDeposit` | QC-AN-J2 — **ETH pause removed** |
| Workchain | Emitted in `Deposit` | `makeAddrStd(0, anAccount)` always WC 0 | QC-AN-J4 **closed** |
| Custody model | USDC in bridge / AAVE | ECC mint (currency #3) | QC-AN-J5 |

---

## QC register — cross-chain

| ID | ETH side | AN side | Auditor view | Author (Stage II) | Disposition |
|----|----------|---------|--------------|-------------------|-------------|
| QC-AN-J1 | `MAX_DEPOSIT_AMOUNT` = `uint64.max` (#20) | `uint64` cap on mint path | ETH cap raised; confirm joint min/max policy. | QC-AN-01 partial | **ack (ETH)** — ETH per-tx cap `uint64.max` ≤ AN `uint64` `fr[2]` bound (no overflow into mint path); AN also gates `amount>0` (`ERR_ZERO_AMOUNT`) + per-chain `getMintCap`. Joint effective range = `(0, uint64.max]`. Open only on final mainnet cap values. |
| QC-AN-J2 | **No EVM pause** (#20) | no pause on AN | ETH pause removed; AN finalize still permissionless for prior L1 events. | *No answer* | **ack (ETH)** — EVM pause removed by design (#20); AN `finalizeDeposit` permissionless. Safety rests on proof + nullifier + source allowlist, not on a pause switch. Asymmetry accepted + documented. |
| QC-AN-J3 | `InvalidAnAccount` | AN `require(anAccount != 0)` / empty withdraw recipient (`ERR_ZERO_RECIPIENT`) | Both sides reject zero. | Stage II: add AN require; **keep** ETH guard | **closed Stage II** — AN require landed; ETH `InvalidAnAccount` / `InvalidRecipient` stay. Do not drop ETH fail-fast. See `BRIDGE-AN-10`. |
| QC-AN-J4 | `anWorkchain` in `Deposit` event | payout `makeAddrStd(0, account)` | Workchain from L1 not enforced on AN. | «Не используется» | **closed (ack)** — workchain in event ignored on AN |
| QC-AN-J5 | USDC trust (QC-A1-2) | ECC mint, no USDC pause hook | Two-domain custody model. | *No answer* | **ack (ETH)** — two-domain custody by design: USDC/AAVE custody on ETH (owner cannot touch principal, QC-A1-2), ECC#3 mint on AN. No shared pause hook. Documented. |

---

## BC items with cross-chain impact

These are filed on the **AN** register but ETH team should ack binding assumptions:

| ID | Why cross-chain |
|----|-----------------|
| **BC-AN-01** | **Closed (2026-07-20):** `f.dappId=0` on AN — PI dapp limbs ignored; no multi-dapp double-mint. |
| **BC-AN-02** | L1 `contractAddr` in PI comes from receipt; ETH team confirms canonical bridge address(es) to pin on AN. **ETH answer (2026-08-18):** current Sepolia deposit bridge = `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`; mainnet address is TBD at deploy. The relayer binds `parsed.chain_id ↔ event.source_chain_id` and runs full `check_binds_to` (amount/contract/block/chainId, QC-OFF-07). The AN owner pins the canonical L1 bridge per chain in the USDCBridge source allowlist (fail-closed; a non-allowlisted source reverts, and the relayer surfaces it as `ERR_UNKNOWN_SOURCE` naming the setter — see `submitter.rs::describe_exit_code`). |

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

| ID | Auditor view | Ask author (joint: ETH ops + AN ops) | Status / disposition (ETH, 2026-08-18) |
|----|--------------|----------------------------------------|----------------------------------------|
| QC-OFF-01 | Daemon uses **strict sequential** `depositId` (`next_target = last_processed + 1`). One deposit that never finalizes (persistent `AnRejected`, bad config) **blocks all later depositIds**. | Intentional? Need skip / out-of-order finalize / alert on `attempts_since_progress`? | **closed** — `--skip-after-attempts` parks a stuck id and advances the cursor (`TickOutcome::Skipped` → `parked_deposit_ids`), and `--max-attempts-warn` emits an operator warning on `attempts_since_progress`. `relayer.rs::record_failure`; test `skips_stuck_deposit_after_max_attempts`. Default stays strict-sequential; skip is opt-in. |
| QC-OFF-02 | If the **only** relayer dies, credit stops until another operator runs `deposit-relayer finalize-one` or their own daemon. | Documented runbook? Is a backup relayer required for mainnet SLA? | **open (ops)** — safe by design: finalize is permissionless, so any operator resumes via `finalize-one` (no theft, only delay). Backup-relayer SLA is a mainnet deployment decision; runbook lives under `scripts/ursus/`. |
| QC-OFF-03 | Any party with ETH RPC + `deposit-prover` + AN access can finalize — relayer is not whitelisted. | Encourage competing relayers / watchdogs? Incentives for permissionless submitters? | **open (policy)** — permissionless-by-design is intentional (liveness). Incentive/watchdog scheme is a product decision, not a code gap. |
| QC-OFF-04 | Relayer holds **AN operator keys** and talks to GraphQL endpoint — MITM or DNS hijack risks wrong submit destination. | TLS pinning, allowlisted AN endpoints, key custody model? | **open (ops)** — endpoints are HTTPS (`shellnet.ackinacki.org`); TLS pinning / endpoint allowlist / key custody are operator-hardening items tracked for mainnet. Note: a wrong destination cannot mint (proof binds the recipient), only cause a failed submit. |
| QC-OFF-05 | Live `is_finalized` pre-check may be unavailable (`submitter.rs` — mock returns false until read API). Restarts may **re-prove** already-finalized ids (waste gas, not double-mint). | When will on-chain nullifier read ship? Is redundant prove acceptable? | **partial** — `AnInterfaceSubmitter::is_finalized` safely returns `false`; a replay is caught by the AN nullifier → `SubmitOutcome::AlreadyFinalized` (idempotent, no double-mint). Redundant prove is acceptable. True pre-check pending the AN nullifier read API (**partner dependency**). |
| QC-OFF-06 | **Partial (#15):** exit `0x1000` → `AlreadyFinalized`; mock path OK (`f10_competing_submit.rs`). Live Revert → `Rejected` (`f10_interface_reverted.rs`). | Map all «already finalized» reverts? Nullifier read API? **G3** | **closed** — full revert map in `submitter.rs`: `classify_reverted` / `classify_call_error` map exit `51` → `AlreadyFinalized`; `describe_exit_code` names `220/222/223/224/229` + the setter that fixes each. Tests `classify_exit_51_as_already_finalized`, `config_reverts_name_the_setter_that_fixes_them`. |
| QC-OFF-07 | ~~Partial local bind~~ **closed in main (#15):** full `check_binds_to` including amount/contract/block/chainId. | — | **closed** (pre-existing). |
| QC-OFF-08 | `state.json` not tagged with chain/bridge/dappId; no lock file — reuse across deploys skips deposits. | Deployment binding + single-writer lock? | **closed** — `state.json` carries `DeploymentIdentity { chain_id, bridge_address, dapp_id }` (`ensure_deployment`, `--force-state` override) + atomic write-temp→rename+fsync + advisory single-writer lock. `state.rs`. |
| QC-OFF-09 | CLI `AN_DAPP_ID` defaults to `"0"` without hex/EXPECTED preflight. | Startup validation vs on-chain dappId? | **closed** — `parse_and_validate_dapp_id` validates the dappId at startup (`an_config` + CLI). |
| QC-OFF-10 | `EthLogSource::fetch` rescans full `[from_block, safe_head]` every tick (10-block chunks) — O(head) RPC. | Incremental scan cursor? | **closed** — incremental cursor: `RelayerConfig::scan_cursor` + persisted `state.scanned_through_block`; `EthLogSource` resumes from the last safe head. `relayer.rs::persist_state`. |
| QC-OFF-11 | `--backoff-multiplier 0` → zero delay hot loop; `state.json` save without fsync; `Pending` after timeout → `Rejected`. | CLI validation + durable persist? | **closed** — `BackoffConfig::validate` rejects `initial==0` and `multiplier==0` (tests `backoff_rejects_zero_multiplier` / `_zero_initial`); durable persist via fsync (QC-OFF-08); `AnPending` retries without advancing the cursor (distinct from `Rejected`). |
| QC-OFF-12 | ~~Stale 7-arg ABI~~ **closed (#15):** `an-bridge-prover/python` ABI matches 2-arg finalize. | — | **closed** (pre-existing). |
| QC-OFF-13 | `TvmAckiNacki` status parsing — live path coverage. | Real receipt parsing from tvm-sdk? | **closed** — live `TvmAckiNacki` parses status via the modern `blockchain { transaction(hash:) }` API and caches the authoritative receipt from `process_message`; `acki-nacki-interface/src/tvm_client.rs::classify_tx_json`. Live-verified end-to-end (deposit finalised on shellnet, 2026-08). |
| QC-PROV-01 | `deposit-prover/prover.rs`: legacy verify may use old instance count; circuit uses **12** PI. | Update off-chain verify + generator to 12? | **closed** — both the verify path and the Solidity-verifier generator use `NUM_PUBLIC_INPUTS = 12` (`= DEPOSIT_NUM_PUBLIC_INPUTS`); the only stale `(11)` comment in `prover.rs` is corrected in this MR. |
| QC-PROV-02 | ~~`test_circuit_mock` panic (`left:4 right:3`)~~ **closed (pin, 2026-07-17):** axiom-eth `@1d61be0`. | — | **closed** (pre-existing). |
| QC-PROV-03 | ~~`max_key_byte_len: 4` vs reference `3`~~ **closed (2026-07-17, dev ack):** circuit uses `3`; regen `deposit_10proofs/` + audit `VK_BLOB` → `724687a4…` (overlay; compare with tvm-sdk after deploy). | — | **closed** (pre-existing). |
| QC-PROV-04 | MPT `key_bytes` padding slots beyond `key_byte_len` are **not** zero-constrained; PoC `deposit-prover/tests/padding_mutation_poc.rs` (`poc_padding_slot_garbage_accepted_*`). Active prefix corruption still fails (`poc_active_key_byte_corruption_rejected`). No false-deposit path found — witness malleability only. | Upstream axiom-eth hardening or accept? | **accepted (dev)** — witness malleability only, no false-deposit path (PoCs confirm active-prefix corruption is rejected). Upstream axiom-eth hardening is tracked but non-blocking for the bridge. |

**Linked BC:** BC-AN-01 **closed** (2026-07-20), BC-AN-02 (witness `contractAddr`). **Linked QC:** QC-AN-09 (griefing bad proofs from any submitter, not only relayer).
