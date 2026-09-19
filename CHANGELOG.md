# Acki Nacki Bridge Release Notes

All notable changes to the bridge are documented in this file — the contracts on
both chains, the ZK circuits and verification keys, the relayer and prover
binaries, and the deployment and operational surface around them.

Written for the people who deploy and run the bridge, not for the people who
wrote it. See the changelog policy in [AGENTS.md](AGENTS.md) for what belongs
here and how versions are assigned.

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

## [Unreleased]

### Breaking Changes

- **The Circuit 4 (withdrawal) verification key is rotated.** The inner
  Poseidon preimage now includes `events_pos`, and the public-input vector
  grows from 10 to 11 with `anchorLayer` (1-indexed, range-checked
  `1..=10`). `withdrawByProof` scans only that layer's window.
  The aggregated Yul grows from 20 990 B / 22 instances to 21 152 B / 23
  instances; the reference `_calldata.bin` is 3 648 B. Redeploy
  `BridgeWithdrawalAggregatorVerifier`; proofs against the old key do not
  verify, and a `WithdrawalPublicInputs` struct without `anchorLayer` will
  not decode.

- **The layer-hashes verification key is rotated. Redeploy that verifier.**
  `LayerHashesAggregatorVerifier` was re-keygen'd at `k_outer = 21`, because at
  20 the outer circuit did not fit the 14 inner public inputs. The runtime
  artefact grows from 19 100 B to 23 111 B, so its address and `extcodehash`
  change and the pin in `ShplonkDeployLib` moves with it. Proofs produced
  against the old key do not verify against the new one; a deployment that
  updates only the bridge will fail every `verifyBlock`. Margin to EIP-170
  (24 576 B) is now 1 465 B, the tightest of the four verifiers — see the
  warning below.

  The other two keys are **unchanged**: `PrimaryAggregatorVerifier.bin`
  and `FallbackAggregatorVerifier.bin` are byte-identical to 0.2.0.
  Primary's and Fallback's `_calldata.bin` fixtures were re-emitted, which
  is a test-vector refresh and not a rotation.

- Deployment: `DeployRealBridge` now requires `WIRE_VERIFY_BLOCK=true` on
  **every** chain (`:132`, unconditional). On mainnet `USE_AXIOM_ORACLE` and
  `WIRE_VERIFY_BLOCK` are `envBool` with no default and `USE_AXIOM_ORACLE`
  must be true; `altTokenId` must be 0. A Sepolia deploy that relied on
  `WIRE_VERIFY_BLOCK` defaulting to false now stops. `LAYER_HASHES_VERIFIER`
  and `WITHDRAWAL_VERIFIER` are still only read by `DeployReuseVerifiersBridge`
  and `DeployGenesisCursorBridge`, which already refused `chainid == 1`.

- `AckiNackiBridge`'s constructor rejects configurations it used to accept: a
  zero `genesisBkSetCommitment` (`ZeroBkSetCommitment`) and a non-canonical
  `genesisBkSetCommitment` or `genesisPrevMaxLevelLayerHash`
  (`FieldElementOutOfRange`), whenever the verifiers are wired; and a
  non-canonical Circuit 4 `dappFr` / `accFr` / `altTokenId` whenever
  withdrawal is wired. These are the values that could never have matched
  `_expectedPrevAnchor` or a Yul-reduced identity instance — deployments
  that were already broken from block one — but a script that passed zeros
  or unreduced words to get through construction will now fail at
  construction.

- Ownership transfer is two-step. `transferOwnership` records `pendingOwner`
  and ownership moves only when that address calls `acceptOwnership`. Any
  runbook or script that assumed `transferOwnership` completes the handover
  needs the second call.

### Added

- `writeOffUnbackedPrincipal()` (owner) zeroes `suppliedPrincipal` when
  `aUsdcBalance() == 0`. Recovers the book state that used to make every
  AAVE pull revert forever after a haircut or a short emergency drain
  (ETH-28). Reverts `NothingToWriteOff` otherwise; emits
  `UnbackedPrincipalWrittenOff`.
- `anchorRemainingAppends(layer, anchor)` and `layerWindowWriteCursor(layer)` —
  read-only views of how close an anchor is to eviction from its 128-slot
  window. A return of N means the Nth further append overwrites it; 0 means it
  is not in the window. Intended as the monitoring hook for the withdrawal
  deadline described under the withdrawal-window item below.
- `VERIFY_GAS_CAP` (1 500 000) bounds the `staticcall` into every Yul verifier,
  so a malformed proof cannot burn the whole transaction gas.
- `YulCodehashMismatch` in `ShplonkDeployLib`: deployment asserts the deployed
  Yul verifier's `extcodehash` against a pinned value, which is what makes an
  accidentally-substituted verifier a failed deploy rather than a live one.
- `TransferAmountMismatch` and `ApproveFailed`: token transfers are measured by
  `balanceOf` delta and `approve` return values are checked, so a
  fee-on-transfer or non-standard token fails closed instead of crediting book
  value that never arrived.
- `contracts/ethereum/verifiers/SIZES` pins every artefact's byte size, and
  `scripts/check_shplonk_artefacts.sh` verifies the eight SHA-256 sums, fails on
  size drift, and warns from 90% of EIP-170 (layer hashes warns today at 94%).
  Growth now shows up in a diff instead of in a reverted deploy.
- `contracts/ethereum/test/WithdrawAnchorEviction.t.sol` — eviction after 128
  appends, a seq_no jump not mass-evicting earlier anchors,
  `anchorRemainingAppends` at its edges, and the same nullifier paid
  against a still-in-window anchor after the original evicted.

