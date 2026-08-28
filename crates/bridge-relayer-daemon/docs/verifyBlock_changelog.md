## Change log / known incidents

Newest first. Each entry captures **what happened, why, what changed, and
the reference commits/paths** so we don't have to reconstruct history next
time we come back to the runbook.

### 2026-08-18 — L2 anchoring landed; Case 7 added

- **What.** `BRIDGE_ANCHOR_LEVEL=2` now switches daemon-live to the L2
  covering-bundle stride (`W² = 16384`, ~91 min chain-time on
  shellnet). Runbook grew Case 7 for the L2 cold-start delta; steady-state
  Cases 2-6 apply verbatim (daemon behavior downstream of bootstrap seed
  is anchor-level opaque).
- **Why.** L2 aggregates 16 L1 sub-bundles into one covering proof —
  one-tenth the on-chain submission cadence for the same coverage.
  Contract itself is level-opaque (single `storedPrevMaxLevelLayerHash`
  scalar); the daemon computes the L2 fold before submitting.
- **Genesis anchors.** `compute_bridge_anchors --level 2 --at-head` picks
  a `W²`-aligned seed and folds `layer_hashes[0..2]` into
  `GENESIS_PREV_MAX_LEVEL_LAYER_HASH`. See
  `crates/an-bridge-prover/bridge-prover-lib/src/bin/compute_bridge_anchors.rs`.
- **First live L2 deploy.** Deploy #12 (2026-08-18) —
  `AckiNackiBridge` at `0xf31E316C7E3FD4aDDBd86d6d63a1444947BFFEEE`,
  seed `9175040` (W²-boundary). See
  `logs/deploy12_l2_20260819_001713.log` for the deploy transcript.
- **Startup drift check (new).** `bridge-prover-daemon/src/main.rs:115`
  refuses to boot if `prover_state.anchor_level != BRIDGE_ANCHOR_LEVEL`
  — prevents cross-level state reuse. Rename the state dir and
  rebootstrap; never auto-migrate.
- **Related.** Withdraw runbook Case 8/9 (fresh L2 deploy, sequential
  stress-loop); `ENRICH_TIMEOUT` bumped 90→120 min in
  `bridge-relayer-daemon/src/withdraw_e2e/driver.rs` to cover the L2
  worst-case single-bundle wait (~101 min).

### 2026-08-04 — RPC-transport hard-abort false-positive

- **Symptom.** Daemon exited at 11:10:38 local after 3 consecutive tx-send
  failures on seq `4891136`. First failure was `error sending request` (a
  transport error); the next two were `tx confirmation timeout`.
- **Nonce check confirmed zero txs actually landed** — the daemon was
  killed by its own retry-counter, not the chain.
- **Root cause.** The `9b07b24` hard-abort logic in
  `bridge-relayer-daemon/src/relayer.rs` treats transport failures
  (network / receipt polling) identically to on-chain reverts. `N=3`
  transport hiccups on a public RPC (publicnode.com) trip the abort.
- **Recovery (this incident).**
  1. Ran the drift check (§Case 4b) → zero drift.
  2. Reset counter atomically:
     `jq '.last_attempt_seqno = .last_processed_seqno | .attempts_since_progress = 0'`.
  3. Relaunched via [Case 3](#case-3--clean-restart-no-state-loss).
     `seed_policy=Resume` confirmed in log.
  4. First cycle regenerated the proof for `4891136` in ~13 min → confirmed
     on Sepolia → `state/prover_state.json` bumped mtime to 11:47.
- **Follow-up (not yet landed).** Refactor the hard-abort classifier so
  transport-error variants only trigger exponential backoff, and only
  on-chain revert variants count towards the N=3 abort budget. Documented
  as an open item; the runbook workaround (§Case 4c) is the current
  mitigation.

### 2026-08-03 — BN254 Fr canonicalization client fix

- **Symptom.** `verifyBlock` on shellnet block `4888576` reverted with
  `AttestationProofRejected()` (selector `0x87bf1c06`). Root-caused to the
  SHPLONK adapter's Fr equality prelude reading a raw 32-byte BE
  `blockId` that was `≥ r` (BN254 scalar modulus).
- **Fix (client-side).** In
  `bridge-relayer-daemon/src/types.rs:81-83`, reduce the raw chain hash
  mod `BN254_FR_MODULUS` before packing it into `U256`:
  ```rust
  block_id: U256::from_be_bytes(b.block_id_be) % BN254_FR_MODULUS,
  ```
  Constant lives in `withdrawal::BN254_FR_MODULUS`.
- **Note.** `BkSetUpdateData::block_id` (line 106) deliberately keeps the
  un-reduced form — `applyBkSetUpdate` compares against a raw SHA-256
  Merkle root (`AckiNackiBridge.sol:834`), not an Fr scalar.
- **Verified txs (post-fix).**
  - `0x12110bb7…` — first bundle after fix (seq `4890624`).
  - `0x5194250e…` — subsequent bundle (seq `4891136`, ACK'd at 11:47 on
    2026-08-04 after the RPC-abort re-run above).
- **Commits (branch `refactoring_and_review_bridge_relayer_demon`):**
  `487a869` (types.rs Fr reduction), `0fc2851`, `ca106f5` (surrounding
  cleanup + BN254_FR_MODULUS constant).
- **Contract-side follow-up.** Adding the same `% BN254_FR` step inside
  the three SHPLONK adapter contracts would make the client fix
  unnecessary; would require redeploy of `PrimaryAggregatorVerifier`,
  `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier` — all
  immutable references in `AckiNackiBridge` so the bridge itself would
  need redeploy too. Not planned; client workaround is stable.

### 2026-08-03 — Deploy #4 (`storedLastSeenBlockSeqNo=0` chicken-and-egg fix)

- Deploys #1–3 constructed the contract with
  `storedLastSeenBlockSeqNo=0`, which required a bootstrap `verifyBlock`
  call with `block_seq_no > 0` to advance it — but the first legit
  `expectedPrevAnchor` check couldn't be satisfied until state was
  seeded. Chicken-and-egg.
- Deploy #4 added `genesisLastSeenBlockSeqNo` as a constructor arg
  (`AckiNackiBridge.sol` constructor). The client's
  `BRIDGE_BOOTSTRAP_SEQNO` must equal this value.
- Current active deploy: [see reference table above](#reference-addresses-current-deploy).
  Post-mortem lives in `bridge-deployer.txt` (Deploy #4 section).

---

**Editing this runbook.** When adding a new incident: keep entries dated,
one-paragraph, and always cite the commit / file:line so the next
maintainer can jump straight to the change without spelunking through
`git log`. Old entries can be pruned once the underlying fix has been
proven for >30 days across restarts.
