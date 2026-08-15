# Каталог направлений тестирования — ETH deposit (полный, дедуплицированный)

**Дата:** 2026-08-14  
**Ветка:** `audit-new`  
**Scope:** L1 `deposit()` → `Deposit` event → `deposit-prover` → `deposit-relayer-daemon` → ETH-сторона PI/binding. Withdraw / `verifyBlock` — вне скоупа (кроме изоляции treasury).

**Источники (ручной аудит, одинаковый промпт):**

| Агент | task_id |
|-------|---------|
| Opus 5 | `58f296d9-c697-4600-8079-a447eb098449` |
| Grok 4.5 | `9a04f1cc-76f7-441d-8c06-49d414af4a9f` |
| GPT 5.6 | `b33f627e-ce53-4e43-8da7-87e12ebffa1c` |

**Связанные артефакты:** `baseline-deposit-eth.md` (open QC), `baseline-deposit-eth-locked.md` (закрыто тестами), `delta-deposit-eth.md` (pass 2026-08-13), `phase-2-deposit-eth-milestone.md` (Phase 2 rollup).

---

## 1. Как читать документ

1. **Ничего не потеряно:** каждая идея из трёх отчётов сопоставлена с каноническим ID `TD-XX` (таблица §4).
2. **Дедупликация:** близкие идеи слиты; исходные ID агентов — в §5.
3. **Классификация:** BC / QC / OK / invariant gap / meta — до PoC BC только «кандидат».
4. **Приоритет:** P0 → P3; фазы имплементации PoC — §6.
5. **PoC:** колонка `PoC` = `pending` | `partial` | `covered` (не писать PoC на `covered`).

**Главный вывод (все три):** L1 `deposit()` покрыт (~69 overlay tests). ROI — граница **event → 12 PI → prover → relayer → AN** и **production RPC/persistence**.

---

## 2. Легенда

| Поле | Значение |
|------|----------|
| **BC** | Bug candidate — нужен воспроизводящий PoC |
| **QC** | Нужен intent автора / ops policy |
| **OK** | Ожидаемое fail-closed или design — PoC документирует |
| **INV** | Invariant gap — свойство заявлено, тест слабый |
| **META** | Качество тест-сьюта / CI gate |
| **Слой** | `L1` · `CIRCUIT` · `RELAYER` · `XCHAIN` · `OPS` · `META` |

---

## 3. Мастер-реестр (все направления)

