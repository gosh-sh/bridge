# Phase 2 — ETH deposit milestone digest (mock / CI)

**Дата:** 2026-08-14  
**Ветка:** `audit-new`  
**Scope:** TD-36–68 (P2 cluster) + META/RELAYER CI gates TD-42 / TD-43 / TD-49 / TD-53 / TD-68  
**Не в scope:** live AN deploy, shellnet E2E, новые PoC

**Связанные артефакты:** `test-directions-deposit-eth-catalog.md` §3/§6, `check_deposit_audit_gates.sh`, `baseline-deposit-eth.md`, `phase-g-status.md`, `delta-deposit-eth.md` § Phase 2 pass + § contracts/bridge migration.

**Post-Phase-2 (2026-08-14):** upstream sync → **`acki-nacki@contracts/bridge`** (`eccUSDCBridge`); AN integration **74 passed**; handoff v2 + BC-AN-01 green — see `delta-deposit-eth.md` § contracts/bridge migration.

---

## 1. Milestone verdict

| Track | Статус |
|-------|--------|
| Mock / CI evidence (TD-36–68) | **closed** — partial PoC + notes на все P2 направления |
| META / RELAYER CI gates (TD-42/43/49/53/68) | **wired** в `check_deposit_audit_gates.sh` |
| Live ops (TD-04 deploy, E-AN-01) | **open** — не BC, owner/ops |

**P0 carry-over:** TD-04 live AN `.tvc` redeploy + owner seed (overlay patch-shaped OK in repo).

---

## 2. Pipeline (mock/CI vs ops)

```
  L1 deposit()          deposit-prover           deposit-relayer           AN USDCBridge
       │                      │                         │                      │
       │  overlay TD-04       │  12 PI circuit          │  scan → prove →      │  finalizeDeposit
       │  (repo OK)           │  TD-42 VkBlob pin       │  submit (mock)       │  (live deploy ops)
       ▼                      ▼                         ▼                      ▼
  Foundry overlay      MockProver + SHPLONK      td_53 mock recovery      TD-04 .tvc redeploy
  ~69 Deposit* tests   TD-43 opcode triple       TD-65 G3 live deferred    owner allowlist seed
                       TD-49 mutation 13/13      shellnet E-AN-01 deferred
```

---

## 3. Rollup table (TD-36–68)

| ID | Тема | Verdict | Key artifact | Open gap |
|----|------|---------|--------------|----------|
| TD-36 | Cross cap ETH vs AN mint | partial QC | `DepositCrossCapPolicy.t.sol`, `td_36_mint_cap_exceeded.rs` | whale / product policy |
| TD-37 | Competing grief / finalize-one | partial QC | `td_37_competing_grief_finalize_one.rs` | competing mock only |
| TD-38 | Stale finalize ABI / PI reorder | partial QC | `td_38_abi_pi_reorder.rs` | ABI drift watch |
| TD-39 | Incremental scan vs O(head) | partial INV | `td_39_incremental_scan_cost.rs` | tail block advance |
| TD-40 | CLI backoff / Pending / fsync | partial QC | `td_40_cli_backoff_pending.rs` | ops policy |
| TD-41 | Mainnet chainId=1 rejected | partial QC | `td_41_*`, `td_41_no_mainnet_chain.sh` | by design |
| TD-42 | VkBlob / SRS pin | partial META **CI wired** | `check_vk_srs_pin.sh`, `td_42_vk_srs_pin.rs` | live `.tvc` redeploy ops |
| TD-43 | MockProver ≠ SHPLONK | partial META **CI smoke** | `check_mock_vs_shplonk_smoke.sh`, `td_43_*` | live shellnet opcode ops |
| TD-44 | timestamp / anWorkchain not in PI | partial INV | `DepositEventPiGap.t.sol`, `td_44_*` | docs QC |
| TD-45 | Returnless ERC20 | covered | unit overlay | — |
| TD-46 | sender = msg.sender | covered | unit overlay | — |
| TD-47 | Reentrant deposit blocked | covered | unit overlay | — |
| TD-48 | DEP-6 isolation | covered | unit overlay | — |
| TD-49 | Mutation kill score | partial META **CI smoke** | `check_mutation_kill_smoke.sh`, KILLED 13/13 | 3 QC survivors documented |
| TD-50 | Field modulus / BN254 aliasing | partial META | `td_50_*` | partial pin |
| TD-51 | Byzantine RPC | partial META | `td_51_byzantine_rpc.rs` | stale chainId cache QC |
| TD-52 | Prover subprocess zombie | partial META | `td_52_prover_subprocess_timeout.rs` | ops orphan monitor |
| TD-53 | E2E runbook recovery | partial META **CI smoke** | `td_53_e2e_runbook_recovery.rs`, `td_53_dry_run_recovery_smoke.sh` | live E-AN-01 ops |
| TD-54 | RLP non-canonical fuzz | partial META | `td_54_rlp_noncanonical_fuzz.rs` | TD-64 corpus |
| TD-55 | Storage diff on failed deposit | partial META | `DepositStorageDiff.t.sol` | fork TD-56 |
| TD-56 | USDC mainnet fork pause/blacklist | partial META | `DepositUsdcMainnetFork.t.sol` | live RPC opt-in |
| TD-57 | dappId hex fuzz | partial META | `td_57_dapp_id_hex_fuzz.rs` | — |
| TD-58 | Pause asymmetry docs | partial QC | `DepositPauseAsymmetry.t.sol` | TD-56 fork |
| TD-59 | Event signature regression | partial META | `DepositEventSignature.t.sol`, `td_59_*` | — |
| TD-60 | Owner treasury guard | partial INV | `DepositOwnerTreasuryGuard.t.sol` | — |
| TD-61 | Allowance dust | partial INV | `DepositAllowanceDust.t.sol` | — |
| TD-62 | depositId gap vs dense counter | partial INV | `DepositCounterDense.t.sol`, `td_62_*` | — |
| TD-63 | ethereum_fetcher parse edge | partial META | `td_63_*` | — |
| TD-64 | Seven-chain header corpus | partial META | `td_64_seven_chain_corpus.rs`, fixtures | re-pin on upgrade |
| TD-65 | G3 Revert / is_finalized loop | partial META **CI smoke** | `td_65_g3_revert_loop.rs`, `check_td65_g3_smoke.sh` | shellnet live ops |
| TD-66 | AAVE interleaving | partial META | `DepositAaveInterleaving.t.sol` | — |
| TD-67 | Competing provers dappId | partial META | `td_67_competing_provers_dapp_id.rs` | — |
| TD-68 | PI count 12 docs gate | partial META **CI wired** | `check_pi_count_docs.sh`, `td_68_pi_count_gate.rs` | partial layout pin |

