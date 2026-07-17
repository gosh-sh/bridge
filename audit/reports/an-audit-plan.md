# Phase F — AN contracts audit plan

**Status:** F0–F8 landed; **70 passed** pytest (F7 + F8 fuzz incl. 10-proof SM + bounce/retry). Non-E2E **paused** pending author answers (`questions-an.md`). Next: `closeout-an.md`; **F8-F** regressions; **F9** ETH CI night (deferred).

**Scope:** `USDCBridge` / `DepositVoucher` / deposit finalization on Acki Nacki. ETH-side audit (Phases A–E) is closed separately.

---

## Toolchain

| Component | Path |
|-----------|------|
| Compiler | `.tools/sold` → `../TVM-Solidity-Compiler` |
| Debugger | `.tools/tvm-debugger` → `../tvm-sdk` |
| Setup | `scripts/setup_an_audit_tools.sh` |
| Contract sync | `scripts/sync_an_contracts.sh` (`origin/dev`) |
| Spec tests | `audit/spec/an/` |
| Contract workspace | `audit/spec/an-contracts/` |

See `audit/spec/an/BUILD.md` and `audit/spec/an/AGENT_CONTEXT.md` (subagents).

---

## Phases (mirror ETH)

| Phase | Deliverable | Status |
|-------|-------------|--------|
| F0 | Toolchain + pytest harness | ✅ |
| F1 | `manual-audit/F1-usdcbridge-deposit.md` | ✅ (BC-AN-01/02) |
| F2 | `manual-audit/F2-usdcbridge-withdraw-admin.md` | ✅ |
| F3 | Unit + integration tests | ✅ **37/37** |
| F4 | MessagePipeline DEP-AN-11/12 | ✅ |
| F5 | BC-AN-01 dual-proof PoC (Hermez SRS) | ✅ |
| F6 | Shellnet e2e | deferred |
| F7 | Property / fuzz layer (Hypothesis + selective debugger) | ✅ |
| **F8** | **MessagePipeline order fuzz** (reorder / race safety) | ✅ **70/70** gate |
| F8-F | Post-author BC/QC regressions | blocked on disposition |
| F9 | ETH fuzz gaps + CI night profile | **in progress** (CC-3 + A4-INV-1 landed) |

---

## F7 — Property & fuzz backlog (can start before author replies)

**Goal:** Close the main methodological gap vs ETH phase D — random inputs and sequence exploration on AN contract surface, without auditing ZK soundness.

**Invariant registry:** `audit/reports/invariants-an.md` (AN-DEP-*, AN-VCH-*, …).

**Dependency:** `pip install hypothesis` (add to `audit/spec/an/requirements-dev.txt` or doc-only in BUILD.md).

**Gate (target):** `make audit-an-test` stays green; new tests marked `@pytest.mark.property` or `slow`; CI `test:an:audit` runs property subset with low `max_examples` (e.g. 50).

---

### F7-A — Pure Python: public-inputs algebra (fast, no tvm-debugger)

**File:** `audit/spec/an/unit/test_public_inputs_properties.py`  
**Engine:** Hypothesis (`st.integers`, `st.binary`, custom strategies).

| # | Property | Expect |
|---|----------|--------|
| A1 | `build_public_inputs` round-trip: `fr_le` recovers depositId, amount, dapp limbs | always |
| A2 | Truncate `pi` to `len < 352` → marked invalid for on-chain path (mirror QC-AN-08) | always |
| A3 | `amount > 2**64-1` encoded in fr[2] → flagged by pre-mint policy (214) when sent to contract | debugger spot-check only |
| A4 | `anAccount == 0` (hi=lo=0) encodable; documents QC-AN-10 | always |
| A5 | Mutate single byte in dapp limbs → `pi_a != pi_b` with same depositId | always |

**Effort:** ~0.5 day. **INV:** AN-DEP-2, AN-DEP-8, AN-DEP-9 (encode side).

---

### F7-B — Replay-key / voucher address (property + 1 debugger test)

**File:** `audit/spec/an/unit/test_replay_key_properties.py`

| # | Property | Expect |
|---|----------|--------|
| B1 | Hash `(depositId, contractAddr, dappId)` injective on random 1000 triples (no collision in sample) | statistical |
| B2 | Flip one limb of dappId → different hash | always |
| B3 | Same triple → same `DepositVoucher` stateInit (compare tvm-debugger address derivation if exposed; else Python reimplementation of `tvm.hash(abi.encode(...))` matching Solidity) | unit |

**Effort:** ~0.5 day. **INV:** AN-VCH-1. Informs BC-AN-01 mitigation design.

---

### F7-C — Counter monotonicity (debugger, seeded)

**File:** `audit/spec/an/integration/test_bridge_counters_property.py`

| # | Property | Expect |
|---|----------|--------|
| C1 | After successful `finalizeDeposit` + pipeline: `getTotalBridged` increases by proof amount | fixture proof_00 |
| C2 | Owner `mintAndSend`: `getTotalMinted` ↑, `getTotalBridged` unchanged | ADM-AN-03 |
| C3 | `initiateWithdrawal` happy path: burned leg ↑, minted unchanged | WD-AN-06 |
| C4 | Sequence (Hypothesis): interleave invalid finalize (bad proof) with valid — bridged minted never decreases | bounded depth ≤ 5 |

**Effort:** ~1 day. **INV:** AN-ADM-3, AN-ACC-2, AN-WD-7, AN-DEP-9.

---

### F7-D — Negative finalize storm (debugger, slow)

**File:** `audit/spec/an/integration/test_finalize_negative_property.py`  
**Mark:** `@pytest.mark.slow` (optional in CI).