| ID | Приоритет | Класс | Слой | Направление | PoC | Gap |
|----|-----------|-------|------|-------------|-----|-----|
| TD-01 | P0 | BC | CIRCUIT | Mutation matrix всех 12 PI + endian/high/low | **partial** `td_01_pi_bind_mutation.rs` | partial layout pin |
| TD-02 | P0 | BC | XCHAIN | PI layout drift: prover 12 PI ↔ AN parser ↔ VK | **partial** `td_02_pi_layout_drift.rs` | prover-only pin |
| TD-03 | P0 | OK | XCHAIN | Forged block: circuit pass, AN anchor reject 224 | **partial** `td_03_forged_block.rs` + `td_03_forged_block_anchor.rs` | — |
| TD-04 | P0 | QC | XCHAIN | AN security patch series — overlay + handoff; live deploy ops | **partial** overlay + `check_td04_deploy_readiness.sh` + owner handoff doc | live AN deploy + owner seed (ops) |
| TD-05 | P0 | OK | XCHAIN | `dappId` injection / double mint namespace | **partial** `td_05_dapp_id_injection.rs` | — |
| TD-06 | P0 | BC | RELAYER | `scan_cursor` = `safe_head` → пропуск deposit в том же окне | **fixed** `td_06_scan_cursor.rs` + `source.rs` | было 0 тестов |
| TD-07 | P0 | OK | RELAYER | Scan cursor on proof failure: no skip, restart rediscover | **partial** `td_07_scan_cursor_proof_failure.rs` | TD-39 tail block advance |
| TD-08 | P0 | OK | RELAYER | Dual-RPC / chainId split-brain (source vs prover vs event) | **partial** `td_08_dual_rpc_chain_id.rs` | live dual-RPC deploy ops |
| TD-09 | P0 | OK | CIRCUIT | MPT differential + systematic witness mutation | **partial** `td_09_mpt_mutation.rs` | — (TD-20 padding QC) |
| TD-10 | P0 | OK | CIRCUIT | Multi-log receipt / wrong `log_index` в том же чеке | **partial** `td_10_log_index.rs` | 4+ log L1 covered in TD-11 Foundry |
| TD-11 | P0 | QC | CIRCUIT | Circuit capacity: MAX_LOG_NUM=3, DATA=128B, MPT depth, header bytes | **partial** `td_11_capacity_bounds.rs` + `DepositMultiLogCapacity.t.sol` | — (TD-13 matrix) |
| TD-12 | P0 | QC | CIRCUIT | EIP-1559-only: L1 принимает legacy/7702, prover reject | **partial** `td_12_eip1559_only.rs` + `DepositEip1559Only.t.sol` | — (TD-13 matrix) |
| TD-13 | P0 | OK | CIRCUIT+L1 | Universal finalizability: supported envelope доказуем | **partial** `td_13_universal_finalizability.rs` + `td-13-finalizability-matrix.md` | forged block (TD-03) |
| TD-14 | P0 | OK | L1 | TR-1 solvency: deposit + AAVE + emergency + donation interleaved | **partial** `DepositTR1Interleaved.t.sol` + `TreasuryHandler` | FoT invariant (TD-23) |
| TD-15 | P0 | OK | XCHAIN | depositId collision: DEP-N-5 identity vs legacy swallow | **partial** `td_15_deposit_id_collision.rs` + notes | live AN patch (TD-04) |
| TD-16 | P0 | OK | OPS | Sepolia ∉ prod allowlist; preflight gate | **partial** `td_16_prod_no_sepolia.sh` + `td_16_sepolia_prod_allowlist.rs` | live AN ops (TD-04) |
| TD-17 | P0 | OK | RELAYER | Reorg / stale blockHash / L2 finality vs fixed confirmations | **partial** `td_17_reorg_finality.rs` | live fork RPC |
| TD-18 | P0 | OK | RELAYER | `state.json` I/O fail + `--force-state` cursor inheritance | **partial** `td_18_state_json_durability.rs` + `reset_for_new_deployment` | fsync kill live |
| TD-19 | P1 | OK | CIRCUIT | Receipt root / header hash decouple; stale blockHash vs receiptsRoot | **partial** `td_19_root_header_decouple.rs` | forged header TD-03 |
| TD-20 | P1 | QC | CIRCUIT | MPT padding witness malleability (QC-PROV-04) — PI byte-identical | **partial** `td_20_padding_malleability.rs` | upstream axiom-eth |
| TD-21 | P1 | OK | CIRCUIT | `promiseCommit` slot 11 — mutation reject + layout pin | **partial** `td_21_promise_commit.rs` | relayer does not bind promiseCommit |
| TD-22 | P1 | OK | CIRCUIT | Tx trie vs receipt trie `transaction_index` coupling | **partial** `td_22_tx_receipt_index_coupling.rs` | multi-tx block fixture |
| TD-23 | P1 | QC | L1 | Fee-on-transfer / USDC upgrade в stateful invariant TR-1 | **partial** `DepositFoTInvariant.t.sol` + `FoTTreasuryHandler` | USDC proxy ops |
| TD-24 | P1 | QC | L1 | USDC blacklist / pause / rebase mid-campaign | **partial** `DepositUsdcBlacklistPause.t.sol` + `BlacklistableERC20` | TD-56 fork |
| TD-25 | P1 | OK | L1 | Cross-function reentrancy (token → все mutating entrypoints) | **partial** `DepositCrossFnReentrancy.t.sol` + `CrossFnReentrantERC20` | deposit-only closed |
| TD-26 | P1 | QC | RELAYER | HOL blocking + `record_skip` / parked IDs recovery | **partial** `td_26_hol_skip_policy.rs` | slow deposit parked (TD-27) |
| TD-27 | P1 | QC | RELAYER | «Не подтверждён» vs «отравлен» — один `record_failure` | **partial** `td_27_slow_vs_poisoned.rs` | concurrent race (TD-28) |
| TD-28 | P1 | OK | RELAYER | Concurrent real submitters / lost receipt / double mint race | **partial** `td_28_concurrent_submit_race.rs` | live tvm-sdk race |
| TD-29 | P1 | QC | RELAYER | RPC fault: `unwrap_or_default` log_index / block_hash | **partial** `td_29_rpc_default_log_index.rs` | — |
| TD-30 | P1 | OK | RELAYER | Prover RPC ≠ source RPC — preflight на старте daemon | **partial** `td_30_dual_rpc_preflight.rs` + `rpc_preflight.rs` | live tvm-sdk |
| TD-31 | P1 | QC | L1+XCHAIN | `anWorkchain` не в PI; AN credits workchain 0 | **partial** `DepositAnWorkchainUnbound.t.sol` + `td_31_*` | TD-44 timestamp |
| TD-32 | P1 | QC | CIRCUIT | Per-L2 header shape matrix (717B, 16–21 fields) | **partial** `td_32_l2_header_matrix.rs` + `fixtures/headers/` (5 shapes) | TD-65 G3 live |
| TD-33 | P1 | QC | OPS | Dust / proof-spam DoS; aggregate cap bypass; relayer cost | **partial** `DepositDustSpam.t.sol` + `td_33_dust_spam_hol.rs` | TD-36 cross cap |
| TD-34 | P1 | QC | L1 | No refund path on L1 — stuck deposit документировать | **partial** `DepositNoL1Refund.t.sol` | TD-35 donation |
| TD-35 | P1 | QC | L1 | Donation → AAVE → skim vs treasury ledger | **partial** `DepositDonationAavePath.t.sol` | TD-66 AAVE interleave |
| TD-36 | P2 | QC | L1+XCHAIN | Cap policy ETH `uint64.max` vs AN mint cap / min deposit | **partial** `DepositCrossCapPolicy.t.sol` + `td_36_mint_cap_exceeded.rs` | whale test |
| TD-37 | P2 | QC | RELAYER | Competing submitters grief / unlock via peer finalize-one | **partial** `td_37_competing_grief_finalize_one.rs` | competing mock |
| TD-38 | P2 | QC | RELAYER | Stale finalize ABI / PI byte reorder in encode | **partial** `td_38_abi_pi_reorder.rs` | ABI inverted |
| TD-39 | P2 | INV | RELAYER | Incremental scan vs O(head) rescan (QC-OFF-10) | **partial** `td_39_incremental_scan_cost.rs` | tail block advance |
| TD-40 | P2 | QC | RELAYER | CLI backoff=0, Pending timeout, fsync policy | **partial** `td_40_cli_backoff_pending.rs` | ops QC |
| TD-41 | P2 | QC | OPS | Mainnet `chainId=1` rejected by design | **partial** `td_41_*`, `td_41_no_mainnet_chain.sh` | by design |
| TD-42 | P2 | META | CIRCUIT | VK/SRS pin + downgrade detection (CI hash gate) | **partial** CI wired — `check_vk_srs_pin.sh`, `check_deposit_audit_gates.sh`, `td_42_vk_srs_pin.rs` | live `.tvc` redeploy ops |
| TD-43 | P2 | META | CIRCUIT | MockProver green ≠ SHPLONK / AN opcode triple | **partial** doc + CI smoke — `check_mock_vs_shplonk_smoke.sh`, `td_43_mock_vs_shplonk.rs` | live shellnet opcode ops |
| TD-44 | P2 | INV | L1 | `timestamp` / `anWorkchain` в event не в PI — mutation docs | **partial** `DepositEventPiGap.t.sol` + `td_44_*` + `td-44-timestamp-workchain-pi-notes.md` | partial |
| TD-45 | P2 | OK | L1 | Returnless / non-standard ERC20 fail-closed | covered | unit |
| TD-46 | P2 | OK | L1 | `sender` = `msg.sender`; no third-party pull | covered | unit |
| TD-47 | P2 | OK | L1 | Reentrant `deposit` blocked | covered | unit |
| TD-48 | P2 | INV | L1 | DEP-6 deposit не трогает verifyBlock state | covered | isolation |
| TD-49 | P2 | META | ALL | Mutation testing: мутанты `deposit()` / `check_binds_to` kill suite | **partial** score doc + CI smoke — `check_mutation_kill_smoke.sh`, `td_49_*` | — |
| TD-50 | P2 | META | CIRCUIT | Field modulus / BN254 aliasing для всех PI | **partial** `td_50_field_modulus_pi.rs` + `td_50_pi_modulus_binding.rs` | partial |
| TD-51 | P2 | META | RELAYER | Byzantine RPC inconsistent responses per tick | **partial** `td_51_byzantine_rpc.rs` + `td-51-byzantine-rpc-notes.md` | stale chainId cache QC |
| TD-52 | P2 | META | RELAYER | Prover subprocess timeout / zombie on cancel | **partial** `td_52_prover_subprocess_timeout.rs` + `td-52-prover-zombie-notes.md` | ops orphan `cargo` monitor |
| TD-53 | P2 | META | RELAYER | E2E runbook: daemon fail → prove-one → finalize-one → restart | **partial** mock CI smoke — `td_53_dry_run_recovery_smoke.sh`, `td_53_e2e_runbook_recovery.rs` | live E-AN-01 ops |
| TD-54 | P2 | META | CIRCUIT | RLP non-canonical fuzz (`rlp_utils` vs alloy) | **partial** `td_54_rlp_noncanonical_fuzz.rs` + `td-54-rlp-fuzz-notes.md` | TD-64 full corpus |
| TD-55 | P2 | META | L1 | Storage diff: failed deposit не двигает counter/treasury | **partial** `DepositStorageDiff.t.sol` + `td-55-storage-diff-notes.md` | fork TD-56 |
| TD-56 | P2 | META | L1 | Fork real USDC proxy pause/blacklist | **partial** `DepositUsdcMainnetFork.t.sol` + `td-56-usdc-mainnet-fork-notes.md` | live RPC opt-in |
| TD-57 | P2 | META | RELAYER | `parse_and_validate_dapp_id` hex fuzz / modulus wrap | **partial** `td_57_dapp_id_hex_fuzz.rs` + `td-57-dapp-id-fuzz-notes.md` | basic reject in `types.rs` |
| TD-58 | P2 | QC | L1 | Pause asymmetry docs vs code (no pause on deposit) | **partial** `DepositPauseAsymmetry.t.sol` + `td-58-pause-asymmetry-notes.md` | TD-56 fork |
| TD-59 | P2 | META | L1 | Event signature / indexed `depositId` regression for log scan | **partial** `DepositEventSignature.t.sol` + `td_59_*` + `td-59-event-signature-notes.md` | нет |
| TD-60 | P2 | INV | L1 | Owner paths не уменьшают treasury без withdraw proof | **partial** `DepositOwnerTreasuryGuard.t.sol` + `OwnerPrincipal.t.sol` + `td-60-owner-treasury-notes.md` | нет |
| TD-61 | P2 | INV | L1 | Allowance dust / exact approve edge cases | **partial** `DepositAllowanceDust.t.sol` + `td-61-allowance-dust-notes.md` | нет |
| TD-62 | P2 | INV | RELAYER | Off-chain `depositId` gap vs on-chain dense counter | **partial** `DepositCounterDense.t.sol` + `td_62_deposit_id_gap.rs` + `td-62-deposit-id-gap-notes.md` | нет |
| TD-63 | P2 | META | CIRCUIT | `ethereum_fetcher` parse edge: data ≠ 128B, bad topic0 | **partial** `td_63_ethereum_fetcher_parse_edge.rs` + `td-63-ethereum-fetcher-parse-notes.md` | нет |
| TD-64 | P2 | META | CIRCUIT | Corpus real blocks all 7 supported chains | **partial** `td_64_seven_chain_corpus.rs` + `td-64-seven-chain-corpus-notes.md` | live re-pin on upgrade |
| TD-65 | P2 | QC | RELAYER | Live `is_finalized` / G3 Revert loop (QC-OFF-06) | **partial** mock CI smoke — `check_td65_g3_smoke.sh`, `td_65_g3_revert_loop.rs` | shellnet live ops |
| TD-66 | P2 | META | L1 | AAVE `_amountSupplyable` + donation + liquid reserve interleaving | **partial** `DepositAaveInterleaving.t.sol` + `td-66-aave-interleaving-notes.md` | нет |
| TD-67 | P2 | META | XCHAIN | Competing provers same event different `dapp_id` PI | **partial** `td_67_competing_provers_dapp_id.rs` + `td-67-competing-provers-dapp-id-notes.md` | нет |
| TD-68 | P2 | META | CIRCUIT | `NUM_PUBLIC_INPUTS==12` CI gate vs docs (PROJECT_FACTS 11) | **partial** `check_pi_count_docs.sh` + `td_68_pi_count_gate.rs` | partial pin |

