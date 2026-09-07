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

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
