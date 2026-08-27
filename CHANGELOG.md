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

- Production-oriented Docker Compose kit for the shellnet → Sepolia L2
  relayer under `crates/bridge-relayer-daemon/deploy/shellnet-l2/`. It includes
  a non-root read-only runtime image, external secret env template, bind-mount
  layout, full artifact/on-chain preflight, parameter finalization and an
  operator status command.
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
