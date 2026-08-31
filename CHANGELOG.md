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
  Ethereum beacon light-client oracle. Polls `finality_update` **and**
  `light_client/updates` (current 512-committee), proves a step via
  `export_step_vk_blob` with `COMMITTEE_JSON_PATH` (real keys + bits + signature,
  not OsRng), and calls `EthBeaconLightClient.submitUpdate`. CLI:
  `beacon-watch`, `prove-one`, `submit-one`, `submit-rotate`, `ancestry-one`,
  `submit-ancestry`, `daemon`. Live AN submit is `--features live-submit`. systemd
  unit is the live loop (no hardcoded `--dry-run --mock-prove`; rotate **on** by
  default, `--no-rotate` opts out). tvm-sdk#284 co-deploys with this contract.
  Flip (`disableOwnerAnchors` + `disableOwnerRotation`):
  `scripts/ursus/flip_deposit_to_light_client.md`.
  Epoch ancestry **on-chain**: `EthBeaconLightClient.submitAncestry(bytes[]
  headerRlps)` keccak256-binds each execution header and walks `parentHash` to a
  proven checkpoint (≤ 31 parents), then pushes those hashes into
  `USDCBridge._acceptedBlockHash`. Operator: `eth-lc-relayer submit-ancestry
  --eth-rpc-url … --checkpoint-hash 0x…` (`ETH_RPC_URL`). Contracts:
  `contracts/an/EthKeccak.sol`, `contracts/an/EthBeaconLightClient.sol`
  (`EthBeaconLightClient_rotate_decider.patch` for the acki-nacki tree).
  Shellnet E2E:
  `scripts/ursus/eth_lc_shellnet_e2e.md`. Audit scope:
  `eth-light-client-prover/docs/m_audit_scope.md`.

### Changed

- `eth-lc-relayer daemon` rotates on a period jump by default (`submitRotate`).
  `--no-rotate` is the shadow/laptop opt-out. tvm-sdk#284 co-deploys with
  `EthBeaconLightClient`; the deposit flip calls `USDCBridge.disableOwnerAnchors`
  and `EthBeaconLightClient.disableOwnerRotation` in the same rollout.
- `export_step_vk_blob` reads `FINALITY_UPDATE_PATH` and, when
  `COMMITTEE_JSON_PATH` / `BOOTSTRAP_PATH` is set, builds a **live** step
  witness (real sync committee). Unset committee path still emits a synthetic
  committee for VkBlob-only keygen.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
