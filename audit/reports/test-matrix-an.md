# AN test matrix (Phase F)

| ID | Layer | Target | Spec test | Status |
|----|-------|--------|-----------|--------|
| F-TC-01 | unit | toolchain symlinks | `unit/test_toolchain_smoke.py` | ✅ |
| F-BR-01 | unit | `getVersion` after ctor | `unit/test_usdcbridge_getters.py` | ✅ |
| F-BR-02 | unit | voucher code hash set | `unit/test_usdcbridge_getters.py` | ✅ |
| F-BR-03 | unit | `getTotalBridged` zero | `unit/test_usdcbridge_getters.py` | ✅ |
| DEP-AN-01 | unit | zero amount → 204 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-02 | unit | bad ZK proof → 220 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-03 | unit | `confirmDeposit` wrong sender → 207 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| QC-AN-01 | unit | amount Fr overflow → 214 | `unit/test_usdcbridge_finalize_negative.py` | ✅ |
| DEP-AN-10 | integration | real proof_00/01 finalize | `integration/test_finalize_deposit_fixture.py` | ✅ |
| DEP-AN-11 | integration | pipeline voucher→confirm | `integration/test_finalize_deposit_pipeline.py` | ✅ |
| DEP-AN-12 | integration | replay proof_00 | `integration/test_finalize_deposit_pipeline.py` | ✅ |
| WD-AN-01 | unit | `initiateWithdrawal` no ECC | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-02 | unit | unsupported tokenId | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-03 | unit | zero ECC amount | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-04 | unit | recipient too long | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-05 | unit | multiple ECC | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| WD-AN-06 | unit | withdraw happy burn+event | `unit/test_usdcbridge_withdraw_happy.py` | ✅ |
| QC-AN-08 | unit | PI too short | `unit/test_usdcbridge_deposit_edge.py` | ✅ |
| QC-AN-10 | unit | anAccount==0 pre-ZK | `unit/test_usdcbridge_deposit_edge.py` | ✅ (QC) |
| DEP-AN-04 | unit | voucher hash mismatch 219 | `unit/test_usdcbridge_deposit_edge.py` | ✅ |
| ADM-AN-01 | unit | `mintAndSend` wrong nonce | `unit/test_usdcbridge_withdraw_admin_negative.py` | ✅ |
| TIP-AN-01 | unit | onTransferReceived wrong sender | `unit/test_usdcbridge_admin.py` | ✅ |
| TIP-AN-02 | unit | onTransferReceived happy | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-02 | unit | mintAndSend not owner | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-03 | unit | mintAndSend happy | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-04 | unit | mintAndSend zero | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-05 | unit | accumulator not whole USDC | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-06 | unit | mintAndSend overflow | `unit/test_usdcbridge_admin.py` | ✅ |
| ADM-AN-07 | unit | setPubkey rotation | `unit/test_usdcbridge_admin.py` | ✅ |
| BC-AN-01 | integration | dual dappId double mint | `integration/test_bc_an_01_*.py` | ✅ dual-proof PoC (Hermez SRS) |
| BC-AN-02 | integration | no L1 bridge allowlist | `integration/test_bc_an_01_*.py` | ✅ pre-ZK |
| QC-AN-07 | integration | voucher brick w/o code | `integration/test_finalize_deposit_voucher_brick.py` | ✅ |
| F7-A | unit/property | PI encode round-trip + mutations | `unit/test_public_inputs_properties.py` | ✅ |
| F7-B | unit/property | replay anchor hash properties | `unit/test_replay_key_properties.py` | ✅ |
| F7-C | integration/property | counter monotonicity (C1–C4) | `integration/test_bridge_counters_property.py` | ✅ |
| F7-D | integration/property | garbage proof storm (no mint) | `integration/test_finalize_negative_property.py` | ✅ |
| F7-E | unit/property | withdraw recipient / ECC table | `unit/test_withdraw_properties.py` | ✅ |
| F8-A | integration/fuzz | message order — no over-mint | `integration/test_pipeline_order_fuzz.py` | ✅ |
| F8-B | integration/fuzz | replay interleave cap | `integration/test_pipeline_order_fuzz.py` | ✅ |
| F8-C | integration/fuzz | two-deposit permutation | `integration/test_pipeline_order_fuzz.py` | ✅ |
| F8-D | integration/fuzz | Hypothesis state machine (finalize↔deliver) | `integration/test_pipeline_state_machine.py` | ✅ |
| F8-E | integration/fuzz | withdraw burn counter after deposit | `integration/test_withdraw_counter_fuzz.py` | ✅ |
| F8-F | integration | BC-AN-01/02 regression (source + pre-ZK) | `integration/test_bc_f8f_regressions.py` | ✅ |
| F8-G | integration/fuzz | cross-counter SM + QC-AN-05 | `integration/test_cross_counter_fuzz.py` | ✅ |
| F8-H | integration/fuzz | ten-proof SM (proof_00..09) | `integration/test_pipeline_multi_proof_fuzz.py` | ✅ |
| F8-I | integration/fuzz | bounce probe + deploy retry | `integration/test_pipeline_bounce_fuzz.py` | ✅ |
| E-AN-01 | e2e | shellnet finalize | acki-nacki `test_usdcbridge_finalize.py` | deferred |

