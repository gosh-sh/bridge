# AN contracts audit — closeout (`audit-new`)

**Date:** 2026-08-13 (Phase G1 sync on `audit-new`)  
**Scope:** `USDCBridge` / `DepositVoucher` on Acki Nacki. ZK opcode internals out of scope.  
**Code base:** synced AN contracts + ETH `main`; overlay pytest **74 passed**.

**Registers:** `questions-an.md`, `questions-cross-chain.md`, `phase-g-status.md`.

---

## Classification policy

| Label | PoC | Meaning |
|-------|-----|---------|
| **OK** | After author confirms | Matches intent |
| **QC** | Required | Reproduced; need intent |
| **BC** | Required | Bug candidate under stated assumptions |

**This pass:** BC = **1 open** (BC-AN-02). BC-AN-01 **closed**. F10: several items **closed/partial in main** (see below).

---

## Phase completion

| Phase | Status |
|-------|--------|
| F0–F8 | ✅ |
| F9 ETH fuzz | ✅ |
| F10 relayer + prover | ✅ **68** cargo tests on `audit-new` |
| Merge main → overlay | ✅ `audit-new` |
| `make audit-an-test` | ✅ **74** pytest |
| `make pre-push-an` | ✅ **42** unit |
| E-AN-01 shellnet E2E | **deferred** (G5) |

---

## Test gates

```bash
./scripts/sync_an_contracts.sh
make audit-an-test              # 74 pytest
make pre-push-an                # 42 unit
make audit-deposit-relayer-test # 68 cargo (F10)
make audit-solidity-test        # 56 overlay
```

---

## BC register

| ID | Severity | PoC | Status |
|----|----------|-----|--------|
| BC-AN-01 | High | `test_bc_an_01_dapp_id_double_mint.py` | **closed** (`f.dappId=0`, 2026-07-20) |
| BC-AN-02 | Medium | `test_bc_an_02_no_l1_bridge_allowlist_pre_zk` | **open** — **G2 focus** |

---

## QC register — AN-only

| ID | Disposition |
|----|-------------|
| QC-AN-01 | **partial** — ETH cap now `uint64.max` on main (#20); min deposit TBD |
| QC-AN-02 | **open** |
| QC-AN-03 | **closed (ack)** |
| QC-AN-04 | **open** — ETH pause removed (#20); asymmetry changed |
| QC-AN-05 | **partial ack** |
| QC-AN-06 | **partial ack** |
| QC-AN-07 | **closed (ack)** |
| QC-AN-08 | **closed (test)** |
| QC-AN-09 | **open** |
| QC-AN-10 | **partial ack** — WD-Q2 `InvalidRecipient` on ETH (#16) |

### Cross-chain joint

| ID | Disposition |
|----|-------------|
| QC-AN-J1 | **open** — ETH cap raised; confirm joint policy |
| QC-AN-J2 | **partial** — ETH pause removed; AN finalize still permissionless |
| QC-AN-J3 | **open** — linked QC-AN-10 |
| QC-AN-J4 | **closed (ack)** |
| QC-AN-J5 | **open** |

---

## F10 off-chain — resolved in main (overlay updated)

| ID | Main (#15 / #18 / #20) | Overlay status |
|----|--------------------------|----------------|
| QC-OFF-07 | Full `check_binds_to` (amount, contract, block, chainId) | test rewritten |
| QC-OFF-11 | `BackoffConfig::validate()` rejects multiplier 0 | proptest updated |
| QC-OFF-12 | Python ABI 2-arg finalize | test expects match |
| QC-PROV-03 | `max_key_byte_len=3` (#18) | **closed** |
| QC-PROV-02 | axiom-eth pin | **closed** |

### F10 — still open

| ID | Priority | Note |
|----|----------|------|
| QC-OFF-06 | **P0** | partial: `AlreadyFinalized` for some exits; live Revert path — **G3** |
| QC-OFF-01 | P1 | `skip_after_attempts` in CLI — policy? |
| QC-OFF-02..05, 08–10, 13 | ops | runbook, state binding, scan cursor |
| QC-PROV-01 | dev tooling | 12 PI in circuit vs legacy verify helper |
| QC-PROV-04 | low | padding malleability PoC |

Detail: `HANDOFF-f10-prover-relayer-ru.txt`, `questions-cross-chain.md` § QC-OFF.

---

## Signoff checklist

- [x] F0–F10 overlay gates green on `audit-new`
- [x] BC-AN-01 closed + regression tests
- [x] Registers synced with main merge (G1)
- [ ] BC-AN-02 disposition (G2)
- [ ] QC-OFF-06 live path (G3)
- [ ] Author ack remaining rows (G4)
- [ ] E-AN-01 shellnet (G5)

**Auditor note:** BC/QC rows are not deleted without author ack. See `phase-g-status.md` for next steps.
