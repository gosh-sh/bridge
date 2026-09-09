# Acki Nacki Bridge Release Notes

All notable changes to the bridge are documented in this file — the contracts on
both chains, the ZK circuits and verification keys, the relayer and prover
binaries, and the deployment and operational surface around them.

Written for the people who deploy and run the bridge, not for the people who
wrote it. See the changelog policy in [AGENTS.md](AGENTS.md) for what belongs
here and how versions are assigned.

## [Unreleased]

### Added

- `docs/eth-light-client.md`: design and deployment reference for the beacon
  light client (components, step update, period rotation, trust switches,
  deployment topology with ports and endpoints, configuration, operating
  numbers), with Mermaid diagrams.
- `eth-lc-relayer` resolves the beacon signing domain from the node it polls:
  `genesis_validators_root` (`/eth/v1/beacon/genesis`) and the fork schedule
  (`/eth/v1/config/spec`), picking the `fork_version` active at the update's
  `signature_slot`, and hands the pair to the prover as `BEACON_FORK_VERSION` /
  `BEACON_GENESIS_VALIDATORS_ROOT`. Sepolia (and any other network with the
  mainnet preset) now proves without code changes; the prover still defaults
  to mainnet Fulu when the variables are unset. `beacon-watch` prints them.
  The state file is pinned to the first `genesis_validators_root` it sees and
  refuses a source on another network.
- `eth-lc-relayer daemon --no-rotate --owner-hop`: on a period jump the
  daemon proves a step of the new period, advances the committee with the
  owner key from that proof's commitment (`setCommitteeCommitment`) and
  submits the same bundle, instead of stopping at `RotateRequired`. Shadow
  only (refused after `disableOwnerRotation`).
- `eth-lc-relayer set-committee --bundle-dir …`: owner
  `setCommitteeCommitment(commitment, period)` from a proven step bundle
  (public-input word 5), recording the period in the state file. Bootstraps a
  fresh `EthBeaconLightClient` (weak-subjectivity anchor) and hops periods
  while `--no-rotate`.
- `crates/eth-light-client-relayer/deploy/shellnet-shadow/`: operator kit for
  a shadow instance on shellnet against Sepolia (build, SRS install, contract
  compile with the Linux `sold` release, giver funding, deploy, status,
  systemd unit, README).
- `eth-lc-relayer` (`crates/eth-light-client-relayer`): operator loop for the
  Ethereum beacon light-client oracle. Polls `finality_update` **and**
  `light_client/updates` (current 512-committee), proves a step via
  `export_step_vk_blob` with `COMMITTEE_JSON_PATH` (real keys + bits + signature,
  not OsRng), and calls `EthBeaconLightClient.submitUpdate`. CLI:
  `beacon-watch`, `prove-one`, `submit-one`, `submit-rotate`, `ancestry-one`,
  `submit-ancestry`, `flip-owner`, `daemon`. Live AN submit is `--features live-submit`. systemd
  unit is the live loop (no hardcoded `--dry-run --mock-prove`; rotate **on** by
  default, `--no-rotate` opts out). After the first accepted `submitUpdate` the
  daemon issues `setLightClient` + `disableOwnerAnchors` +
  `disableOwnerRotation` (`--no-flip-owner` opts out; one-shot:
  `eth-lc-relayer flip-owner`). Relayer keys must be the owner pubkey.
  `AN_USDC_BRIDGE` / `AN_USDC_ABI_PATH`. tvm-sdk#284 co-deploys with this contract.
  Epoch ancestry **on-chain**: `EthBeaconLightClient.submitAncestry(bytes[]
  headerRlps)` keccak256-binds each execution header and walks `parentHash` to a
  proven checkpoint (≤ 31 parents), then pushes those hashes into
  `USDCBridge._acceptedBlockHash`. The daemon walks that chain after every
  accepted `submitUpdate` when `ETH_RPC_URL` is set (`--eth-rpc-url`); it also
  calls `rePushAnchor` so a bounce before `setLightClient` is retried. Operator
  one-shot: `eth-lc-relayer submit-ancestry --eth-rpc-url … --checkpoint-hash
  0x…`. Contracts: `contracts/an/EthKeccak.sol`,
  `contracts/an/EthBeaconLightClient.sol` (`EthBeaconLightClient_rotate_decider.patch`
  for the acki-nacki tree; `scripts/check_eth_beacon_lc_sources.sh` keeps them
  in lockstep). `updateCode` / `onCodeUpgrade` persist the committee, head,
  proven-hash set and `reAnchorsApplied` across a VkBlob rotation.
  `reAnchorCommittee` is the logged weak-subjectivity hatch after
  `disableOwnerRotation` (does not write exec hashes; `getCommitteeState`
  exposes `reAnchorsApplied`). Shellnet E2E:
  `scripts/ursus/eth_lc_shellnet_e2e.md`. Audit scope:
  `eth-light-client-prover/docs/m_audit_scope.md`.