---

## 4. Детализация по кластерам

### 4.1 Cross-chain / PI / AN binding (TD-01–05, 15–16, 67–68)

**TD-01 — Mutation matrix 12 PI**  
Атакуем: подмена `depositId`, `sender`, `amount`, `contractAddress`, `chainId`, `anAccount*`, `blockHash*`, `promiseCommit`; endian и swap high/low.  
Test: Rust — per-slot flip → `test_circuit_mock` unsatisfied; relayer `check_binds_to` reject; golden vector ↔ Solidity event.  
Агенты: Grok DEP-PI-MUT, GPT DEP-T03, Opus D-02/D-09 partial.

**TD-02 — PI layout drift prover ↔ AN**  
Layout prover: `[depositId, sender, amount, contractAddress, chainId, dappIdHi, dappIdLo, anAccountHi, anAccountLo, blockHashHi, blockHashLo, promiseCommit]`.  
Риск: AN parser читает 8 Fr по legacy offsets → wrong recipient.  
Test: parse `fixtures/proof_00/public_inputs.bin` алгоритмом AN; CI `check_voucher_abi_consistency`; VK sha256 match.  
Агенты: Opus D-02 (главный), Grok DEP-PI-COUNT, GPT DEP-T03 partial.

**TD-03 — Forged block / no canonical anchor**  
Схема доказывает inclusion в header, не каноничность в консенсусе.  
PoC: synthetic witness → MockProver **pass**; mock AN anchor gate → **reject** `ERR_UNKNOWN_BLOCK` (224); control hash admitted → finalize. Notes: `td-03-forged-block-notes.md`. **partial OK**.  
Агенты: GPT DEP-T01, Opus D-01.