- `scripts/keccak-tvm-bench/`: executes `EthKeccak` on a TVM instead of
  reasoning about it. `run.sh` compiles the exit-code wrapper `KeccakCheck.sol`
  against any copy of the library (`--lib`, default `contracts/an/EthKeccak.sol`)
  and runs it in `tvm-cli debug run --tvc`: no network, no keys. Measured with
  sold 0.81.0 / tvm-cli 3.0.6: the library as deployed on shellnet
  (`reference/EthKeccak_1.4.0_as_deployed.sol`, code hash `78905cf7...`) throws
  exit 50 on every input at the first `bc[i] = ...`; the fixed library returns
  the right digests at 12.93M gas per keccak-f permutation against the 10M
  per-transaction limit (`acki-nacki/node/blockchain.conf.json` p20/p21), and the
  642-byte Sepolia header 11683168 (`fixtures/`) runs out of the debugger's
  16.7M credit. `fetch_headers.py` rebuilds header RLPs from any JSON-RPC the
  way `header_rlp.rs` does; `emulate_live.sh` replays `submitAncestry` against
  the live light-client account with `tvm-cli runx`. Second, independent
  measurement of the ancestry gas wall (gosh-sh/bridge#36).
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
  `  USDCBridge._acceptedBlockHash`. The daemon fetches that chain after every
  accepted `submitUpdate` when `ETH_RPC_URL` is set (`--eth-rpc-url`) and runs
  `link_headers` locally; on-chain `submitAncestry` is `--submit-ancestry`
  (default **off**) because two headers already cost ~130 M gas against the
  10 M limit. It also calls `rePushAnchor` so a bounce before `setLightClient`
  is retried. Operator one-shot: `eth-lc-relayer submit-ancestry --eth-rpc-url … --checkpoint-hash
  0x…`. Contracts: `contracts/an/EthKeccak.sol`,
  `contracts/an/EthBeaconLightClient.sol` (the standalone variant; shellnet runs
  the constant-sink one from `acki-nacki` `contracts/exchange`, and what crosses
  between them is fixes and comments — see Removed). `updateCode` / `onCodeUpgrade` persist the committee, head,
  proven-hash set and `reAnchorsApplied` across a VkBlob rotation.
  `reAnchorCommittee` is the logged weak-subjectivity hatch after
  `disableOwnerRotation` (does not write exec hashes; `getCommitteeState`
  exposes `reAnchorsApplied`). Shellnet E2E:
  `scripts/ursus/eth_lc_shellnet_e2e.md`. Audit scope:
  `eth-light-client-prover/docs/m_audit_scope.md`.

### Changed

- `withdrawByProof` and `_pullFromAave` cap the AAVE pull at
  `min(suppliedPrincipal, aUsdcBalance)`. A phantom book no longer
  blocks a payout that already fits in liquid USDC. A zero aToken delta
  on `supplyToAave` is `AaveSupplyFailed`, not `AaveWithdrawFailed`.
  `harvestYield` transfers the requested `amount`.

- `docs/EVM-contracts-spec.md` trade-off items 3, 5, 6 and 10 rewritten: items 5
  (single-step ownership), 6 (`approve` return ignored) and most of 10 (genesis
  unvalidated) are closed by this release, and item 3 now states the real
  withdrawal boundary instead of calling an evicted anchor "unredeemable".
- The withdrawal deadline, written down for the first time. Each layer keeps
  128 anchors, and the witness builder escalates a layer at a time
  (`--anchor-layer auto`, the relayer default). Because an L(n) anchor is
  appended only at its own W^n boundary, the window grows with that boundary
  rather than repeating the one below: L1 = 128 × W·P = 131 072 seq (**≈ 12
  hours**); L2 = 128 × W² = 2 097 152 seq (**≈ 8 days**). L1→L2 is ×(W/P) =
  **16**; only L2→L3 and above are ×128. Past the highest active layer a
  payout is stranded in `treasuryBalance`. Adding a layer rescues only an
  event whose T_n the chain has not yet passed. No code changed here; the
  horizon was always this and was documented as 12 hours.

- `scripts/check_english_only.py` treats mathematical letters as notation, like
  the unaccented Greek it already accepts: the Mathematical Alphanumeric Symbols
  block (double-struck, bold, italic, script, fraktur), the letterlike
  double-struck / script / black-letter capitals and superscript Latin letters
  (`𝔾₂`, `ℤ`, `limbᵢ·(2⁸⁸)ⁱ`). They occur only in formulas, never in another
  language's prose, so the pairing and light-client notes no longer trip the
  hygiene pipeline. Cyrillic, accented Greek, CJK and the rest still fail.

- On-chain `submitAncestry` is opt-in (`--submit-ancestry` / `SUBMIT_ANCESTRY`,
  default **off**). The daemon used to fire a 32-header call every epoch whenever
  `ETH_RPC_URL` was set; two headers already cost 129.7 M gas against a 10 M
  limit, so every cycle burned ~0.7 vmshell past `tvm.accept()` on a call that
  cannot succeed. `ETH_RPC_URL` still attaches the execution RPC: the epoch is
  fetched and `link_headers` runs locally. Dropping the URL is no longer the
  only lever, and no longer takes the local check with it. The one-shot
  `submit-ancestry` subcommand stays, and warns. Measured by @SeHor05 on both
  `v3.0.6.an` and tvm-sdk#284 (gosh-sh/bridge#36).
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
  `contracts/an/EthBeaconLightClient.sol` re-emitted. The tvm-sdk
  opcode fixtures still carry the old blob and need
  `scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh`. Verified on Sepolia: the same blob comes
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
  the checkpoint and runs `link_headers` locally. On-chain `submitAncestry`
  is `--submit-ancestry` (default off). `submitUpdate`
  late-registers a skipped checkpoint of the current committee (`CheckpointBackfilled`,
  head not rewound). Sink notify uses `bounce: true`; a drop emits
  `AnchorPushBounced` and is retried via `rePushAnchor`. `encode_header_rlp`
  fails closed when `keccak256(rlp)` does not match the node's `block.hash`.
- `export_step_vk_blob` reads `FINALITY_UPDATE_PATH` and, when
  `COMMITTEE_JSON_PATH` / `BOOTSTRAP_PATH` is set, builds a **live** step
  witness (real sync committee). Unset committee path still emits a synthetic
  committee for VkBlob-only keygen.

### Fixed

- The deposit form accepted an Ethereum address as an Acki Nacki recipient. It
  required *at most* 64 hex characters, so a pasted 40-character address was
  left-padded into a well-formed non-zero `bytes32`, passed the contract's
  `anAccount != 0`, was bound in-circuit, and credited an account nobody owns —
  with deposit being one-way, the length check was the last place to catch it.
  Now exactly 64. The workchain field, which Acki Nacki ignores since `dappId`
  replaced the concept, is no longer editable and is pinned to 0.
- `MAX_FORWARD_GAP` was sized when the thinning factor `P` was 4 and the bundle
  stride 512, where its literal 2048 meant "four bundles". `P` is 8 and the
  stride 1024, so the same literal had quietly become two bundles, and a sibling
  relayer that advanced three between ticks would halt this one with
  `HistoryDrift` for no reason. Now derived from `BUNDLE_STRIDE_L1`, with a
  const assertion that the three-bundle floor holds at build time. Four
  restatements of `P = 4` / stride 512 as the current setting corrected
  (module docs, thinning tests, prover-daemon README, verifyBlock runbook).
- `covering_bundle_seq_no` treated a burn that landed exactly on a stride
  boundary as already covered by that boundary's bundle. The proof uses the
  opposite rule (`l1_anchor_boundaries`: for `e = 1024`, `K = 2048`, because
  the root at 1024 covers the previous batch), so `wait_for_coverage` returned
  one bundle too early and `resolve_anchor_layer` probed a height the chain
  may not have produced yet. 1 in 1024 burns; re-running later worked. Now the
  strictly-next multiple, matching the proof.
- `anchorRemainingAppends` NatSpec said the anchor survives N appends where it
  survives N-1.
- `applyBkSetUpdate` attestation `lastSeen` is the live layer cursor
  (`storedLastSeenBlockSeqNo`). The prover was baking the BK-update cursor,
  so after the first `verifyBlock` every rotation failed
  `AttestationProofRejected`. Once AN rotated, `verifyBlock` then failed
  `BkSetCommitmentMismatch` and unwithdrawn anchors aged out. The prover now
  uses the layer cursor; a test drives `verifyBlock` then `applyBkSetUpdate`
  with a mock that checks the argument.
- Production `verifyBlock` tests that lack `bound_scenario.json` now
  `vm.skip` instead of returning, so the hole shows up in the forge summary.
- `DeployRealBridge` on mainnet also requires `altDstChainId` and
  `altDstHostChainId` to be 0, matching the existing `altTokenId` require.
- Constructor now rejects a non-canonical Circuit 4 `dappFr` / `accFr` /
  `altTokenId`. A raw word cannot equal a Yul-reduced instance, so the
  previous values would have made every withdrawal revert permanently.
- `supplyToAave` books the aUSDC delta, not the USDC sent, so a rounding
  pool cannot inflate `suppliedPrincipal` above the shares the bridge
  holds. `emergencyWithdrawAll` keeps `principal - received` when the
  drain pays short; leftover-aToken still reverts unchanged.
- `forge` default profile no longer enables `ffi`. Tests only read the
  tree; write permission is limited to `deployment_real.json`.
- `verifyBlock` no longer SSTOREs per-slot window heights (~29k gas on a
  ten-layer call). `lastHeight` and `LayerAnchorAppended` remain; the
  relayer paints `HistoryWindow.heights` from those logs on resurrect.
- Documented that two identical AN burns in one block share a Circuit 4
  nullifier (`msg_id` is not in the preimage): the second payout is
  permanently blocked. Closing it needs a Circuit 4 re-keygen.
- After `emergencyWithdrawAll` the surplus is liquid: `harvestYield`
  reverts `NoYield` (it only sees AAVE). Collect with `skimExcessUsdc`
  (QC-A1-3). Test: `test_harvestYield_afterEmergency_revertsNoYield`.
- GitHub Woodpecker now `forge build` + `fmt --check` +
  `forge test --no-match-contract Fork` on every PR. Solidity compile
  used to live only on the GitLab mirror, so a broken head could stay
  mergeable on GitHub.
- L2 anchoring is the shellnet operational default (Deploy #12), not
  smoke-pending. Daemons log `info` on L2 startup; `AnchorMode::default()`
  stays L1 for local/CI.
- Spec §7.3 states the QC-A2-2 rule: `applyBkSetUpdate` attestation
  `lastSeen` is the live layer cursor. Re-prove if `verifyBlock` advances
  between prove and submit.

- The step VkBlob gate only checked that `step_vk_blob.bin` had
  `accumulator_limbs = 0`. It did not compare the fixture to the `VK_BLOB`
  embedded in `EthBeaconLightClient.sol`, so a re-emit that updated one and
  not the other was silent. tvm-sdk#284 shipped a step fixture (`bd108c08…`)
  that shares this contract's first 11 813 bytes and then diverges — same
  circuit params, different key material — which is why a real step proof
  from that PR does not verify here. The current key is `2d66c205…` (this
  fixture and both light-client copies); the SDK copy is the stale pre-
  `extra_data` rotation. `scripts/check_rotate_vkblob_accumulator.sh` now
  requires contract `VK_BLOB` == the step fixture (and the sha256 sidecar),
  and if a sibling `tvm-sdk` tree is present, that fixture too. Rotate already
  had the equivalent check.
- `EthKeccak` never computed a hash in the TVM. Three `sold` behaviours, each
  fatal on the first input: `uint64[5] bc;` declares a **zero-length** array, so
  the first write of the theta step threw exit 50 (this is what aborted every
  `submitAncestry` after the encoding fix); `x << n` on a `uint64` is
  range-checked, so `_rotl` threw exit 4 as soon as a rotation dropped a set bit;
  and `~` on a `uint64`, plus narrowing a shifted lane with `uint8()`, are
  refused for the same reason. The array is now allocated explicitly (once, not
  per round), rotation is done in `uint256` and masked back, `~x` is `x ^
  MASK64`, and `_squeeze32` masks before narrowing. Nothing about the algorithm
  changed. Verified by execution rather than inspection, in tvm-debugger 3.0.6
  against a wrapper contract: keccak256("") and keccak256("abc") match their
  vectors, a 136-byte input matches `cast keccak` (the multi-block absorb path),
  and the real 642-byte Sepolia header of block 11683168 hashes to its own block
  hash `6b83c33d…122f83`. Defects reported by @Skydev0h from shellnet
  (gosh-sh/bridge#36); `acki-nacki` `contracts/exchange/EthKeccak.sol` is
  byte-identical to this file modulo its pragma, so `EthKeccak_sold_fixes.patch`
  carries the same change there — it applies cleanly to the head of
  gosh-sh/acki-nacki#2618 and moves the light client's code hash from
  `78905cf7…9ed532` to `812f2b9f…da6dab`. Ancestry still cannot run: see Known
  issues.

- `submitAncestry` and `rePushAnchor` were rejected by the light client
  (compute phase, exit 252) because two byte orders were in play. The step
  circuit splits a hash with `node_hi_lo` — each 16-byte half read
  little-endian — so `submitUpdate` keys an anchor as
  `(LE(h[0..16]) << 128) | LE(h[16..32])`, and that word is what the bridge
  holds and what the deposit public inputs carry. Keccak in the VM returns
  Ethereum byte order, so `submitAncestry` looked up
  `_provenEthSlot[keccak(rlp)]`, never found the checkpoint and failed
  `ERR_UNKNOWN_CHECKPOINT`; the daemon sent `rePushAnchor` in the same wrong
  order. `submitAncestry` now re-packs through `_piForm` where hashes meet the
  store (parent links are still compared in Ethereum order), and the daemon
  converts with `anchor_key_hex`. Observed on shellnet (2026-09-10); ancestry
  had never been run live before. No migration: every hash already stored came
  from `submitUpdate` and is already keyed right.
  `EthBeaconLightClient_rotate_decider.patch` regenerated. Shellnet runs the
  bridge-deployed variant of the contract, maintained in `acki-nacki`
  `contracts/bridge`, which landed its own equivalent fix as `181b0c6a` and is
  live via `updateCode` (code hash `78905cf7…`, state intact). This copy now
  uses the same `_piForm` name and body, so the two trees differ only
  structurally.

### Known issues

- **Epoch ancestry cannot run on Acki Nacki**, so the light client anchors only
  the epoch checkpoint — 1 execution block of 32 — and a deposit in any other
  block still needs the owner's `setAcceptedBlockHash`. Gas is now the only
  reason: with the `sold` defects fixed (see below) `EthKeccak` computes the
  right hashes, but it is software keccak — one permutation is 12.93M gas and
  the real 642-byte Sepolia header of block 11683168 is 64.68M, against a 10M
  per-transaction limit (p20/p21). Hashing the empty string is already over the
  limit, so no input size makes `submitAncestry` callable, and a 32-header walk
  is ~2e9 gas. Closing this needs a keccak-256 builtin in the node, the way
  `ZKHALO2VERIFYWITHVK` was added, or the parent chain proven in-circuit.
  Defects found on shellnet 2026-09-11; gas re-measured offline in
  tvm-debugger 3.0.6 on 2026-09-13; a full 32-header walk measured on a real
  VM (tvm-debugger from `v3.0.6.an` and tvm-sdk#284) 2026-09-14: 2 headers
  129.7 M, slope 64.65 M/header, 32 headers OOG at 1e9, ≈2.07 G extrapolated
  against 10 M `gas_limit`. The daemon therefore does **not** send
  `submitAncestry` unless `--submit-ancestry` is set; `ETH_RPC_URL` still
  fetches the epoch and runs `link_headers` locally. `submitUpdate`,
  `submitRotate` and `rePushAnchor` are unaffected; so are withdrawals.

- `EthBeaconLightClient._pushExecHash` sent the `acceptBlockHashFromLightClient`
  message to `addr_none` when no `USDCBridge` was configured: the unset
  `_usdcBridge` is `addr_none`, not `address(0)`, so the guard passed, the
  action phase aborted with result code 34 and the whole `submitUpdate` was
  rolled back although the proof had verified. Guard is now
  `!_usdcBridge.isNone() && _usdcBridge != address(0)` (now in `_notifySink`,
  so `rePushAnchor` is covered too). Observed on the first shellnet shadow
  deploy (2026-09-04). `EthBeaconLightClient_rotate_decider.patch` regenerated.

### Removed

- `EthBeaconLightClient_rotate_decider.patch`, and with it the claim that the
  two light-client copies are kept identical. The patch created
  `contracts/exchange/EthBeaconLightClient.sol` wholesale from this repo's copy,
  which stopped being an upgrade once acki-nacki began maintaining its own:
  applying it would have replaced the deployed constant sink and its constructor
  sender check with an unset `_usdcBridge` — anchors silently stop reaching the
  bridge — and added a field to the `updateCode` migration cell that the live
  `onCodeUpgrade` cannot decode. `scripts/check_eth_beacon_lc_sources.sh`
  asserted that this patch reproduced our copy verbatim, i.e. it enforced the
  hazard rather than catching it.

  What actually differs between the two trees is small and deliberate: the sink
  (constant plus sender check there, settable `_usdcBridge` and the extra
  migration field here), the version string and the pragma floor. Both carry
  `_piForm`, `provenQueue` and the rotate decider. So the delivery is now two
  narrow patches instead of a file — `EthKeccak_sold_fixes.patch` (behaviour)
  and `EthBeaconLightClient_encoding_and_gas_notes.patch` (comments only, code
  hash verified unchanged at `78905cf7…9ed532`) — and the gate was rewritten to
  assert scope: patches stay inside `contracts/exchange/`, carry no sink wiring
  in either direction, the keccak patch only moves their library toward
  `contracts/an/EthKeccak.sol`, and the notes patch adds nothing but comments.
  Verified that the rewritten gate rejects the removed patch. The rotate VkBlob
  gate now reads the `ROTATE_VK_BLOB` literal from
  `contracts/an/EthBeaconLightClient.sol` rather than from the patch.

  Correcting myself: I described the drift as "221 lines, dominated by a
  `provenQueue` that exists in `contracts/exchange` and not in `contracts/an`"
  ([#36](https://github.com/gosh-sh/bridge/pull/36#issuecomment-5655184319)).
  That was a diff against the wrong commit — `github/eth-light-client-prover-m6`
  resolves against the remote named `github` unless spelled
  `github/github/eth-light-client-prover-m6`, so I had compared an ancestor from
  before `provenQueue` landed. The real diff is 206 lines and `provenQueue` is in
  both.


## [0.2.0] – 2026-09-11

### Breaking Changes

- **Sub-workspace directory renamed `crates/an-bridge-prover/` → `crates/bridge-prover-libraries/`.**
  Only the parent directory moved: every Cargo package name inside the
  sub-workspace is unchanged, so `cargo -p bridge-prover-lib` and friends
  keep working. The workspace exclude, every path-dep, and every runbook,
  script, env file and CI reference have been repointed. Migrations:
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
  Package name, `cargo -p …` selector, `[[bin]]` name, crate directory and
  the sub-workspace symlink move together. Everything sits under the
  `withdraw` subcommand, ahead of sibling subcommands like `deposit`.
  Migrations:
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
  The shared shellnet burner now lives once, in
  `crates/bridge-prover-libraries/shellnet.common` under
  `RELAYER_PRIVATE_KEY`, covering both the deployer and the relayer role.
  Replace `set -a && source contracts/ethereum/.env.shellnet && forge
  script …` with `PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL={1,2}
  ./scripts/deploy_bridge_bundle.sh`, run from
  `crates/bridge-prover-libraries/`: the wrapper re-derives genesis anchors
  from live chain head and writes `L{1,2}_config/env` atomically.

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
- **The first withdrawal after this upgrade regenerates the Circuit-4
  proving keys.** Cached keys now carry an `event_manifest.json` naming the
  circuit revision they were built for, the manifest format version, and a
  SHA-256 of each key file. Caches written before this release have none, so
  they count as cold and are rebuilt. There is no in-place migration:
  nothing in the old files records which circuit produced them, which is the
  gap being closed.

  **Before the first run on each host, `params/` must be writable with
  ~4.3 GB free.** A host that is read-only or short on space now refuses in
  stage 1 instead of failing after the burn — which also means an upgrade
  can turn a previously-working host into one that refuses until it is given
  room. The keygen itself still runs in stage 5, after the burn, as before.
  README, "Reaching `params/` from your own shell", carries the space check,
  the procedure for regenerating up front instead, and `probe_event_keys`
  with its `--repair` for a corrupt cache.

  **Restart `bridge-verifier-daemon` only after the keys are regenerated.**
  It reads the verifying key and never generates one; against a cache with
  no manifest it exits with "event VK not found … run the event prover
  (Circuit 4) first". Order the upgrade regenerate → restart. The CLI
  regenerates on its own, and the bundle relayer does not use Circuit 4.

  **Do not point two builds at one `--params-dir`.** Nothing locks the
  directory, a build that does not understand a manifest treats the cache as
  cold, and two keygens interleaving can leave a cache that passes every
  check while holding the other build's keys. Give a new build its own
  directory until every consumer is upgraded.

  Key files are now written atomically — temp file, fsync, rename — so an
  interrupted or out-of-space keygen leaves nothing half-written. Modes are
  unchanged, but **ownership is not preserved and cannot be**: a rename
  publishes a new inode owned by whoever wrote it. A `params/` seeded by a
  provisioning script or by root moves to the account that runs keygen on
  the first regeneration. `chown` it back if something relied on ownership
  rather than on the mode.
- **`--from-keys` must now be mode `0400`, not `0600`.** The CLI reads the
  multisig owner's key file and never writes it, so read-only-to-owner is
  the tightest mode that works, and it is now an exact requirement rather
  than a "no group or world bits" range. **Every existing key file needs
  one command**, because the previous documentation told operators to set
  `0600`:

  ```bash
  chmod 400 /path/to/owner.keys.json
  ```

  The refusal names the mode it found and the exact remedy, so a run that
  hits this is one copy-paste from working. `scripts/deploy_msig_and_mint.sh`
  now emits `0400` directly. Unchanged: idempotency state files under
  `--state-dir` stay `0600` — those the CLI does write.

### Added

- **New binary `ackinacki-bridge`** — end-user CLI for withdrawing USDC from
  an Acki Nacki multisig to an EVM recipient. Counterpart to `daemon-live`:
  the daemon owns bundle proving, this CLI owns per-withdrawal composition.
  It runs the six-stage pipeline (preflight → idempotency reserve → burn →
  capture, with resurrect and coverage-wait as stage 4b → Circuit-4 SHPLONK
  proof → `withdrawByProof`) and reads **no local `prover_state.json`**: its
  only view of prover state is the on-chain contract, so it works against any
  deploy the operator has RPC and `--bridge-address` for.

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

  ```
  ackinacki-bridge withdraw \
    --from <dapp_id>::<account_id> --from-keys /path/to/owner.keys.json \
    --to 0xRecipient --to-chain 11155111 --amount 1.000000
  ```

  Global flags: `--json` (one-line JSON on stdout, human logs on stderr),
  `--yes`, `--non-interactive`, `--dry-run`, `--allow-retry`.

  Every flag has a matching environment variable: `BRIDGE_GQL_ENDPOINT`,
  `USDC_BRIDGE_ACCOUNT_ID`, `RPC_URL`, `BRIDGE_ADDRESS`,
  `BURNER_PRIVATE_KEY` (the `withdrawByProof` signer, distinct from the AN
  multisig owner key), `BRIDGE_AGGREGATOR_DIR`, `BRIDGE_VERIFIERS_DIR`,
  `BRIDGE_PARAMS_DIR`, `BRIDGE_PK_CACHE_DIR`, and the new
  `BRIDGE_WITHDRAW_STATE_DIR` (per-withdrawal idempotency state, defaulting
  to `$CONFIG_DIR/withdraw-state/`).

  Exit codes distinguish "nothing broadcast" from "broadcast, outcome
  unknown" so a wrapper does not blind on a single non-zero: `0` success or
  dry-run OK, `2` preflight refused, `3` duplicate in-flight refused, `10`
  do not treat this identity as untouched, `11` capture timeout, `12` proof
  failed, `13` `withdrawByProof` reverted.

  Idempotency is a SHA-256 dedup key over `(from, to, to_chain, amount)`.
  State files hold only chain-observable identifiers — AN tx hash,
  `WithdrawalInitiated` msg id, block seq no, ETH tx hash — never key
  material, and `--from-keys` and `--eth-private-key` contents are never
  logged, printed or persisted. The burn payload defaults to `bounce = true`
  so USDC returns to the source multisig on any bridge revert; the
  historical Python driver used `bounce = false`.

- **`scripts/install.sh` installs from published releases, and
  `QUICKSTART.md` is one withdrawal in seven steps.** A withdrawal needs four
  artifacts that a checkout alone does not give you — the CLI, the
  `aggregate-proof` subprocess it shells out to, `solc 0.8.19`, and the
  Hermez `kzg_bn254_21.srs` ceremony — plus the verifier bytecode the proof
  is self-checked against. A host missing any of them fails at stage 5, after
  the burn.

  The script downloads them and compiles nothing, so neither Rust nor a
  checkout is required. `--check` reports without downloading, `--prefix`
  chooses where it lands (default `~/.local/share/ackinacki-bridge`), and
  `--yes` skips the questions. Release assets are verified against the
  release's `SHA256SUMS` — an asset the list does not name is refused rather
  than installed on the strength of a successful download — and `solc`
  against the version it reports, because another version emits different
  bytecode and fails the stage-5 self-check.

  It also writes the profile, taking the published one and rewriting only its
  seven path keys to absolute paths inside the prefix; endpoints, the bridge
  address and the pinned identity stay as released. The installed tree puts
  the aggregator at `<prefix>/aggregator/target/release/aggregate-proof`
  because that is the shape `--aggregator-dir` resolves, and the closing
  message names the `PATH` export explicitly: the aggregator looks up `solc`
  by bare name, so a pinned copy that is not on `PATH` is not used.

  **This needs the release to publish three assets**:
  `ackinacki-bridge-linux-x86_64.tar.gz` (the two binaries, `verifiers/*.bin`
  and the profile), `kzg_bn254_21.srs`, and `SHA256SUMS`.
  `BRIDGE_RELEASE_BASE` points the script at a mirror or an internal build.

- **Helper scripts under `crates/ackinacki-bridge/scripts/`.**
  `deploy_msig_and_mint.{sh,py}` deploys a fresh single-custodian
  `UpdateCustodianMultisigWallet` and seeds it with 1 USDC on ECC[3] via
  `USDCBridge.mintAndSend`, stopping there — it fires no
  `initiateWithdrawal` and runs no Rust binary. It emits eval-able
  `export WITHDRAW_FROM=…` and `export WITHDRAW_FROM_KEYS=…` on stdout with
  everything else on stderr, so `eval "$(scripts/deploy_msig_and_mint.sh)"`
  leads straight into `local_smoke.sh` (dry-run wrapper) or `live_smoke.sh`
  (real submit). `check_fixture_prereqs.sh` refuses before any of that if
  `tvm-cli` is absent or cannot execute on this platform — the usual cause
  being a binary built for another architecture, which otherwise surfaces
  part-way through a deploy. `scripts/check_bridge_abi_in_sync.sh` guards
  that the two runtime `USDCBridge.abi.json` copies stay byte-identical.

- **Production Docker Compose kit for the shellnet → Sepolia L2 relayer**
  under `crates/bridge-relayer-daemon/deploy/shellnet-l2/`: non-root
  read-only runtime image, external secret env template, bind-mount layout,
  full artifact and on-chain preflight, and an operator status command. The
  service uses `restart: unless-stopped` and reruns the fail-closed
  preflight on every start. The `bridge-evm-aggregator` lockfile is now
  tracked so target-host and image builds can use `cargo build --locked`.

- **New environment variables consumed by `bridge_prover_lib::paths`:**
  `BRIDGE_CONFIG_DIR` (broad selector, resolving both state and proofs
  under it), and the narrower `BRIDGE_STATE_DIR` and `BRIDGE_PROOFS_DIR`,
  which win when set. **Export `BRIDGE_CONFIG_DIR` before sourcing the
  per-mode env file** — the env files no longer set it themselves. New
  launch scripts under `crates/bridge-prover-libraries/scripts/` source that
  file: `launch_withdraw_e2e.sh` (dry-run L1),
  `launch_withdraw_e2e_real.sh` (real submit L1),
  `launch_withdraw_e2e_l2.sh` (dry-run L2) and
  `replay_withdraw_shplonk.sh` (offline replay against a retained witness).

- **CI now reaches the `ackinacki-bridge` crate.** It lives in the
  `bridge-prover-libraries` sub-workspace, so no existing `*:rust:*` job
  touched it and every one of its tests was unrun.

  | Job | Runs on | What it covers |
  |-----|---------|----------------|
  | `build:rust:ackinacki-bridge` | every pipeline | `cargo build --locked -p ackinacki-bridge --all-targets` |
  | `test:rust:ackinacki-bridge` | every pipeline | the crate's suite, plus the `bridge-prover-lib` key-cache and ceremony probes |
  | `lint:rust:ackinacki-bridge:{fmt,clippy}` | every pipeline | 118 formatting diffs and four clippy warnings were invisible before; `--all-targets`, `allow_failure: false` |
  | `test:rust:ackinacki-bridge:enospc` | opt-in | the reserve-under-ENOSPC test against a real 1 MiB tmpfs |
  | `test:rust:ackinacki-bridge:keycache` | scheduled or manual | the fixture-dependent key-cache and alternate-keyset tests — a ~464 MB ceremony and two ~7 min keygens |

  The last two are deliberately not per-MR gates: keycache takes ~20 minutes
  and ENOSPC needs `CAP_SYS_ADMIN`. **That leaves a real gap between merge
  time and the nightly schedule**, named here rather than papered over.

  The key-cache job also runs the keygen-lock and sweep tests, which none of
  its four substring filters previously matched, and **asserts how many
  tests its filters select**: a libtest filter matching nothing still exits
  0, so a renamed test used to stop running with the job still green.

  Two variables are set in the project's CI settings rather than in
  `.gitlab-ci.yml`. `BRIDGE_ENOSPC_RUNNER=1` once a runner carrying the
  `privileged` tag exists — the variable is the switch and the tag is not,
  because a job whose tag no runner carries sits `pending` and the scheduled
  pipeline never finishes. `BRIDGE_ALT_KEYS_REF` names a commit carrying a
  *different* event keyset; unset, the job skips its three alternate-keyset
  regressions and says so, and set to a ref without that path it fails
  rather than passing empty.

- **README documents the one-time KZG ceremony provisioning (Step 0).**
  `crates/bridge-prover-libraries/params/` is gitignored and no
  operator-facing document said how to create it. A withdrawal needs exactly
  one file, `kzg_bn254_21.srs` (~256 MB); lower degrees are derived from it.
  The `~17 GB` figure in the flags table described a `params/` shared with a
  bundle relayer and is corrected — a withdraw-only machine needs roughly
  3 GB plus the aggregator's `pk_cache/`.

### Changed

- **`ackinacki-bridge withdraw` takes five per-request flags; everything
  else comes from the `$BRIDGE_CONFIG` profile.**

  ```
  ackinacki-bridge withdraw \
      --from <dapp_id::account_id> --from-keys <path> \
      --to <0x…> --to-chain <chain-id> --amount <usdc> \
      [--dry-run] [--yes] [--json]
  ```

  `--rpc-url`, `--bridge-address`, `--gql-endpoint`, `--anchor-layer`,
  `--i-know-the-wait`, `--params-dir`, `--aggregator-dir`,
  `--verifiers-dir`, `--work-dir`, `--snark-dir`, `--pk-cache-dir`,
  `--state-dir` and `--usdc-bridge-account` are read from the profile, which
  the CLI sources with `dotenvy` before clap reads any `env=` attribute.
  Precedence: **explicit flag > shell environment > profile > compiled
  default**. Existing flag-heavy invocations keep working.

  `config/bridge_config` is now a symlink to `config/bridge_config.shellnet`
  with identical content, and `bridge_config.local` and
  `bridge_config.mainnet` ship beside it, so switching network is
  `export BRIDGE_CONFIG=./config/bridge_config.local`. The hardcoded
  shellnet default on `--usdc-bridge-account` is gone: the value comes from
  `USDC_BRIDGE_ACCOUNT_ID` in each profile, so a wrong-network profile fails
  loudly instead of silently talking to the shellnet canonical account.
  `deploy_msig_and_mint.py` drops its `MODE=shellnet|local` branch and reads
  `NETWORK` / `BRIDGE_GQL_ENDPOINT` / `USDC_BRIDGE_KEY_PATH` from the same
  file, deriving local detection from the resolved URL — adding a network is
  one new profile and no Python edit. New dependency: `dotenvy = "0.15"`.

- **The smoke scripts source `config/bridge_config` by default** rather than
  the relayer's `L1_config/env`, and dropped ~90 lines each: they export
  `$BRIDGE_CONFIG`, canonicalize `$BRIDGE_SNARK_DIR` to absolute (the
  aggregator subprocess changes directory), and pass the five intent flags.
  Both now `cd` to `crates/ackinacki-bridge/`, so the profile's relative
  `BRIDGE_PARAMS_DIR=../bridge-prover-libraries/params` resolves, and both
  require `BURNER_PRIVATE_KEY` in the caller's environment.
  `live_smoke.sh` passes `--anchor-layer 2 --i-know-the-wait` by default to
  match the pinned L2 deploy; override with `ANCHOR_LAYER=1`.

- **The pinned `BRIDGE_ADDRESS` rotated to
  `0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7`** (was
  `0x8D9190666128ab897C5ABd8C107239A197e08467`). The new deploy's `usdc()`
  points at a real Circle FiatToken
  (`0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`), not the previous bridge's
  mint-anyone test token — so treasury seeding can no longer use the old
  faucet's `mint(address,address,uint256)`. Confirm the token with
  `cast call $BRIDGE_ADDRESS 'usdc()(address)'` and use the balance the
  burner already holds, or ask a Circle-token minter.

- **The shellnet profile documents how to tell a live deploy from a dead
  one, because an address alone does not.** The values are unchanged, but
  the relayer was moved to a second deploy
  (`0x8545129b215B248944A3aE40f711F34CAb458644`, over a new AN-side
  eccUSDCBridge) and rolled back within the day, and the retired deploy
  answers every getter exactly like the live one. A bridge nobody advances
  accepts the burn and then never produces a covering bundle, so the run
  waits out `COVERAGE_WAIT` and exits 11 or 12 with the USDC gone. The
  profile carries the check — `storedLastSeenBlockSeqNo` plus the
  `BlockVerified` cadence, ~437 Sepolia blocks (~87 min) in L2 mode — and
  says that a last event older than that means this is not the live deploy.

  Two invariants are written down beside it:

  - `BRIDGE_ADDRESS` and `USDC_BRIDGE_ACCOUNT_ID` move **together**. A
    deploy is pinned at construction to one AN-side account, and
    `withdrawByProof` compares the `(dappFr, accFr)` a proof carries against
    that pinning *before* verifying the proof, so a half-updated profile is
    a refusal after the burn. The id never has to be looked up:
    `cast call $BRIDGE_ADDRESS 'bridgeWithdrawalAccFr()(uint256)'` printed
    as `064x` **is** this line.
  - A deploy carries its **own** treasury and switching does not bring the
    balance along. `treasuryBalance` is a counter on the bridge, not an
    address, and only `deposit` moves it — a plain USDC `transfer` funds
    nothing, leaves the tokens as skimmable liquid surplus, and the
    withdrawal still reverts with `WithdrawTreasuryShortfall`.

  Nothing has to be re-provisioned on the prover side for either deploy:
  both verifier stacks end in a Yul runtime matching this build's embedded
  `BridgeWithdrawalAggregatorVerifier.bin` byte for byte.

- **`--allow-retry` resumes in place instead of overwriting the state
  file.** It used to rewrite the prior record with a fresh `Reserved`,
  dropping the stored `an_tx_hash`, after which the orchestrator re-fired
  the burn — a second `sendTransaction` for the same withdrawal, on a
  multisig with no nonce guard. It now keeps the record verbatim and skips
  any stage whose outputs are on file. `Confirmed` and `Submitted` refuse
  the flag outright: the first has paid out, the second has an unresolved
  in-flight EVM tx to reconcile on chain. `Failed` **with** a hash resumes
  without the flag, because the only production writer of `Failed` is the
  post-burn revert path; `Failed` without one still restarts clean.

- **The record reaches `Submitted` only after `withdrawByProof` returns a
  tx hash.** It used to flip immediately before the call, so an RPC error,
  a wallet reject or a gas-estimation failure left the record claiming a tx
  was broadcast, and later runs refused as duplicates over something that
  never hit Sepolia. `Submitted` and `Confirmed` are now both written inside
  the paid branch.

- **The CLI prompts before the AN burn unless `--yes` is passed.** `--yes`
  and `--non-interactive` were accepted by clap and never consulted. The
  prompt prints the source multisig, recipient and chain, amount, target
  bridge and the anchor-mode wait estimate. No TTY without `--yes` refuses
  with exit 2.

- **Capture-stage errors map to exit 11, not 12.** GQL unreachable, event
  never emitted, poll ceiling exceeded — these are reconcile-and-resume
  situations, and the catch-all `ProofFailed` sent operators to debug the
  aggregator when the burn had bounced or the AN GQL was down. Stage 4b's
  coverage wait still maps to 12, being a prove-path prerequisite.

- **Capture is multi-user safe.** The CLI no longer youngest-picks a shared
  `USDCBridge → ExtOut` queue after firing its burn. It chain-follows the
  multisig transaction hash through two GraphQL hops —
  `transaction(hash).out_messages` → the outbound whose `dst` is
  USDCBridge → `message(hash).dst_transaction.out_messages` → the outbound
  whose `dst` is `makeAddrExtern(618)` — and persists the msg id and its
  block seq no at the `Captured` stage. Concurrent operators can no longer
  capture each other's events even in the sub-second window between burns.
  The daemon's `run_once` is unchanged.

- **`--dry-run` documentation corrected to preflight-only scope.** README,
  runbook Step 4, runbook Case 8 and `local_smoke.sh`'s header claimed it
  ran the full pipeline including `dry_run_withdraw`. The code has always
  stopped after preflight. (Its scope has since grown again — see Fixed.)

- **The CLI documents split by audience.** The single
  `docs/live_cli_withdraw_runbook.md` covered both default users on the
  pinned shellnet deploy and advanced users deploying their own bridge.
  `crates/ackinacki-bridge/README.md` is now the default-user runbook —
  wallet setup, multisig deploy, treasury check, dry run, real submit, with
  every real value inlined — and the old file, renamed
  `docs/advanced_user_withdraw_runbook.md`, keeps what only advanced users
  need: the L1-vs-L2 timing model, the self-deploy sequence (Steps L0–L5),
  the stress-test loop and the failure-mode catalog.

- **`bridge-prover-daemon` and `bridge-verifier-daemon` resolve their paths
  at runtime** via `bridge_prover_lib::paths` instead of hardcoding
  `./state/` and `proofs/` at compile time. Defaults match the previous
  literals, so operators who do not set `BRIDGE_CONFIG_DIR` see no change.

- **`bridge-relayer-daemon` docs**: Case 1 is a single unified cold-start
  section with a `BRIDGE_CONFIG_DIR=./L1_config` / `./L2_config` selector at
  the top, and Case 7 collapses to a table of the four operator-visible L2
  deltas — W²-aligned bootstrap seqno, up to ~101 min first-verify wait,
  `layers=2` log field, ~15 sub-bundle diagnostic window.
  `live_withdrawByProof_runbook.md` Case 8 sources `L2_config/env` instead
  of `.env.shellnet.l2`.

- **Python E2E drivers deduplicated.** `deploy_multisig` and `mint_usdc`
  moved to `python/helper/msig.py`;
  `materialize_usdc_bridge_key_from_node_config` and
  `validate_usdc_bridge_key` to `helper/bridge_e2e.py`. Both drivers import
  the shared implementations, keeping the fresher variant — explicit
  `RuntimeError` on multisig-materialization timeout, richer owner-key
  mismatch hints.

### Fixed

#### Exit codes

- **A stage-1 refusal on a withdrawal that already has a record on disk is
  exit 10, not exit 2.** Exit 2's contract has two halves — nothing was
  broadcast, **and** there is no record for this identity — and a long list
  of refusals satisfied only the first. A wrapper keying on 2 to mean
  "clean slate, safe to retry" would have retried into a live reservation,
  and on a multisig with no replay guard that is a second burn.

  What moves to 10, all of it pre-send:

  - the six stage-1 checks — balance, destination chain, bridge deploy,
    signer key, prover artifacts, client context;
  - a missing `--eth-private-key`, `--work-dir`, `--params-dir`,
    `--aggregator-dir` or `--verifiers-dir`;
  - an unset `HOME` with no `--state-dir` — there is nowhere to read a
    record from, so the run cannot claim the withdrawal is untouched;
  - a record that cannot be read, including the read taken behind a
    contended lock;
  - every pre-send refusal on the resume path, and the four reservation
    failures it reaches;
  - `--anchor-layer` above 2, which is exit 2 with no record and 10 with
    one;
  - the pre-send check that this run still holds its withdrawal lock.

  The refusals name the AN transaction already on the wire where there is
  one, say nothing new was sent or written, and say not to delete the
  record. A first run's preflight refusal is unchanged: with no prior burn,
  exit 2 is exactly right.

- **Refusals downstream of the burn report their own stage.** Four
  state-file writes after the burn — two of them after `withdrawByProof`
  had paid out — reported exit 2, which means "refused before sending".
  A stage-6 signer failure is now **13**, an internal inconsistency at the
  start of capture is **12**, and a post-burn state-write failure carries
  the stage it is in and names what has already happened. An invariant
  broken after the burn is exit 12 rather than an `expect` panic: exit 101
  is not one of this CLI's codes, so a `--json` consumer got no envelope
  at all about a withdrawal whose USDC had moved.

- **`--dry-run` reads the idempotency store before it reports.** It still
  reserves nothing and writes nothing, but it used not to LOOK, so all
  eight refusals it can raise came out as exit 2 — including over a
  `burned` record whose hash is on file. A dry run that finds a record now
  reports **10**, names it, and forbids deleting it; over a terminal
  record, one in flight without `--allow-retry`, or one with no AN tx
  hash it reports **3** with the real run's own remedy, where it used to
  exit 0. With `HOME` unset and no `--state-dir` it still runs — it is
  meant to be safe anywhere — but its refusals there are 10.
  `--dry-run` therefore produces 0, 2, 3 or 10.

- **A full or closed output stream no longer replaces the exit code with
  101.** `println!` panics when the write fails, so `> /dev/full`, a full
  disk or a closed pipe discarded the whole 0/2/3/10/11/12/13 contract —
  worst in the success summary, reached only after both chains had moved.
  Every terminal write is a checked `write_all` with one fallback hop to
  the other stream. With both gone the process stays silent and still
  exits with the code that describes what happened to the money.

- **A `tvm_client` context that cannot be built is exit 2, not 10.** It
  only builds configuration and does not connect, so every failure is
  local and pre-send. Both call sites now share one constructor.

- **Exit 10 covers six situations.** The README table described it as "AN
  burn WAS broadcast", which is one of them. It now names what they share,
  which is what a script should key on: do not treat this identity as
  untouched. Exit 2's row gains the half that distinguishes it — no record
  on disk.

#### The double-burn guard

- **Two concurrent runs of the same withdrawal no longer both burn.** The
  reservation answered the same way whether it had created the record or
  found someone else's, so the second run read a record with no hash on it
  — because the first had not returned from the broadcast yet — and sent a
  second `initiateWithdrawal`. Finding a record somebody else created is
  now exit 3, and the record's fields are no longer how the two cases are
  told apart.

- **An unreadable state directory is no longer read as an empty one.**
  `Path::exists()` folds every `stat` failure into `false`, so a directory
  this process cannot traverse — wrong mode, wrong owner, a half-restored
  backup — meant "no record", and the run burned again. Only `NotFound`
  means there is no record; anything else refuses.

- **The withdrawal lock is held for the whole run, on every path.** It
  used to cover the send alone, and only in one arm of one branch: the
  resume path took none at all, so a resuming run was invisible to the
  liveness probe that the exit-3 refusal promises, and two concurrent
  resumes both reached submit — where the loser's `withdrawByProof`
  reverts on the nullifier and writes `Failed` over the winner's
  `Confirmed`. Releasing the lock anywhere in between now fails to
  compile. The last step before the broadcast asks the kernel whether this
  run still holds it and refuses with exit 10 if not, and
  `stage 3/6: broadcasting the burn` logs `locked=true|false`.

- **A lock this run could not take is a refusal, not a downgrade.** Every
  failure was read as "this filesystem does not implement `flock`" and the
  run continued holding nothing. Only `ENOLCK`, `EOPNOTSUPP` and `ENOSYS`
  mean that now; everything else refuses with exit 2 and nothing sent.
  **Runs that previously continued past a lock failure will now stop.**
  Relatedly, the first withdrawal on a host now takes its lock at all — the
  lock file lives in the state directory, which the reservation had not yet
  created, so `open` returned ENOENT and the run proceeded holding nothing.

- **Deleting the state record no longer substitutes for `--allow-retry`.**
  A record removed between the run's first read and its reservation — the
  window the exit-3 message sends a reconciled operator into — is restored
  and put back through `reserve`, so a `burned` record deleted in that
  window needs the flag exactly as one still on disk does. A restored
  `confirmed` or `submitted` record is refused with exit 3 rather than
  carried through capture, prove and a **second** `withdrawByProof`.

- **A `Failed` record keeps its AN tx hash.** `Failed` is written only by
  the post-burn `withdrawByProof` revert path, so the burn has already
  landed; wiping it dropped the hash and sent the next run into the burn
  branch. Concretely: hit a `WithdrawTreasuryShortfall`, top up the
  treasury, re-run — and the old code burned a second time. Only `Failed`
  with no hash still wipes.

- **A state record that claims a burn it cannot name is refused on read.**
  No `an_tx_hash` with a status of `burned`, `captured`, `proved`,
  `submitted`, `failed` or `confirmed`, or a `key` that disagrees with the
  filename, cannot both be true. These are reachable by hand, and the
  post-burn recovery procedure asks operators to edit exactly those fields.

- **The reservation is durable before the burn goes out.** The record, the
  state directory and every directory level the run creates are `fsync`ed
  before `sendTransaction`; a reservation that fails to write removes its
  own partial file. Previously a power loss between reservation and burn
  could lose the record while the burn landed.

- **A withdrawal refused before broadcast leaves no record.** Declining the
  prompt used to write a `reserved` file that made the next identical
  invocation fail with exit 3. The confirmation and every fallible pre-send
  step now run before the reservation, which is taken immediately before
  the message goes on the wire.

- **A reservation that fails after publishing says so.** If the directory
  fsync failed after the record was linked, the refusal still read "no
  partial record was left behind" while a complete record sat on disk. The
  record is deliberately not removed: unlinking a published reservation is
  the double-burn the file exists to prevent.

#### Refusal messages

- **The exit-3 refusal says what to do, and reports whether another run is
  executing the withdrawal.** A `reserved` record with no `an_tx_hash` has
  two meanings the file cannot separate — a run is inside the burn right
  now, or a run died in that window. A withdrawal now holds an advisory
  `flock` on `<state-dir>/<key>.lock`, and a later run reports which case
  it is. The refusal names the record's path, states that `--allow-retry`
  does not override it, and gives the two branches: hash found on chain →
  write it in and resume; nothing broadcast **and** no live holder →
  delete and re-run. Where `flock` is unavailable the verdict comes back
  unanswered rather than as a guess.

- **The reconciliation steps depend on the liveness verdict.** They were
  baked into the message and printed under every verdict, including
  "another process … RIGHT NOW … Do not touch the record" two lines above
  an unconditional "write its hash into an_tx_hash". A held lock now gets
  the wait and nothing else. Reconciliation under a live holder is not
  merely risky: the chain state it reads is being written as it reads.

- **The "could not be determined" verdict no longer blames the
  filesystem.** `EACCES` on the state directory and `EMFILE` reach the same
  arm as a mount that cannot `flock`, and the errno was discarded — an
  operator out of file handles was sent to check their mount. The verdict
  names no cause, and both arms log theirs as a `warn` line above the
  refusal.

- **`--allow-retry` is no longer advised where it cannot work.** A
  hash-less record was told to "re-run with `--allow-retry`", which reaches
  a different exit 3 saying the flag does not override it — leaving
  deletion of the record as the only escape an operator could find, which
  is what permits a second burn. The advice remains on records that carry a
  hash. `confirmed` and `submitted` refuse the flag outright and now say so
  instead of repeating the generic line.

- **The duplicate refusal prints a hash, not Rust syntax.** `Prior AN tx:
  Some("0x2a91…")` came from `{:?}` on an `Option<String>`; the quotes
  travelled with a careless copy and `cast` then rejected the argument, and
  the absent case rendered as a bare `None`. Both fields now print the
  value alone or `none recorded`. `Debug` was also what escaped control
  characters in those values, so the replacement escapes deliberately — a
  record is a file anything can rewrite, and it must not be able to forge a
  line in its own refusal.

- **The pinned-identity refusal prints `(dappFr, accFr)` in hex.** The last
  line sends you to `USDC_BRIDGE_ACCOUNT_ID`, which every profile writes as
  64 lowercase hex characters, while the pair above it arrived as `U256`
  decimal — so the comparison it prescribed could not be done by eye. Exit
  code and `--json` envelope are unchanged; only the four numbers inside
  `message` are rendered differently.

- **Burn and signing failures no longer echo the key file.** The SDK's
  signing errors embed the public key verbatim and the first eight
  characters of the secret, and both reached stderr and `--json`. These
  refusals now carry the SDK error code and nothing else.

- **A malformed argument no longer crashes the CLI.** Truncating untrusted
  arguments split multibyte characters, so a `--to` with non-ASCII at the
  wrong offset exited 101 instead of emitting the error envelope. Control
  characters are escaped rather than replayed into the terminal.

- **The burn confirmation prompt refuses instead of reading an answer
  nobody saw.** Every line was written with the result discarded, so with
  stderr unwritable — a closed pager, a full disk — the terminal sat blank
  and whatever was typed counted as consent to an irreversible burn. A
  redirect to a file is unaffected; only a write that actually fails is
  refused.

- **The ceremony refusal says why.** An SRS that loads but is not the
  Hermez Perpetual Powers of Tau produced "no usable ceremony at k=N" plus
  instructions to provision a file that was already there. The sentence
  that matters — its toxic waste is public, so every proof made with it is
  forgeable — was the error's source and was never printed. A file that is
  present but unreadable now reports that rather than "no ceremony", and an
  absent one no longer warns as unreadable.

- **`--json` errors carry the cause chain.** The envelope rendered only the
  outermost `Display`, so the aggregator's stderr and the keygen-lock
  timeout were dropped entirely. A `causes` array is added alongside
  `message`, outermost first. **`message` is unchanged** — consumers match
  on it. Human mode gains indented `caused by [N]:` lines, and both modes
  walk the chain through one function.

- **`--json` covers usage and configuration errors.** A malformed
  invocation or an unloadable `$BRIDGE_CONFIG` bypassed the contract
  entirely: clap printed its own usage block and the config path used an
  undocumented exit 1. Both emit the one-line envelope and exit 2.
  `--help` and `--version` still print normally and exit 0.

#### Preflight

- **`solc` was an undeclared, unchecked runtime dependency of every
  withdrawal.** `aggregate-proof` compiles the generated Yul verifier at
  stage 5 by shelling out to `solc` and self-checks the bytecode against
  the committed `BridgeWithdrawalAggregatorVerifier.bin`. With no `solc` on
  `PATH` that is a panic in a subprocess — surfacing as exit 12 — reached
  **after** the irreversible burn and up to ~91 min of waiting. Stage 1 now
  refuses a real run without it, and the version must be exactly `0.8.19`:
  another emits different bytecode and fails the same self-check at stage
  5. Platform build metadata after `+` is ignored. README gains Step 0b and
  the runbook a third mandatory build step. Both now say that `--dry-run`
  skips every prover-artifact check, so a clean dry run is not evidence
  that stage 5 can finish.

- **The EVM side is checked before the AN burn, on every run including
  `--dry-run`.** None of it needs a signing key: an `RPC_URL` whose
  `eth_chainId` is not `--to-chain`; a `--bridge-address` with no contract
  behind it; an unset or incomplete verifier stack (`adapter →
  shplonkVerifier → yulVerifier`, code required at every level, with the
  deployed Yul runtime byte-compared against the local `.bin` when
  `--verifiers-dir` is given, otherwise skipped with a warning); a bridge
  pinned to different `(bridgeWithdrawalDappFr, bridgeWithdrawalAccFr)`
  values than this withdrawal will prove; a `treasuryBalance` that already
  cannot cover the amount. The first contract call used to happen in stage
  4b, after the burn and up to ~101 min of anchor wait. The treasury check
  is a preflight, not a guarantee — the treasury is shared and can be
  drained again while a withdrawal waits.

- **The signing key and the prover artifacts are checked in stage 1 too,
  on real runs.** Parsing `BURNER_PRIVATE_KEY`, the KZG ceremony at **both**
  degrees a withdrawal loads (k=20 and k=21 — checking only the larger
  missed a bad `kzg_bn254_20.srs`, which `load_srs` prefers by exact
  filename), the Circuit-4 key cache, a runnable `aggregate-proof`, and
  writable output directories with room for what will be written. These
  need the submit-only flags, so a dry run does not reach them: a clean
  `--dry-run` means "nothing about either chain is misconfigured", not "a
  real run will succeed", and its `--help` now says so.

- **`--dry-run` no longer requires `BURNER_PRIVATE_KEY`, the prover
  directories, or `HOME`.** Those five flags are optional at parse time and
  resolved only for a real withdrawal, which refuses at stage 1 naming all
  of them at once instead of clap listing nine flags before any check runs.
  The unset-`HOME` refusal is likewise raised only when the run will
  actually use the state directory — under systemd, cron and most Docker
  images the one command whose purpose is to be safe to run anywhere was
  failing with a double-burn refusal about a directory it never touches.

- **`ackinacki-bridge withdraw` refuses when `HOME` is unset instead of
  putting its state directory in the current directory.** The fallback made
  the double-burn guard depend on where the operator was standing. Pass
  `--state-dir` or `BRIDGE_WITHDRAW_STATE_DIR` under systemd, cron, `sudo`
  without `-H`, and many Docker images.

- **A `BRIDGE_CONFIG` that is not valid UTF-8 is refused instead of
  silently ignored.** A variable whose bytes are not UTF-8 was treated like
  an unset one and the profile was never sourced — silently, because this
  runs before tracing starts. `BRIDGE_WITHDRAW_STATE_DIR` comes from the
  profile, and the dedup key does not include the directory, so the record
  and the lock for a withdrawal in flight end up somewhere nobody is
  reading and the run burns again. Exit 2, with the value rendered lossily
  and escaped.

- **Preflight accepts the `--from` form the CLI itself mandates.** The
  `getCustodians` call threaded the extended `dapp_id::account_id` string
  into the ABI encoder, which rejects it (`Invalid address`). The encoder
  path uses `from.legacy()` now; the extended form is still used for the
  BOC fetch and for log lines. Before this, every dry run and every live
  withdraw failed at preflight step 3 regardless of on-chain state.

- **The USDCBridge liveness check no longer misreports an Active bridge as
  `Unknown`.** The GraphQL query asked for the numeric `info.acc_type` and
  matched it as a string, falling back to `"Unknown"` on every
  well-deployed bridge. It requests `info.acc_type_name` now.

- **`--from-keys` refusals name the actual problem**, and a `0x`-prefixed
  or short keys.json is accepted. A missing file and a directory were both
  reported as "not owner-only readable". Preflight normalised the file's
  halves while the signing path only lowercased them, so such a file passed
  preflight and then failed against the very key preflight had approved —
  permanently, with the same command and the same file. The two halves are
  now verified to be an actual key pair during preflight rather than at
  burn time, where a mismatch surfaced as exit 10 though nothing had been
  broadcast. Key files are parsed as hex only: an all-decimal-digit key is
  a hex key.

- **`--to 0x0000…0000` is refused at preflight.** The ERC-20 leg could
  otherwise succeed against a token whose `transfer` treats the zero
  address as a burn sink, consuming an ECC[3] draw against an unrecoverable
  recipient. Exit 2.

- **`--amount` is capped at `u64::MAX` micro-USDC at preflight.** The
  ECC[3] balance and the `initiateWithdrawal(amount)` argument are u64 on
  the wire, and a larger value caused a silent cast at broadcast time.
  Exactly `u64::MAX` micros (`18446744073709.551615` USDC) is still
  accepted. Exit 2.

- **`--anchor-layer` above 2 is refused.** `--anchor-layer 3` parsed, and
  the three places that consumed it disagreed: the coverage wait used the
  L1 stride, the resurrected `BridgeState` was stamped level 3, and the
  prompt printed "unbounded". The run burned and then waited against a
  stride that does not match the level it recorded — and no relayer
  advances an anchor above layer 2, so that wait never ends. `auto`, `1`
  and `2` are the accepted values.

- **`--yes` and `--non-interactive` can be passed together**, which is the
  normal shape for a CI wrapper.

#### The Circuit-4 key cache

- **Two keygens can no longer run over each other in one `params_dir`.**
  That directory is shared with the bundle daemon. Per-file writes are
  atomic, but the manifest is written last and hashes whatever is on disk
  at that moment, so two processes building different circuits could
  publish a manifest that is internally consistent and describes a mixed
  keyset: every later check passes, the verdict is "warm", and the proof
  fails at stage 5 after the burn. Keygen now takes an exclusive `flock` on
  `<prefix>_keygen.lock` per circuit, so unrelated circuits still run in
  parallel. A second arrival waits up to 30 minutes with progress logged,
  then refuses rather than blocking a withdrawal forever, and on getting in
  it adopts what the other process published. **Never delete the lock file
  to "clear" it** — the kernel releases it when the holder exits, so a lock
  that is held proves a live holder.

- **The proving key is re-verified at stage 5, before it is used.** Its
  ~2.65 GB digest was streamed once, in preflight, and between that and its
  use sit the irreversible burn and up to ~91 minutes of anchor wait. A key
  replaced in that window — an rsync, a restored backup, another keygen —
  was not caught, and a same-length corruption past the embedded verifying
  key deserialises happily. Stage 5 re-streams the key and compares it
  against the digest preflight recorded, not against the manifest read
  again, so replacing key and manifest together does not pass. A mismatch
  is exit 12 with instructions that resume rather than re-burn.

- **The key cache is validated, not just counted.** Stage 1 asks the key
  manager what it will do with `--params-dir` instead of testing that
  `event_pk.bin` exists. A proving key with no matching config or verifying
  key means keygen will run, so the ~3 GB headroom check applies; a
  truncated one beside a valid verifying key is refused outright. The two
  keys are also checked as a **pair** — two keysets from different circuit
  revisions each load cleanly, and the proof is made with the proving key's
  embedded verifying key while self-verification uses `event_vk.bin`.
  A directory at one of the four key paths — usually a bind mount whose
  host path does not exist — is refused in stage 1 rather than surfacing as
  a stage-5 `EISDIR`. So is a FIFO or device node, which used to hang the
  probe with no timeout: `open(2)` on a FIFO blocks until a writer appears.
  A real run makes two extra passes over the ~2.65 GB key during preflight;
  `--dry-run` makes none, so it says nothing about the cache.

- **An interrupted keygen no longer leaves 2.65 GB nobody can see.** Key
  files are published through a temp file that removes itself on drop — but
  not when the process is killed, and a proving-key write is ~2.65 GB over
  minutes. What was left behind was a `.tmpXXXXXX` dotfile that `ls` did
  not show and nothing removed, holding exactly the headroom the next
  keygen needs. `probe_event_keys` now reports them on every invocation and
  removes them under `--repair`, matching `.tmp` plus exactly six
  alphanumerics and regular files only. **The sweep takes every keygen lock
  first and refuses if any is held**, naming the circuit: that name shape is
  what a live keygen is writing into, and unlinking it kills the keygen at
  the end of its seven minutes — under the withdraw CLI, at stage 5 after
  the burn. The preflight's out-of-space refusal names how much of the
  shortfall they hold and the command that reclaims it.

- **The halo2 circuit crates are pinned by revision, not `branch = "main"`.**
  `EVENT_CIRCUIT_REVISION` is bumped by hand and is the only thing between
  a moved Circuit 4 and a "warm" verdict over keys built for the old one —
  the manifest, the revision and the digests all still match, because the
  *files* did not change. Under a branch, one `cargo update` did that
  silently and the mismatch surfaced as a rejected proof at stage 5. All
  five crates now name revision `5356b178cce8ab5a283096032c774533bcab8e28`,
  the commit `Cargo.lock` already resolved, so nothing that gets built
  changes. Not covered: `crates/bridge-snark-utils` declares the same
  crates at `branch = "main"`, has no committed lockfile, and is built by
  no CI job.

#### Documents

- **A state-write failure has its own exit-10 recovery.** When the AN side
  succeeds and the CLI cannot record it — a full disk, a read-only mount, a
  state directory it may not write — the refusal points at the advanced
  runbook's Case 3a, which is capture-timeout diagnostics for exit 11 and
  whose two sub-cases are both about an event that never arrived. An
  operator whose log says `capture + prove complete` reads that heading and
  concludes they are in the wrong place. Case 3a gains a third sub-case,
  and the README names this as its own exit-10 situation: the outcome is
  not unknown here, only the record is behind. The remedies differ — a
  record still carrying an `an_tx_hash` resumes on a plain `--allow-retry`,
  while a hash-less `reserved` one needs reconciliation first.

- **Every `cast logs` command in the withdraw docs was unrunnable, and each
  failed silently** — an empty result reads as "nothing happened on chain",
  the opposite of what a recovery needs. `--from-block latest-2000` and
  `-1000` are not things `cast` accepts, and both now compute the height
  from `cast block-number`. `grep -c BlockVerified` counted a string
  `cast logs` never prints, and answered `0` for a healthy daemon; it
  counts `blockNumber` lines now. And
  `WithdrawalExecuted(uint256,address,uint256,uint256)` is not an event
  this bridge emits — it is `WithdrawalByProofExecuted(uint256 indexed
  nullifier, address indexed recipient, uint256 amount, uint256 indexed
  tokenId, address submitter)`, so the query returned nothing after a
  *successful* payout, in the two places an operator reaches while
  reconciling one.

  The bundle-daemon health check also gets a criterion that matches what it
  guards: freshness rather than a count. Bundles land ~437 Sepolia blocks
  apart (~87 min) in L2 mode and `COVERAGE_WAIT` is 120 min, so a daemon one
  cadence behind consumes the whole stage-4b budget *after* the burn. It
  reads the last event's age and pairs it with a GraphQL query for the
  chain's own `seq_no`, which is what separates a stalled relayer from an
  idle chain.

- **The witness check read the wrong nesting level.**
  `jq .layer_idx work_dir/event_*_witness.json` answers `null` on a healthy
  run — the field lives under `.anchor` — and `null` is not `1`, so the
  check reported the failure it exists to detect. It is
  `jq '.anchor | {layer_idx, height}'` now, with `height` included because
  it must equal the `target_covering_seq_no` stage 4b printed. Relatedly,
  no binary ever prints `layer_idx=`, which both documents told operators
  to grep for: the enricher logs `resolved anchor: L2` and `anchor_layer=L2`
  (1-indexed), and `layer_idx` is a witness-JSON field that is 0-indexed.

- **README's dedup-key formula did not describe the key.** It promised
  "SHA-256 of `{from}|{to}|{to_chain}|{amount}` (all ASCII)". Only `from` is
  ASCII: the recipient is hashed as **20 raw bytes**, the chain id as a
  big-endian `u64` and the amount as a big-endian `u128`. The record's
  filename is that digest and Case 3a asks the operator to compute it, so
  the published formula sent them to the wrong file. Two golden vectors are
  now beside it.

- **`proof_event_<seq>.json` is not in `work_dir/`.** Both directory
  listings placed it there and two runbook diagnostics looked for it there;
  it is written only under `--prover-out-dir`, which has no default, so the
  listings described a file that never appears and the diagnostics could not
  fire. The witness file beside it does live under `--work-dir`.

- **Witness and proof files are named after the event again.** The seq_no
  stamped into `event_<seq>_witness.json` and `proof_event_<seq>.json` was
  hard-coded to `0`, so two withdrawals through one `--work-dir` overwrote
  each other's witness — which the runbook tells operators to keep, because
  regenerating it is expensive. The documents also had the witness filename
  backwards (`witness_event_<seq>.json`).

- **There is no age at which deleting a `Reserved` record is safe.** Both
  documents said files over 24 hours old with no `an_tx_hash` could be
  pruned because "the burn never happened" — asserting as fact exactly the
  half the code says is unknowable, about the file that prevents a second
  burn. They now state the two real conditions: on-chain reconciliation
  showing no `initiateWithdrawal`, **and** no process holding the
  withdrawal.

- **The delete-the-record procedure covers all three liveness verdicts.**
  It listed two and said to delete in the second, so an operator on a
  lockless mount reading by elimination deleted a record while another run
  may have been inside `burn::send`. The three verdicts are a table now,
  the third routed to "do not delete" with its two ways out. The same
  two-verdict elimination appeared in three more places — the README's
  cleanup rule, a "safe to prune between demos" list, and step 3 of the
  refusal the CLI itself prints — and all now point at the canonical
  procedure. A test holds the shipped documents to it: any block that sends
  someone to the exit-3 refusal for the liveness answer has to say the
  answer can be missing.

- **Exit 10 no longer tells you to deploy a fresh multisig.** Its
  remediation for a key mismatch ended "re-run
  `scripts/deploy_msig_and_mint.sh` … and start over". That deploys a NEW
  multisig — a different `--from`, so a different dedup identity — which
  orphans the record for the burn already on the wire. Both documents now
  say to correct the key file and re-run the same command.

- **Exit 10 no longer promises the AN tx hash is on the record.** The table
  said the record is "persisted with the AN tx hash", which is the reverse
  of the dominant case: a failure inside the send propagates before the
  write, so the CLI cannot record a hash it never learned. The hash is
  there only for the exit 10s raised after the send returned.

- **A re-run resumes a recorded burn only with `--allow-retry`.** Five
  sentences across both documents promised it unconditionally. A test now
  holds them to it: any sentence promising a reader that their re-run will
  resume has to name the flag it needs.

- **Case 3a's first diagnostic matches the log again.** It grepped for
  `capture: matched`, `capture: polling` and `enrich_witness: filling`,
  none of which any binary has ever written, so an operator could not
  classify their incident before reaching the advice below it. It greps
  `captured WithdrawalInitiated event` now — the one line the capture stage
  writes on success, whose absence is the whole diagnosis.

- **The live relayer runbook** now provisions the required K=22 SRS,
  documents runtime `solc 0.8.19`, isolates the relayer cursor per mode,
  treats deploys as irreversible broadcasts and keeps production secrets
  outside Git. The deployment helper no longer writes a private key into
  tracked config, archives previous prover state instead of deleting it,
  and requires `CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS` before any
  broadcast.

#### Scripts and fixtures

- **`scripts/deploy_msig_and_mint.sh` works on Linux and emits a 0400 keys
  file.** The committed `python/bin/tvm-cli` was a macOS-arm64 binary that
  PATH injection made win over a working system install ("Exec format
  error"). It is no longer tracked, and tvm-cli is discovered by trying
  candidates until one answers `version` (`CLI_NAME` still overrides). The
  emitted keys file is mode 0400, which the very next documented step
  requires.

- **`scripts/deploy_msig_and_mint.py` quotes what it prints.** README Step
  2 is `eval "$(scripts/deploy_msig_and_mint.sh)"`, so every character on
  stdout becomes shell code: `WITHDRAW_FROM_KEYS` derives from
  `BRIDGE_WORK_DIR`, so a path with a space produced a broken `export` and
  one containing `;` or `$(…)` executed. The path is `shlex.quote`d, and
  the address is refused unless it is `<64-hex>::<64-hex>`. The Python
  helpers also no longer build `cd {dir} && {cmd}` for `shell=True`; they
  pass `cwd=`, which hands the path to the kernel rather than to a parser.

- **Importing the Python helpers no longer runs `tvm-cli`.**
  `helper/common.py` resolved the binary at import and ran
  `tvm-cli version` through a shell with no timeout, so a candidate that
  blocks hung anything that merely imported the module. Resolution is lazy,
  cached and bounded by a five-second timeout with stdin closed, and its
  banner goes to stderr — it used to go to stdout, straight into the `eval`
  above.

- **The fixture's `tvm-cli` resolver fails where it decides.** When every
  candidate failed its probe it returned the first one anyway, so the
  deploy started and the failure arrived later as "Exec format error" from
  a command the operator never chose; with nothing on `PATH` it returned a
  path that does not exist in this repository. It raises now, listing every
  candidate. `scripts/check_fixture_prereqs.sh` picks the same one: it used
  `command -v`, which answers with the first match only, and therefore
  refused the exact arrangement the fixture supports.

- **`scripts/check_voucher_abi_consistency.py`'s default `--compiled` path
  exists.** It pointed at a nonexistent directory.

- **The CI alternate-keyset guard can fire.** `git ls-tree` ran after a
  `cd` into `crates/bridge-prover-libraries` and asked for that path again,
  so it always answered empty: setting `BRIDGE_ALT_KEYS_REF` failed the job
  every run while blaming the operator's ref, and the three regressions
  never ran.

- **`scripts/live_smoke.sh` forwards its arguments.** It `exec`ed the CLI
  without `"$@"`, so `live_smoke.sh --allow-retry` ran without the flag and
  produced a perfectly plausible refusal. Extras are echoed as
  `extra args:` before the run.

- **`bridge-prover-lib`'s test suite stops reporting a moving number.**
  Four tests in `paths::tests` mutate the process-wide environment behind a
  guard that restored but did not serialise, and a fifth reads what they
  write; measured at 7 failures in 40 runs before, 0 in 40 after. Expect
  **`108 passed; 2 failed; 16 ignored`** — the two always being
  `keys::common::tests::load_srs_downsizes_*`, which need a
  `params/kzg_bn254_17.srs` this repository does not ship. Any other number
  is a real regression. None of this was visible in CI: every
  `-p bridge-prover-lib` invocation is filtered to `keys::`, and no fmt or
  clippy job covers the crate. **Until a fmt job lands, do not run
  `cargo fmt` against it** — it is 347 hunks from rustfmt's output at HEAD.

#### Elsewhere

- **`aggregate-proof` receives an absolute `--inner-snark` path.** The
  subprocess is spawned with `current_dir = aggregator_dir`, so a relative
  snark path — which the default `--snark-dir=./shplonk-snark` produces —
  resolved against the wrong CWD and the subprocess exited with `No such
  file or directory`.

- **The state record drops its unused `proof_json_path` field.** It was
  written as `null` on every record and read by nothing. `--prover-out-dir`
  still writes `<dir>/proof_event_<seq>.json`; what it never did was put
  that path on the record. Records stay compatible in both directions, so a
  rollback mid-withdrawal is unaffected.

- **The burn stage no longer invents a success.** A transaction carrying
  neither `aborted` nor `compute.exit_code` was read as "fine", and an
  empty transaction id was left-padded into `0x000…0` and reported as the
  burn's hash — the first wrote a durable `burned` status after a
  possibly-reverted call, the second produced a hash matching nothing in
  the event query, forever. Both are exit 10 now. A short id is also
  lowercased, since the event query is byte-exact and an upper-case one
  timed out five minutes after the money moved. This logic is now its own
  function with tests, where previously nothing in the suite could reach
  it.

- **On-AN ABI artifacts realigned to shellnet.** Dropped stale
  `anWorkchain int8` from `confirmDeposit` inputs and the `DepositFinalized`
  event in both runtime copies of `USDCBridge.abi.json`; rewrote
  `DepositVoucher.abi.json`'s constructor to the 5-arg `(depositId,
  contractAddr, dappId, amount, anAccount)` schema. Withdraw runtime paths
  were already correct — no calldata change.

- **GraphQL BK-update range queries** cap their open-ended upper bound at
  the schema's signed 64-bit `Int` maximum instead of serializing
  `u64::MAX`, which live servers reject during integer coercion.

- **L2 warm-resume startup** compares the immutable genesis anchor at
  `anchor_level - 1`; a valid level-2 state no longer fails drift
  validation after a clean restart.

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
- `crates/bridge-prover-libraries/python/bin/tvm-cli` and
  `crates/ackinacki-bridge/tvm-cli.conf.json` are no longer tracked;
  both are now gitignored. Supply `tvm-cli` on `PATH` or via `CLI_NAME`.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