### Fixed

- `EthBeaconLightClient._pushExecHash` sent the `acceptBlockHashFromLightClient`
  message to `addr_none` when no `USDCBridge` was configured: the unset
  `_usdcBridge` is `addr_none`, not `address(0)`, so the guard passed, the
  action phase aborted with result code 34 and the whole `submitUpdate` was
  rolled back although the proof had verified. Guard is now
  `!_usdcBridge.isNone() && _usdcBridge != address(0)` (now in `_notifySink`,
  so `rePushAnchor` is covered too). Observed on the first shellnet shadow
  deploy (2026-09-04). `EthBeaconLightClient_rotate_decider.patch` regenerated.

### Changed

- `EthBeaconLightClient` keeps proven execution hashes for **one year** of
  Ethereum slots (`SLOTS_PER_YEAR = 2_628_000`). `isProven` / `isAcceptedBlockHash`
  are false outside that window; `rePushAnchor` and `submitAncestry` refuse an
  expired hash. A FIFO compact (128 entries per tx) deletes the keys and calls
  `forgetBlockHashFromLightClient` on the sink so `USDCBridge._acceptedBlockHash`
  cannot outlive the oracle. The bridge method is `USDCBridge_forget_block_hash_from_light_client.patch`
  (same sender gate as `acceptBlockHashFromLightClient`, idempotent `delete`). `updateCode` encoding of the proven set changed
  (`mapping(hash => slot)` + queue); existing shadow deployments cannot carry the
  old `mapping => bool` across this upgrade — redeploy or re-prove from the
  checkpoint. Off-chain replica: `crates/eth-light-client-relayer/src/contract_model.rs`.
- **Step VK rotated: `bd108c08…` → `2d66c205…`.** `execution.rs` padded
  `extra_data` (List[byte,32]) with `load_constant`, so the constraint system
  carried `32 - len` extra constant-equality cells and the VK depended on the
  finalized block's `extra_data` length. The fixture VK was emitted over a
  27-byte mainnet `extra_data`; a 25-byte Sepolia block produced a different
  VkBlob and would have been rejected by the deployed contract. The chunk is
  now a zero-padded 32-byte witness (soundness unchanged: the payload root is
  bound to the signed state by `execution_branch`). Regression test
  `execution_root_shape_is_independent_of_extra_data_len`. Fixture
  `eth-light-client-prover/fixtures/step_vkblob/` and the `VK_BLOB` in
  `contracts/an/EthBeaconLightClient.sol` re-emitted,
  `EthBeaconLightClient_rotate_decider.patch` regenerated from it; the tvm-sdk
  opcode fixtures still carry the old blob and need the same rotation
  (`scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh`). Verified on Sepolia: the same blob comes
  out of the mainnet fixture (27 B), a Sepolia block with 25 B and one with
  18 B of `extra_data`.
- `crates/eth-light-client-relayer` builds with `--features live-submit`
  outside the tvm-sdk workspace: the crate manifest now mirrors tvm-sdk's
  `[patch]` tables (gosh `halo2-axiom` / `halo2-lib` / `axiom-eth` forks);
  before, cargo resolved two `halo2_axiom` versions and `tvm_vm` failed to
  compile.
- `prove-one` and the daemon keep the prover transcript
  (`prover-stdout.log` / `prover-stderr.log`) next to the bundle and report the
  stderr tail on failure instead of a bare exit status.