**TD-04 — AN patch series gate (overlay + owner handoff; live deploy ops)**  
Patch `USDCBridge_12pi_chainid_allowlist`: 12 PI, allowlist, anchor, DEP-N-5, mint cap, M-of-N.  
PoC: `td_04_an_patch_gate.rs`, `check_an_overlay_patch_matrix.sh`, `check_td04_deploy_readiness.sh`, `docs/operations/td-04-an-deploy-owner-handoff.md`. **partial QC** — repo ready; live deploy + seed = owner.  
Агенты: Opus D-01, Grok DEP-AN-PATCH.

**TD-05 — dappId injection**  
`dappId` не в L1 event; witness из config. Два proof на один deposit → два voucher keys.  
PoC: `td_05_dapp_id_injection.rs` + `MockAnSubmitter::with_expected_dapp_id`.  
Два bundle A/B на один event → mint ≤1; wrong dappId → `ERR_WRONG_DAPP` (223). **partial OK**.  
Агенты: Grok DEP-DAPP-INJ, GPT DEP-T04, Opus D-03.

**TD-15 — depositId collision без chainId**  
Voucher identity без `srcChainId`; same bridge addr on two L2 → replay swallow.  
PoC: `td_15_deposit_id_collision.rs`, `MockAnSubmitter::accepting_dep_n5`, `td-15-deposit-id-collision-notes.md`.  
DEP-N-5 → два mint; legacy depositId-only → collision documented (BC risk pre-patch). **partial OK**.  
Агенты: Opus D-04, Grok DEP-CREATE2-COLLISION.

**TD-16 — Sepolia in prod allowlist**  
Test: `prod` profile ∩ testnets = ∅; `production_preflight.sh` fails on 11155111.  
PoC: `td_16_prod_no_sepolia.sh`, `PRODUCTION_DEPOSIT_CHAIN_IDS` vs `TESTNET_ONLY`, report `td-16-sepolia-prod-allowlist.md`. **partial OK**.  
Агенты: Opus D-05.

### 4.2 Circuit / MPT / prover soundness (TD-09–13, 19–22, 32, 42–44, 50, 54, 63–64)

**TD-09 — MPT differential + mutation harness**  
Test: independent trie builder; mutate compact flags, node order, RLP lengths, roots; classify accept/reject.  
Агенты: GPT DEP-T02, Grok DEP-MPT-EDGE, Opus D-09.

**TD-10 — Multi-log / log_index**  
Deposit через Safe/4337 → 4+ logs; wrong index in witness.  
Агенты: Grok DEP-MULTI-LOG, GPT DEP-T02 partial, Opus D-07.

**TD-11 — Circuit capacity bounds**  
`MAX_LOG_NUM=3`, `MAX_DATA_BYTE_LEN=128`, MPT depth 10/11, calldata/header max.  
Test: `capacity_bounds.rs` matrix + Foundry 4-log mock Safe.  
Агенты: Opus D-07, GPT DEP-T06 partial, Grok DEP-HDR-L2 partial.

**TD-12 — EIP-1559 only**  
`circuit_v2` constrains tx type 0x02.  
Test: witnesses type 0/1/4 reject; Foundry L1 deposit still succeeds.  
Агенты: Opus D-08, GPT DEP-T06.

**TD-13 — Universal finalizability property**  
∀ successful L1 deposit ∃ proof OR L1 must reject unsupported envelope upfront.  
PoC: `td_13_universal_finalizability.rs` (6 cases), matrix `td-13-finalizability-matrix.md`, Foundry `DepositFinalizability.t.sol`.  
Supported envelope → MockProver pass; TD-11/12 unsupported → reject (QC-by-design). **partial OK**.  
Агенты: GPT DEP-T06.

**TD-19 — Root / header decouple**  
Receipt trie root A + header `receiptsRoot` B → MockProver reject (BC-CIRCUIT-004).  
PoC: `td_19_root_header_decouple.rs`, notes `td-19-root-header-decouple-notes.md`. **partial OK**.  
Агенты: Grok DEP-MPT-EDGE.

**TD-20 — Padding malleability**  
Trailing MPT `key_bytes` padding: slot 2 garbage → MockProver pass, PI byte-identical (QC-PROV-04).  
PoC: `td_20_padding_malleability.rs`, `td-20-padding-malleability-notes.md`. **partial QC**.  
Агенты: Grok DEP-PAD-SEM, QC-PROV-04.

**TD-21 — promiseCommit**  
Slot 11 appended by EthCircuitImpl; keccak coprocessor commitment.  
PoC: `td_21_promise_commit.rs`, `test_circuit_mock_with_pi_corruption`, notes `td-21-promise-commit-notes.md`. **partial OK**.  
Агенты: Grok DEP-PROMISE, Opus D-22.

**TD-22 — Tx/receipt index coupling**  
Adversarial receipt@i + tx trie@j → MockProver reject; shared `tx_idx` witness.  
PoC: `td_22_tx_receipt_index_coupling.rs`, notes `td-22-tx-receipt-index-notes.md`. **partial OK**.  
Агенты: Opus D-21.

