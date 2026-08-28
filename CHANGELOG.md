# Acki Nacki Bridge Release Notes

All notable changes to the bridge are documented in this file — the contracts on
both chains, the ZK circuits and verification keys, the relayer and prover
binaries, and the deployment and operational surface around them.

Written for the people who deploy and run the bridge, not for the people who
wrote it. See the changelog policy in [AGENTS.md](AGENTS.md) for what belongs
here and how versions are assigned.

## [Unreleased]

<!--
Add entries here, grouped under the headings below, most disruptive first.
Drop a heading if it has no entries. Do not add a version number — a human
assigns it when the release is tagged.

### Breaking Changes
### Added
### Changed
### Fixed
### Removed
-->

### Breaking Changes

- **`contracts/ethereum/.env.shellnet` and `.env.shellnet.l2` deleted.**
  The single shared shellnet burner (`0xb586…2307`) now lives once, in
  `crates/an-bridge-prover/shellnet.common` under `RELAYER_PRIVATE_KEY`,
  and covers both the deployer and the relayer role. Manual deploy
  paths that used to `set -a && source contracts/ethereum/.env.shellnet
  && forge script …` are retired — use
  `PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL={1,2}
  ./scripts/deploy_bridge_bundle.sh` from `crates/an-bridge-prover/`
  instead (the wrapper re-derives genesis anchors from live chain head
  and writes `L{1,2}_config/env` atomically). Per-deploy provenance
  (Deploy #8 / #12 anchors) remains in git history.

- **Per-mode config directory layout in `crates/an-bridge-prover/`.**
  The parallel `state/` + `state_l2/` + `proofs/` + `proofs_l2/` +
  `.env.shellnet` + `.env.shellnet.l2` layout is retired. Runtime data
  now lives under `L1_config/{env,state,proofs,work_dir}` and
  `L2_config/{env,state,proofs,work_dir}`. Shared env lines are
  extracted to a single `shellnet.common` sourced by each per-mode
  `env` file. Operators must migrate any existing on-disk state before
  restarting the daemon (`mv state L1_config/state && mv proofs
  L1_config/proofs`); the file-first startup guard reads only the new
  paths.

### Added

- **New binary `bridge-withdraw-e2e-cli`** — end-user CLI for withdrawing
  USDC from an Acki Nacki multisig to an EVM recipient via the bridge.
  Third-party-operator-facing counterpart to `daemon-live`: the daemon
  owns bundle proving (running anywhere — not necessarily on the same
  host); this CLI owns per-withdrawal composition. Runs the seven-stage
  pipeline (preflight → idempotency reserve → burn → capture →
  resurrect `BridgeState` from `AckiNackiBridge` at `--bridge-address`
  and wait for the covering bundle to land → Circuit-4 SHPLONK proof →
  `withdrawByProof`). **No local `prover_state.json` is read** — the
  CLI's only view of prover state is the on-chain contract, so it works
  against any deploy the operator has RPC + `--bridge-address` for.

  Subcommand surface:
  ```
  bridge-withdraw-e2e-cli withdraw \
    --from <dapp_id>::<account_id> --from-keys /path/to/owner.keys.json \
    --to 0xRecipient --to-chain 11155111 --amount 1.000000
  ```
  Global flags: `--json` (one-line JSON on stdout, human logs on stderr),
  `--yes` (skip prompt), `--non-interactive` (refuse instead of
  prompting), `--dry-run` (compose + prove but broadcast nothing on
  either side), `--allow-retry` (blunt override of the duplicate-
  in-flight refusal; will be replaced by `--resume` in v2).

  Env vars consumed (each has a matching `--flag`): `BRIDGE_GQL_ENDPOINT`,
  `USDC_BRIDGE_ACCOUNT_ID` (shellnet default `1a1a…1a1a`),
  `PROVER_STATE_PATH`, `RPC_URL`, `BRIDGE_ADDRESS`,
  `RELAYER_PRIVATE_KEY` (signer for `withdrawByProof`; distinct from
  the AN multisig owner key), `BRIDGE_AGGREGATOR_DIR`,
  `BRIDGE_VERIFIERS_DIR`, `BRIDGE_PARAMS_DIR`, `BRIDGE_PK_CACHE_DIR`,
  and `BRIDGE_WITHDRAW_STATE_DIR` (new — per-withdrawal idempotency
  state table; defaults to `$CONFIG_DIR/withdraw-state/`).

  Exit codes distinguish "nothing broadcast" from "broadcast, outcome
  unknown" so operator scripts don't blind on a single non-zero:
  `0` success (or dry-run OK), `2` preflight refused,
  `3` duplicate in-flight refused, `10` AN burn broadcast but final
  outcome unknown (reconcile via GQL), `11` capture timeout,
  `12` proof failed, `13` `withdrawByProof` reverted / dry-run reverted.

  Idempotency: SHA-256 dedup key on `(from, to, to_chain, amount)`. State
  files under `$BRIDGE_WITHDRAW_STATE_DIR` hold only chain-observable
  identifiers (AN tx hash, `WithdrawalInitiated` msg id, block seq no,
  ETH tx hash) — never key material. `--from-keys` and
  `--eth-private-key` contents are never logged, printed, or persisted.

  Burn payload defaults to `bounce = true` so USDC returns to the source
  multisig on any bridge revert (the historical Python driver used
  `bounce = false`; the Rust CLI's default is the safer of the two).

- **Helper scripts under `crates/bridge-withdraw-e2e-cli/scripts/`**:
  `local_smoke.sh` (dry-run wrapper: full pipeline including
  `dry_run_withdraw`, no broadcast) and `live_smoke.sh` (real submit).
  Both source `$BRIDGE_CONFIG_DIR/env` (default `L1_config/env`) for
  daemon-shared plumbing and expect the caller to export the
  per-withdrawal identity vars: `WITHDRAW_FROM`, `WITHDRAW_FROM_KEYS`,
  `WITHDRAW_TO`, `WITHDRAW_TO_CHAIN`, `WITHDRAW_AMOUNT`. Both emit
  per-mode absolute `--snark-dir` so the aggregate-proof subprocess
  finds the intermediate `.snark` file. Multisig deploy and EVM-side
  USDC treasury seeding are intentionally not scripted here — the
  vendored Python driver and `cast` remain the source of truth for
  those one-time setup steps.

- New environment variables consumed by `bridge_prover_lib::paths`:
  `BRIDGE_CONFIG_DIR` (broad selector — resolves both state and proofs
  under `$BRIDGE_CONFIG_DIR/`), `BRIDGE_STATE_DIR` and `BRIDGE_PROOFS_DIR`
  (narrower overrides, win over `BRIDGE_CONFIG_DIR` when set).
  Operators must `export BRIDGE_CONFIG_DIR=./L1_config` (or
  `./L2_config`) **before** sourcing the per-mode env file; the env
  files no longer set `BRIDGE_CONFIG_DIR` themselves.
- New launch scripts under `crates/an-bridge-prover/scripts/` that
  source the per-mode env file: `launch_withdraw_e2e.sh` (dry-run L1),
  `launch_withdraw_e2e_real.sh` (real submit L1),
  `launch_withdraw_e2e_l2.sh` (dry-run L2), `replay_withdraw_shplonk.sh`
  (offline replay against a retained enriched witness). All emit
  per-mode absolute snark-dir paths so the aggregate-proof subprocess
  finds the intermediate `.snark` file.

### Changed

- **`bridge-withdraw-e2e-cli` capture is now multi-user safe.**
  The CLI no longer youngest-picks a shared `USDCBridge → ExtOut` queue
  after firing its burn. Instead it chain-follows the multisig transaction
  hash (`an_tx_hash` returned by `sendTransaction`) through the two GraphQL
  hops the AN node exposes: `transaction(hash: an_tx_hash).out_messages`
  → pick outbound whose `dst == USDCBridge` → `message(hash:
  msig_out_msg_id).dst_transaction.out_messages` → pick outbound whose
  `dst == makeAddrExtern(618)` → the WithdrawalInitiated msg_id. The
  msg_id and its block_seq_no are persisted at the `Captured` idempotency
  stage. Concurrent operators can no longer capture each other's events
  even in the sub-second window between burns. New functions:
  `bridge_gql_fetcher::gql_client::{query_tx_out_messages,
  query_msg_dst_tx_out_messages, query_bridge_extout_by_id}` and
  `bridge_relayer_daemon::withdraw_e2e::capture_targeted_withdrawal_event`.
  The daemon's `run_once` still uses the baseline-snapshot path via
  `capture_next_withdrawal_event` — no behavior change there.

- `bridge-relayer-daemon` docs Case 1 (`live_relayer_bridge_verifyBlock_runbook.md`)
  is now a single unified cold-start section with a
  `BRIDGE_CONFIG_DIR=./L1_config` / `BRIDGE_CONFIG_DIR=./L2_config`
  selector at the top. Case 7 collapses to a table of
  the four operator-visible L2 deltas (W²-aligned bootstrap seqno, up
  to ~101 min first-verify wait, `layers=2` log field, ~15
  sub-bundle diagnostic window). `live_withdrawByProof_runbook.md`
  Case 8 was updated to source `L2_config/env` instead of
  `.env.shellnet.l2`.
- `bridge-prover-daemon` and `bridge-verifier-daemon` binaries no
  longer hardcode `./state/` and `proofs/` at compile time; they
  resolve paths at runtime via `bridge_prover_lib::paths`. Defaults
  match the pre-refactor literals (`./state`, `./proofs`,
  `./state/bootstrap_seed.json`), so existing operators who do not set
  `BRIDGE_CONFIG_DIR` see no behavioral change.

- Python E2E drivers deduplicated. `deploy_multisig` / `mint_usdc`
  moved to new `crates/an-bridge-prover/python/helper/msig.py`;
  `materialize_usdc_bridge_key_from_node_config` /
  `validate_usdc_bridge_key` moved to `helper/bridge_e2e.py`. Both
  `test_deploy_and_withdraw_only.py` and
  `generate_withdrawals_with_live_event_proving.py` now import the
  shared implementations. Fresher variant kept in the merge (explicit
  `RuntimeError` on multisig-materialization timeout, richer
  owner-key mismatch hints). Also fixes a stray syntax break in
  `GqlClient.fetch_bridge_extouts` introduced by an earlier
  docstring trim.

### Fixed

- **On-AN ABI artifacts realigned to shellnet (`acki-nacki@cf664666b`).**
  Dropped stale `anWorkchain int8` from `confirmDeposit` inputs and the
  `DepositFinalized` event in both runtime copies
  (`crates/an-bridge-prover/python/contracts/USDCBridge.abi.json` and
  `crates/bridge-withdraw-e2e-cli/abi/USDCBridge.abi.json`); rewrote
  `DepositVoucher.abi.json` constructor to the 5-arg
  `(depositId, contractAddr, dappId, amount, anAccount)` schema.
  Withdraw runtime paths (`initiateWithdrawal`, `mintAndSend`,
  `finalizeDeposit`) were already correct — no calldata change.
- `scripts/check_voucher_abi_consistency.py` default `--compiled` now
  points at `crates/an-bridge-prover/python/contracts/` (was a
  nonexistent path).

### Added

- `scripts/check_bridge_abi_in_sync.sh` — `cmp`-based guard that the
  two runtime `USDCBridge.abi.json` copies stay byte-identical.

### Removed

- **`relayer daemon-prover` subcommand deleted.** The legacy file-based
  standalone verifyBlock daemon (reads `proof_<seqno>.json` bundles
  from `PROVER_PROOFS_DIR`, submits `verifyBlock`) is superseded by
  `daemon-bridge` (file-based, both legs on one EOA) and `daemon-live`
  (in-process, GraphQL-driven, bundle-only). No active systemd unit,
  runbook, CI job, or E2E test invoked `daemon-prover` — the last
  reference was an "example manual/one-off invocation" in
  `AGENTS.md` which is also removed. Operators who need the standalone
  verifyBlock leg can still use `submit-verify-block` (one-shot) or
  `daemon-bridge` (long-running).
- `scripts/ursus/USDCBridge.abi.json` and
  `crates/an-bridge-prover/python/contracts/README.md` — unreferenced
  ABI mirror and its documentation. Systemd/env templates under
  `scripts/ursus/` retained.
- Fossil `.tvc` files under `python/contracts/`
  (`USDCBridge.tvc`, `DepositVoucher.tvc`); nothing loaded them.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
