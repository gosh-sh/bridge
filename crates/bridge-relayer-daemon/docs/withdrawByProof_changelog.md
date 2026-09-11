## Change log / known incidents

Newest first.

### 2026-08-18 — L2 anchoring code-complete + operator readiness (pre-Deploy #12)

- **Change.** All 8 stages of
  [`docs/l2_anchoring_implementation_plan.md`](../../bridge-prover-libraries/docs/l2_anchoring_implementation_plan.md)
  landed (commit `bf0d41a` closes stages 5–8; `c7923c7` covers 1–4).
  Level-parametric across the stack:
  `compute_bridge_anchors --level {1|2}`, `daemon-live --anchor-level`
  (env `BRIDGE_ANCHOR_LEVEL`), and the existing
  `withdraw-e2e --anchor-layer {auto|1|2} --i-know-the-wait`.
  `BootstrapSeed` v2 and `BridgeState` v5 persist `anchor_level` for
  cross-startup drift detection; the relayer daemon refuses on-chain
  stride mis-alignment at boot.
- **`ENRICH_TIMEOUT` bumped 90 → 120 min.** File
  `bridge-relayer-daemon/src/withdraw_e2e/driver.rs:172`. L2's worst-case
  single-bundle wait is ~101 min chain + ~10 min prover; the previous
  90-min budget was L1-tuned and would time out before the enricher
  could resolve a fresh T₂ boundary. L1 unaffected in healthy runs
  (typical L1 resolve time is seconds).
- **Contract is level-opaque — confirmed by code read.**
  `AckiNackiBridge.sol` constructor (lines 509–537) stores only a
  scalar `_vb.genesisPrevMaxLevelLayerHash`; `_layerWindows` starts
  empty. `_expectedPrevAnchor` (lines 990–997) returns that scalar iff
  `_highestActiveLayer() == 0` (first verifyBlock). After the first
  successful proof, `_appendLayerHashes` (lines 902–913) writes both
  `_layerWindows[1]` and `_layerWindows[2]` in one call when the proof
  carries `numLayers = 2`. No Solidity change required for L2 deploys.
- **Cases 8, 9, and L2 timing model added to this runbook.** Case 6 is
  the operator sequence for the first L2 E2E cycle; Case 7 is the
  sequential-withdrawal stress loop (2–3 cycles per session).
- **Not yet exercised live.** Deploy #12 (first L2 Sepolia deploy) has
  not run yet. Case 6's dry-run and Case 7's loop will land as a
  subsequent change-log entry once executed.

### 2026-08-18 — Deploy #10 first live `WithdrawalExecuted` + treasury-seeding case

- **Milestone.** First successful on-chain `withdrawByProof` against
  `AckiNackiBridge 0xa44E35151962684f54Af8aaD2675E726ED848E59` — tx
  `0x35d7254b430f1e475ef16d7f60b295bd0226c906ec5b019d4b2ca408ca657c85`
  at Sepolia block 11,513,799 (`WithdrawalExecuted` emitted; 1.000000
  USDC delivered to `0x742d35Cc…f44e`).
- **Ordering discovery.** After the SHPLONK pipeline fix (commit
  `b22f6c7`, driver.rs now composes `Circuit4ShplonkPipeline`), dry-run
  passed the crypto path (anchor check + verifier both green) but the
  submit reverted with `WithdrawTreasuryShortfall(1_000_000, 0)`
  (selector `0xbb651fce`). Root cause was operational, not
  cryptographic: Deploy #10 was freshly bootstrapped and no one had ever
  bridged IN, so `treasuryBalance == 0`.