- `eth-lc-relayer daemon` rotates on a period jump by default (`submitRotate`)
  and, after the first accepted `submitUpdate`, issues the one-way owner flip
  (`USDCBridge.setLightClient` + `disableOwnerAnchors`,
  `EthBeaconLightClient.disableOwnerRotation`). `--no-rotate` / `--no-flip-owner`
  are the shadow/laptop opt-outs. Relayer keys must be the owner pubkey.
  `disableOwnerAnchors` succeeds when `_lightClient` is set (not only when an
  attester quorum exists). With `ETH_RPC_URL` the same tick then `rePushAnchor`s
  the checkpoint and `submitAncestry`s the epoch parent chain. `submitUpdate`
  late-registers a skipped checkpoint of the current committee (`CheckpointBackfilled`,
  head not rewound). Sink notify uses `bounce: true`; a drop emits
  `AnchorPushBounced` and is retried via `rePushAnchor`. `encode_header_rlp`
  fails closed when `keccak256(rlp)` does not match the node's `block.hash`.
- `export_step_vk_blob` reads `FINALITY_UPDATE_PATH` and, when
  `COMMITTEE_JSON_PATH` / `BOOTSTRAP_PATH` is set, builds a **live** step
  witness (real sync committee). Unset committee path still emits a synthetic
  committee for VkBlob-only keygen.

### Changed

- **`ackinacki-bridge withdraw` invocation reduced to five per-request
  flags; all network endpoints/plumbing resolved from `$BRIDGE_CONFIG`
  profile file.**
  The withdraw call is now:
  ```
  ackinacki-bridge withdraw \
      --from <dapp_id::account_id> --from-keys <path> \
      --to <0x…> --to-chain <chain-id> --amount <usdc> \
      [--dry-run] [--yes] [--json]
  ```
  Everything else (`--rpc-url`, `--bridge-address`, `--gql-endpoint`,
  `--anchor-layer`, `--i-know-the-wait`, `--params-dir`,
  `--aggregator-dir`, `--verifiers-dir`, `--work-dir`, `--snark-dir`,
  `--pk-cache-dir`, `--state-dir`, `--usdc-bridge-account`) is picked
  up from the profile file pointed to by `$BRIDGE_CONFIG` — the CLI
  auto-sources it at startup via `dotenvy` before clap reads any
  `env=` attr. Precedence: **explicit `--flag` > shell env > profile
  file > compiled default**. Existing `--flag`-heavy invocations
  continue to work.
    - `config/bridge_config` is now a **symlink** to the new
      `config/bridge_config.shellnet` (identical content to the
      pre-split file). Two sibling profiles ship alongside:
      `bridge_config.local` (local docker-compose devnet) and
      `bridge_config.mainnet` (commented-out placeholder for the
      future mainnet deploy). Switching network is a one-liner:
      `export BRIDGE_CONFIG=./config/bridge_config.local`.
    - Four flags gained `env=` attrs so they can live in the profile
      file instead of the CLI invocation: `BRIDGE_ANCHOR_LAYER`,
      `BRIDGE_I_KNOW_THE_WAIT` (accepts `true`/`false`/`1`/`0`),
      `BRIDGE_WORK_DIR`, `BRIDGE_SNARK_DIR`.
    - The hardcoded shellnet default on `--usdc-bridge-account`
      (`1a1a…1a1a`) is removed from `src/args.rs`; the value now
      comes from `USDC_BRIDGE_ACCOUNT_ID` in each profile (so a
      wrong-network profile fails loudly with a clap "missing
      required argument" instead of silently talking to the shellnet
      canonical account).
    - `scripts/local_smoke.sh` and `scripts/live_smoke.sh` drop
      ~90 lines each — they now just export `$BRIDGE_CONFIG`,
      canonicalize `$BRIDGE_SNARK_DIR` to absolute (the aggregator
      subprocess CWD-changes), and pass only the 5 intent flags.
      The old `BRIDGE_CONFIG_DIR` env-var fallback for relayer-style
      `L{1,2}_config/env` sourcing is removed — self-deploy users
      should either point `$BRIDGE_CONFIG` at their local profile or
      shadow `BRIDGE_ADDRESS` in the shell.
    - `scripts/deploy_msig_and_mint.py` drops its `MODE=shellnet|local`
      branch and reads `NETWORK` / `BRIDGE_GQL_ENDPOINT` /
      `USDC_BRIDGE_KEY_PATH` from the same profile file. Local
      detection is now derived from the resolved `NETWORK` URL
      (`127.*` / `localhost`), not a mode flag. Adding a new network
      is a single new profile file — no Python edit.
    - New dep: `dotenvy = "0.15"` in `crates/ackinacki-bridge/Cargo.toml`.