**TD-32 — L2 header matrix**  
Arbitrum 16 / Shanghai 17 / Ecotone 20 / Prague/Isthmus 21 fields; 717 byte cap; `verify_block_header_rlp` hash bind.  
PoC: `td_32_l2_header_matrix.rs`, fixtures `fixtures/headers/`, notes `td-32-l2-header-matrix-notes.md`. **partial QC** (BC-D06 slot-width liveness cliff documented, not BC).  
Агенты: Grok DEP-HDR-L2, Opus D-07.

### 4.3 Relayer / daemon / RPC (TD-06–08, 17–18, 26–30, 38–40, 51–52, 57, 62, 65)

**TD-06 — scan_cursor bug** (`source.rs:404`)  
После `fetch(N)` cursor = `safe_head`; `fetch(N+1)` в том же окне → eternal `None`.  
Test: `scan_cursor_regression.rs` — deposits in blocks 100, 101; head 120.  
Агенты: Opus D-06, GPT DEP-T11.

**TD-07 — Scan cursor on proof failure**  
Production `EthLogSource` + persisted cursor; restart must re-find failed target.  
PoC: `td_07_scan_cursor_proof_failure.rs` (`ScanCursorDepositSource` mirrors `effective_scan_from`). **partial OK**.  
Агенты: GPT DEP-T11.

**TD-08 — Dual-RPC chainId**  
Mock: logs Sepolia + `eth_chainId` Base; prover RPC ≠ source RPC.  
Агенты: Grok DEP-CHAIN-BIND, GPT DEP-T05, Opus D-14 partial.

**TD-17 — Reorg / finality**  
Forkable history; block hash replacement; confirmations=0 documented (QC).  
PoC: `td_17_reorg_finality.rs` — safe_head gate, simulated head drop, stale hash → 224 (TD-03). **partial OK**.  
Агенты: все три (DEP-REORG, DEP-T15, D-10).

**TD-18 — state.json durability**  
Kill before/after write/fsync/rename; force-state must reset cursors on deployment mismatch.  
PoC: `td_18_state_json_durability.rs`, `reset_for_new_deployment` in `ensure_deployment(force)`. **partial OK**.  
Агенты: Grok DEP-STATE-RACE, GPT DEP-T12, Opus D-17/D-18.

**TD-26 — HOL + skip policy**  
State-machine: transient ≠ permanent; parked ids in `state.json`; recovery after skip.  
PoC: `td_26_hol_skip_policy.rs`, notes `td-26-hol-skip-notes.md`. **partial QC**.  
Агенты: Grok DEP-HOL-SKIP, GPT DEP-T13.

**TD-27 — Slow deposit parked as failure**  
`NotYetAvailable` vs `ProofFailed` share `record_failure`; `attempts_since_progress` indistinguishable.  
PoC: `td_27_slow_vs_poisoned.rs`, notes `td-27-slow-poisoned-notes.md`. **partial QC**.  
Агенты: Opus D-16.

**TD-28 — Concurrent submit race**  
Barrier + lost receipt mock; mint count ≤ 1; second `AlreadyFinalized`.  
PoC: `td_28_concurrent_submit_race.rs`, notes `td-28-concurrent-race-notes.md`. **partial OK**.  
Агенты: GPT DEP-T14, Grok DEP-COMPETE.

**TD-29 — RPC unwrap_or_default**  
Missing logIndex / blockHash defaults → mapping fail or wrong receipt index; anchor reject.  
PoC: `td_29_rpc_default_log_index.rs`, notes `td-29-rpc-default-notes.md`. **partial QC**.  
Агенты: Opus D-11.

**TD-30 — Prover/source RPC preflight**  
`run_daemon` / `prove-one`: mismatch `eth_chainId` → fail before tick; closes TD-08 gap.  
PoC: `td_30_dual_rpc_preflight.rs`, `rpc_preflight.rs`, notes `td-30-rpc-preflight-notes.md`. **partial OK**.  
Агенты: Opus D-14, GPT relayer section.

**TD-57 — dappId hex parse fuzz**  
`parse_and_validate_dapp_id`: accept canonical hex ≤32B; reject garbage/overwidth/NUL; QC-OFF-09 zero gate; no silent truncate at 65 hex.  
PoC: `td_57_dapp_id_hex_fuzz.rs`, notes `td-57-dapp-id-fuzz-notes.md`. **partial QC**.  
Агенты: Opus D-20.

**TD-59 — Event signature / indexed depositId**  
topic0 canonical hash across L1 / relayer `Deposit::SIGNATURE_HASH` / prover `get_deposit_event_signature`; topic1 = BE `depositId`; wrong sig skipped.  
PoC: `DepositEventSignature.t.sol`, `td_59_log_scan_signature.rs`, `td_59_event_signature_regression.rs`, notes `td-59-event-signature-notes.md`. **partial OK**.  
Агенты: Opus D-27.

### 4.4 L1 on-chain accounting (TD-14, 23–25, 31, 34–35, 45–48, 55–56, 58, 60–61, 66)

**TD-14 — TR-1 interleaved**  
Handler: deposit, supply, withdraw AAVE, emergency, skim, donation; ghost ledger.  
PoC: `DepositTR1Interleaved.t.sol`, extended `TreasuryHandler` (accrue/skim/donate). **partial OK**.  
Агенты: Grok DEP-TR1-AAVE, GPT DEP-T07, Opus D-13.

**TD-23 — FoT in invariant**  
`FeeOnTransferERC20` + `FoTTreasuryHandler` → TR-1 fails under FoT; inverted invariant documents gap (QC-A1-2).  
PoC: `DepositFoTInvariant.t.sol`, notes `td-23-fot-invariant-notes.md`. **partial QC**.  
Агенты: Grok DEP-FOT-ASSUME, GPT DEP-T08.

**TD-24 — Blacklist/pause USDC**  
`BlacklistableERC20`; pause/blacklist fail-closed on `transferFrom`; no retroactive pull.  
PoC: `DepositUsdcBlacklistPause.t.sol`, notes `td-24-blacklist-pause-notes.md`. **partial QC** (not BC). Fork mainnet USDC → TD-56.  
Агенты: Grok DEP-USDT-STYLE, GPT DEP-T08.

