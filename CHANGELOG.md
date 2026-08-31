# Acki Nacki Bridge Release Notes

All notable changes to the bridge are documented in this file — the contracts on
both chains, the ZK circuits and verification keys, the relayer and prover
binaries, and the deployment and operational surface around them.

Written for the people who deploy and run the bridge, not for the people who
wrote it. See the changelog policy in [AGENTS.md](AGENTS.md) for what belongs
here and how versions are assigned.

## [Unreleased]

### Added

- `eth-lc-relayer` (`crates/eth-light-client-relayer`): operator loop for the
  Ethereum beacon light-client oracle. Polls `GET /eth/v1/beacon/light_client/finality_update`,
  proves a step (subprocess `export_step_vk_blob` or `--mock-prove`), and calls
  `EthBeaconLightClient.submitUpdate`. CLI: `beacon-watch`, `prove-one`,
  `submit-one`, `daemon`. Live AN GraphQL submit is `--features live-submit`;
  default binary is shadow-capable (`--dry-run --mock-prove`). `--enable-rotate`
  stays off until tvm-sdk#284 is on every node. Does **not** flip
  `USDCBridge.finalizeDeposit` onto the oracle. systemd unit:
  `scripts/ursus/eth-light-client-relayer.service` (shadow flags in `ExecStart`).

### Changed

- `eth-light-client-prover` example `export_step_vk_blob` reads
  `FINALITY_UPDATE_PATH` when set (live beacon JSON from the relayer); otherwise
  it still uses the baked fixture.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