- **Fix landed in this runbook.** New [Case 5](#case-5--withdrawtreasuryshortfall--bridge-treasury-empty)
  with the Aave-faucet recipe (`FAUCET.mint(USDC, wallet, amount)` →
  `USDC.approve(bridge)` → `bridge.deposit(amount, 0, 0x11…11)`). Also
  added a treasury-seed step (now
  [Step 2](#step-2--seed-the-bridge-treasury-fresh-deploy-only)) to
  Case 1 so future fresh-deploy demos do the seed BEFORE firing the
  burn, and added the selector row to Case 3's revert table.
- **Rule of thumb.** Fresh deploys must seed the treasury or every
  `withdrawByProof` will revert on the payout leg regardless of proof
  quality. Faucet + deposit costs ~2 tx (<30s wall), fund enough to cover
  the demo's burns. Existing deploys inherit their prior treasury; check
  with `cast call $BRIDGE 'treasuryBalance()(uint256)'`.

### 2026-08-17 — Deploy #8: tight-lookahead rerun after Deploy #7 mis-timing

- **Symptom.** Deploy #7 (`0x822E98…2f90`, seed_seqno=8759296, +262
  lookahead) launched at 09:15 local. Bundle daemon started, but the
  burn (`test_deploy_and_withdraw_only.py`) fired ~30 min later at
  chain head — event landed at seq_no 8767488 while daemon anchor was
  still at 8759296. Covering bundle was 8 bundles ahead of daemon anchor;
  projected wait ≈ 90 min.
- **Root cause.** Between `compute_bridge_anchors --at-head` and the
  actual burn, chain advanced ~8 bundles. The lookahead+lag interacted
  so the event's covering bundle was `daemon_anchor + 8·W·P` instead of
  `daemon_anchor + 1·W·P`.
- **Recovery (this incident).**
  1. Killed the daemon (`kill -9` after SIGTERM was ignored during
     mid-flight aggregator subprocess) and archived `state/` +
     `relayer-state.json` under `state.deploy7_20260817_094240/` and
     `relayer-state.deploy7_20260817_094453.json`.
  2. Re-ran `compute_bridge_anchors --at-head` immediately before the
     new forge deploy. Fresh anchors seed=8768512, +75 lookahead only.
  3. Deploy #8 landed at `0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B`
     with all C4 wiring intact (`accFr` canonical `0x1a1a…1a1a`).
  4. Bundle daemon cold-started with `seed_policy=Explicit(8768512)`.
  5. Burn fired at chain head during bundle 1's proving window; event
     captured at seq_no `8770355`. Covering bundle = 8770560 = bundle 2.
  6. Total wall time ~17 min (vs 90 min projected on Deploy #7) — a
     3.3× speedup driven entirely by anchor freshness + burn timing.
- **Rule of thumb landed in this doc.** For fresh demos, run
  `compute_bridge_anchors --at-head` within 3 min of `forge script`, and
  fire the burn within 5 min of daemon startup. Miss either window and
  the wait grows by ~12 min per additional bundle of catch-up.

### 2026-08-17 — `withdraw-e2e` CLI subcommand landed

- **Change.** Commit `4085c5f` added
  `bridge-relayer-daemon/src/withdraw_e2e/{mod.rs, driver.rs, capture.rs}`
  and the `Cmd::WithdrawE2E` CLI variant in `src/bin/relayer.rs`.
  Replaces the Python driver's steps 5–7 (event capture, witness
  enrichment, proving, on-chain submit) with a single in-process
  entry point.
- **First on-chain demo.** Deploy #8, this runbook. Prior E2E validations
  (2026-08-15 seq 8251308 etc.) were daemon-verified via
  `bridge-verifier-daemon`, NOT via on-chain `withdrawByProof`. Deploy #8
  is the first time `withdrawByProof` executes against a live proof
  produced by the Rust relayer.

### 2026-08-13 — Horizontal-chain event proving landed (fire-window dropped)

- **Change.** Commit `7bb3da9` removed the fire-window constraint that
  had forced burns to land inside a specific W·P slot. Combined with
  `2eacdf7` (explicit L1/L2 dispatch in `build_event_anchor_chain`) the
  orchestrator can now anchor an event at any layer L ≥ 1 as long as the
  covering bundle is verified.
- **Consequence for this runbook.** No fire-window guard. Just fire the
  burn — `--anchor-layer auto` picks L1 for anything within one bundle
  of the covering key, L2 for anything within `W` bundles, etc.
  `--i-know-the-wait` is only needed if you're forcing an explicit
  layer ≥ 2 (rarely the right call for demos).

### 2026-08-06 — `WIRE_WITHDRAW_BY_PROOF` mandatory outside anvil

- **Change.** Commit `a43993b` in the contracts repo made C4 wiring
  mandatory on any chain other than anvil (chainid 31337). Sepolia
  deploys **must** provide `WITHDRAW_ACC_FR` at construction; the
  constructor rejects zero.
- **Consequence.** Never set `WIRE_WITHDRAW_BY_PROOF=false` for
  Sepolia. Every deploy from #4 onward has C4 baked in.