**TD-25 — Cross-function reentrancy**  
Malicious token callbacks into supply/verifyBlock/etc.  
PoC: `DepositCrossFnReentrancy.t.sol`, `CrossFnReentrantERC20.sol`, notes `td-25-cross-fn-reentrancy-notes.md`. **partial OK** (not BC).  
Агенты: Grok DEP-CEI-RE, GPT DEP-T09.

**TD-31 — anWorkchain not in PI**  
L1 accepts any `int8`; PI slots 5/6 = dappId; circuit skips WC word; AN credits WC 0 (QC-AN-J4).  
PoC: `DepositAnWorkchainUnbound.t.sol`, `td_31_an_workchain_unbound.rs`, `td_31_workchain_ignored.rs`, notes `td-31-an-workchain-notes.md`. **partial QC** (not BC).  
Агенты: Grok DEP-WC-UNBOUND, Opus D-15.

**TD-34 — No L1 refund**  
Phase 4.3 retired deposit `withdraw()`; stuck USDC — ops only (park/skip, owner emergency).  
PoC: `DepositNoL1Refund.t.sol`, notes `td-34-no-l1-refund-notes.md`. **partial QC** (not BC). Cross-ref TD-12/13/26, `bridge_verification.md`.  
Агенты: Opus D-12.

**TD-35 — Donation → AAVE path**  
TR-3 donation ≠ treasury; supply/skim/excess semantics; TR-1 solvency after full path.  
PoC: `DepositDonationAavePath.t.sol`, notes `td-35-donation-aave-notes.md`. **partial QC** (not BC). Cross-ref TD-14, `EmergencyYield.t.sol`.  
Агенты: Grok DEP-DONATE-AAVE, Opus D-26.

**TD-45–48 — Covered anchors** (не новые PoC): returnless, sender binding, reentrancy, DEP-6 isolation.

### 4.5 Ops / economic / meta (TD-33, 36–37, 39–41, 49, 53, 59, 65–68)

**TD-33 — Dust spam / economic load**  
N×1 unit deposits; proving cost O(N); HOL without skip; recovery with `--skip-after-attempts`.  
PoC: `DepositDustSpam.t.sol`, `td_33_dust_spam_hol.rs`, notes `td-33-dust-spam-notes.md`. **partial QC** (not BC). Cross-ref TD-26, TD-36, QC-A1-1.  
Агенты: GPT DEP-T10, Grok DEP-CAP-POLICY partial.

**TD-36 — Cross cap policy**  
L1 per-tx `uint64.max`, aggregate uncapped; AN `setMintCap` exit 229 → relayer HOL.  
PoC: `DepositCrossCapPolicy.t.sol`, `td_36_mint_cap_exceeded.rs`, notes `td-36-cross-cap-policy-notes.md`. **partial QC** (QC-AN-J1, not BC). Cross-ref TD-33, `DepositWhaleCap.t.sol`.  
Агенты: Grok DEP-CAP-POLICY, QC-AN-J1.

**TD-37 — Competing submitters grief / finalize-one unlock**  
HOL poison → skip/park; peer `finalize-one` / `submit` unlocks; competing → `AlreadyFinalized`; bad proof grief.  
PoC: `td_37_competing_grief_finalize_one.rs`, notes `td-37-competing-grief-notes.md`. **partial QC** (QC-OFF-02, not BC). Cross-ref TD-26/28/65, `f10_competing_submit.rs`.  
Агенты: Grok DEP-COMPETE, Opus D-19.

**TD-38 — Stale finalize ABI / PI reorder**  
2-arg `finalizeDeposit`; swap PI slots → bind fail; operand/`parsed` drift QC.  
PoC: `td_38_abi_pi_reorder.rs`, notes `td-38-stale-abi-notes.md`. **partial QC** (QC-OFF-12, not BC). Cross-ref TD-02, `f10_e_abi.rs`.  
Агенты: Grok DEP-ABI-STALE.

**TD-39 — Incremental scan vs O(head) rescan**  
Per-tick `eth_getLogs` chunk count grows with `safe_head`; fetch does not advance cursor (TD-06).  
PoC: `td_39_incremental_scan_cost.rs`, notes `td-39-incremental-scan-notes.md`. **partial INV/QC** (QC-OFF-10). Cross-ref TD-06/07.  
Агенты: Grok DEP-SCAN, QC-OFF-10.

**TD-40 — CLI backoff=0, Pending timeout, fsync**  
`BackoffConfig::validate` rejects zero initial/multiplier; `confirm_timeout_secs` → `Pending` retry; `state.save` atomic (TD-18).  
PoC: `td_40_cli_backoff_pending.rs`, notes `td-40-backoff-fsync-notes.md`. **partial QC** (DEP-BACKOFF, D-16). Cross-ref TD-18/26/28, `f10_proptest`.  
Агенты: Grok DEP-BACKOFF, Opus D-16.

**TD-41 — Mainnet `chainId=1` rejected by design**  
Six L2 + Sepolia only; L1 `eth_chainId=1` fails `ensure_supported_chain_id_value` at daemon preflight.  
PoC: `td_41_mainnet_chain_id.rs`, `td_41_mainnet_reject.rs`, `td_41_no_mainnet_chain.sh`, notes `td-41-mainnet-chain-id-notes.md`. **partial QC/OK** (DEP-MAINNET-ID). Cross-ref TD-30.  
Агенты: Grok DEP-MAINNET-ID.

**TD-49 — Mutation testing score**  
Full `check_binds_to` kill matrix (7 bound fields); L1 `deposit()` mutants; circuit PI flips.  
PoC: `td_49_mutation_kill_matrix.rs`, `DepositMutationKill.t.sol`, `td_49_mutation_kill.rs`, notes `td-49-mutation-score-notes.md`. **partial META/QC** — **KILLED 13/13** pinned; 3 QC survivors; CI smoke `check_mutation_kill_smoke.sh`. Cross-ref TD-01/05/21/25/31/55/61, `PROJECT_FACTS.md`.  
Агенты: Grok DEP-MUTATION-TESTING, Opus D-25.

