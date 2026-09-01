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
  host); this CLI owns per-withdrawal composition. Runs the six-stage
  pipeline (preflight → idempotency reserve → burn → capture (with
  resurrect + coverage-wait as stage 4b) → Circuit-4 SHPLONK proof →
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
  prompting), `--dry-run` (**preflight-only** — validates flags, key
  file perms, single-custodian check, USDCBridge resolution, and the
  ECC[3] balance, then stops; does NOT compose the burn, produce the
  Circuit-4 proof, or run `dry_run_withdraw` on the EVM side),
  `--allow-retry` (blunt override of the duplicate-in-flight refusal;
  will be replaced by `--resume` in v2).

  Env vars consumed (each has a matching `--flag`): `BRIDGE_GQL_ENDPOINT`,
  `USDC_BRIDGE_ACCOUNT_ID` (shellnet default `1a1a…1a1a`),
  `RPC_URL`, `BRIDGE_ADDRESS`,
  `BURNER_PRIVATE_KEY` (signer for `withdrawByProof`; distinct from
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

- **`crates/bridge-withdraw-e2e-cli/scripts/deploy_msig_and_mint.{sh,py}`**
  — one-shot helper that deploys a fresh single-custodian
  `UpdateCustodianMultisigWallet` on the target AN cluster (default
  MODE=shellnet) and seeds it with 1 USDC on ECC[3] via
  `USDCBridge.mintAndSend`. Reuses `python/helper/msig.py::deploy_multisig`
  (same code path the full-orchestrator drivers use); stops at the
  ECC[3] mint — does NOT fire `initiateWithdrawal`, does NOT run any
  Rust binary. Emits eval-able `export WITHDRAW_FROM=…` and `export
  WITHDRAW_FROM_KEYS=…` lines on stdout (everything else on stderr),
  so the caller can `eval "$(scripts/deploy_msig_and_mint.sh)"` and
  proceed directly to `scripts/local_smoke.sh`. Fills the previous
  gap where the CLI's scripts dir assumed the multisig was already
  deployed by hand. Env overrides: `MODE`, `NETWORK`, `GRAPHQL_URL`,
  `WORK_DIR`, `USDC_BRIDGE_KEY_PATH`.

- Production-oriented Docker Compose kit for the shellnet → Sepolia L2
  relayer under `crates/bridge-relayer-daemon/deploy/shellnet-l2/`. It includes
  a non-root read-only runtime image, external secret env template, bind-mount
  layout, full artifact/on-chain preflight, parameter finalization and an
  operator status command. The service uses `restart: unless-stopped` for
  host/Docker recovery and reruns the fail-closed preflight on every start.
- The `bridge-evm-aggregator` lockfile is now tracked so target-host and image
  builds can use `cargo build --locked` reproducibly.
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

- **`bridge-withdraw-e2e-cli/scripts/local_smoke.sh` and `live_smoke.sh`
  now source the standalone `config/bridge_config` by default** instead
  of the relayer's `L1_config/env`. Aligns the scripts with the
  runbook (§"Quick resume checklist") — the CLI is a standalone tool
  with its own single config, so its smoke wrappers should not depend
  on a co-located relayer deploy. Escape hatch: set
  `BRIDGE_CONFIG_DIR=/path/to/L{1,2}_config` to fall back to the
  relayer-style env. Both scripts now `cd` to
  `crates/bridge-withdraw-e2e-cli/` (was `bridge/`), so relative
  `BRIDGE_PARAMS_DIR=../an-bridge-prover/params` in `bridge_config`
  resolves correctly. Both scripts also now require
  `BURNER_PRIVATE_KEY` in caller env (mirrors the deliberate omission
  in `bridge_config` — see runbook §"Wallet setup"). `live_smoke.sh`
  additionally passes `--anchor-layer 2 --i-know-the-wait` by default
  to match the L2-anchored reference deploy pinned in `bridge_config`
  (`0x3fB082…8638`); override with `ANCHOR_LAYER=1` in caller env for
  advanced-user L1 deploys.

- **`bridge-withdraw-e2e-cli --allow-retry` now resumes in place
  instead of overwriting the state file.** v1 used to rewrite the prior
  record with a fresh `Reserved` on `--allow-retry`, dropping the
  stored `an_tx_hash`; the orchestrator then unconditionally re-fired
  `burn::fire`, causing a second multisig `sendTransaction` for the
  same withdrawal (a double-spend on the AN side, since the multisig
  has no EVM-style nonce guard). Post-fix, `--allow-retry` keeps the
  prior record verbatim and the orchestrator skips any stage whose
  outputs are already on file — burn is never re-broadcast once
  `an_tx_hash` is set. `Confirmed` and `Submitted` are terminal states
  that refuse `--allow-retry` outright (`Confirmed` already paid out;
  `Submitted` has an unresolved in-flight EVM tx that must be
  reconciled on-chain before any retry). `Failed` with an
  `an_tx_hash` on file resumes verbatim without `--allow-retry` (the
  only production writer of `Failed` is the post-burn
  `withdrawByProof` revert path, so the AN burn is already done —
  wiping would double-burn on retry). `Failed` without `an_tx_hash`
  (defensive branch for manual state edits) still triggers a
  clean-slate restart. See runbook §Idempotency semantics for the
  full state transition table.

- **`bridge-withdraw-e2e-cli` idempotency state file only transitions
  to `Submitted` after `withdrawByProof` returns a tx hash.**
  Previously the record was flipped to `Submitted` immediately before
  the `submit_withdraw` call, so an RPC error, wallet reject, or gas
  estimation failure would leave the on-disk record falsely claiming
  a tx was broadcast — subsequent runs would refuse-duplicate on
  `Submitted` even though nothing ever hit Sepolia. Post-fix,
  `Submitted → Confirmed` are both written only inside the `Paid`
  branch after the tx hash is in hand.

- **`bridge-withdraw-e2e-cli` prompts before the AN burn unless
  `--yes` is passed.** The `--yes` and `--non-interactive` flags,
  previously accepted by clap but never consulted, now gate a live
  stdin confirmation immediately before `burn::fire`. The prompt
  prints the source multisig, recipient + chain, USDC amount, target
  bridge, and anchor-mode wait estimate, then reads y/yes from stdin.
  Non-interactive stdin (no TTY) without `--yes` refuses with exit 2.
  `--non-interactive` without `--yes` continues to refuse upstream in
  `main::dispatch`.

- **`bridge-withdraw-e2e-cli` capture-stage errors now map to exit
  code 11 (`CaptureTimeout`) instead of 12 (`ProofFailed`).**
  `capture_targeted_withdrawal_event` failures — GQL unreachable,
  event never emitted, poll ceiling exceeded — are a
  reconcile-and-resume situation, not a Circuit-4 prover problem. The
  prior catch-all `ProofFailed` mapping misled operators into
  debugging the aggregator subprocess when the burn was in fact
  bounced or the AN GQL was down. The `wait_for_coverage` step (stage
  4b) still maps to `ProofFailed` because "waiting for the covering
  bundle" is a prove-path prerequisite.

- **`bridge-withdraw-e2e-cli --dry-run` docs corrected to
  preflight-only scope.** The README, runbook Step 4, runbook Case 8,
  and `scripts/local_smoke.sh` header previously claimed `--dry-run`
  ran the full pipeline up to and including `dry_run_withdraw`. The
  code (since the CLI landed) has always stopped after preflight,
  returning a stub `WithdrawSuccess`. Docs now match the code:
  argument shape, key perms, single-custodian check, USDCBridge
  resolution, ECC[3] balance — then stop. Exit codes 10–13 are
  unreachable under `--dry-run`.

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

- **`aggregate-proof` subprocess now receives an absolute
  `--inner-snark` path.** `SubprocessAggregator::aggregate` spawns the
  aggregator with `current_dir = aggregator_dir`; a relative snark path
  (the `bridge-withdraw-e2e-cli` default `--snark-dir=./shplonk-snark`
  hits this) resolved against the wrong CWD and the subprocess exited
  with `No such file or directory (os error 2)`. Canonicalized in
  `aggregator.rs::SubprocessAggregator::aggregate` before argv
  construction; the snark file always exists at that point (the inner
  prover just wrote it), so the canonicalize is safe. First observed
  running `bridge-withdraw-e2e-cli withdraw` against shellnet from the
  crate root without an explicit `--snark-dir` override.

- **`bridge-withdraw-e2e-cli` error output now walks the anyhow
  `#[source]` chain.** `output.rs::print_error` used to print only the
  top-level `Display`, so any wrapped `CliError::ProofFailed { source }`
  (and any nested `anyhow::Context`) was invisible — the user saw
  `Circuit4ShplonkPipeline::prove failed` with no hint that the real
  cause was, e.g., the aggregator subprocess stderr. Human-mode errors
  now emit indented `caused by [N]:` lines beneath the top-level
  message. `--json` mode is unchanged.

- **`bridge-withdraw-e2e-cli withdraw` preflight USDCBridge liveness
  check no longer misreports an Active bridge as `acc_type is Unknown`.**
  The GraphQL query at `preflight.rs::query_usdc_bridge_state` asked
  for `info.acc_type` (a numeric enum: `1 = Active`) and then tried to
  match it as a string, silently falling back to `"Unknown"` and
  failing the `== "Active"` check on every well-deployed shellnet
  bridge. Fixed by requesting the string-typed `info.acc_type_name`
  alias instead (`"Active"`/`"Uninit"`/…) and matching that. First
  observed on shellnet's canonical zerostate USDCBridge
  (`0000…::1a1a…`), which is Active per tvm-cli but was reported
  Unknown by the CLI.

- **`bridge-withdraw-e2e-cli withdraw` preflight `getCustodians` call
  no longer rejects the very `--from` form the CLI itself mandates.**
  Args validation requires `--from dapp_id::account_id` (the tvm-cli v3
  extended form) and rejects the legacy `0:<acc>` shape, but
  `preflight.rs` was threading that same extended string into
  `tvm_sdk::abi::encode_message`'s `address` field, which the ABI
  encoder does not accept and which failed with `Invalid address
  [Invalid argument: 0]`. The encoder path now uses `from.legacy()`
  (`0:<acc>`); `.extended()` is still used for the `get_account` BOC
  fetch (which does accept it) and for user-facing log lines. First
  observed running `scripts/local_smoke.sh --dry-run` on shellnet
  against a freshly-deployed single-custodian multisig; before the fix,
  every dry-run and every live withdraw would fail at preflight step 3
  regardless of on-chain state.

- **`bridge-withdraw-e2e-cli` no longer double-burns ECC[3] when
  retrying after a `withdrawByProof` revert.** The `Failed` arm of
  `idempotency::reserve` used to wipe the prior record with a fresh
  `Reserved`, dropping the recorded `an_tx_hash`. Because `Status::Failed`
  is only written by the post-burn `withdrawByProof` revert path
  (`orchestrator.rs::WithdrawSubmitOutcome::Reverted`), the burn had
  already broadcast — but the orchestrator's stage-3 resume branch
  reads `record.an_tx_hash.is_some()`, so wiping caused it to fall into
  the else branch and call `burn::fire()` a second time. Concrete
  failure mode: operator hits a `WithdrawTreasuryShortfall` (Case 3e /
  exit 13), tops up treasury, re-runs the same command → second
  `initiateWithdrawal` on the AN side, second ECC[3] debit for the
  same withdrawal. Post-fix, `Failed` with `an_tx_hash` present
  returns the prior record verbatim (resume path skips
  `burn::fire()`); only `Failed` without `an_tx_hash` (defensive
  branch for manual state edits) still wipes. Regression test:
  `idempotency::tests::reserve_over_failed_with_an_tx_hash_preserves_prior_record`.
- **`bridge-withdraw-e2e-cli` now refuses `--to
  0x0000000000000000000000000000000000000000` at preflight** (Sergey
  review R7). The USDCBridge ERC-20 leg could otherwise succeed against
  a token whose `transfer` treats the zero address as a burn sink,
  permanently consuming an ECC[3] draw against an unrecoverable
  recipient. Refusal path: `parse_to` in `args.rs`, `ArgInvalid { flag:
  "to", .. }`, exit 2. Test: `args::tests::to_rejects_zero_address`.
- **`bridge-withdraw-e2e-cli` now caps `--amount` at `u64::MAX`
  micro-USDC at preflight** (Sergey review R8). The multisig ECC[3]
  balance and the AN-side `initiateWithdrawal(amount)` argument are u64
  on the wire; letting a larger value through caused a silent downstream
  cast in `burn::fire` at broadcast time rather than a clean preflight
  refusal. Boundary: exactly `u64::MAX` micros (== `18446744073709.551615`
  USDC) is still accepted. Refusal path: `parse_amount` in `args.rs`,
  `ArgInvalid { flag: "amount", .. }`, exit 2. Tests:
  `args::tests::amount_rejects_above_u64_max_micro`,
  `args::tests::amount_accepts_u64_max_micro_exact`.
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
### Fixed

- GraphQL BK-update range queries now cap their open-ended upper bound at the
  schema's signed 64-bit `Int` maximum instead of serializing `u64::MAX`, which
  live GraphQL servers reject during integer coercion.
- L2 warm-resume startup now compares the immutable genesis anchor at
  `anchor_level - 1`; a valid level-2 state no longer fails drift validation
  after a clean restart.
- The live relayer runbook now provisions the required K=22 SRS, documents
  runtime `solc 0.8.19`, isolates the relayer cursor per mode, treats deploys
  as irreversible broadcasts, keeps production secrets outside Git and
  reflects the prover's sequential-stage but multi-core execution model. The
  deployment helper no longer writes a private key into tracked config and
  archives previous prover state instead of deleting it; an explicit
  `CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS` gate is now required before
  any broadcast.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
