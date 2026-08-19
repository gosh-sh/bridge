# Live `verifyBlock` E2E — Change log / known incidents

Companion to [`live_verifyBlock_runbook.md`](./live_verifyBlock_runbook.md).
Newest first. Each entry captures **what happened, why, what changed, and
the reference commits/paths** so we don't have to reconstruct history next
time we come back to the runbook.

## 2026-08-04 — Deploy #5 (storage v2.0 + NB-Q1/Q8 remediation)

- **Why re-deploy.** Four contract changes landed between 2026-08-03 and
  2026-08-04 and were folded into a fresh deploy rather than run
  mixed-source/bytecode against Deploy #4.
- **Contract changes:**
  - `f8c5ba0` — storage v2.0. `storedNumLayers` + `storedLayerHashes[10]`
    removed; hot-path SSTOREs on both eliminated (~31.9 k gas / verifyBlock).
    `storedPrevMaxLevelLayerHash` promoted to `immutable` (holds the
    genesis seed forever); new `getLatestPerLayer()` view is the correct
    per-layer state observation.
  - `7da8878` — `applyBkSetUpdate` folds BK-set commitments LE (NB-Q1
    blocker from Sergey's 07-27 answer).
  - `09c1686` — `withdrawByProof` flat `_isKnownAnchor` across all 10
    windows (NB-Q1 Option D; no feature flag).
  - `7a645e5` — `DeployShellnetE2EBridge` now requires the C4 verifier +
    `WITHDRAW_ACC_FR` on any chain != anvil (NB-Q8). The
    `BridgeWithdrawalAggregatorVerifier` .bin ships in the bundle even
    though this runbook does not exercise `withdrawByProof`.
- **ABI removals versus prior deploys.** Any tooling calling
  `storedNumLayers()`, `storedLayerHashes(uint256)` or
  `getStoredLayerHashes()` will revert against Deploy #5. Health-check
  block in the runbook updated to use `getLatestPerLayer()` instead.
- **Fresh genesis anchors** were regenerated via
  `compute_bridge_anchors --at-head` against shellnet GraphQL. Bootstrap
  seed advanced from `4887552` (Deploy #4) to `5495808` (Deploy #5,
  bundle boundary 1024). Genesis `bk_set_commitment` unchanged (shellnet
  BK rotation is off).
- **Reference addresses** for Alina's live shellnet deploy are in the
  runbook's
  [Live reference deploy](./live_verifyBlock_runbook.md#live-reference-deploy-alinas-shellnet)
  section.
- **How to re-deploy your own bundle:** see
  [Deploy your own bridge bundle](./live_verifyBlock_runbook.md#deploy-your-own-bridge-bundle-external-users).

## 2026-08-04 — RPC-transport hard-abort false-positive

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
  1. Ran the drift check
     ([runbook §Case 4b](./live_verifyBlock_runbook.md#4b-drift-check--local-state-vs-on-chain))
     → zero drift.
  2. Reset counter atomically:
     `jq '.last_attempt_seqno = .last_processed_seqno | .attempts_since_progress = 0'`.
  3. Relaunched via
     [runbook Case 3](./live_verifyBlock_runbook.md#case-3--clean-restart-no-state-loss).
     `seed_policy=Resume` confirmed in log.
  4. First cycle regenerated the proof for `4891136` in ~13 min → confirmed
     on Sepolia → `state/prover_state.json` bumped mtime to 11:47.
- **Follow-up (not yet landed).** Refactor the hard-abort classifier so
  transport-error variants only trigger exponential backoff, and only
  on-chain revert variants count towards the N=3 abort budget. Documented
  as an open item; the runbook workaround
  ([§Case 4c](./live_verifyBlock_runbook.md#4c-reset-the-attempts-counter))
  is the current mitigation.

## 2026-08-03 — BN254 Fr canonicalization client fix

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

## 2026-08-03 — Deploy #4 (`storedLastSeenBlockSeqNo=0` chicken-and-egg fix)

- Deploys #1–3 constructed the contract with
  `storedLastSeenBlockSeqNo=0`, which required a bootstrap `verifyBlock`
  call with `block_seq_no > 0` to advance it — but the first legit
  `expectedPrevAnchor` check couldn't be satisfied until state was
  seeded. Chicken-and-egg.
- Deploy #4 added `genesisLastSeenBlockSeqNo` as a constructor arg
  (`AckiNackiBridge.sol` constructor). The client's
  `BRIDGE_BOOTSTRAP_SEQNO` must equal this value.
- Current active deploy: see
  [Live reference deploy](./live_verifyBlock_runbook.md#live-reference-deploy-alinas-shellnet)
  (Deploy #5 superseded #4 on 2026-08-04 — see entry above).

---

**Editing this changelog.** When adding a new incident: keep entries dated,
one-paragraph, and always cite the commit / file:line so the next
maintainer can jump straight to the change without spelunking through
`git log`. Old entries can be pruned once the underlying fix has been
proven for >30 days across restarts.