- **`ackinacki-bridge` docs split into a default-user runbook (README)
  and an advanced self-deploy runbook.**
  The previous single `docs/live_cli_withdraw_runbook.md` was one long
  file covering both audiences (default users hitting the pinned
  shellnet deploy AND advanced users deploying their own bridge +
  relayer). It is now split so each audience gets a doc scoped to
  their path:
    - `crates/ackinacki-bridge/README.md` — default-user runbook.
      Wallet setup → `scripts/deploy_msig_and_mint.sh` (fresh
      single-custodian AN multisig + 1 USDC seed) → treasury check
      → dry-run → real submit, with the full exemplary `cargo run`
      command inlining every real value (RPC/GQL URLs, pinned
      `BRIDGE_ADDRESS 0x0F4F…fc7`, relative paths for
      `--params-dir`, `--aggregator-dir`, `--verifiers-dir`).
      Includes simplified timing model (L2 only), exit codes,
      per-scenario error summaries, idempotency semantics, safety,
      file layout.
    - `crates/ackinacki-bridge/docs/live_cli_withdraw_runbook.md` →
      renamed to `docs/advanced_user_withdraw_runbook.md`. Scope
      narrowed to what advanced users need beyond the README: full
      L1 vs L2 timing model, self-deploy sequence
      (Steps L0–L5: `compute_bridge_anchors` →
      `deploy_bridge_bundle.sh` → treasury seed → cold-start
      daemon → wait for first bundle → CLI), stress-test loop,
      and the deep failure-mode catalog (revert-selector table,
      cast-trace decoding, USDCBridge keypair-drift diagnostic).
      Cross-references the README for the CLI invocation shape
      instead of duplicating it.
    - Internal references in `crates/ackinacki-bridge/config/bridge_config`
      updated to point at the new file names.

### Breaking Changes

- **Sub-workspace directory renamed `crates/an-bridge-prover/` → `crates/bridge-prover-libraries/`.**
  The prover-side sub-workspace root (the one that holds
  `bridge-prover-lib`, `bridge-prover-daemon`, `bridge-verifier-daemon`,
  `bridge-event-witness`, `bridge-event-prover-lib`,
  `bridge-event-halo2-prover`, `bridge-gql-fetcher`, `bridge-snark-wrap`
  and the `bridge-relayer-daemon` / `ackinacki-bridge` symlinks) moves
  under a self-describing name. The workspace exclude in the root
  `Cargo.toml`, every path-dep resolving through the sub-workspace,
  every runbook + script + env file + CI reference has been repointed.
  All Cargo package names inside the sub-workspace are unchanged (only
  the parent directory moved), so `cargo -p bridge-prover-lib` etc. work
  unchanged. Concrete migrations:
    - `cd crates/an-bridge-prover` → `cd crates/bridge-prover-libraries`
      everywhere (docs, scripts, systemd unit files, personal shell
      history).
    - `relayer prove-withdraw --an-bridge-prover-dir …` →
      `--bridge-prover-libraries-dir …`; env var
      `AN_BRIDGE_PROVER_DIR` → `BRIDGE_PROVER_LIBRARIES_DIR`.
    - `BRIDGE_PARAMS_DIR=../an-bridge-prover/params` (in the standalone
      CLI `bridge_config`) → `../bridge-prover-libraries/params`.
    - Compose / systemd bind-mounts pointing at
      `crates/an-bridge-prover/…` need the same path swap.
    - Any personal `.env.local` or shell profile setting
      `AN_BRIDGE_PROVER_DIR=…` needs renaming to
      `BRIDGE_PROVER_LIBRARIES_DIR=…`.

- **CLI crate + binary renamed `bridge-withdraw-e2e-cli` → `ackinacki-bridge`.**
  Cargo package name, `cargo -p …` selector, `[[bin]]` name, on-disk
  crate directory (`crates/bridge-withdraw-e2e-cli/` → `crates/ackinacki-bridge/`),
  and the `bridge-prover-libraries` sub-workspace symlink all move together.
  All existing functionality is now nested under the `withdraw`
  subcommand (already the case in earlier `[Unreleased]` entries — the
  rename is purely cosmetic, made ahead of adding sibling subcommands
  like `deposit`). Concrete migrations:
    - Build: `cargo build -p ackinacki-bridge` (was `-p bridge-withdraw-e2e-cli`),
      run from `crates/bridge-prover-libraries/`.
    - Invoke: `./target/release/ackinacki-bridge withdraw --from … --to …`
      (was `./target/release/bridge-withdraw-e2e-cli withdraw …`).
    - Log filter: `RUST_LOG=ackinacki_bridge=info` (was `bridge_withdraw_e2e_cli=info`).
    - Config: `config/bridge_config` moved with the crate to
      `crates/ackinacki-bridge/config/bridge_config`; contents unchanged.
    - Smoke scripts (`scripts/local_smoke.sh`, `live_smoke.sh`,
      `deploy_msig_and_mint.{sh,py}`) moved with the crate; behaviour unchanged.