**TD-42 — VK/SRS pin + downgrade detection**  
SHA-256 pin `9dacd998…` (5006 B); fixture ↔ USDCBridge `VK_BLOB`; downgrade probes.  
PoC: `check_vk_srs_pin.sh`, `check_deposit_audit_gates.sh`, `td_42_vk_srs_pin.rs`, notes `td-42-vk-srs-pin-notes.md`. **partial META/QC** — CI gate wired (DEP-VK-SRS-PIN covered); `.tvc` redeploy ops. Cross-ref TD-04, `preserve_audit_vk_blob.sh`.  
Агенты: Grok DEP-VK-SRS-PIN, Opus D-23.

**TD-43 — MockProver vs SHPLONK opcode triple**  
MockProver / `verify_proof` struct ≠ Blake2b SHPLONK acceptance; `verify_deposit_opcode_triple` helper.  
PoC: `td_43_mock_vs_shplonk.rs`, `opcode_triple_verify.rs`, notes `td-43-mock-vs-shplonk-notes.md`. **partial META/QC** (DEP-MOCK-VS-REAL doc + CI smoke). Cross-ref TD-42, `verify_opcode_triple.rs`, `PROJECT_FACTS.md` § verification layers.  
Агенты: Grok DEP-MOCK-VS-REAL, Opus D-25.

**TD-53 — E2E shellnet pipeline**  
Full deposit→prove→finalize; F10-G / E-AN-01.  
PoC: `td_53_e2e_runbook_recovery.rs`, `td-53-shellnet-e2e-runbook-notes.md`, `td_53_dry_run_recovery_smoke.sh`. **partial META/QC** (mock CI smoke wired); live shellnet E-AN-01 ops (G5).  
Агенты: Grok DEP-E2E-SHELL, GPT runbook.

**TD-65 — G3 live Revert loop / `is_finalized` stub**  
`AnInterfaceSubmitter`: exit 51 → `AlreadyFinalized`; generic `Reverted` → HOL; stub `is_finalized` always false (QC-OFF-05).  
PoC: `td_65_g3_revert_loop.rs` (8 tests), `check_td65_g3_smoke.sh`, notes `td-65-g3-live-notes.md`. **partial META/QC** (mock CI smoke wired); live shellnet = ops. Cross-ref TD-37, TD-26, TD-53.  
Агенты: Grok DEP-INTERFACE-REVERT-LOOP, QC-OFF-06.

**TD-68 — Docs drift 11 vs 12 PI**  
CI fails if PROJECT_FACTS/README say 11.  
Агенты: Grok DEP-PI-COUNT.

---

## 5. Маппинг исходных ID агентов → TD-XX

### Grok 4.5 (`9a04f1cc…`)