| # | Property | Expect |
|---|----------|--------|
| D1 | Random bytes as `proof` + valid-shaped `pi` → always 220 or pre-check fail; `getTotalBridged` unchanged | Hypothesis `st.binary` |
| D2 | Valid `pi`, random proof length → no mint | same |
| D3 | `tvm.accept()` griefing path: N failures do not mint (state snapshot) | MessagePipeline + snapshot |

**Effort:** ~1 day. **INV:** AN-DEP-4, F7-E.

---

### F7-E — Withdraw fuzz (unit, no proof)

**File:** extend `unit/test_usdcbridge_withdraw_admin_negative.py` or new `test_withdraw_properties.py`

| # | Property | Expect |
|---|----------|--------|
| E1 | Random `recipient` length 0..128 → revert iff > 64 | Hypothesis |
| E2 | Random attached ECC maps (0, 1, 2+ keys) → 216/217/221 | table-driven |

**Effort:** ~0.5 day. **INV:** AN-WD-1..5.

---

### F7-F — Post-author regression (blocked on disposition)

| Trigger | Test to add |
|---------|-------------|
| BC-AN-01 fix: `EXPECTED_DAPP_ID` | `test_bc_an_01_second_dapp_id_reverts` — second proof must fail |
| BC-AN-01 fix: drop dappId from hash | update BC-AN-01 PoC to expect replay on second identical proof |
| BC-AN-02 fix: `EXPECTED_L1_BRIDGE` | integration with two contractAddr PI values |
| QC-AN-10 fix | `anAccount==0` → revert before accept |

**Do not land** until authors pick mitigation — avoids testing wrong behaviour.

---

## F8 — MessagePipeline order fuzz (**core audit tool**)

**Goal:** Explore non-FIFO delivery of internal messages (voucher deploy ↔ `confirmDeposit`) to hunt race conditions / double-mint under reordering. Uses `MessagePipeline.reorder_inflight`, `process_one_at`, Hypothesis schedules.

**Files:** `pipeline_fuzz_helpers.py`, `integration/test_pipeline_order_fuzz.py`

| # | Invariant | Expect |
|---|-----------|--------|
| MPL-1 | `getTotalBridged.minted` monotone while draining queue | always |
| MPL-2 | Random delivery ≤ FIFO canonical oracle (no over-mint) | Hypothesis |
| MPL-3 | Replay same proof in batch + random order ≤ single-mint cap | Hypothesis |
| MPL-4 | Two fresh deposits: swapped deploy order still reaches FIFO total | deterministic |

**F8b — state machine** (`test_pipeline_state_machine.py`): Hypothesis `RuleBasedStateMachine` interleaves `enqueue_finalize`, `deliver_one_random`, `deliver_one_fifo`, `shuffle_inflight`, `replay_last_finalize`.

**F8c — withdraw counters** (`test_withdraw_counter_fuzz.py`): `initiateWithdrawal` is atomic (no pipeline); fuzzes `burned ≤ minted` after deposit-only minting.

**F8d — cross-counter** (`test_cross_counter_fuzz.py`): state machine interleaves deposit pipeline + owner `mintAndSend` + withdraw; documents QC-AN-05 (`burned > minted` on bridged leg after owner path).

**F8e — ten-proof SM** (`test_pipeline_multi_proof_fuzz.py`): proofs `0..9` (unique `deposit_id`), inflight enqueue + shuffle up to 20 msgs.

**F8f — bounce/retry** (`test_pipeline_bounce_fuzz.py`): `bounce:true` probe to unregistered dest via `MessagePipeline._try_bounce_for_missing_dest`; deploy-message retry idempotency.

**Mark:** `@pytest.mark.fuzz`, `@pytest.mark.slow`

**Limitation (documented):** tvm-debugger single-threaded queue — not full multi-block TVM scheduler; bounce/retry semantics partial.

---

### F7 implementation order (completed)

---

### F7 — Explicit non-goals

- Fuzzing Halo2 / `ZKHALO2VERIFYWITHVK` internals (tvm-sdk scope).
- Full **multi-block** TVM scheduler fuzz (cross-block seqno/time) — out of scope; **in-block** MessagePipeline reorder **is** F8.
- Proving random witnesses (deposit-prover runtime).
- Porting dex Hypothesis **wasm matcher** wholesale.

---

## F9 — ETH fuzz expansion (**core audit tool**, cross-repo)

Fuzzing is not optional — it is the primary verification layer alongside manual review. ETH phase D baseline: 11 handler/fuzz tests; gaps below extend coverage.

| ID | Action | Status |
|----|--------|--------|
| F-CC-03 | `invariant` bkSet agreement across mock verifyBlock steps | ✅ `InvariantsVerifyBlock.t.sol` |
| F-A4-1 | Owner-ops handler — treasury never decreases | ✅ `InvariantsOwner.t.sol` |
| CI night | `FOUNDRY_PROFILE=ci` job in GitLab (5000 fuzz / 1000 inv) | todo |
| AN F8 mirror | Message order on withdraw/admin pipeline | backlog |

---

## F1 manual focus (reference)

1. **finalizeDeposit** — 11 PI passthrough, VK blob, opcode call
2. **DepositVoucher** — constructor arity, confirmDeposit, ECC mint
3. **Recipient binding** — `anAccountHigh`/`anAccountLow` reassembly (256-bit)
4. **Replay** — depositId nullifier
5. **Trust** — dappId, amount, ECC currency id

---

## Knowledge base

- AN/TVM: `audit/knowledge/`
- Hermez pins: `audit/knowledge/hermez_kzg_pins.md`
- Questions: `questions-an.md`, `questions-cross-chain.md`

---

## Out of scope

- tvm-sdk opcode implementation audit (smoke only)
- Full node async semantics not modeled in tvm-debugger (document as test limitation)