---

## F10 — Off-chain deposit pipeline (Rust, not pytest gate)

| ID | Layer | Scenario | Target | Status |
|----|-------|----------|--------|--------|
| F10-A | deposit-prover | PI ↔ L1 witness binding + padding PoC | `prover.rs`, `tests/f10a_binding.rs`, `tests/padding_mutation_poc.rs` | ✅ `max_key_byte_len=3`; VkBlob `724687a4…` |
| F10-A | deposit-prover | MPT key padding witness malleability | `tests/padding_mutation_poc.rs` | ✅ QC-PROV-04 (no false-deposit path) |
| F10-B | relayer | fault injection (state, mismatch, decode) | `tests/f10_fault_injection.rs` + unit | ✅ |
| F10-B | relayer | ProofFailed / AnRejected recoverable | `crates/deposit-relayer-daemon/` | partial (unit) |
| F10-C | relayer | competing submitters, one mint | `tests/f10_competing_submit.rs` | ✅ |
| F10-D | relayer | confirmation depth helper | `source.rs` + `tests/f10_proptest.rs` | ✅ |
| F10-F | relayer | head-of-line blocking (stuck depositId) | `tests/f10_head_of_line.rs` | ✅ |
| F10-G | relayer | proptest (PI, encode, state, backoff) | `tests/f10_proptest.rs` | ✅ |
| F10-B | relayer | partial `check_binds_to` PoC (QC-OFF-07) | `src/types.rs` | ✅ |
| F10-E | interface | Reverted → Rejected (QC-OFF-06) | `tests/f10_interface_reverted.rs` | ✅ |
| F10-E | interface | canonical 2-arg ABI vs params | `tests/f10_e_abi.rs` | ✅ |
| QC-OFF-01..13, QC-PROV-01 | ops/prover | relayer + prover + interface | `questions-cross-chain.md` | open |
| QC-PROV-02 | prover | axiom-eth pin `@1d61be0` | `deposit-prover/Cargo.toml` | **closed (pin)** |
| QC-PROV-03 | prover | `max_key_byte_len = 3` | `circuit_v2.rs` | **closed (2026-07-17, dev ack)** |
| QC-PROV-04 | prover | MPT `key_bytes` padding not zero-constrained | `tests/padding_mutation_poc.rs` | open (witness malleability) |

Threat model: `audit/reports/manual-audit/F10-offchain-deposit-pipeline.md`.  
Synthesis: `audit/reports/manual-audit/F10-subagent-synthesis.md`.

**Gate (target):** `make audit-deposit-relayer-test` → unit + F10 overlay + proptest.

**Gate:** `make audit-an-test` → **74 passed** (2026-07-17, F7 + F8 fuzz + F8-F BC).

Direction: `audit/reports/an-audit-direction.md`.