- **`contracts/ethereum/.env.shellnet` and `.env.shellnet.l2` deleted.**
  The single shared shellnet burner (`0xb586…2307`) now lives once, in
  `crates/bridge-prover-libraries/shellnet.common` under `RELAYER_PRIVATE_KEY`,
  and covers both the deployer and the relayer role. Manual deploy
  paths that used to `set -a && source contracts/ethereum/.env.shellnet
  && forge script …` are retired — use
  `PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL={1,2}
  ./scripts/deploy_bridge_bundle.sh` from `crates/bridge-prover-libraries/`
  instead (the wrapper re-derives genesis anchors from live chain head
  and writes `L{1,2}_config/env` atomically). Per-deploy provenance
  (Deploy #8 / #12 anchors) remains in git history.

- **Per-mode config directory layout in `crates/bridge-prover-libraries/`.**
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

- **`relayer daemon-live` GraphQL failover + retry.** The daemon now takes a
  primary Acki Nacki GraphQL endpoint (`--gql-endpoint` /
  `BRIDGE_GQL_ENDPOINT`, unchanged) plus an optional ordered failover list
  `--gql-failover-endpoints` / `BRIDGE_GQL_FAILOVER_ENDPOINTS` (comma-separated,
  e.g. `http://bm1:8080/graphql,http://bm2:8080/graphql`). Every GraphQL
  request starts at the primary, retries it 3 times 1 s apart, then moves to
  the next endpoint, and cycles through the whole list until one attempt
  succeeds — there is no stickiness and no give-up: a request that never
  succeeds blocks the daemon tick, which is what the new metrics and the
  error-rate alert are for. Any failure counts: transport error, timeout,
  non-2xx status, undecodable body, a GraphQL `errors` array, and a `null`
  block for the block/attestation/bk-set-update queries the daemon depends on.
  Tuning (all optional): `BRIDGE_GQL_RETRIES_PER_ENDPOINT` (3),
  `BRIDGE_GQL_RETRY_DELAY_MS` (1000), `BRIDGE_GQL_REQUEST_TIMEOUT_SECS` (30),
  `BRIDGE_GQL_CONNECT_TIMEOUT_SECS` (10, new — a black-holed endpoint no longer
  costs the full request timeout per attempt), `BRIDGE_GQL_MAX_ROUNDS` (unset =
  loop forever). Other `bridge-gql-fetcher` users (`bridge-prover-daemon`,
  `bridge-verifier-daemon`, `compute_bridge_anchors`, `ackinacki-bridge`,
  `relayer withdraw-e2e`) keep the previous single-attempt behaviour.
- **`relayer daemon-live --metrics-addr` / `RELAYER_METRICS_ADDR`** (e.g.
  `0.0.0.0:9464`) starts a Prometheus text exporter at `GET /metrics`. Unset =
  no listener. First metrics, all labelled by GraphQL `endpoint` and `op`:
  `relayer_gql_requests_total` (attempts), `relayer_gql_errors_total{kind}`
  (`transport|timeout|http_status|decode|graphql_error|null_data`),
  `relayer_gql_failovers_total{from,to}`, `relayer_gql_full_rounds_total` (a
  request went through every endpoint without success) and the histogram
  `relayer_gql_request_duration_seconds{outcome}`. Alert example:
  `sum(rate(relayer_gql_errors_total[1m])) * 60 > 10`.
- Compose kit (`crates/bridge-relayer-daemon/deploy/shellnet-l2/`): the
  `relayer` service now sets `RELAYER_METRICS_ADDR=0.0.0.0:9464` and publishes
  it on `${RELAYER_METRICS_LISTEN:-127.0.0.1:9464}` (set the scrape-network
  address in `.env`); `runtime.env.example` gained
  `BRIDGE_GQL_FAILOVER_ENDPOINTS`; `preflight.sh` (run by the container
  entrypoint on every start) now fails only when neither the primary nor any
  failover endpoint answers, so one dead Block Manager no longer keeps the
  container in a restart loop; `status.sh` queries the endpoints in the same
  order and prints the `relayer_gql_*` counters.

- **New binary `ackinacki-bridge`** — end-user CLI for withdrawing
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
  ackinacki-bridge withdraw \
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

- **Helper scripts under `crates/ackinacki-bridge/scripts/`**:
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

- **`crates/ackinacki-bridge/scripts/deploy_msig_and_mint.{sh,py}`**
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
- New launch scripts under `crates/bridge-prover-libraries/scripts/` that
  source the per-mode env file: `launch_withdraw_e2e.sh` (dry-run L1),
  `launch_withdraw_e2e_real.sh` (real submit L1),
  `launch_withdraw_e2e_l2.sh` (dry-run L2), `replay_withdraw_shplonk.sh`
  (offline replay against a retained enriched witness). All emit
  per-mode absolute snark-dir paths so the aggregate-proof subprocess
  finds the intermediate `.snark` file.

### Changed

- **`ackinacki-bridge/config/bridge_config` pinned `BRIDGE_ADDRESS`
  rotated to the newly team-deployed L2 shellnet bridge
  `0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7`** (was
  `0x8D9190666128ab897C5ABd8C107239A197e08467`). The new deploy's
  `usdc()` points at a **real Circle FiatToken**
  (`0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`), not the previous
  bridge's mint-anyone test token (`0x94a9D9…5e4C8`) — so Case 1a
  Step 2 (treasury seeding) can no longer use the
  `0xC959…3f42D` faucet's `mint(address,address,uint256)`. Confirm
  the correct token with `cast call $BRIDGE_ADDRESS 'usdc()(address)'`
  and use whatever balance the burner already holds on that token, or
  ask a Circle-token minter for more. First successful E2E withdraw
  against the new deploy: AN burn
  `0xf2c407d2476803970f0ef68c4d3f8e888f9d0b34e23e532210d9b2426f498769`
  → ETH tx
  `0x7ccefdae4cca5812d339c969e69ab4242f7c4d6cf6c503265b658201fe30bf1b`
  (0.9 USDC, ~25 min wall time — the 23 min tail was remote-relayer
  latency waiting for the covering L2 boundary at seq 12,812,288).

- **`ackinacki-bridge/scripts/local_smoke.sh` and `live_smoke.sh`
  now source the standalone `config/bridge_config` by default** instead
  of the relayer's `L1_config/env`. Aligns the scripts with the
  runbook (§"Quick resume checklist") — the CLI is a standalone tool
  with its own single config, so its smoke wrappers should not depend
  on a co-located relayer deploy. Escape hatch: set
  `BRIDGE_CONFIG_DIR=/path/to/L{1,2}_config` to fall back to the
  relayer-style env. Both scripts now `cd` to
  `crates/ackinacki-bridge/` (was `bridge/`), so relative
  `BRIDGE_PARAMS_DIR=../bridge-prover-libraries/params` in `bridge_config`
  resolves correctly. Both scripts also now require
  `BURNER_PRIVATE_KEY` in caller env (mirrors the deliberate omission
  in `bridge_config` — see runbook §"Wallet setup"). `live_smoke.sh`
  additionally passes `--anchor-layer 2 --i-know-the-wait` by default
  to match the L2-anchored reference deploy pinned in `bridge_config`
  (`0x3fB082…8638`); override with `ANCHOR_LAYER=1` in caller env for
  advanced-user L1 deploys.

- **`ackinacki-bridge --allow-retry` now resumes in place
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

- **`ackinacki-bridge` idempotency state file only transitions
  to `Submitted` after `withdrawByProof` returns a tx hash.**
  Previously the record was flipped to `Submitted` immediately before
  the `submit_withdraw` call, so an RPC error, wallet reject, or gas
  estimation failure would leave the on-disk record falsely claiming
  a tx was broadcast — subsequent runs would refuse-duplicate on
  `Submitted` even though nothing ever hit Sepolia. Post-fix,
  `Submitted → Confirmed` are both written only inside the `Paid`
  branch after the tx hash is in hand.

- **`ackinacki-bridge` prompts before the AN burn unless
  `--yes` is passed.** The `--yes` and `--non-interactive` flags,
  previously accepted by clap but never consulted, now gate a live
  stdin confirmation immediately before `burn::fire`. The prompt
  prints the source multisig, recipient + chain, USDC amount, target
  bridge, and anchor-mode wait estimate, then reads y/yes from stdin.
  Non-interactive stdin (no TTY) without `--yes` refuses with exit 2.
  `--non-interactive` without `--yes` continues to refuse upstream in
  `main::dispatch`.

- **`ackinacki-bridge` capture-stage errors now map to exit
  code 11 (`CaptureTimeout`) instead of 12 (`ProofFailed`).**
  `capture_targeted_withdrawal_event` failures — GQL unreachable,
  event never emitted, poll ceiling exceeded — are a
  reconcile-and-resume situation, not a Circuit-4 prover problem. The
  prior catch-all `ProofFailed` mapping misled operators into
  debugging the aggregator subprocess when the burn was in fact
  bounced or the AN GQL was down. The `wait_for_coverage` step (stage
  4b) still maps to `ProofFailed` because "waiting for the covering
  bundle" is a prove-path prerequisite.

- **`ackinacki-bridge --dry-run` docs corrected to
  preflight-only scope.** The README, runbook Step 4, runbook Case 8,
  and `scripts/local_smoke.sh` header previously claimed `--dry-run`
  ran the full pipeline up to and including `dry_run_withdraw`. The
  code (since the CLI landed) has always stopped after preflight,
  returning a stub `WithdrawSuccess`. Docs now match the code:
  argument shape, key perms, single-custodian check, USDCBridge
  resolution, ECC[3] balance — then stop. Exit codes 10–13 are
  unreachable under `--dry-run`.

- **`ackinacki-bridge` capture is now multi-user safe.**
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
  moved to new `crates/bridge-prover-libraries/python/helper/msig.py`;
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
  (the `ackinacki-bridge` default `--snark-dir=./shplonk-snark`
  hits this) resolved against the wrong CWD and the subprocess exited
  with `No such file or directory (os error 2)`. Canonicalized in
  `aggregator.rs::SubprocessAggregator::aggregate` before argv
  construction; the snark file always exists at that point (the inner
  prover just wrote it), so the canonicalize is safe. First observed
  running `ackinacki-bridge withdraw` against shellnet from the
  crate root without an explicit `--snark-dir` override.

- **`ackinacki-bridge` error output now walks the anyhow
  `#[source]` chain.** `output.rs::print_error` used to print only the
  top-level `Display`, so any wrapped `CliError::ProofFailed { source }`
  (and any nested `anyhow::Context`) was invisible — the user saw
  `Circuit4ShplonkPipeline::prove failed` with no hint that the real
  cause was, e.g., the aggregator subprocess stderr. Human-mode errors
  now emit indented `caused by [N]:` lines beneath the top-level
  message. `--json` mode is unchanged.

- **`ackinacki-bridge withdraw` preflight USDCBridge liveness
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

- **`ackinacki-bridge withdraw` preflight `getCustodians` call
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

- **`ackinacki-bridge` no longer double-burns ECC[3] when
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
- **`ackinacki-bridge` now refuses `--to
  0x0000000000000000000000000000000000000000` at preflight** (Sergey
  review R7). The USDCBridge ERC-20 leg could otherwise succeed against
  a token whose `transfer` treats the zero address as a burn sink,
  permanently consuming an ECC[3] draw against an unrecoverable
  recipient. Refusal path: `parse_to` in `args.rs`, `ArgInvalid { flag:
  "to", .. }`, exit 2. Test: `args::tests::to_rejects_zero_address`.
- **`ackinacki-bridge` now caps `--amount` at `u64::MAX`
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
  (`crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json` and
  `crates/ackinacki-bridge/abi/USDCBridge.abi.json`); rewrote
  `DepositVoucher.abi.json` constructor to the 5-arg
  `(depositId, contractAddr, dappId, amount, anAccount)` schema.
  Withdraw runtime paths (`initiateWithdrawal`, `mintAndSend`,
  `finalizeDeposit`) were already correct — no calldata change.
- `scripts/check_voucher_abi_consistency.py` default `--compiled` now
  points at `crates/bridge-prover-libraries/python/contracts/` (was a
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
  `crates/bridge-prover-libraries/python/contracts/README.md` — unreferenced
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