---

## 4. META CI gate cluster (closed for mock/CI)

| TD | Gate / smoke | Order in `check_deposit_audit_gates.sh` |
|----|--------------|----------------------------------------|
| TD-68 | `check_pi_count_docs.sh` | 1 |
| TD-42 | `check_vk_srs_pin.sh` | 2 (VkBlob pin) |
| TD-43 | `check_mock_vs_shplonk_smoke.sh` | 3 (~6 min SHPLONK) |
| TD-49 | `check_mutation_kill_smoke.sh` | 4 (~3 min circuit kills) |
| TD-53 | `td_53_dry_run_recovery_smoke.sh` | 5 (mock E2E recovery) |
| TD-65 | `check_td65_g3_smoke.sh` | 6 (G3 mock classification) |
| TD-04 | `check_an_overlay_patch_matrix.sh` | 7 (overlay patch-shaped) |

Also: `ci_an_audit.sh` runs TD-42 + TD-04 after AN contract build.

---

## 5. Suite deltas (2026-08-14)

| Suite | Count | Command |
|-------|-------|---------|
| `deposit-relayer-daemon` | **~264** | `cd crates/deposit-relayer-daemon && cargo test --locked` |
| `deposit-prover` (lib + integration) | **~170+** | `cd deposit-prover && cargo test` |
| ETH overlay Deposit* (audit profile) | **~69** | `cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test --match-contract Deposit` |
| AN audit unit (overlay) | **42** | `AN_AUDIT_INTEGRATION=0 bash scripts/ci_an_audit.sh` |

Phase G baseline (2026-08-13): relayer 68 → expanded TD suite; overlay 69 Deposit tests stable.

---

## 6. Ops blockers (не BC)

| ID | Blocker | Owner |
|----|---------|-------|
| TD-04 | Live AN `.tvc` redeploy + owner seed — handoff v2 **`eccUSDCBridge`** (`setTrustedL1Bridge`); `docs/operations/td-04-an-deploy-owner-handoff.md` | ops / partner |
| TD-53 | Shellnet E-AN-01: live daemon dry-run + two-relayer race | ops |
| TD-65 | Live G3 `is_finalized` / Revert loop on shellnet | ops |

Mock paths: TD-53 `td_53_dry_run_recovery_smoke.sh`; TD-65 `check_td65_g3_smoke.sh`.

---

## 7. Closed for mock/CI (explicit)

1. **TD-04 handoff v2** — `eccUSDCBridge@contracts/bridge`, `setTrustedL1Bridge` seed; `check_td04_deploy_readiness.sh`.
2. **TD-42 VkBlob** — fixture ↔ overlay pin, downgrade probes, CI gate.
3. **TD-43 asymmetry** — doc in PROJECT_FACTS; opcode triple smoke (MockProver ≠ SHPLONK).
4. **TD-49 mutation** — KILLED 13/13 pinned; 3 QC survivors (dappId / promiseCommit / anWorkchain).
5. **TD-45–48** — covered regression anchors (no new PoC).

---

## 8. Phase 3 outline

1. **Ops track:** TD-04 — owner executes `docs/operations/td-04-an-deploy-owner-handoff.md` (deploy + seed).
2. **Live E2E:** TD-53 shellnet dry-run per `td-53-shellnet-e2e-runbook-notes.md` (E-AN-01); mock smoke wired in `check_deposit_audit_gates.sh`.
3. **Maintenance:** periodic corpus re-pin (TD-64 seven-chain headers); VkBlob rotation procedure (TD-42 → embed → overlay build → redeploy).
4. **Regression hygiene:** keep `check_deposit_audit_gates.sh` green on MR; TD-65 shellnet when G3 API available.
5. **Deposit verify tiers 1–3 backlog:** `deposit-verify-three-tier-backlog.md` (Mock → standalone SHPLONK → tvm-debugger SDK; shellnet = tier 4 defer).
5. **Author ack:** remaining QC rows in `baseline-deposit-eth.md` (cross-cap, USDC trust, HOL policy).

---

## 9. Commands

    bash scripts/check_deposit_audit_gates.sh
    make audit-deposit-relayer-test
    make pre-push-audit