| Grok ID | → TD |
|---------|------|
| DEP-PI-MUT | TD-01 |
| DEP-DAPP-INJ | TD-05 |
| DEP-CHAIN-BIND | TD-08 |
| DEP-TR1-AAVE | TD-14 |
| DEP-FOT-ASSUME | TD-23 |
| DEP-MPT-EDGE | TD-09, TD-19 |
| DEP-WC-UNBOUND | TD-31 |
| DEP-REORG | TD-17 |
| DEP-HOL-SKIP | TD-26 |
| DEP-STATE-RACE | TD-18 |
| DEP-COMPETE | TD-37 |
| DEP-USDT-STYLE | TD-24 |
| DEP-DONATE-AAVE | TD-35 |
| DEP-CAP-POLICY | TD-33, TD-36 |
| DEP-HDR-L2 | TD-32 |
| DEP-CEI-RE | TD-25 |
| DEP-ISO | TD-48 |
| DEP-PAUSE | TD-58 (**partial** no bridge pause #20) |
| DEP-PI-COUNT | TD-02, TD-68 (**partial** — `check_pi_count_docs.sh` + td_68 gate) |
| DEP-PROMISE | TD-21 |
| DEP-SENDER-TOPIC | TD-01 |
| DEP-TX-PATH | TD-12 |
| DEP-MULTI-LOG | TD-10 |
| DEP-PAD-SEM | TD-20 |
| DEP-ABI-STALE | TD-38 |
| DEP-SCAN | TD-39 |
| DEP-BACKOFF | TD-40 |
| DEP-MAINNET-ID | TD-41 |
| DEP-E2E-SHELL | TD-53 (**partial** — mock CI smoke wired) |
| DEP-TIMESTAMP-FREE | TD-44 (**partial** — event ts not PI / not relayer-bound) |
| DEP-COUNTER-GAP | TD-62 (**partial** dense L1 + relayer HOL) |
| DEP-ALLOWANCE-DUST | TD-61 (**partial** exact approve / dust) |
| DEP-OWNER-CANT-STEAL | TD-60 (**partial** owner treasury guard) |
| DEP-VK-SRS-PIN | TD-42 (**partial** — CI gate wired) |
| DEP-FETCH-ABI | TD-63 (**partial** parse edge) |
| DEP-MOCK-VS-REAL | TD-43 (**partial** — doc + CI smoke) |
| DEP-COMPETING-PROVERS | TD-67 (**partial** dappId namespace), TD-37 |
| DEP-INTERFACE-REVERT-LOOP | TD-65 (**partial** — mock CI smoke wired) |
| DEP-MUTATION-TESTING | TD-49 (**partial** — score doc + CI smoke) |
| DEP-FUZZ-AMOUNT-WORD | TD-50 (**partial** — amount high-zero + low-half boundary) |
| DEP-CREATE2-COLLISION | TD-15 |

### GPT 5.6 (`b33f627e…`)

| GPT ID | → TD |
|--------|------|
| DEP-T01 | TD-03 |
| DEP-T02 | TD-09 |
| DEP-T03 | TD-01, TD-02 |
| DEP-T04 | TD-05 |
| DEP-T05 | TD-08 |
| DEP-T06 | TD-11, TD-12, TD-13 |
| DEP-T07 | TD-14 |
| DEP-T08 | TD-23, TD-24 |
| DEP-T09 | TD-25 |
| DEP-T10 | TD-33 |
| DEP-T11 | TD-06, TD-07 |
| DEP-T12 | TD-18 |
| DEP-T13 | TD-26 |
| DEP-T14 | TD-28 |
| DEP-T15 | TD-17 |
| Mutation testing bullets | TD-49 |
| Foundry storage diff / fork | TD-55 (**partial** storage diff), TD-56 |
| Prover corpus / RLP fuzz / field aliasing | TD-64 (**partial** 7-chain corpus), TD-54 (**partial** RLP fuzz), TD-50 |
| Relayer Byzantine / zombie / runbook | TD-51 (**partial** Byzantine RPC), TD-52 (**partial** prover zombie), TD-53 (**partial** mock CI smoke) |

### Opus 5 (`58f296d9…`)

| Opus ID | → TD |
|---------|------|
| D-01 | TD-03, TD-04 |
| D-02 | TD-02 |
| D-03 | TD-05 |
| D-04 | TD-15 |
| D-05 | TD-16 |
| D-06 | TD-06 |
| D-07 | TD-11, TD-32 |
| D-08 | TD-12 |
| D-09 | TD-09 |
| D-10 | TD-17 |
| D-11 | TD-29 |
| D-12 | TD-34, TD-36 |
| D-13 | TD-14, TD-23 |
| D-14 | TD-30 |
| D-15 | TD-31 |
| D-16 | TD-27, TD-40 |
| D-17 | TD-18 |
| D-18 | TD-18 |
| D-19 | TD-37, TD-67 (**partial** competing dappId) |
| D-20 | TD-57 (**partial** hex fuzz) |
| D-21 | TD-22 |
| D-22 | TD-21 |
| D-23 | TD-42 |
| D-24 | TD-50 |
| D-25 | TD-49 |
| D-26 | TD-35 (**partial**), TD-66 (**partial** AAVE interleave) |
| D-27 | TD-59 (**partial** event sig regression) |

---

## 6. Приоритезация имплементации PoC

### Фаза 0 — блокеры (P0)

| TD | Статус PoC |
|----|------------|
| TD-04 | **partial** — overlay + owner handoff; **live AN deploy + owner seed** ops |
| TD-06 | ✅ fix + regression tests |
| TD-01 | partial — relayer bind field mutation |
| TD-02 | partial — documents AN 8-Fr drift |
| TD-03 | partial OK — forged block anchor |
| TD-05 | partial OK — dappId injection |
| TD-07 | partial OK — scan cursor proof failure |
| TD-08 | partial OK — dual-RPC chainId |
| TD-09 | partial — MPT mutation |
| TD-10 | partial OK — log_index |
| TD-11 | partial QC — capacity bounds |
| TD-12 | partial QC — EIP-1559 only |
| TD-13 | partial OK — universal finalizability |
| TD-14 | partial OK — TR-1 interleaved |
| TD-15 | partial OK — depositId collision |
| TD-16 | partial OK — Sepolia prod gate |
| TD-17 | partial OK — reorg/finality |
| TD-18 | partial OK — state.json durability |

**Milestone:** bridge P0 PoC queue closed except TD-04 live AN deploy (ops/XCHAIN).

См. каталог §3 для полного реестра.

### Фаза 1 — P1 hardening (~1–2 недели)

TD-19–35, TD-28–30, TD-32, TD-20–22, TD-65 (G3 live path).

### Фаза 2 — P2 / meta / load (**milestone closed mock/CI**, 2026-08-14)

TD-36–68: partial evidence + notes на все P2 направления. META CI gates wired: TD-68 → TD-42 → TD-43 → TD-49 → TD-04 overlay matrix (`check_deposit_audit_gates.sh`).

**Digest:** `audit/reports/phase-2-deposit-eth-milestone.md` — rollup table, suite deltas, ops blockers, Phase 3 outline.

| Closed mock/CI | TD-04 overlay, TD-42 VkBlob pin, TD-43 SHPLONK asymmetry, TD-49 mutation 13/13, TD-45–48 covered |
| Open ops | TD-04 live deploy, TD-53 shellnet E-AN-01, TD-65 live G3 |

### Уже covered — не писать новые PoC (только regression)

TD-45, TD-46, TD-47, TD-48 + строки в `baseline-deposit-eth-locked.md`.

---

## 7. Статистика

| Метрика | Число |
|---------|-------|
| Канонических направлений `TD-XX` | **68** |
| P0 | 18 |
| P1 | 17 |
| P2 (incl. covered/meta) | 33 |
| PoC **partial** (evidence + notes) | **~62** (TD-01–68 minus 4 covered; TD-06 fixed) |
| PoC **covered** (skip new PoC) | **4** (TD-45–48) |
| META CI gates wired | **7** (TD-68, TD-42, TD-43, TD-49, TD-53, TD-65, TD-04 matrix) |
| Phase 2 milestone | **closed (mock/CI)** — see `phase-2-deposit-eth-milestone.md` |
| Ops blockers (не BC) | **3** (TD-04 live deploy, TD-53 E-AN-01, TD-65 shellnet) |

---

## 8. Следующий шаг (Phase 3)

1. **Ops:** TD-04 — `docs/operations/td-04-an-deploy-owner-handoff.md` + `check_td04_deploy_readiness.sh`.
2. PoC зелёный + BC → `BRIDGE-XXX`; PoC + QC → обновить `baseline-deposit-eth.md`.
3. Обновлять колонку PoC в §3 при merge; держать `check_deposit_audit_gates.sh` green на MR.

**Индекс:** короткий консенсус — `test-directions-deposit-eth.md` (ссылка сюда). Phase 2 rollup — `phase-2-deposit-eth-milestone.md`.
