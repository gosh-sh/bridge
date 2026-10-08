# Prep plan — testing `bridge-relayer-daemon` against shellnet with BK-set rotation ON

**Status:** research-only, not for execution. Written 2026-10-08 on branch
`feature/bk-set-updates-flow-test-fixes`. Author: Alina + Claude. Pairs with
Sergey Hor's shellnet activation instruction
[`turn_on_bk_sets_on_shellnet.md`](turn_on_bk_sets_on_shellnet.md) and the
existing bundle-only runbook
[`crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md`](crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md).

---

## 0. TL;DR — where we actually are

1. **Relayer daemon code is READY.** The ETH-36 two-slot BK-set model landed on
   `main` 2026-09-25 (commit `33a2384`, PR #56). Phase 1 of `Relayer::tick`
   (`crates/bridge-relayer-daemon/src/relayer.rs:204-302`) submits
   `applyBkSetUpdate` ahead of the layer cursor; Phase 2 verifies against the
   outgoing set for `seqNo <= N`. `history_consistency.rs:132-162` recognizes
   `last_bk_set_update_seq_no_rewound`, `_jumped`, `_uncovered`;
   `startup_decide.rs:412-416` folds the counter into drift. **No code change
   needed in the relayer to run against a rotating shellnet.**
2. **Contract is READY.** `AckiNackiBridge.sol:970-981` enforces the
   `BkUpdateSeqNoNotMonotonic` + "uncovered second rotation" invariants that
   the daemon code mirrors.
3. **Prover-side bk-update witness is READY.** `bridge-prover-lib/src/live_driver/bk_update.rs`
   plus `probe_bk_updates.rs` CLI already prove rotations against live GQL
   (verified by `test_both_circuits` work).
4. **Documentation is NOT ready.** The current runbook explicitly says
   (line 22-23): *"shellnet keeps its BK set fixed from genesis, so a rotation
   never fires under it"* — the operational playbook for the rotation lane
   does not exist. **This is the real gap.**
5. **One optimization is still on a feature branch, not main.** Commit
   `189baba` (branch `pruvendo/eth-37-lastseen`, 2026-10-05) tightens the order
   to *"Submit verifyBlock(N) before applyBkSetUpdate(N)"*. Current `main`
   ordering (Phase 1 before Phase 2) handles rotation correctly for shellnet
   test purposes; the lastSeen refinement is a hardening pass. **Not a
   blocker for the first live test.**

---

## 1. September 2026 history on `main` — what shipped

Grep key: `git log --since=2026-09-01 --until=2026-10-01 --oneline | grep -i 'ETH-3\|bk\|rotation'`.

| Commit     | Date       | What |
|-----------|-----------|-------|
| `33a2384` | 2026-09-25 | **ETH-36 (PR #56)**: apply rotation ahead of layer cursor; keep outgoing set until next bundle. Lands on main the two-slot model (`storedBkSetCommitment` + `storedPrevBkSetCommitment`). |
| `71afd8c` | 2026-09-23 | Pin `applyBkSetUpdate` reads; reject uncovered second rotation in Check B. |
| `6900ea8` | 2026-09-23 | Ack outgoing-set `verifyBlock`; defer prover rotation with driver's stride. |
| `5efd26d` | 2026-09-23 | Mirror ETH-36 two-slot BK-set model in prover + verifier daemons. |
| `9f8ce81` | 2026-09-23 | Pass prove-time `last_seen` into `applyBkSetUpdate`, not live cursor. |
| `33c1c4b` | 2026-09-23 | Keep prover on outgoing BK set until next bundle past N. |
| `13e7089` | 2026-09-21 | `verifyBlock` accepts previous BK set — rotation need not sit on a bundle boundary. |
| `7950271` | 2026-09-20 | Baked attestation `lastSeen` into `applyBkSetUpdate`; defer rotation until `verifyBlock` covers it. |
| `5596e26` | 2026-09-25 | `docs: runbook scope no longer implies daemon can't rotate BK sets.` ← intro reword only; recovery procedures NOT written. |

All of the above are on `main` as of 2026-10-08. Our branch is at same tip (`git rev-list --left-right --count main...HEAD` → `0 0`).

**Not yet on `main`** (open PR work on branch `pruvendo/eth-37-lastseen`):
- `189baba` 2026-10-05 — ordering tightening (verifyBlock(N) *before* applyBkSetUpdate(N)) and some lastSeen refinements. Lower priority for first-cut testing.

---

## 2. Contract surface — what the EVM bridge now enforces

`contracts/ethereum/src/AckiNackiBridge.sol`, key callsites:

- `storedLastBkSetUpdateSeqNo` (line 189) — new scalar, 0 until first rotation.
- `storedPrevBkSetCommitment` (line ~185) — the outgoing (previous) BK-set commitment, kept alongside `storedBkSetCommitment`.
- `applyBkSetUpdate(blockSeqNo, …)` (line 951):
  - revert `BkUpdateSeqNoNotMonotonic(blockSeqNo, storedLastBkSetUpdateSeqNo)` if `blockSeqNo <= storedLastBkSetUpdateSeqNo` (line 970-971).
  - revert "uncovered second rotation" if `storedLastBkSetUpdateSeqNo > storedLastSeenBlockSeqNo` and `storedLastBkSetUpdateSeqNo != 0` (line 979-981). First rotation is always allowed.
- `verifyBlock(…)` (line 747, 1159, 1156): accepts the previous commitment when `storedLastBkSetUpdateSeqNo != 0 && blockSeqNo <= storedLastBkSetUpdateSeqNo` (`BkSetCommitmentMismatch` otherwise).

**Invariant graph (shellnet, 5 BKs, `minBK=4` turned ON):**

```
  rotation on-chain at block N  → applyBkSetUpdate(N)
    ├─ requires: lastSeen covers prev rotation (N_prev <= lastSeen)
    └─ sets: storedLastBkSetUpdateSeqNo = N; storedPrevBkSetCommitment = old; storedBkSetCommitment = new
  verifyBlock(seqNo)
    ├─ seqNo <= N → attest against storedPrevBkSetCommitment (outgoing set signs its own exit bundle)
    └─ seqNo >  N → attest against storedBkSetCommitment (new set)
```

Daemon mirror in `relayer.rs:204-305`.

---

## 3. Relayer daemon — what it already does (no changes needed)

### 3.1 Phase 1 (apply rotation), `relayer.rs:204-302`

- Reads `bk_target = on_chain.last_bk_set_update_seq_no + 1` and asks `BkUpdateSource` for a proof.
- **Defer rule** (line 213-234): if `last_bk > last_seen`, do *not* submit a second rotation — Phase 2 will run first and move the cursor. Logs `"deferring applyBkSetUpdate until verifyBlock covers the previous rotation"`.
- **Hard-stuck guard** (line 218-228): if the pending rotation `N2 < next_bundle_boundary(lastSeen)` AND prev `N1 > lastSeen`, there is no reachable bundle target in `[N1, N2]` — returns `Stuck` immediately (not a silent spin).
- **Cold-start sanity** (line 235-244): compares `upd.old_commitment_l2 == on_chain.bk_set_commitment`, treats mismatch as `BkUpdateReverted` (shellnet BLS keys drifted from `bk_set.*.json`).
- **Deferred prover ack** (line 254-260): `prover_rotation_may_ack(new_state, bundle_stride)` keeps the prover on the outgoing set until the next bundle target is past `N`.

### 3.2 Phase 2 (verifyBlock), `relayer.rs:304+`

- Unchanged flow; the `BridgeClient::read_state` returns `last_bk_set_update_seq_no`, `bk_set_commitment` (= new) and `prev_bk_set_commitment` (= outgoing). The attestation witness picks the correct one via `expected_bk` logic in `bridge.rs:637-700`.

### 3.3 Startup drift, `startup_decide.rs:82-83, 412-416`

- `apply_now = chain.last_bk_set_update_seq_no > local.stored_last_bk_set_update_seq_no && chain.last_bk_set_update_seq_no != 0` — chain-fresh rotation after a daemon crash triggers warm-resume cleanly.
- `relayer-state.json` must round-trip `stored_last_bk_set_update_seq_no`; drift message is `"local {local_seq}: stored_last_bk_set_update_seq_no={a}, chain={b}"`.

### 3.4 History-consistency guard, `history_consistency.rs:122-162`

Three post-ack checks that fire against the on-chain snapshot:

| Variant | Trigger |
|---------|---------|
| `last_bk_set_update_seq_no_rewound` | chain decreased since last ack — impossible without a redeploy or re-org. |
| `last_bk_set_update_seq_no_jumped` | gap bigger than one — a *different* actor rotated between ticks. |
| `last_bk_set_update_seq_no_uncovered` | `remembered.last_bk > actual.lastSeen` — contract-impossible on a second apply; implies someone replayed state. |

### 3.5 Prover-side witness, `bridge-prover-lib/src/live_driver/bk_update.rs` + `probe_bk_updates.rs`

- `BkUpdateSource` trait abstracted; `LiveBkUpdateSource` fetches BK-set-update proof from GQL.
- `probe_bk_updates` is the dry-run CLI: polls GQL for the next unapplied rotation, logs its `old_commitment_l2` / `new_commitment_l2` / `block_seq_no` without touching Sepolia. **This is the diagnostic tool for Phase 0 of the live test** (see §6).

---

## 4. Documentation gaps

Measured against the current runbook (1376 lines, scope-locked to `verifyBlock`-only):

| Section needed | Status |
|----------------|--------|
| Pre-flight: how to detect that shellnet BK rotation is actually live | **Missing.** No `getConfig` / `getDetails` recipe; no mention of Sergey's instruction. |
| Expected startup behaviour when chain's `last_bk > 0` and local state has `last_bk = 0` | **Missing.** Code path exists (`startup_decide.rs:82`), the runbook does not call it out. |
| Phase 1 observational signals (log lines, metrics, GQL queries) | **Missing.** Only the "Phase 2 verifyBlock reverted" branch is documented. |
| Recovery: `BkSetCommitmentMismatch` after a rotation | **Half-done** (Case 5, line 1028): mentions it as a BLS-keys-drift symptom; does NOT distinguish "we missed a rotation" vs "wrong `bk_set.json`". |
| Recovery: `BkUpdateSeqNoNotMonotonic` revert from Phase 1 | **Missing.** |
| Recovery: `BkUpdateUncoveredPrevRotation` (contract revert path from line 979) | **Missing.** |
| Recovery: Phase 1 `Stuck` from `relayer.rs:219` (two rotations with no bundle target in `[N1, N2]`) | **Missing.** This is the *only* operator-fatal rotation path and is undocumented. |
| State file: `prover_bk_set.json` lifecycle under rotation | **Partial** (file-ref table at line 1273 mentions it). |
| Explicit expectation that `bk_set.shellnet.json` is a snapshot that WILL go stale once rotation fires — and how to refresh | **Missing.** Current intro (line 27-35) pins Poseidon commitment `0x08eb0a…ca71c` + SHA `c77e3d…898f`; both change after the first rotation. |

---

## 5. Structured version of Sergey's `setConfig` activation recipe

Sergey's doc (`turn_on_bk_sets_on_shellnet.md`) is a correct procedure; this is its decomposition into phases + preconditions. **Reality check:** the shellnet owner (SeHor05 / Sergey) runs the actual `setConfig`, not us. We prepare + observe.

### 5.1 Preconditions (collect BEFORE requesting the toggle)

| Datum | Where to get it | Why |
|-------|-----------------|-----|
| Confirmed shellnet owner keypair | Ask Sergey — `config/BlockKeeperContractRoot.keys.json` of the current shellnet deploy | setConfig is `onlyOwner`. |
| Current `epochDuration` | `tvm-cli -j runx -m getConfig` against `0:7777…7777` | setConfig overwrites ALL 6 fields. |
| Current `walletTouch`, `nlinit`, `isNeedNumberOfActiveBlockKeepers`, `needNumberOfActiveBlockKeepers` | NO getter — read from shellnet deploy env (`WALLET_TOUCH`, `NLINIT`, …) **before** the toggle. Zero room for guessing. | Same reason — must be preserved. |
| Current number of active BKs | `tvm-cli -j runx -m getDetails` → `numberOfActiveBlockKeepers` | Must be ≥ 5. If someone already fell to 4, the toggle has different mechanics. |
| Candidate BKs in queue | Ask Sergey / query BK roster | If zero candidates, rotation won't produce replacement, net just drops to 4 active. Still exercises our rotation path — but a weaker test. |

### 5.2 Phases

**Phase A — Pre-flight (us, before Sergey toggles):**
1. Pull `getConfig` + `getDetails` on `0:7777…7777`. Pin the output in `bk_rotation_test_prep_plan.md` sibling file (`shellnet_bk_root_pre_rotation.json`) for post-mortem.
2. Snapshot current `bk_set.shellnet.json` SHA + Poseidon commitment; cross-check with `/v2/bk_set` from a node (shellnet.ackinacki.org returns 404 for that endpoint per `AGENTS.md`; needs direct node:8600 access from Sergey).
3. Run `cargo run --bin probe_bk_updates -- --endpoint https://shellnet.ackinacki.org/graphql --poll` for ~30 min. Expect zero updates. Baseline that the probe actually reports "none pending" and the GQL path works end-to-end before the toggle.
4. Deploy a *fresh* Sepolia bridge instance (Deploy #8 or later), fully L2-anchored, so initial `storedLastBkSetUpdateSeqNo = 0` and we start in the ETH-36 happy path. Record anchors.

**Phase B — Toggle (Sergey):**
1. Sergey calls `setConfig(epochDuration, 4, isNeed=preserved, needNum=preserved, 200, 5000)` with the preserved fields from Phase A.
2. Confirm receipt — ask for TX hash + the resulting `getDetails` output.

**Phase C — Observational window (us, chain-level, BEFORE touching the relayer):**
1. For one `epochDuration` cycle (660 s per default = ~11 min) run `probe_bk_updates --poll` and record every update emitted by GQL. Expected cadence: at most one per epoch rotation.
2. For each update, verify:
   - `old_commitment_l2` matches the shellnet snapshot (`bk_set.shellnet.json` first time; the daemon's `prover_bk_set.json` thereafter).
   - `block_seq_no` is monotonic.
   - `new_commitment_l2` matches a legitimately queried BK roster at that height (requires a node:8600 fetch at ≥ `block_seq_no`).
3. ONLY proceed to Phase D after at least ONE clean rotation has been seen end-to-end at this read-only level.

**Phase D — Live relayer dry-run against the rotating chain:**
1. Point `relayer daemon-live` at the fresh Sepolia bridge. Start with L1 anchoring (bundle stride 1024, faster feedback) BEFORE jumping to L2.
2. Expected timeline per Phase 1 + Phase 2 ticks:
   - Phase 1 fires `applyBkSetUpdate(N)` within one tick of GQL emitting the rotation.
   - Phase 2 continues `verifyBlock` on bundle boundaries; one of the next bundles will land with `seqNo <= N`, exercise the "accept previous commitment" path.
   - Phase 1 defer-rule fires only if a *second* rotation lands before Phase 2 catches up — observable in logs.
3. Success criteria for first attempt:
   - At least 2 rotations applied end-to-end: `TickOutcome::BkUpdateApplied` × 2 in logs.
   - No `RelayerError::Stuck` from Phase 1 line 219 for a full 2-hour window.
   - `storedLastBkSetUpdateSeqNo` on Sepolia stays `<= storedLastSeenBlockSeqNo` at all times (sampled every tick in the `chain_bk_upd` log line, `relayer.rs:2253-2255`).

**Phase E — Rollback / abort criteria:**
- Phase 1 returns `Stuck` → stop daemon, grab snapshots, hand back to Sergey for `minBK=5` toggle.
- `applyBkSetUpdate` reverts with `BkSetCommitmentMismatch` three times in a row → see §6.1.

---

## 6. Risk / failure-mode catalogue

### 6.1 `BkSetCommitmentMismatch` on Phase 1 (`applyBkSetUpdate`)

**Cause.** The proof's `old_commitment_l2` does not equal on-chain `storedBkSetCommitment`. Root causes, in order of likelihood:
1. `bk_set.shellnet.json` is stale — it is a snapshot pinned 2026-07-08, rotation changes the live set. **Mitigated** by the fact that only the *first* rotation needs the file; subsequent rotations load from `prover_bk_set.json`.
2. Shellnet BLS keys drifted independently of our tracking (manual node replacement).
3. Fresh bridge deploy got the wrong `GENESIS_BK_SET_COMMITMENT` env — audit `compute_bridge_anchors --at-head` output against the chain head immediately before deploy.

**Mitigation.** Phase A step 2 (snapshot cross-check). First rotation after enabling must be caught by `probe_bk_updates` BEFORE pointing the daemon at it.

### 6.2 Phase 1 `Stuck` — "two rotations with no bundle target in `[N1, N2]`"

**Cause.** Two rotations fire within `< bundle_stride` seq_nos (i.e. within ~5 min at L1, ~91 min at L2 shellnet-time). Chain-side this requires a BK that joined and immediately left; unlikely on a stable network with `minBK=4` and 5 active BKs but **possible under churn**.

**Mitigation.** First run on **L1 anchoring** (shorter stride → tighter window where this condition can arise). If it fires, log + abort is the correct behaviour. Operator plays Sergey's `minBK=5` card and we re-plan.

### 6.3 `last_bk_set_update_seq_no_uncovered` from history-consistency

**Cause.** Local state remembers `last_bk > actual.lastSeen` after a drive-by chain roll-back or a wrong GQL endpoint. Contract makes this impossible to reach by legitimate means.

**Mitigation.** Only ever happens with corrupted state or wrong endpoint. `relayer.rs` fails fast; operator inspects `relayer-state.json`.

### 6.4 Stale `bk_set.shellnet.json` after rotation starts

**Fact.** The file is a one-time bootstrap artefact. Once `storedLastBkSetUpdateSeqNo != 0`, the authoritative BK set is `prover_bk_set.json` locally and `storedBkSetCommitment` on-chain.

**Risk.** If someone restarts cold with the stale shellnet.json AND the bridge is rotated, bootstrap will mis-anchor. Must document this explicitly in the new runbook (currently the file-reference table at line 1273 is coy about it).

### 6.5 Prover rotation ack coupling

**Fact.** The prover is NOT acked immediately on `applyBkSetUpdate` — it stays on the outgoing set until `prover_rotation_may_ack` returns true (next bundle target > N). This is correct per spec; the risk is that if the daemon is killed between `applyBkSetUpdate` landing and the ack, the live driver on restart will NOT have been informed of the rotation.

**Mitigation.** `startup_decide.rs:82` detects "chain is ahead on bk_set, local isn't" and triggers a resumption path. Needs to be tested explicitly in Phase D (induce a daemon kill 30 s after a rotation applies).

### 6.6 Deploy-script env drift

**Fact.** `scripts/deploy_bridge_bundle.sh` writes `L2_config/env` with `GENESIS_BK_SET_COMMITMENT` from `compute_bridge_anchors --at-head` captured AT DEPLOY TIME. If a rotation fires between the capture and `verifyBlock`'s first attempt (realistic given shellnet speed), the first verify will revert.

**Mitigation.** Timebox the deploy → first bundle window to << epoch_duration. Already standard practice; worth calling out explicitly.

---

## 7. Phased work plan (what to actually DO before the live test)

### Phase 1 — Preparation (code-level, before Sergey toggles)
Est. 1 session.
- [ ] Add targeted test to `relayer.rs` exercising Phase 1 defer-rule and `Stuck` branch end-to-end (currently covered by unit tests, but no integration test with a mock BkUpdateSource that returns two rotations with no gap). Not required for live test but closes an audit hole.
- [ ] Run `probe_bk_updates` against shellnet right now (rotation OFF). Expect "none pending" — baselines the diagnostic path.
- [ ] Capture current `getConfig` + `getDetails` output from `0:7777…7777`. Save under `/Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/shellnet_bk_root_pre_rotation.json`.
- [ ] Dry-read `bk_set.shellnet.json` SHA/Poseidon vs on-chain `storedBkSetCommitment` on current Deploy #7 bridge — must match before we accept any rotation sequence.

### Phase 2 — Documentation (write before Sergey toggles)
Est. 1 session.
- [ ] Draft `crates/bridge-relayer-daemon/docs/live_relayer_bk_set_rotation_runbook.md` as a *delta* on top of the existing runbook. Sections (see §4 gap table): Pre-flight, Phase-1 observational signals, Recovery × 4 (mismatch / monotonic revert / uncovered revert / Phase-1 Stuck), state-file lifecycle, `bk_set.shellnet.json` staleness.
- [ ] Fold Sergey's `turn_on_bk_sets_on_shellnet.md` recipe into §5 of that new runbook as "Phase A/B/C" (upstream owns the toggle; we own the preconditions).
- [ ] Review with Sergey — he confirms the Phase-A pre-flight list is exhaustive (walletTouch / nlinit values specifically).

### Phase 3 — Coordinated toggle + observation (needs Sergey)
Est. 1 session + wall time.
- [ ] Sergey executes Phase B (setConfig to minBK=4). We stand by with `probe_bk_updates`.
- [ ] Phase C observational window: ≥ 1 clean rotation observed via GQL/CLI only, no relayer involvement yet.

### Phase 4 — Live relayer run (L1 first, then L2)
Est. 1 session + ~4h L1 run + ~6h L2 run.
- [ ] Fresh Sepolia Deploy #8 (burner key, L1 anchoring, standard recipe).
- [ ] `relayer daemon-live` for ≥ 2 h, watch `chain_bk_upd` log line + `BkUpdateApplied` outcomes.
- [ ] Repeat on L2 anchoring (fresh Deploy #9) once L1 is clean. L2's longer bundle stride reduces §6.2 risk, so L1 is the real stress test.

### Phase 5 — Post-mortem + runbook finalization
- [ ] Fold anything surprising from Phase 4 into the draft runbook from Phase 2.
- [ ] PR against `main` with the new runbook + CHANGELOG `Unreleased/Added` entry (per `AGENTS.md`).

---

## 8. Open questions (ask Sergey before Phase 3)

1. What are the real current `walletTouch` and `nlinit` values on shellnet? (No getter; Phase A fails without this.)
2. After rotation turns on, will Sergey turn it off for test convenience, or does the test have to live with continuous rotation? (Affects whether Phase 4 can plan a controlled rotation count.)
3. Is `/v2/bk_set` reachable at all from outside the shellnet node network, or do we need Sergey to script a snapshot at known seq_nos? (Our `AGENTS.md` says `shellnet.ackinacki.org:8600` returns 404 for `/v2/bk_set`.)
4. If a BK leaves and no candidate replaces it, do we proceed with 4-active testing or wait? (Affects test realism.)

---

## 9. Files touched by this plan

**Read-only:** everything under `crates/bridge-relayer-daemon/src/`, `contracts/ethereum/src/AckiNackiBridge.sol`, `crates/bridge-prover-libraries/bridge-prover-lib/src/live_driver/`.

**Will be written (Phase 2):**
- `crates/bridge-relayer-daemon/docs/live_relayer_bk_set_rotation_runbook.md` (new).
- `CHANGELOG.md` `## [Unreleased]` entry (per AGENTS.md).

**Will be written (Phase 1):**
- `/Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/shellnet_bk_root_pre_rotation.json` (snapshot, not committed).

**No code changes to the relayer are planned.** Code is ready per §3.
