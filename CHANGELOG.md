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
### Added

- **Deposit anchor-wait diagnostics.** The relayer exports
  `deposit_relayer_waiting_for_anchor`,
  `deposit_relayer_oldest_anchor_wait_timestamp_seconds`, the
  `deposit_relayer_ticks_total{outcome="waiting_for_anchor"}` outcome, and
  `stage="is_block_accepted"` label in
  `deposit_relayer_stage_duration_seconds`.
  Outstanding entries in `state.json` gain optional
  `anchor_wait_started_unix` and `anchor_wait_last_warned_unix` timestamps;
  older state files remain readable. Missing anchors warn immediately by
  default; `--anchor-wait-warn-after-secs` (`ANCHOR_WAIT_WARN_AFTER_SECS`,
  default `0`) can delay the warning when coverage is monitored separately.
  Warnings repeat at most every 15 minutes per deposit across restarts.
  The logs identify the deposit, chain and block, distinguish the initial
  wait from a delayed warning, and remain visible if saving state fails.
  Before manual admission, compare the logged block number and hash using
  `eth_getBlockByNumber` on an independent RPC for that chain; an expected
  delivery delay alone is not a reason to admit a hash manually.

### Changed

- **Deposit retries alternate with new work.** Finite retry rounds prevent
  incoming traffic from indefinitely delaying older deposits. An anchor wait
  resets the daemon polling delay to `--backoff-initial-secs`, including
  after idle polls or errors. Per-deposit anchor polling uses the larger of
  existing failure backoff and one quarter of the observed wait age, with
  the age component capped at 60 seconds; `OUTSTANDING_BACKOFF_SECS=0`
  disables per-id delays. Fresh waits retain the short base interval.
- **`deposit_relayer_outstanding_deposits` includes healthy anchor waits.**
  Adjust alerts that treat any nonzero value as failure to distinguish
  failed attempts and missing anchors using their age and coverage.
  An uncovered block can require the anchor writer's intervention.
- **The deposit daemon's `AN_BRIDGE_ABI_PATH` must include
  `isAcceptedBlockHash(uint256,uint256) returns (bool)`.** Upgrade a custom
  ABI alongside the daemon; a missing getter causes tick errors and stops
  delivery. The full bridge ABI shipped in the repository includes it.

### Fixed
### Removed
-->

## [Unreleased]

### Documentation

- **`README.md` opens with *The current deployment*.** No bridge address is
  pinned in the tree any more, so a checkout had nowhere to read the live
  ones. The section carries the Sepolia bridge and its deployment block, the
  USDC it accepts, the shellnet `eccUSDCBridge` version, code hash and light
  client, and the four settings to copy. Read back from both chains.

### Documentation

- New [`REFACTORING.md`](REFACTORING.md) at the repository root, registered in
  `DOCS.md`: a backlog of changes that are not bugs. Six entries to start —
  `num_active_layers` naming the opposite of what it means; the EVM deployment
  address pinned separately by the CLI profile and the web form, which is how
  the form spent a week on a retired bridge; the deposit and withdrawal sides
  keeping separate chain lists that have already drifted over Arbitrum One; the
  literal `vmshell` amounts on the `DepositVoucher` deploy and its
  `confirmDeposit` callback; guard scripts wired into no pipeline; and the
  unbounded owner mint. Entries carry `file:line` and the shape of the change,
  and are deleted when done rather than ticked.

### Fixed

- **The deposit relayer waits for the deposit block's AN anchor before
  proving or submitting it.** Ethereum `--confirmations` can finish before
  the light-client relayer delivers that block. Previously each retry built
  a proof and called `finalizeDeposit` too early, producing exit 224 and
  `an_rejected` alerts. A missing anchor now causes a healthy wait without
  proving, submission, failed attempts or parking. Getter errors and actual
  contract rejections remain visible. Upgrade the daemon; no contract
  redeploy or VK change is needed.

- **The web deposit form explains what the wallet said instead of printing
  the raw rejection.** Clicking *Connect Wallet* twice before answering
  MetaMask used to put the whole EIP-1193 error object — code, message and
  the extension's stack — into a browser `alert()`. The form now reads the
  error's `code` and says what to do: `-32002` points at the extensions
  button, because MetaMask keeps a pending connection request behind its
  toolbar icon and never raises a window for it; `4001` reports a rejection
  in one line; `4100` asks for the wallet to be unlocked; `4902` explains the
  prompt that offers to add Sepolia. Anything else shows the provider's own
  `message`. The untouched object still goes to the browser console.
- **The web deposit form's *Connect Wallet* button no longer sends a second
  request while the first is open.** It is disabled and reads *Check
  MetaMask…* until the wallet answers, so a second click cannot produce the
  `-32002` it used to.

### Documentation

- Clarify the owner-anchor flip trust model (bridge-audit #57 / #106).
  `disableOwnerAnchorsOn(chainId)` disables direct owner writes for that chain,
  while owner upgrades and light-client replacement remain available. Bridge
  upgrades reset the flags and recorded clients: restore configuration and
  repeat the intended flips afterwards. NatSpec and operator runbooks now state
  these powers consistently. An owner retaining `updateCode` can install
  arbitrary replacement code that changes or removes the current checks, so
  both owners remain trusted after a flip. No contract behavior, ABI or
  deployment artifact changes.

- Ethereum and Acki Nacki share one per-deposit ceiling of `type(uint64).max`
  USDC base units. `AckiNackiBridge.deposit` reverts `DepositTooLarge` above it;
  `finalizeDeposit` refuses a proven amount that does not fit `uint64` with
  exit 214. There is no extra product minimum besides `amount > 0`, and the
  100 USDC figure in older notes is not in the contracts. The deposit
  form now uses that same `uint64` cap and Circle Sepolia USDC
  (`0x1c7D4B19…`), with the bridge address supplied at build time as
  described below. It used to refuse amounts above 100 USDC and approve
  the Aave faucet token. Test USDC comes from Circle's faucet, not an
  in-form mint. The ursus deposit-relayer example uses the same full-history
  Sepolia RPC (`rpc.sepolia.ethpandaops.io`) as the bridge-relayer example;
  it must retain logs back to the configured deployment block.
- New `crates/deposit-relayer-daemon/docs/operator_runbook.md`: how to run the
  deposit relayer. It covers one daemon per AN bridge and backing up
  `state.json`, later ids going first while failed ids retry from
  `outstanding_deposits`, parking with `--skip-after-attempts` and finishing
  parked deposits with `prove-one` + `finalize-one`, which outcomes are retried
  automatically (pending, paused lane) and which need a person, a fix for each
  exit code, the `--force-state` checklist after a redeploy, alert rules over
  the Prometheus metrics (`deposit_relayer_outstanding_deposits` for a stuck
  id), and failing over to a cold standby by copying the full `state.json`
  (QC-OFF-01, -02, -08, -11). A pending submit stays at the base outstanding
  delay (it does not increment attempts). `--force-state` also drops
  outstanding ids and logs them. An outstanding id with no stored log is
  scanned from the persisted `scan_done_through_block` plus one, not from
  `--from-block` and not to the live head.

- `live_relayer_bridge_verifyBlock_runbook.md`: the `AttestationProofRejected`
  row told operators to re-prove `applyBkSetUpdate` against the live
  `storedLastSeenBlockSeqNo`. That value reverts
  `AttestationLastSeenNotBeforeSeqNo`; pass the `last_seen` the attestation
  was proven against.

- `docs/aave-yield.md` gains *If AAVE lost money*: what still pays out when
  `suppliedPrincipal()` is above `aUsdcBalance()`, and when to call
  `writeOffUnbackedPrincipal()`. It no longer says `harvestYield` accepts
  `type(uint256).max` — it never did.

- Pause is two owner switches, not one. `AckiNackiBridge.pause()` on Ethereum
  does not call `eccUSDCBridge.setPaused`, and `setPaused` does not call
  `pause()`; Circle can still freeze USDC itself. An incident that must stop
  both directions of the bridge has to flip both switches —
  `contracts/an/README.md`, `docs/EVM-custody-and-accounting.md`.
- The prover and circuit docs describe BK-set rotation as it works:
  `applyBkSetUpdate` with a Circuit 1A/1B attestation of the rotation block
  and its opened L2/L3 leaves, no circuit of its own. Gone are the
  "Circuit 3 (BK Set Update)" future-work item, references to a plan
  document that does not exist, and the claims that shellnet rotation is off
  (it has been on since 2026-10-08) and that the daemons take the BK set from
  GraphQL (they read the file at `BRIDGE_BK_SET_CONFIG`; on shellnet it must
  be the set at the seed) — in `crates/bridge-prover-libraries/TECHNICAL_README.md`,
  the prover daemon and circuits READMEs and `PROBE_BK_UPDATES.md`.
- `contracts/an/README.md` gains *Upgrading the bridge*: why the voucher's code
  is part of replay protection, and that an upgrade changing it has to ship a
  block cutoff in the same code so past deposits stop going through. The
  current contracts need no such restriction — the voucher's code is installed
  by the zerostate and `updateCode` is called with an empty `userCell`.
- `scripts/ursus/flip_deposit_to_light_client.md` and
  `scripts/ursus/eth_lc_shellnet_e2e.md` pin the Gloas fork slot with
  `eth-lc-relayer set-gloas-fork-slot` after `deployLightClient`. A fresh
  deploy rejects every `submitUpdate` / `submitRotate` with 256 until it
  runs.
- `docs/EVM-contracts-spec.md` §7.1 and the `PrevAnchorMismatch` runbook
  row: `expectedPrevAnchor(n)` keeps anchoring at the highest layer the
  bridge has recorded. A window with `dataLen > 0` is never cleared, so
  that layer stays in `t` when later key blocks skip it.
- `AckiNackiBridge.deposit`'s `anWorkchain` is telemetry. It is kept in
  the ABI and the `Deposit` event so the log shape stays stable, but it
  is not a deposit-proof public input and Acki Nacki always credits
  workchain 0 (`eccUSDCBridge._parsePublicInputs` /
  `address.makeAddrStd(0, anAccount)`). A wrong value changes nothing.
  That is the accepted product choice, not a gap to close by putting
  workchain back into the circuit (QC-AN-J3 / gosh-sh/bridge-audit#59).

### Breaking Changes

- **Every prover the relayers start is a binary the operator names, checked
  at startup; the `cargo run` fallbacks are gone.** A daemon started with a
  wrong path used to find out at its first proof, or, with no binary where
  it looked, to build one from whatever source tree sat on the host. Now
  each path is a flag with an environment variable, the file has to exist
  and be executable before anything connects, and the daemons never derive
  `target/release/…` from a directory or call `cargo`.
  - `eth-lc-relayer daemon` / `prove-one`: `--step-prover-bin`
    (`LIGHT_CLIENT_STEP_PROVER_BIN`), the prebuilt `export_step_vk_blob`
    example (`daemon`: required unless `--mock-prove`); `daemon
    --rotate-prover-bin` (`LIGHT_CLIENT_ROTATE_PROVER_BIN`), the prebuilt
    `rotate_tree_n8` example (`--features aggregation`), required unless
    `--no-rotate` or `--mock-prove`, and ignored with `--no-rotate`.
    `--prover-dir` (`LIGHT_CLIENT_PROVER_DIR`) is the working directory of
    the provers: required when the daemon may rotate (`rotate_tree_n8` reads
    its shard snarks and outer SRS there, and the period check its fixture),
    otherwise optional, a relative `--srs-path` resolving against it. The
    SRS is checked at startup to be where the step prover will read it.
    Units no longer need `cargo` on `PATH` or a rustup home.
  - `deposit-relayer daemon` / `prove-one`: `--fetch-deposit-data-bin`,
    `--export-vk-blob-bin`, `--export-blake2b-proof-bin`
    (`DEPOSIT_FETCH_DEPOSIT_DATA_BIN`, `DEPOSIT_EXPORT_VK_BLOB_BIN`,
    `DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN`), the three prebuilt examples;
    `--deposit-prover-dir` stays as their working directory (the `data/`
    SRS and proving-key cache live under it) and now also reads
    `DEPOSIT_PROVER_DIR`.
  - `relayer daemon-live`, `withdraw-e2e` and `prove-withdraw-shplonk`:
    `--aggregate-proof-bin` (`BRIDGE_AGGREGATE_PROOF_BIN`) replaces
    `--aggregator-dir` (`BRIDGE_AGGREGATOR_DIR`, `AGGREGATOR_DIR`); the
    subprocess runs in the binary's own directory. `relayer prove-withdraw`:
    `--prover-bin` (`BRIDGE_WITHDRAW_PROVER_BIN`) replaces
    `--bridge-prover-libraries-dir`, and `--work-dir`
    (`BRIDGE_WITHDRAW_PROVER_WORK_DIR`) is required.
  - shellnet-l2 kit: the image keeps `aggregate-proof` at
    `/opt/gosh-relayer/bin/aggregate-proof`, `container-paths.sh` exports
    `BRIDGE_AGGREGATE_PROOF_BIN`, and the preflight pins the checksum under
    that name. A host-layout `runtime.env` sets `BRIDGE_AGGREGATE_PROOF_BIN`
    instead of `BRIDGE_AGGREGATOR_DIR`. Rebuild the image to pick this up.
  - `ackinacki-bridge withdraw` is unchanged: it keeps `--aggregator-dir` and
    hands the library the binary its own preflight already checks.

  Migration: build the provers once (`cargo build --release --example
  export_step_vk_blob` and `cargo build --release --features aggregation
  --example rotate_tree_n8` in `eth-light-client-prover/`; `cargo build
  --release --examples` in `deposit-prover/`; `cargo build --release --bin
  aggregate-proof` in `crates/bridge-evm-aggregator`), put the paths in the
  env files (`scripts/ursus/*.env.example`,
  `crates/eth-light-client-relayer/deploy/shellnet-shadow/env.example` and
  `crates/bridge-relayer-daemon/deploy/shellnet-l2/runtime.env.example`
  show where) and the unit files (`scripts/ursus/*.service`), then restart.
  A daemon started from an old unit refuses with the name of the missing
  flag.

- **`eccUSDCBridge` is 1.12.0, and neither 1.11.0 exists.** Two branches each
  built a 1.11.0: one adding owner `mintAndSendBridged`, one carrying the
  deposit ABI without `anWorkchain` and its regenerated key. They are now one
  contract with both changes, rebuilt with sold 0.80.0, code hash
  `a1fd7d19…`. Any note naming `aeeadc7f…` or `774c9571…` describes a binary
  that was never deployed. `NEWEST_KNOWN_BRIDGE_VERSION` is 1.12.0.

- **Deposits no longer have a workchain parameter.** For a fresh deployment,
  use `deposit(uint256,bytes32)` and
  `Deposit(uint256,address,uint256,bytes32,uint256)`; the event's non-indexed
  data is now 96 bytes. The CLI, frontend, deposit relayer and prover use this
  format, and witness JSON no longer includes `an_workchain`. Deploy the matching
  Ethereum contract and `eccUSDCBridge` 1.12.0 with the regenerated deposit
  verification key (code hash `a1fd7d19…`), and use the updated clients
  and prover together. Proofs for the previous event format do not verify under
  this key. Withdrawal interfaces, DApp ID handling and the 12 deposit public-input
  slots are unchanged. The shipped CLI profile and relayer template no longer
  pin an earlier EVM deployment: set `BRIDGE_ADDRESS` and its deployment block
  (`BRIDGE_DEPLOY_BLOCK`, and `FROM_BLOCK` for the deposit relayer). Replace old
  values in reused profiles explicitly. Build the frontend with
  `BRIDGE_CONTRACT_ADDRESS` set to that same new EVM deployment; without it,
  deposits and approvals are disabled before any wallet request.

  This rollout assumes a fresh pair and provides no in-place migration of a
  funded bridge. Before replacing an existing AN bridge's code, pause new
  deposits on its old EVM bridge and finalize every pending old-format deposit
  with the old AN bridge unpaused and its matching prover/relayer. Verify the corresponding
  finalized AN credit for every old deposit ID below the paused EVM bridge's
  `depositCounter` before replacing the AN code or removing those tools.
  The 1.12.0 key cannot finalize 128-byte events,
  and the old EVM contract has no deposit refund path. Custody and outstanding
  withdrawals on an existing pair require a separate migration plan.

  `ackinacki-bridge deposit` now refuses an AN bridge older than 1.12.0
  before requesting a transaction. For the new deployment, configure the
  matching EVM/AN pair and its trust settings first, install the matching prover
  and clients, then publish the new addresses and start deposits. Do not switch
  clients alone against an earlier deployment. The installer preserves an
  existing profile, so its deployment values must be updated by the operator.
- **`eccUSDCBridge` 1.12.0 adds owner `mintAndSendBridged` for CLI exit 37.**
  After a voucher-consumed `confirmDeposit` abort the deposit cannot be
  finalized again, Ethereum already holds the USDC, and ordinary
  `mintAndSend` does not increment `getTotalBridged` — last withdrawers
  then hit 233. `mintAndSendBridged` matches `confirmDeposit`: it mints
  ECC[3] and credits `_totalMintedBridgeByToken`. It shares `_mintNonce`
  with `mintAndSend`. Do **not** use it for ordinary owner mint: that
  would invent bridged inventory the Ethereum treasury does not hold.
  `mintAndSend` / `mintAndSendAccumulator` / TIP-3 mint still increment
  only `getTotalMinted`. `mintAndSendBridged` is not bound to a deposit
  id: the operator must match the aborted deposit's recipient and amount,
  once — a second call or a mistyped `value` adds inventory Ethereum does
  not hold.

  ECC[3] stays fungible: the cap is still an inventory check, not a
  per-depositor reservation. That half of QC-AN-05 is the accepted
  product choice.

  Upgrade with `updateCode` and an empty `userCell` — the storage *layout*
  is unchanged, but `onCodeUpgrade` still `resetStorage`s and restores
  only pubkey, usdcWallet, `_totalMinted`, both mint nonces, the two
  bridged maps and the voucher code. It wipes `_paused`,
  `_trustedL1Bridge`, `_acceptedBlockHash`, `_lightClientCode`,
  `_lightClients`, `_lightClientL1ChainId` and `_ownerAnchorsDisabled`.
  After the call, re-run `setPaused` if the lane was paused,
  `setTrustedL1Bridge`, and `setLightClientCode` / `deployLightClient`
  before deposits work again (222/224 until then). Owner anchors come
  back on for every chain (`_ownerAnchorsDisabled` is wiped). If a chain
  had been flipped to light-client-only, call `disableOwnerAnchorsOn`
  again after `deployLightClient` — it turns the owner writer off and
  reverts 226 until a client exists. The ABI gains `mintAndSendBridged`.
  `eccUSDCBridge` 1.12.0,
  code hash `a1fd7d19…`, rebuilt with sold 0.80.0. The deposit CLI's
  exit-37 message names the aborted deposit's `depositId`, amount, recipient,
  EVM bridge and chain, and tells the operator to call `mintAndSendBridged`.
  `NEWEST_KNOWN_BRIDGE_VERSION` is 1.12.0.

- **`eccUSDCBridge.initiateWithdrawal` refuses a burn that would take
  bridged burned above bridged minted for that token, exit 233
  (`ERR_BURN_EXCEEDS_BRIDGED`).** Read the totals with `getTotalBridged`.
  Ordinary owner `mintAndSend` / `mintAndSendAccumulator` and TIP-3 mint
  increment only `getTotalMinted` and add no headroom. 1.12.0's
  `mintAndSendBridged` is the exception, for exit-37 recovery only. ECC[3]
  is fungible, so such ECC can still be withdrawn against headroom that
  deposits created: the cap limits total bridge payouts to total deposits,
  not one depositor's own withdrawal.

  A fresh zerostate starts both counters at 0, so the cap holds from the
  first burn. `updateCode` copies the maps as they are. On a live bridge
  that is already inverted (`getTotalBridged` burned above minted — the
  current shellnet USDCBridge answers minted 0, burned 1 000 000), every
  burn is refused until `finalizeDeposit` brings minted up to burned.
  That is not the same as a fresh deploy.

  Included in the `eccUSDCBridge` 1.12.0 build described above. The withdrawal
  ABI is unchanged. The withdraw CLI preflight checks
  `getTotalBridged` and refuses before the burn.
  `eccUSDCBridge` 1.10.0, code hash `4b2a9600…`, rebuilt with sold 0.80.0.
  The 1.10.0 ABI was unchanged; 1.12.0 above adds `mintAndSendBridged`.
  The withdraw CLI preflight checks `getTotalBridged` and refuses before
  the burn.

- **The relayer's owner flip and the deposit CLI read the light client per
  chain, and the relayer now needs a 1.9.0 bridge.** `eth-lc-relayer
  flip-owner` / `daemon --flip-owner` takes the chain id from the light
  client's own `getConfig`, checks the bridge's `getLightClient(l1ChainId)`
  against `AN_LIGHT_CLIENT`, reads `isOwnerAnchorsEnabled(chainId)` for the
  already-flipped signal and calls `disableOwnerAnchorsOn(chainId)` — it no
  longer calls `getAnchorConfig` or the no-argument `disableOwnerAnchors`.
  Against a 1.8.0 bridge `getLightClient(uint256)` has a different function
  id than that release's `getLightClient()`, so upgrade the bridge to 1.9.0
  **before** the relayer: an older bridge answers the new call with "function
  ID is wrong". Both slim relayer ABIs under
  `crates/eth-light-client-relayer/abi/` changed with it (`getConfig` on the
  client; `getLightClient`, `isOwnerAnchorsEnabled` and
  `disableOwnerAnchorsOn` on the bridge).

  `ackinacki-bridge deposit` reads `getLightClient(chainId)` for the
  deposit's own chain on a 1.9.0 bridge and keeps `getAnchorConfig` below
  that, so a deposit on a chain whose client was not the last one deployed is
  no longer refused. A bridge that reports 1.9.0 without the per-chain getter
  is answered once and falls back with a warning instead of being retried for
  ever. 1.9.0 was the newest version the CLI was checked against in that
  change; 1.12.0 above is now `NEWEST_KNOWN_BRIDGE_VERSION`.

- **One light-client code, one client per source chain, addressed by code
  salt.** `setLightClientCode` now only stores the code. The address is
  derived when `deployLightClient` runs, from that code salted with
  `abi.encode(l1ChainId)`, and kept in `_lightClients[l1ChainId]`; the same
  code therefore yields a different account for every chain, and
  `getLightClient` takes the chain id. `initialize` on the client decodes its
  own salt and refuses a chain id that does not match it
  (`ERR_WRONG_CODE_SALT`, 258), so an address is bound to the chain it
  follows. `acceptBlockHashFromLightClient` and
  `forgetBlockHashFromLightClient` check the sender against the client of the
  chain in the call, so a client for one chain can no longer admit or forget
  hashes for another; `disableOwnerAnchors` requires a client deployed for
  that chain rather than a stored id.

  Versions and code hashes after both changes below: `eccUSDCBridge` 1.9.0,
  `697d1711…`; `EthBeaconLightClient` 1.5.0, `3e6e5102…`. The bridge's
  `getLightClient` takes a chain id, so its ABI changed with it. A light
  client deployed before this change came from unsalted
  code, so the bridge derives a different address for its chain and stops
  recognizing it as a writer — redeploy it with `deployLightClient`. Nothing
  is lost: the client holds no funds, and its head and committee come back
  from the chain it follows. The bundled bridge ABIs under `crates/` are
  updated for the new `getLightClient` signature.

- **The light client's constructor takes no arguments; the bridge initializes
  it with a second call.** `EthBeaconLightClient` gains
  `initialize(pubkey, l1ChainId, bootstrapCommittee, bootstrapPeriod)`, which
  only `USDCBridge` may call and only while the owner key is still zero
  (`ERR_ALREADY_INITIALIZED`, 257). `deployLightClient` takes the same
  arguments as before and now sends two messages: the deploy, then
  `initialize` — in that order, so it is the first message the new account
  sees. Nothing about the settings travels in the state init any more.

  `onlyOwnerPubkey` on the client now also refuses while the owner key is
  zero. An unsigned external message carries `msg.pubkey() == 0`, so without
  that check every admin call would pass in the window between the deploy and
  `initialize`.

  **Both Acki Nacki artefacts are rebuilt, and they have to be rebuilt
  together.** The bridge builds the client's *initial data cell* from the
  layout compiled into it (`abi.encodeStateInit({contr: EthBeaconLightClient,
  varInit: {}, …})`), so any change to the client's state variables makes a
  previously built bridge binary unable to deploy it. That is not theoretical:
  the artefact pair in this repository had drifted — the bridge binary was
  built before the light client gained `_gloasForkSlot` and
  `_gloasForkSlotSet`, so it built a 310-byte data cell where the client's code
  expected 318, and on a network raised from those artefacts the client's
  constructor aborted with `cell underflow` (exit 9), leaving an account with
  code and no storage at the derived address. The current hashes are the ones
  given above.

  `contracts/an/EthBeaconLightClient.sol`, the standalone copy the
  shellnet-shadow kit deploys with `tvm-cli`, keeps its four-argument
  constructor; `deploy-contract.sh` is unchanged.

- **The beacon light client's step and rotate verification keys are rotated
  for Gloas (ePBS), and both proofs gain a public input.** From Gloas the
  beacon `finalized_checkpoint`, the sync committees and the execution block
  hash sit at different generalized indices (finality 169 → 735, next sync
  committee 87 → 2946, execution block hash 812 → 2856), so the circuits pick
  the index per header from its slot against a Gloas fork slot that is now a
  public input: the step proof has 11 inputs (`gloasForkSlot` at index 10) and
  the rotate proof 16 (`gloasForkSlot` after `period`). Step and rotate proofs
  made for the previous circuits no longer verify — regenerate them, and
  replace `EthBeaconLightClient` code (step VkBlob sha256 `581910a739788e1e4269c9cbf848c29f57a9e9a49e60f5e0178cc1ed67f397db`,
  rotate VkBlob sha256 `4dc3f0a71a1b18775fb13e7ba96ec4eac78f7615b9e521e6bdf9d89b71d4cf54`) with `updateCode`, passing
  `abi.encode(uint64 gloasForkSlot)` as the user cell, or call
  `setGloasForkSlot` after an upgrade with an empty cell. Until the slot is
  set, `submitUpdate` and `submitRotate` reject everything with exit code 256;
  a proof made for a different slot fails with 255. The value is
  `GLOAS_FORK_EPOCH × 32`: `11296768` on Sepolia, `type(uint64).max` (Gloas
  not scheduled) on mainnet. The owner can change it while unset or while
  owner rotation is still enabled; after `disableOwnerRotation` only
  `updateCode` can. Later `updateCode` calls with an empty user cell keep the
  current value. That entry predates the light-client bring-up change above:
  the client now takes no constructor arguments and the bridge initializes it
  with a second call.
  From Gloas, `submitUpdate` anchors the payload bid's `parent_block_hash`
  rather than the checkpoint slot's own execution hash.
  `eth-lc-relayer ancestry-one` starts its walk at that anchored hash
  and scans `checkpoint_slot - 32 ..= checkpoint_slot` (33 slots), so a
  deposit inside a Gloas checkpoint's own payload is not reported as
  covered by that checkpoint.
- **The rotate proof's `period` is proven.** The rotate circuit computes it
  as the attested slot's period plus one (`attested_slot / 8192 + 1`, the
  period the proven next committee serves) instead of loading it as a free
  witness, so a hand-off can no longer be submitted under a period of the
  sender's choosing. `submitRotate` stores it in `_committeePeriod` and emits
  it in `CommitteeRotated`; nothing gates on it. This is part of the rotate
  verification key rotation (rotate VkBlob sha256 `4dc3f0a7…`):
  rotate proofs made before it do not verify. `eth-lc-relayer` refuses a
  rotate proof whose period is not the one it is rotating into, and it
  does not start `rotate_tree_n8` for a live period that tree cannot
  prove (the tree is a committed fixture, period 9413 without an
  `update_period_*.json`; that file may be the beacon `/updates`
  array). Prove the hand-off on n14 and `submit-rotate`.   `--dry-run`
  pins the Gloas slot from the beacon spec (the mock submitter has no
  contract).   A live daemon reads `getGloasForkSlot` before a prove (not on an
  idle tick) and uses that slot: a published `GLOAS_FORK_EPOCH` that the contract
  does not hold would otherwise spend a full step prove and come back
  255. A daemon that started while Gloas was still `FAR_FUTURE_EPOCH`
  re-fetches `/eth/v1/config/spec` until the beacon publishes a real
  slot, so it does not freeze on `u64::MAX` and stop at the fork.
  After a manual `submit-rotate` the next tick reads
  `getCommitteeState` and continues; `submit-rotate --state` also
  writes `last_committee_period` for a restart. `contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.tvc`
  is rebuilt with both changes (same `sold` as before, built against a
  tvm-sdk with `KECCAK256`): the light client's code hash moves
  `8d49b996…cca292` → `8d2ec6dc…ae5336`. The ABI gains `setGloasForkSlot`,
  `getGloasForkSlot` and the `_gloasForkSlot` / `_gloasForkSlotSet`
  fields.
- **`eccUSDCBridge.disableOwnerAnchors` is per-chain.** The previous bool
  `_ownerAnchorsEnabled` turned the owner writer off for every source
  chain at once, so a flip left the six L2s on the deposit allowlist
  with no writer. `disableOwnerAnchorsOn(uint256 chainId)` now sets
  `_ownerAnchorsDisabled` for any chain that has a light client deployed
  for it; a chain without one reverts `ERR_LIGHT_CLIENT_UNSET` (226) and
  chain id 0 reverts `ERR_NOT_LIGHT_CLIENT_CHAIN` (227). The no-argument
  `disableOwnerAnchors` serves the chain whose client was deployed last,
  which is also all `getAnchorConfig()` reports;
  `isOwnerAnchorsEnabled(chainId)` is the per-chain view and
  `getLightClientL1ChainId()` that last chain id (0 before the first
  deploy). L2 deposits keep `setAcceptedBlockHash`. The relayer calls the
  explicit per-chain form (see the light-client bring-up entry above). The bridge reports 1.8.0. Rebuild
  `eccUSDCBridge.tvc` (sold 0.80.0) and upgrade the premined 1.7.0
  bridge; `onCodeUpgrade` resets storage and does not migrate
  `_lightClient` or the L1 id, so both `disableOwnerAnchors` and
  `deployLightClient` revert 226 until `setLightClientCode` runs again.
  Then `deployLightClient` stores the L1 id it is given — the constructor
  message bounces if the light client is already at that address, so
  pass the same id that client was constructed with.
  `ackinacki-bridge deposit` reads `isOwnerAnchorsEnabled(chainId)` on
  1.8.0, and `getAnchorConfig().ownerAnchorsEnabled` on 1.7.0 (that
  getter does not exist there, and retrying it would hang). An L1 flip
  does not refuse L2 deposits **on a CLI from this branch**. Released
  CLIs still read `getAnchorConfig().ownerAnchorsEnabled` as the
  deposit's own mode, so after the flip they treat an L2 deposit as
  light-client and refuse it (`LcFailure::Network`). Ship this CLI
  before flipping.
  A flip through `disableOwnerAnchorsOn` is the same switch: the CLI
  recognises that call in the bridge's history, and the bundled ABI
  matches the compiled 1.8.0 one so it can decode it.
  Do not flip until something late-registers skipped checkpoints so
  consecutive proven anchors sit within one ancestry span: accepted
  checkpoints are often two or more epochs apart, and `eth-lc-relayer`
  still leaves the flip off unless started with `--flip-owner`.
- **`daemon`, `daemon-bridge` and `smoke-fixture` take the bundle stride
  from the bridge.** They read `bundleStride()`; for a bridge deployed
  before that field, set `--anchor-level` / `BRIDGE_ANCHOR_LEVEL` (1 or 2)
  to the level of its anchors, or the command does not start (`bridge …:
  the bridge cannot report bundleStride() (…) and no anchor level is
  configured`). When both are known they must agree. These commands used to
  assume the L1 stride: on an L2 bridge Check B let `last_seen` move 4 096
  instead of 65 536, so one `verifyBlock` whose receipt was lost stopped
  the anchor leg for good, and the stall warning came after 16 polls. The
  server template `scripts/ursus/bridge-relayer.env.example` sets
  `BRIDGE_ANCHOR_LEVEL=2`. `RelayerConfig::new` takes the stride; it has
  no default.
- **`VerifyBlockConfig` gained `uint64 genesisLastBkSetUpdateSeqNo`.** All
  callers that build the struct directly (`DeployShellnetE2EBridge`,
  `DeployRealBridge`, `DeployReuseVerifiersBridge`,
  `DeployGenesisCursorBridge`, `DeployTestBridge`, `GenDepositSetup`, and
  any custom deploy script) must supply the new field. The constructor
  writes it verbatim to `storedLastBkSetUpdateSeqNo`, seeding the
  on-chain bk-update cursor past rotations already folded into
  `genesisBkSetCommitment`. On chains with no historical rotations in
  the GQL retention window (fresh devnet, local) pass `0`; on a chain
  whose GQL still exposes pre-bootstrap rotations, pass the highest
  such `bkSetUpdates.height` — `compute_bridge_anchors` now emits it as
  `GENESIS_LAST_BK_SET_UPDATE_SEQNO`, and
  `crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`
  sources it in automatically via `vm.envOr(..., 0)` in
  `DeployShellnetE2EBridge.s.sol`. Without this
  cursor, a daemon cold-started against such a chain halts on its first
  tick with `prover out of sync` because the drain picks up a historical
  rotation whose pre-state no longer matches the live BK-set commitment.
- **`BootstrapSeed` schema bumped v2 → v3** (added
  `last_bk_set_update_seq_no: u64`). v2 seed files fail
  `BootstrapSeed::load` with the same schema-version diagnostic as the
  v1→v2 bump. Manual migration mirrors it:
  `mv state state.pre_v3_$(date +%Y%m%d_%H%M%S) && mkdir state`, then
  cold-restart the prover daemon so it writes a fresh v3 seed. The
  verifier daemon then picks up the new file on its next startup; the
  startup drift check will refuse to open the daemon until seed and
  state agree.
- **`compute_bridge_anchors` emits a new env line
  `GENESIS_LAST_BK_SET_UPDATE_SEQNO`.**
  `crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`
  persists it into the generated `$CFG_DIR/env` next to
  `BRIDGE_BOOTSTRAP_SEQNO`. Operators do not need to set the var by
  hand; the forge script already consumes it through `vm.envOr(..., 0)`.
- **`submitAncestry` fills the span between two proven anchors instead of
  walking back from one.** The headers go in oldest first, and both ends are
  checked: `headerRlps[0]` and the last header must each hash to a block
  already in the proven set (`ERR_UNKNOWN_CHECKPOINT`, 249), and only the
  headers between them are anchored. The array holds 3 to 33 entries — two
  anchors and the 31 blocks of an epoch. `submitUpdate` keeps its shape: the head
  still only moves forward, and a skipped checkpoint can still be
  late-registered with its own proof and its own slot, which is what keeps
  consecutive anchors within a span's reach. Which committee may sign it is a
  change of its own, below.

  Before, the walk started from one proven checkpoint and anchored that
  block's *parents*, with the checkpoint's slot attached. Anyone could chain
  the calls backwards: each run made 31 more hashes live, every one of them a
  valid starting point for the next, and the one-year window never expired
  them because they carried a fresh slot. History could be replayed in
  indefinitely and the client's state grew with it, and expired keys that the
  FIFO had not yet passed stayed live while they carried a fresh slot. A chain
  in the old order, or one whose far end is not an anchor, is now refused.

  Both ends are checked *before* `tvm.accept()`, inside the pre-accept gas
  credit, so a span that cannot admit anything is refused without the light
  client paying for the walk: a payload starting at a real anchor and ending
  nowhere costs it nothing at all, because the message never reaches `accept`.

  **This does not make `submitAncestry` cheap to abuse.** The call is
  permissionless, the contract pays for everything after `accept`, and a
  transaction that throws still spends the gas it used. A payload whose two
  ends are both real anchors with forged headers between them — they link to
  each other and break only at the last link — costs the light client
  1,917,462 gas and admits nothing, measured on a local node. No cheap check
  bounds this, because verifying the chain is the work the gas pays for. The
  same exposure has been in `submitUpdate` since 1.4.1, where `accept` has to
  precede the halo2 verify. What keeps it from stranding the client is that
  the client mints its own gas: `rePushAnchor` is permissionless, cheap and
  reaches `ensureBalance`, so anyone can restore the balance floor. Operators
  should expect a griefer to be able to burn the client's minted balance, and
  should watch `getBalance` rather than assume the floor holds.

  Callers have to be updated: headers oldest first, and the span must end on a
  proven anchor. `AncestryAccepted.checkpointHash` still carries the newest
  anchor of the span, as it always did.

- **`EthBeaconLightClient` hashes execution headers with the TVM `KECCAK256`
  instruction instead of the `EthKeccak` software implementation, and
  `submitAncestry` fits the gas limit.** One 642-byte Sepolia header cost
  64.68M gas before and costs 4,722 now. Measured on a locally built node
  against the 10M per-transaction limit, with the client sitting at exactly
  its `MIN_BALANCE` floor: 748,015 gas for a 5-header span and 1,976,017 for a
  full 33-header span, where the old software walk extrapolated to ~2.07G.
  Both include the two anchor hashes the call now does before `tvm.accept()`,
  about 24,000 of that. Measured end to end, compute and action phase both,
  not compute alone. A span whose far end is not an anchor costs the light
  client nothing at all: it is refused before `accept`, no transaction is
  committed and the balance does not move.
  On-chain ancestry therefore works: a deposit in a
  non-checkpoint block is covered without the owner key, as long as the
  checkpoints bracketing it are both proven and no more than a span apart. A
  checkpoint the daemon skipped can be late-registered through `submitUpdate`
  with its own proof, which brings the bracketing anchors back within reach —
  but **the daemon does not do it on its own**: it proves only the latest
  finality update. An operator who kept the update's JSON can late-register it
  by hand — `eth-lc-relayer prove-one --finality-json <file>` proves whatever
  it is handed and `submit-one --bundle-dir` sends it — but nothing watches for
  a skip and nothing fetches an older update from the beacon node. Since a
  step proof takes longer than an epoch, accepted checkpoints are often two or
  more epochs apart, the span is then wider than one call can carry, and those
  blocks need `setAcceptedBlockHash` from the owner until something
  late-registers the checkpoint between them. Do not call
  `disableOwnerAnchors` while that is still true.

  This needs a node whose VM has the instruction (opcode `0xC7 0x4B`,
  tvmlabs/tvm-sdk `8b108bac`) and a `sold` built against that SDK; against an
  older node the light client throws on an unknown opcode. The light client's
  code hash moves `314ac6b8…6092f5` → `1078d5f1…64945a`, and `getVersion`
  reports 1.4.8 instead of 1.4.1, which is how an operator tells a client that
  takes the new payload from one that does not. On a live network
  the owner upgrades the deployed client in place with `updateCode`, which
  keeps its address, head, committee and proven set. **Do not call
  `setLightClientCode` after that upgrade:** it stores the code *and*
  re-derives `_lightClient` from it, and the bridge accepts anchor pushes only
  from that address, so the live client would start failing
  `ERR_INVALID_SENDER` (207) against an address where nothing is deployed. It
  belongs to a fresh `deployLightClient`, not to an in-place upgrade. `getCommitteeState` gains a
  `prevCommittee` return value and the ABI's `fields` gain `_prevCommittee`;
  no other function or event changes. `EthKeccak.sol` keeps only `rlpParentHash`, the RLP
  reader, which is now what the walk spends most of its gas on.

- **The relayer builds ancestry payloads in the new shape, and the
  `submit-ancestry` one-shot needs the older anchor.** `eth-lc-relayer
  submit-ancestry` takes a new required `--older-anchor-hash` next to
  `--checkpoint-hash` (the newer anchor), and `--max-headers` now defaults to
  33. Without this change every `submitAncestry` the old relayer sent would
  be refused: it built the payload newest-first and capped it at 32, so its
  last header is not a proven anchor and the call fails the two-end check with
  `ERR_UNKNOWN_CHECKPOINT` (249) — before `tvm.accept()`, so the message is
  rejected outright rather than reverting on chain and costing the client
  gas.

  What an operator sees from the daemon. Each checkpoint the chain confirms —
  whether this call put it on the head or an earlier one did and its
  confirmation was lost — queues the span back to the checkpoint it replaced; one poll works the span at the
  front of that queue, and the entry leaves only when it is settled — sent, or
  given up with "span given up: these blocks need setAcceptedBlockHash". A
  span the chain refuses is given up after three paid submissions, and so is
  one whose submission ends in an unknown outcome — a wait timeout or a
  dropped connection comes after the message was broadcast, so it may have
  been executed and charged, and the daemon counts it rather than resending
  for ever. One whose *headers* will not fetch retries for free and is given up
  only after 15 minutes of failures, because nothing of that reaches the
  chain, so an execution RPC that rate-limits costs no coverage.
  The queue holds at most 64 spans; past that the oldest is given up, since
  its anchors are the ones most likely outside the one-year window.

  The pair is fixed when the span is queued and each entry carries its own
  accounting, so a span is never re-paired with a later checkpoint and one
  span's refusal cannot spend another's budget. The state file gains
  `owed_spans`, which defaults to empty, so an existing state file is read as
  before.

- **A transaction can no longer queue more sink messages than its balance
  floor covers, which was losing honest ancestry spans.** Every hash the light
  client anchors or forgets is an internal message carrying `SINK_VALUE`
  (1 vmshell). One call could queue `MAX_EVICT_PER_TX` = 128 forgets plus up
  to 31 pushes: 159 vmshell against a `MIN_BALANCE` of 100. Messages are paid
  in the action phase in creation order, and `ensureBalance` was called after
  them, so the mint was too late to help — the first send that crossed the
  balance failed with action code 37 and the whole transaction aborted after a
  compute phase that had reported success, with the gas spent and nothing
  written. The relayer then resent the same valid payload until its budget ran
  out and gave the span up, so blocks that were covered became blocks needing
  `setAcceptedBlockHash`. No attacker needed: expired keys are contiguous in
  the FIFO, so the first time a year's worth comes due the forgets arrive in
  full batches.

  Two changes. Forgets per transaction are now capped separately at 32
  (`MAX_FORGETS_PER_TX`), so the worst case one call can queue is 32 + 33 = 65
  vmshell and fits under the floor with the gas of a full span inside it;
  examinations stay at 128, so compaction keeps its rate and only the messages
  are bounded. And the mint is now taken before anything is queued, sized for
  that worst case. An operator sees `AnchorsExpired` in batches of at most 32
  where it used to be up to 128, and more transactions to work through a large
  expiry cluster. Measured on a locally built node: a 33-header span with the
  client at exactly 100 vmshell now completes with action code 0 and all 31
  blocks anchored; with the old 128-forget bound the same call aborts in the
  action phase at that balance. The mint is not what makes this safe:
  `gosh.mintshellq` is clamped to the dapp's credit and quietly yields nothing
  when that is spent — on a self-rooted test deployment it yields nothing at
  all — which is why the bound, not the mint, is the fix.

- **A checkpoint skipped near the end of a sync-committee period can still be
  registered after the rotation.** `submitUpdate` keeps the commitment a
  `submitRotate` replaced (`_prevCommittee`) and accepts a proof signed by it
  for late registration only — never to move the head, which still needs the
  committee in force. The replaced committee may sign only around
  the head it left behind: the attested slot must be within one period either
  side of the finalized slot recorded when the rotation landed. The slack on
  the later side is what makes the feature work at all — that recorded slot is
  the last head the leaving committee moved, not the end of its period, so the
  checkpoint it skipped is attested *after* it and an upper bound at the
  recorded slot itself would exclude exactly the skips this exists for. The
  bound is a slot, read out of the signed header by the step circuit, not the
  rotate proof's period label. The owner hatches
  (`setCommitteeCommitment`, `reAnchorCommittee`) clear `_prevCommittee`
  rather than carrying it: they replace a committee the owner is rejecting, or
  jump several periods, so neither is a hand-off. `getCommitteeState` returns
  `prevCommittee` so an operator can see what is trusted. Before this, a checkpoint the daemon had not proven by
  the time `submitRotate` landed was unanchorable for good: its proof carries
  the old commitment and `ERR_WRONG_COMMITTEE` (242) refused it for ever, so
  the 32 blocks of that epoch could only be admitted by the owner's
  `setAcceptedBlockHash`, and not at all after `disableOwnerAnchors`. A period
  is about 27 hours and a step proof takes longer than an epoch, so a skip at
  the edge of a period was the likely case, not a corner one.

  `_prevCommittee` is **not** in the `updateCode` migration cell, and must not
  be: that cell is built by the code being replaced, so a live 1.4.1 client
  writes the old 15 fields and a decode expecting 16 would read 256 bits from
  where the period and the flags sit, corrupting the trust root after
  `tvm.resetStorage()`. The encode and decode are byte-identical to 1.4.1. An
  upgraded client therefore starts with no previous committee, which only
  means no catch-up across the rotation already behind it; the field fills on
  the next rotation. The same goes for `_prevCommitteeHeadSlot`, the second new
  field, a `uint64` next to it: also set to 0 on the upgrade, also filled by
  the next rotation, and the late path stays closed until both are non-zero.

  `getCommitteeState` gains a `prevCommittee` output and no other function or
  event changes. **Every caller of that getter has to take the new ABI**, not
  just positional ones: the compiled ABI is version 2.4 with no explicit
  function id, so the id is derived from the signature including its outputs,
  and an added `uint256` output changes it. A caller still holding the 1.4.1
  ABI sends an id this code does not dispatch and the getter simply fails. The
  relayer's embedded copy of the ABI is updated here. The shadow kit's
  `status.sh` and `deploy-contract.sh` read whatever `AN_LC_ABI_PATH` points
  at, so they need the new ABI file beside them and no change of their own.
  The
  ABI's `fields` section gains `_prevCommittee` and `_prevCommitteeHeadSlot`
  at positions 11 and 12, so account data decoded with the 1.4.1 ABI reads
  rubbish from `_committeePeriod` onward: anything decoding this client's state
  off-chain needs the new ABI.

  The window is slack around a proven slot, not a period boundary. When the
  head is near the end of its period at the rotation it reaches into the next
  one, and an early hand-off puts it wherever the sender put the head. Signing
  there needs a supermajority of the retired committee's keys — the long-range
  assumption this design already carries — but the contract does not
  enforce "the committee of the attested slot's period".

- **`submitRotate` no longer decides anything by the period on the proof, and
  refuses a hand-off that does not change the committee.** The rotate circuit
  loaded its period as a free witness (`rotate_aggregation`: `ctx.load_witness`),
  so the label was a number the sender typed. Monotonicity
  (`pi.period > _committeePeriod`) was therefore not a replay guard but a way
  to brick the call: anyone could take a genuine hand-off, label it
  `type(uint64).max` and submit it — the call is permissionless — after which
  every later rotation failed `ERR_STALE_PERIOD` (245) for good. With
  `disableOwnerRotation` already run, nothing but an owner `reAnchorCommittee`
  every period could carry the committee at all. No bound on the label fixes
  this, because any bound still lets a genuine hand-off be labelled at its own
  edge and shut the path there.

  What prevents replay is the committee chain, which was already there:
  `pi.currentCommitteeCommitment == _currentCommittee`, so once a rotation
  lands the same proof no longer matches, and the hand-off out of a given
  committee is unique. So the period is now out of the decision entirely: the
  monotonicity test is gone, and `ERR_STALE_PERIOD` (245) can only come from
  the owner's `reAnchorCommittee`. `_committeePeriod` and `CommitteeRotated`
  carry the rotate proof's period, which the circuit now binds to the attested
  slot (the rotate verification key entry above). It is a report for
  operators, and nothing gates on it.

  `submitRotate` also now requires `nextCommitteeCommitment != _currentCommittee`
  (`ERR_SELF_ROTATION`, 253). A genuine proof with
  `next == current` exists for every period boundary: the last header of a
  period is signed in the next one and its state still carries the signer as
  `next_sync_committee`. It would have passed, changed nothing visible, and
  overwritten `_prevCommittee` with `_currentCommittee` — silently stranding
  every checkpoint the replaced committee had not registered yet.

  **Still open, and not fixable here.** Nothing binds a committee to a period
  except that free label, so a genuine hand-off submitted early — the next
  committee is in the headers of the current period, so the proof exists from
  its first slot — installs a committee that has signed nothing yet. The head
  then cannot move until the chain reaches that committee's period (up to ~27
  h), and the replaced committee's unregistered checkpoints are stranded. This
  is permissionless, repeatable once per period, and true of the deployed
  1.4.1 as well. The fix is to bind the hand-off to a slot inside the rotate
  circuit, which is not in this change.

- **A rotate whose confirmation is lost no longer stops the daemon for
  good.** `call_contract` broadcasts the message and waits for it in one
  call, so a wait that times out can leave the rotation on the chain while
  the daemon records nothing. Every later poll then re-entered the period
  jump, re-proved the same rotation and was refused — the proof starts from a
  committee the chain has already replaced (`ERR_WRONG_COMMITTEE`, 242) —
  and the light-client head stopped advancing until an operator edited the
  state file. The state file gains `rotate_in_flight_period`, which defaults
  to unset, so an existing file is read as before: a rotate sent with an
  unknown outcome is written down there, and a 242 or `ERR_SELF_ROTATION`
  (253) refusal of that same period is read as "the earlier attempt landed",
  recorded, and the daemon goes back to submitting updates. Without an
  attempt of its own on record those codes are still a plain refusal — then
  the chain is on a committee this daemon cannot rotate from, which needs an
  operator.

- **The deposit-prover verification key is rotated again, for Amsterdam
  block headers.** Since the Glamsterdam fork (Sepolia 2026-10-06, epoch
  353024; Hoodi and mainnet not yet scheduled) execution headers carry 23
  fields, `blockAccessListHash` and `slotNumber` after `requestsHash`, and
  the previous key could not prove a deposit in such a block. The circuit
  now accepts headers of 16 to 24 fields up to 792 bytes, so one key covers
  mainnet before and after the fork, Sepolia, and the L2s the bridge reads
  (Arbitrum 16 fields, Blast 20, Base, OP, Mantle and World Chain 21), with
  one spare slot for the next appended field. A header padded with junk is
  still rejected. The deposit-prover encoder (`rlp_utils`) and
  `eth-lc-relayer` (`header_rlp`, used with `--submit-ancestry`) emit the
  two new fields. Rebuild both binaries from this source.

  Post-fork Sepolia blocks are anchored by this branch's light client:
  the step circuit picks Gloas generalized indices from `gloasForkSlot`
  (see the Breaking entry above). A deploy that has not set the slot
  still refuses every update with 256; a proof made for a different
  slot fails with 255.

  `eccUSDCBridge` embeds the new key, reports `version` 1.7.0, and
  `0.80.0_compiled/exchange/eccUSDCBridge.tvc` is rebuilt with sold 0.80.0;
  the ABI is unchanged. Operators: deploy that tvc and, at the same time,
  rebuild the `deposit-prover` examples (`cargo build --release
  --examples`) on every relayer host and anywhere `ackinacki-bridge
  deposit` proves. Proofs made with the previous key (bridge 1.6.0) stop
  verifying, and a stale `target/release/examples/<name>` produces proofs
  the new key rejects. The current minimum AN bridge version for
  `ackinacki-bridge deposit` is 1.12.0, as described above.

- **The deposit-prover verification key is rotated, and the rotation
  closes two forgeries.** Under the previous key — and every earlier one
  built from `main`'s circuit — the circuit did not tie the
  fields it reads out of the `Deposit` log to the log's bytes, and it
  did not require the log index to point at one of the receipt's logs:
  a slot past the receipt's last log (the circuit holds 20) carried
  whatever bytes the prover put there. Either way a prover holding any
  real receipt in an accepted block could choose the `depositId`,
  `sender`, `amount` (below 2^128), AN recipient and bridge address,
  and `finalizeDeposit` would mint against them. Nothing in the bridge
  bounded the damage. Deploy the new bridge as soon as possible.

  The enclosing transaction may be EIP-2930 (type 1) or EIP-1559 (type 2), with
  calldata up to 2048 bytes. Type 2 access-list payload is up to
  512 bytes (RLP header excluded); type 1's access list shares a
  field slot with type 2 calldata, so it may be up to 2048 bytes,
  but a type 1 transaction's calldata and access list together
  must fit its ~2805-byte leaf. Each receipt log's payload may be
  at most 2211 bytes (2048 of data with four topics; more data if
  there are fewer topics), at most 20 logs. That is what lets a SafeL2
  `SafeMultiSigTransaction` through, including a two-signer
  MultiSend of `approve` + `deposit` (~1152 B of log data). The
  keccak pin is 128 permutations (~17 KB of preimages, header and
  both MPT paths included): a receipt of up to about 13.5 KB fits,
  i.e. about six logs at 2048 bytes, fewer with a large type 1
  transaction. A typical Safe or 4337 receipt fits. Deposits in a
  transaction of any other type (legacy 0, blob 3, EIP-7702 4, an L2
  system type) are neither provable nor refundable: send again as
  type 1 or 2.

  Proofs made for the previous key stop verifying. That includes a
  `proof.bin` / `public_inputs.bin` that `ackinacki-bridge deposit`
  left in its work directory (`--resume` reuses it, and the bridge
  refuses it with 220, exit 33) and any `prove-one` bundle made
  before the upgrade. Relayers must
  finish or drain such deposits before the bridge is upgraded, or
  prove them again after.

  Those constraints first shipped as bridge 1.6.0. The tree now carries
  the 1.7.0 tvc of the Amsterdam entry above, which includes them.
  Operators deploy that tvc, not a 1.6.0 artefact. On every relayer host
  rebuild the `deposit-prover` examples (`cargo build --release
  --examples`) and do **not** pass `--max-data-byte-len 256` — the
  default is now 2048 and is part of the key. With a leftover
  `--max-data-byte-len 256` (or any `--degree`, `--max-log-num`
  other than 18 and 20) the proofs it makes are rejected by the
  embedded key, and a deposit that does not fit the smaller shape
  is refused with a retryable exit 1 naming the flags, never the
  final exit 3. The relayer prefers `target/release/examples/<name>`
  when the file exists and does not check it matches the source.
  A cached `data/*.pk` is not reused: its file name carries a
  fingerprint of the circuit, and the first proof after the upgrade
  generates the new ~3.4 GB key and then deletes the old one, so
  `data/` briefly needs room for both. `ackinacki-bridge deposit`
  installs get the prover tools from a release (`install.sh`), so
  they need a new release; the current CLI refuses an AN bridge older than
  1.12.0, as described above.

  The prover now refuses a deposit it cannot prove before loading
  the SRS or the key. `fetch_deposit_data` exits 3 when the
  enclosing transaction is any type other than 1 or 2 (legacy 0,
  blob 3, EIP-7702 4, an L2 system type such as OP-stack 0x7e), or
  its leaf, calldata or
  access list is over the circuit's cap — that refusal used to be
  exit 1, which the relayer retried forever. `export_blake2b_proof`
  and `export_vk_blob` check the log count, every log's topics and
  data, the receipt and header length, the MPT depths and the
  keccak budget, and also exit 3 — but only under the key's own
  shape (the default flags); under other flags they exit 1. `deposit` ends such an operation
  at exit 35 instead of a resumable 32. `deposit-relayer daemon`
  skips such a deposit at once, whatever `--skip-after-attempts`
  says, instead of retrying it forever: the tick is counted as
  `deposit_relayer_ticks_total{outcome="unprovable"}`, the id goes
  to a new `state.json` field `unprovable_deposit_ids` (gauge
  `deposit_relayer_unprovable_deposits`), and the log says only
  the EVM bridge operator can return its funds. `finalize-one`
  cannot help these. `parked_deposit_ids` and
  `deposit_relayer_parked_deposits` keep holding only deposits
  parked by `--skip-after-attempts`. An existing `state.json` loads
  unchanged. Deposits parked or given up on under the previous key
  (the 256-byte log shape) should be re-driven after this deploy:
  the new key can prove the Safe and 4337 receipts that the old
  one refused.

  The tvm-sdk opcode fixtures and the acki-nacki copy of
  `deposit-prover/fixtures/deposit_10proofs` carry the old blob
  and must be refreshed from this repository's fixtures.

- **`ackinacki-bridge withdraw` needs the `aggregate-proof` of its own
  release.** Stage 5 passes `aggregate-proof --slot-report`, which an older
  build refuses, after the burn; preflight now refuses an `aggregate-proof`
  whose `--help` does not name it, and one it cannot read to hash — with exit 2,
  or exit 10 on a run that resumes a recorded burn. Upgrade the two together: on
  an install made by `scripts/install.sh`, download the script again and run it
  — this one counts an `aggregate-proof` without `--slot-report` as missing and
  replaces it from the latest release, while an older copy calls it ok; from a
  checkout, rebuild `aggregate-proof` (`cargo build --release --bin
  aggregate-proof` in `crates/bridge-evm-aggregator`). Do not rebuild or replace
  it while a withdrawal is between preflight and its proof — the anchor wait at
  stage 4b can last up to 120 min, and the wait for the prove lock has no bound:
  stage 5 refuses with exit 12 an `aggregate-proof` that is not the binary
  preflight hashed, and the way on is `--allow-retry`, whose preflight checks
  the new one.
- **The compose kit's `preflight.sh` requires `BRIDGE_DEPLOY_BLOCK` in the
  runtime env.** Add it (the block `AckiNackiBridge` was deployed in, above 0
  and not above the head) to the kit's runtime env file before upgrading the
  image: without it every start dies with "BRIDGE_DEPLOY_BLOCK is missing"
  and the container restarts in a loop, sending nothing. When the RPC serves
  historical code, preflight also checks it against the chain: a bound after
  the deploy block is refused, the deploy block or an earlier bound passes;
  an RPC without historical state skips the check with its error in a
  warning. `runtime.env.example` and the kit README document it together
  with the two optional scan knobs.

- `applyBkSetUpdate` takes `attestationLastSeen` after `blockSeqNo`
  (selector `0x2a2c14a0` → `0xdcb4c795`) and adds
  `storedPrevBkSetCommitment` at slot 11. Redeploy the bridge first,
  then run the relayer only against that deployment — the new
  `storedPrevBkSetCommitment()` getter is not on the live contract, so
  every tick, `verify-fixture` and `verify-prover-proof` fail there.
  Circuit 1A/1B proves `block_seq_no > last_seen`; pass the word baked
  into the proof, not the live cursor after `verifyBlock(N)`. Old
  nine-argument calldata does not decode.

- `applyBkSetUpdate(N)` no longer waits for `verifyBlock` to cover N.
  It stores the outgoing set in `storedPrevBkSetCommitment` and
  `verifyBlock` accepts that set for `blockSeqNo <= N`. A second
  rotation is blocked only when applying it would cross an unverified
  bundle boundary — the contract computes
  `nextOwedBoundary = ((storedLastSeenBlockSeqNo / bundleStride) + 1) * bundleStride`
  and reverts `PrevSlotStillOwedForBundle(storedLastBkSetUpdateSeqNo, nextOwedBoundary)`
  when `nextOwedBoundary <= storedLastBkSetUpdateSeqNo`. Rotation bursts
  that land before the next unverified bundle boundary apply
  back-to-back, and so does the first rotation past it; a further
  rotation is refused until `verifyBlock` advances the layer cursor past
  that boundary. No permanent stall. `daemon-live` does not run into the
  revert: while a rotation it applied past the boundary is still waiting
  for its prover ack, the relayer does not pick up the next one, and
  applies it right after the next `verifyBlock`. Nothing is logged for
  the waiting rotation.

- **`AckiNackiBridge` constructor adds `bundleStride` to
  `VerifyBlockConfig`.** The field is the prover's bundle width in block
  seq-nos — L1 (W·P) = 1024, L2 (W²) = 16384 (with W=128, P=8) — and is
  immutable after construction; it feeds the `applyBkSetUpdate`
  boundary invariant above. When the verifier triple is wired a zero
  `bundleStride` is rejected (`InvalidBundleStride`); when
  `verifyBlock` is disabled the field is unused and may be zero.
  Switching anchor level (L1 ↔ L2) requires a redeploy. Deploy scripts
  read `BUNDLE_STRIDE` from the env; the shellnet-E2E deploy script
  (`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`)
  sets it from `LEVEL=1|2` automatically.

- **`aggregate-proof` self-checks the verifier source, so every verifiers
  directory now needs `<name>.sol` beside `<name>.bin`.** It used to compile
  the verifier it regenerates and compare bytecode, which is why `solc` had
  to be on every host that proves. It now compares the generated Solidity
  source with the committed `contracts/ethereum/verifiers/<name>.sol`. The
  source is fully determined by the aggregator key, so the check catches the
  same key drift; the refusal still reads `aggregator VK drift`, now names
  the first differing line, and says that a `snark-verifier` upgrade can
  cause it too — in that case regenerate both files of each pair.
  What has to carry the four `*AggregatorVerifier.sol` files:
    - a source checkout already does;
    - the release bundle and the relayer image now ship them — upgrade
      `aggregate-proof`, the relayer image and a `scripts/install.sh` install
      together, not one at a time;
    - a self-deployed verifiers directory (`--verifiers-dir` /
      `BRIDGE_VERIFIERS_DIR` with `--allow-verifier-drift`) needs the `.sol`
      that `export-inner-aggregator` wrote next to its `.bin`, and
      `BridgeWithdrawalAggregatorVerifier_calldata.bin` (word 23 is the
      withdrawal adapter's `vkDigest` pin). `scripts/install.sh` treats
      that `_calldata.bin` as part of a complete verifiers directory.
  `BridgeWithdrawalAggregatorVerifier.sol` is regenerated from this release's
  rotated Circuit-4 key and compiles to the committed `.bin`. Because a
  compiled `.bin` ends with `solc`'s CBOR metadata, whose hash commits to the
  source's keccak256, compiling to the identical `.bin` proves a `.sol` is
  exactly the source of that `.bin` — which also holds for
  `LayerHashesAggregatorVerifier.sol`, regenerated here at `k_outer = 22`,
  and for `PrimaryAggregatorVerifier.sol` and
  `FallbackAggregatorVerifier.sol`, regenerated in `6360455` with the
  other two. All four sources match the `.bin` files in this tree.
- **`aggregate-proof --allow-bin-drift` is now `--allow-source-drift`.** Same
  meaning — a bootstrap escape hatch for a verifier whose source is not
  committed yet — with no alias for the old spelling.
- **The Circuit 4 (withdrawal) verification key is rotated.** The inner
  Poseidon preimage now includes `events_pos`, and the public-input vector
  grows from 10 to 11 with `anchorLayer` (1-indexed, range-checked
  `1..=10`). `withdrawByProof` scans only that layer's window.
  Behavioural consequence of the `events_pos` binding: two structurally
  identical `WithdrawalInitiated` events emitted in the same source
  block (matching on all four of `tokenId`, `amount`, `recipient` and
  `sender`) now nullify to distinct values and can both pay out on the
  ETH side; pre-rotation they would have collided on the second
  withdraw as a replay.
  The aggregated Yul is 21 314 B / 24 instances; the reference
  `_calldata.bin` is 3 680 B. Redeploy
  `BridgeWithdrawalAggregatorVerifier`; proofs against the old key do not
  verify, and a `WithdrawalPublicInputs` struct without `anchorLayer` will
  not decode. The extra tuple field also **changes the `withdrawByProof`
  4-byte selector**: from the previous ten-slot `0x6e6f66ad`
  (`withdrawByProof(bytes,(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256))`)
  to the eleven-slot `0xa9753d18`
  (`withdrawByProof(bytes,(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256))`,
  verified with `cast sig`). Integrators that hand-craft calldata (`cast call`
  scripts, custom relayers, front-ends) have to update the tuple
  signature; a client still emitting the old selector will see the
  call revert with empty returndata (no fallback and no router
  string), not a decode error deeper in. `EVENT_CIRCUIT_REVISION` goes from 2 to 3, so every
  prover host regenerates its Circuit 4 keys on first use — the bump
  is what makes the key cache reject a pre-rotation
  `event_pk.bin` / `event_vk.bin`. Keygen (over the `K = 20` SRS) runs
  during the next Circuit-4 proof job on that host, whichever fires
  first among the four entrypoints that go through the shared prover
  library: `ackinacki-bridge withdraw`, `relayer prove-withdraw`,
  `relayer prove-withdraw-shplonk`, and `relayer withdraw-e2e`. In all
  four the shared library emits an info-level line at
  `bridge-prover-lib/src/keys/event.rs:100` when keygen actually
  starts, whose runtime visibility depends on the entrypoint's
  tracing filter: `relayer prove-withdraw-shplonk` and
  `relayer withdraw-e2e` default to `info` and print it;
  `ackinacki-bridge withdraw` defaults to
  `warn,ackinacki_bridge=info,bridge_relayer_daemon=info`
  (`crates/ackinacki-bridge/src/main.rs:247`) and so silences the line
  unless `RUST_LOG` is set; `relayer prove-withdraw` runs the prover
  as a subprocess (`SubprocessWithdrawalProver`) and buffers its
  stderr, surfacing it only on failure
  (`crates/bridge-relayer-daemon/src/withdraw_prover.rs:231-249`).
  Only `ackinacki-bridge withdraw` also prints the preflight forecast
  `"Circuit-4 keys will be generated on this run"` (from
  `crates/ackinacki-bridge/src/preflight.rs:1595`) at its stage-1
  preflight — that's a prediction, not the keygen start, and the
  numbered stages are a CLI concept only. The rotation has nothing to
  do with `verifyBlock` on the
  ETH side, which reads only the aggregator VK.
  `bridge-verifier-daemon` does not run keygen itself: on startup it
  looks for the event VK and, if absent, exits with
  `"event VK not found in <params-dir>. Run the event prover
  (Circuit 4) first to generate keys."` The prover-side warning to
  watch for on a stale cache is `"cached event keys do not match
  their manifest, or the manifest could not be read (…); ignoring
  them. Re-run the prover to regenerate."` — the signal the revision
  bump landed and the cache is about to be regenerated.

  To skip the ~2.65 GB write and the associated wall time, preseed
  the params directory with the complete four-file Circuit-4 cache,
  not just the keys: `event_pk.bin`, `event_vk.bin`,
  `event_config_params.json` **and** `event_manifest.json`. The
  params directory is `--params-dir` for `prove-withdraw-shplonk` /
  `withdraw-e2e` (and for the CLI's `ackinacki-bridge withdraw`);
  `relayer prove-withdraw` has no `--params-dir` flag and looks under
  `<work-dir>/params` instead. The revision is a field
  (`circuit_revision`) inside the manifest, and `install_cached_keys`
  will only accept a preseed if the manifest is present, parseable,
  in the expected `manifest_format`, carries the current
  `circuit_revision`, and its SHA-256 digests over `event_vk.bin` and
  `event_config_params.json` match the on-disk bytes. Copying only
  `pk` + `vk` leaves `event_config_params.json` missing, so
  `install_cached_keys` bails at `load_config` and keygen runs
  regardless — no WARN in that case. The warning above fires whenever
  the config loaded but the manifest failed any of the checks above,
  including a stale `circuit_revision` — which is exactly the
  cache-invalidation path described a paragraph earlier. The PK is
  deliberately not hashed by `install_cached_keys`, so a corrupted
  `event_pk.bin` passes preseed silently and only fails later when it
  is actually used. All four files have to come from the same
  successful keygen run on some other host.

- **All four SHPLONK aggregator adapters bind on-chain to the inner-circuit
  VK.** Upgrade `aggregate-proof`, the relayer, `ackinacki-bridge` (a
  `scripts/install.sh` install) and `contracts/ethereum/verifiers/`
  together with the contracts. The CLI reads
  `BridgeWithdrawalAggregatorVerifier_calldata.bin` from `--verifiers-dir`
  as well as the `.bin` and `.sol`. Calldata from an old `aggregate-proof` has
  no digest slot, and every new adapter rejects it
  (`AttestationProofRejected` / `LayerHashesProofRejected` /
  `WithdrawalProofRejected`). `AckiNackiBridge` has to be redeployed:
  `primaryVerifier`, `fallbackVerifier`, `layerHashesVerifier` and
  `bridgeWithdrawalVerifier` are `immutable`. `DeployReuseVerifiersBridge`
  and `DeployGenesisCursorBridge` take adapter addresses from
  `PRIMARY_VERIFIER` / `FALLBACK_VERIFIER` / `LAYER_HASHES_VERIFIER` /
  `WITHDRAWAL_VERIFIER` — do not pass pre-rotation adapters there.

- `withdrawByProof` reverts `LayerOutOfRange` when `anchorLayer` is 0 or
  greater than 10, the same error `getLayerWindow` already uses.
  `InvalidNumLayers` stays on `verifyBlock` and on the layer-window
  views (`layerWindowLen`, `layerWindowWriteCursor`,
  `anchorRemainingAppends`). A caller that caught `InvalidNumLayers` on
  a bad withdrawal layer will need to catch `LayerOutOfRange` instead.

  Each adapter constructor is `(address _shplonkVerifier, bytes32 _vkDigest)`,
  rejects a zero digest with `InvalidVkDigest()`, rejects a pin at or above
  the BN254 scalar-field modulus `r` with `VkDigestExceedsFieldModulus()`
  (guards an operator who passes a raw 32-byte hash in place of a real Fr
  digest; a chain id is far below `r` and is not what this guard catches —
  it rejects about 81% of random 32-byte values), and exposes `vkDigest()`.
  `verifyPrimaryAttestation` and `verifyFallbackAttestation` compare word
  16, `verifyLayerHashesMovement` word 26, `verifyWithdrawal` word 23,
  then delegate to Yul. A pin is word `12 + N` of the matching
  `<name>_calldata.bin` (big-endian `bytes32`); after deploy check with
  `cast call <adapter> "vkDigest()(bytes32)"`. The library helper
  `bridge_evm_aggregator::vk_binding::expected_vk_digest` has no CLI.

  Against 0.2.0 the artefacts are: Primary 21 494 → 21 655 B / calldata
  3 840 → 3 872 B; Fallback 21 493 → 21 655 B / 3 840 → 3 872 B;
  LayerHashes 19 100 → 19 263 B / 3 072 → 3 104 B (`k_outer` stays 22);
  Withdrawal 20 990 → 21 314 B / 3 616 → 3 680 B. Primary and Fallback are
  the tightest at 88% of EIP-170. The aggregator cache stem moved
  `__v2__` → `__v3__`: the first run re-keygens every outer PK; old slots
  stay on disk until deleted. `numLayers` was already 5 at 0.2.0.

- **The layer-hashes verification key is rotated. Redeploy that verifier.**
  Against 0.2.0 `LayerHashesAggregatorVerifier` stays at `k_outer = 22`.
  The runtime artefact grows from 19 100 B to 19 263 B and its
  `_calldata.bin` from 3 072 B to 3 104 B (the extra digest word).
  Address and `extcodehash` change with the pin in `ShplonkDeployLib`.
  Proofs against the old key do not verify; a deployment that updates
  only the bridge will fail every `verifyBlock`. Margin to EIP-170
  (24 576 B) is 5 313 B. Primary and Fallback at 21 655 B (88%) are
  the tightest of the four.

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

### Documentation

- `contracts/an/README.md` gains *Upgrading the bridge*: why the voucher's code
  is part of replay protection, and that an upgrade changing it has to ship a
  block cutoff in the same code so past deposits stop going through. The
  current contracts need no such restriction — the voucher's code is installed
  by the zerostate and `updateCode` is called with an empty `userCell`.
- `disableOwnerAnchors` is documented as L1-only in `contracts/an/README.md`,
  `docs/eth-light-client.md` §3.5, and
  `scripts/ursus/flip_deposit_to_light_client.md`. After the flip, L2s keep
  the owner writer; L1 still covers only the blocks a span reaches when
  accepted checkpoints sit more than a span apart, because nothing
  late-registers the ones the daemon skipped. Check 3 of the deposit
  CLI names both `disableOwnerAnchors` and `disableOwnerAnchorsOn` as
  the switch-off.

### Added

- `deposit-relayer daemon --outstanding-backoff-secs` /
  `OUTSTANDING_BACKOFF_SECS` (default 2): base delay before a failed deposit
  is retried, doubled on each later failure of that id up to 64×. Gauge
  `deposit_relayer_outstanding_deposits`. `state.json` gains
  `outstanding_deposits` (per-id attempts, `retry_after_unix`, and the
  Deposit tx / block / log index when known) and `last_finalized_deposit_id`
  (seeds `deposit_relayer_last_finalized_deposit_id` across restarts).

- `AckiNackiBridge.writeOffUnbackedPrincipal()` (owner): after an AAVE loss,
  cuts `suppliedPrincipal` down to `aUsdcBalance()` and emits
  `UnbackedPrincipalWrittenOff(amount)`; moves no tokens. Reverts
  `NothingToWriteOff` when the book is already covered. Use it once the loss
  is accepted, so `accruedYield()` reflects interest again (ETH-28).
  Available only on a bridge deployed from this release.

- `harvestYield(amount)` pays at most the surplus of liquid USDC + aUSDC
  over `treasuryBalance`, so `harvestYield(accruedYield())` is safe after
  AAVE rounding and after a write-off. It reverts
  `HarvestWouldBreakSolvency(amount, backing, treasuryBalance)` only when
  that surplus is zero. After a loss, interest goes to users first.

- **`deposit-relayer daemon` no longer proves deposits that are already
  finalized on Acki Nacki.** Before proving, it computes the address of the
  deposit's `DepositVoucher` and checks whether that account is active. The
  address comes from the deposit id, source contract and source chain plus
  the voucher code stored in the bridge. If the voucher is active, the daemon
  advances without proving, which saves a full proof (minutes) when an
  operator replays a deposit range or another relayer got there first.
  Before this the check always answered "not finalized".
  - The voucher code is read once per process from the bridge's account
    data, using the ABI in the AN config's `bridge_abi_path`. That ABI must
    list the bridge's storage `fields`, including `_depositVoucherCode`; the
    one compiled under `contracts/an/0.80.0_compiled/` does.
  - If the check cannot run (unreachable endpoint, ABI without the field),
    the daemon logs `voucher pre-check failed; proving the deposit anyway`
    and behaves as before.
  - The check now runs after the deposit event is fetched, not before, since
    it needs the event's source contract and chain (bridge-audit#26).

- AAVE pulls are exact and bounded by the aToken balance (ETH-28):
  - `withdrawByProof` tops up the missing part of a payout from all of
    `aUsdcBalance()`, interest included, instead of stopping at
    `suppliedPrincipal`. When liquid USDC + aUSDC cannot cover the payout it
    reverts `InsufficientTreasury`; a bridge without an AAVE position used to
    fail later, inside the USDC transfer.
  - `withdrawFromAave(amount)` accepts at most
    `min(suppliedPrincipal, aUsdcBalance)`; `type(uint256).max` pulls exactly
    that. A larger explicit amount reverts `InvalidAmount` rather than being
    trimmed to a partial pull, so `WithdrawnFromAave` always reports a pull
    that was paid in full.
  - `supplyToAave` reverts `AaveSupplyFailed(requested, 0)` when AAVE mints
    no aUSDC (was `AaveWithdrawFailed`).
  - `harvestYield` sends exactly `amount` to `yieldRecipient`; anything a
    pool pays above it stays on the bridge.

- `deposit-relayer daemon` accepts `AN_DAPP_ID=0` / `--dapp-id 0` without
  `--dry-run`; it used to refuse to start. Zero is the right value:
  `eccUSDCBridge` pins every deposit's dappId to 0 and ignores the proof's
  dappId inputs. `scripts/ursus/deposit-relayer.env.example` now ships
  `AN_DAPP_ID=0`. The value has no effect on-chain, but it is part of the
  `state.json` deployment binding (`chain_id`, EVM bridge, `AN_DAPP_ID`).
  Changing it on an existing file fails start-up with a deployment
  mismatch; `--force-state` then discards the cursor, parked ids,
  outstanding ids, `last_finalized_deposit_id` and unprovable ids even
  though no bridge changed (QC-OFF-09).

- `deposit-relayer daemon` no longer re-scans Ethereum logs while it retries a
  deposit that AN keeps refusing. Once a deposit is found, retries reuse it
  with no `eth_getLogs` or `depositCounter()` call, and `scan_done_through_block`
  in `state.json` moves to the block before the deposit right away instead of
  waiting for a finalize, so a restarted daemon finds it again in a single
  10-block chunk. `deposit_relayer_eth_scan_from_block` reflects that. Nothing
  to configure (QC-OFF-10).

- `deposit-relayer` reads `--state` from `DEPOSIT_RELAYER_STATE`.
  `scripts/ursus/deposit-relayer.env.example` has always set that variable,
  but nothing read it, so a daemon started by the unit kept `state.json` in
  its working directory (`/home/ubuntu/bridge-e2e/deposit-relayer-state.json`
  on the Ursus host). **Before upgrading**, if your env file sets
  `DEPOSIT_RELAYER_STATE`, move the existing file there. Otherwise the daemon
  starts over at `--start-deposit-id` and forgets its parked ids
  (QC-OFF-08).

- A `finalizeDeposit` the daemon reports as pending no longer counts toward
  `--skip-after-attempts`. This covers a confirmation that outlasted the
  60-second timeout, and a paused lane (`ERR_PAUSED`, 231). Every pending tick
  used to raise `attempts_since_progress`, so after a long pause the first real
  failure parked a healthy deposit. Pending ticks are now logged at `info`
  (QC-OFF-11).

- `deposit-relayer daemon --force-state` now starts `state.json` over when the
  file was stamped for another deployment (chain id, EVM bridge address or
  `AN_DAPP_ID`). It used to re-stamp the binding and keep the old
  `last_processed_deposit_id`, `scan_done_through_block` and
  `parked_deposit_ids` and `unprovable_deposit_ids`. After a bridge redeploy,
  that silently skipped the new bridge's deposits up to the old cursor. The
  daemon now logs every discarded field (record parked and unprovable ids
  before you run it) and begins at `--start-deposit-id`. If the AN bridge did
  not change, the replay is cheap: deposits that are already finalized are
  recognised by their voucher and are not proven again (QC-OFF-08).

- **`deposit-relayer daemon` judges `finalizeDeposit` by the voucher deploy
  and `confirmDeposit`, not by the bridge transaction's exit code.** The
  bridge exits 0 as soon as the proof verifies; the mint happens later. A
  failed voucher or confirm is Rejected; a duplicate (voucher exit 51) is
  AlreadyFinalized; hops not yet visible — including a GraphQL
  `transaction: null` that parses as no children — are Pending and are
  polled until `--confirm-timeout`. The AN GraphQL endpoint must serve
  `blockchain { message(hash) }`, which shellnet does (QC-OFF-06 /
  bridge-audit#27).

- Gloas (ePBS) support in the beacon light client. `eth-lc-relayer` reads
  `GLOAS_FORK_EPOCH` from the beacon node's `/eth/v1/config/spec` and hands
  the fork slot to the step and rotate provers as `BEACON_GLOAS_FORK_SLOT`;
  run by hand, the provers default to the network's own value (Sepolia
  `11296768`, mainnet not scheduled) and take the variable as an override. It
  must equal the contract's slot. From Gloas the beacon block carries a
  payload bid instead of the payload, so the hash the light client stores for
  a finalized block is the bid's `parent_block_hash`, the execution block that
  block built on, read from `finalized_header.execution_block_hash`; before
  Gloas it is still the payload's own `block_hash`.
- `EthBeaconLightClient.setGloasForkSlot(uint64)` (owner) and
  `getGloasForkSlot() → (uint64 gloasForkSlot, bool isSet)`; errors 255 (proof made for
  a different Gloas fork slot) and 256 (fork slot not set). `eth-lc-relayer
  set-gloas-fork-slot --slot …` is the operator call; a fresh deploy rejects
  every submission with 256 until it runs.
- **`daemon-live` checks the bridge's `bundleStride` at startup.** It reads
  `bundleStride()` and refuses to start when it differs from the stride of
  its anchor level: `bridge … has bundleStride=…, but this daemon runs at
  L… with stride … (BRIDGE_ANCHOR_LEVEL / --anchor-level)`. A bridge
  deployed with a different `BUNDLE_STRIDE` reverts rotations the daemon
  expects to land, and only a redeploy clears that. A bridge deployed
  before the field existed is refused too (`bridge …: the bridge cannot
  report bundleStride() (…)`): it still applies the old rotation gate
  (`VerifyBlockLagBehindRotation`), which the daemon no longer follows, so
  its rotations would revert until the daemon aborted.
- **`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`
  takes `BRIDGE_SEED_SEQNO`.** When set, the bridge is seeded at that key
  block (`compute_bridge_anchors --seed-seqno`; it must be a multiple of
  the level's stride and at or below chain head) instead of the newest
  boundary at deploy time (`--at-head`). Use it with a BK-set file built
  for the same seed.
- `ackinacki-bridge withdraw` checks in stage 1, dry runs included, that
  neither bridge is paused: `AckiNackiBridge.paused()` and the Acki Nacki
  bridge's `isPaused()`. Either one refuses with exit 2 before the burn.
  Before, a paused Acki Nacki bridge bounced the burn and the run waited out
  the capture (exit 11); a paused EVM bridge let the burn through and the run
  failed in stage 4b or 6. A resume over a recorded burn checks only the EVM
  side and refuses with exit 10, naming `--allow-retry`.
- **`ackinacki-bridge deposit`: USDC from an EVM wallet to an Acki Nacki
  account.** The wallet signs `approve` and `deposit` from a QR code —
  WalletConnect v2 (`--qr-mode walletconnect`, the default) or EIP-681
  (`--qr-mode eip681`, or `both`) — and keeps its keys: the CLI takes no EVM
  private key and no Acki Nacki key. It then waits for the deposit block's
  anchor on Acki Nacki, proves the deposit on the same machine, sends
  `finalizeDeposit` and confirms the credit by the deposit's identity
  `(chainId, EVM bridge, depositId)`, not by the recipient's balance.

  ```
  ackinacki-bridge deposit --network sepolia --amount <usdc> --to <dapp_id>::<account_id>
  ```

  - **Network:** `sepolia` only. Ethereum mainnet is not a deposit network;
    `bridge_config.mainnet` cannot be used for `deposit`.
  - **Profile keys**, next to the shared `RPC_URL`, `BRIDGE_ADDRESS`,
    `BRIDGE_GQL_ENDPOINT`, `USDC_BRIDGE_ACCOUNT_ID` and `BRIDGE_WORK_DIR`:
    `BRIDGE_DEPOSIT_PROVER_DIR` (`--deposit-prover-dir`),
    `BRIDGE_DEPOSIT_STATE_DIR` (`--state-dir`, default
    `$HOME/.bridge-deposit-state`), `BRIDGE_DEPOSIT_CONFIRMATIONS`
    (`--confirmations`, 12) and `BRIDGE_WC_PROJECT_ID` (`--wc-project-id`;
    a release build compiles one in, a build from source has none).
    `bridge_config.shellnet` and `bridge_config.local` set the first three
    to `./deposit-prover`, `./deposit-state` and 12. With `HOME` unset or
    empty, `--state-dir` and `--work-dir` have no default: every `deposit`
    run needs `--state-dir`, and a new deposit or a dry run `--work-dir`
    too, or it is refused with exit 2.
  - **Other flags:** `--pair-timeout-s` (300), `--recovery-window-s` (600),
    `--anchor-timeout-s` (0, no limit), `--relayer-grace-s` (120),
    `--prover-timeout-s` (1800), `--credit-timeout-s` (300), `--qr-out`,
    `--uri-only`, `--qr-invert`, `--wc-relay-url` (default
    `wss://relay.walletconnect.org`), `--from-address` (required by
    `eip681` and `both`) and `--dry-run`. A time limit takes at most ten
    years, `--pair-timeout-s` at most 30 days; a larger value is exit 2.
    `--help` does not echo the values of `--rpc-url`, `--gql-endpoint` and
    `--wc-project-id`, and a deposit run prints configured URLs without their
    path, query or credentials.
  - **Operations.** Each deposit gets an operation id and a record,
    `<state-dir>/<op-id>.json`. `--resume <op-id>` (or `--resume
    <depositId>` once the deposit is confirmed) continues it; `--tx-hash`
    binds a transaction when the search for it is ambiguous; `--abandon
    <op-id>` releases an operation whose EVM outcome is unknown after you
    have checked the wallet. While an outcome is unknown, a new deposit with
    the same network, bridge, recipient and amount, or from the same sender,
    is refused with exit 3. Keep one `--state-dir` per sender: operations in
    another directory are invisible to these checks.
  - **Exit codes** `20` wallet (not paired, rejected, timed out, or a
    smart-contract account), `21` `approve`, `22` the deposit reverted (USDC
    not taken), `30` the deposit transaction not found yet, `31` the Acki
    Nacki side timed out, `32` the proof, `33` `finalizeDeposit` refused,
    `34` the credit not confirmed in time, `35` the deposit is on chain but
    cannot be proven or is not the one requested, `37` the bridge
    transaction that should have minted the credit aborted. 30–34 are
    resumable; for 35 and 37 only the operator can finalize, return or pay
    out the USDC, and the CLI prints what to give them. `0` is success, `2`
    a refusal before the deposit was requested (an `approve` may have been
    sent by then), `3` a run blocked by another run or
    by an operation whose outcome is unknown. `withdraw`'s `10`–`13` are
    unchanged.
  - **Requirements.** `--state-dir` and the prover directory must be on a
    filesystem with `flock`: unlike `withdraw`, `deposit` refuses without it
    (exit 2). The prover needs ~10 GB of RAM and ~3.4 GB of disk, the
    proving key it writes on its first proof; a proof takes one to two
    minutes on a 20-thread host. The RPC must serve every receipt and raw
    transaction of a block, and the `--gql-endpoint` host must serve
    GraphQL and `POST /v2/messages`.
  - **Refusals before any transaction:** Safe and other smart-contract
    accounts, and ERC-4337 accounts (their deposits cannot be proven; with
    `--qr-mode eip681` only those that already have code); a
    paused EVM bridge (`paused()`) or Acki Nacki bridge (`isPaused()`); an
    Acki Nacki bridge that does not trust the EVM bridge; a voucher code the
    CLI does not know; prover tools that do not run on this host (each is
    started once with `--help` and must exit 0 within 10 s, so a binary
    built against a newer glibc is refused here); a `--work-dir` that cannot
    be created or written. A `--resume` whose proof is still to be built
    checks the last two as well, with the exit code of its stage. An
    EIP-7702 account passes with a warning to turn off gas sponsoring and
    batched calls. With owner anchors disabled a deposit
    is allowed only if the light client demonstrably anchors blocks a
    deposit can use; ancestry does not run on Acki Nacki today, so such a
    bridge takes no deposits from the CLI.
  - `--qr-mode eip681` cannot ask the wallet for a type-2 transaction, and
    a legacy one makes the deposit unprovable (exit 35). The CLI asks you to
    accept that risk; `--yes` accepts it, and Ctrl-C at the question ends the
    run.
- **`deposit` requires an Acki Nacki bridge at or above
  `MIN_BRIDGE_VERSION`.** The CLI sends `finalizeDeposit` again when an
  earlier send's outcome is unknown, which is safe only on a bridge that
  cannot mint one deposit twice. Preflight reads `getVersion()` and refuses
  an older bridge (exit 2; on `--resume`, the exit code of the operation's
  stage). The constant lives in
  `crates/ackinacki-bridge/src/deposit/an_preflight.rs` and stays unset
  until such a bridge version exists; a build without it refuses every
  bridge, and the release pipeline refuses to build a tag. The cargo feature
  `dev-unfixed-bridge` turns the refusal into a warning for development
  against an older bridge; a release never enables it.
- **Release assets for deposits.** `ackinacki-bridge-linux-x86_64.tar.gz`
  now carries `deposit-prover/`: `fetch_deposit_data`,
  `export_blake2b_proof` and `export_vk_blob`, built unchanged from
  `deposit-prover`'s examples on the same commit, `configs/circuit_params.json`
  and an empty `data/`. A new asset, `kzg_params_18.srs` (33,554,692
  bytes), is the Hermez ceremony at degree 18. It is not downloaded — the
  storage `deposit-prover/download_trusted_setup.sh` points at refuses
  access — but derived in the release pipeline with `deposit-prover`'s
  `downsize_srs` from the `kzg_bn254_21.srs` carried from release to
  release. From `v0.2.0`'s `kzg_bn254_21.srs` (sha256
  `871ae7e7403d2c0eb37263cd05e44220ef9b5fb86884e91021b9106eb22f761c`) it
  comes out as sha256
  `ca97cea5566cec45421f7b2a945c462da6cb759fd8d22791b41b4a3d479ffefc`.
  `SHA256SUMS` lists it, so a release has four assets.
  `crates/ackinacki-bridge/scripts/stage_deposit_prover.sh <dir>
  [--no-build]` lays out the same prover directory from a checkout, and
  `scripts/check_deposit_prover_bundle.sh <prover dir>
  deposit-prover/fixtures/deposit_10proofs` checks one.
- **`scripts/install.sh` installs the deposit prover** into
  `<prefix>/deposit-prover/` and `kzg_params_18.srs` into its `data/`,
  checks the SRS against `SHA256SUMS` and the Hermez [s]·G2, and runs both
  prover tools once so that a C library too old for them shows up at
  install time. The profile it writes gets `BRIDGE_DEPOSIT_PROVER_DIR`,
  `BRIDGE_DEPOSIT_STATE_DIR` (`<prefix>/deposit-state`) and
  `BRIDGE_DEPOSIT_CONFIRMATIONS`. Over an installation from before
  deposits, it downloads the bundle again and appends those three keys to
  the existing profile, changing nothing else in it; `--check` reports such
  a profile as missing them. It now wants ~13 GB free instead of ~6 GB
  (withdrawal keys ~9.5 GB and the deposit proving key ~3.4 GB).

- **`deposit` draws its QR codes as images on terminals that can show them:**
  kitty graphics (kitty, Ghostty, Konsole from 22.04), iTerm2 inline images
  (iTerm2, WezTerm) and sixel. Before the first code the CLI asks the
  terminal what it shows, for a fraction of a second; keys pressed meanwhile
  are lost, and a run in the background does not ask. Other terminals, and
  tmux, screen, zellij and the terminals of nvim and emacs, keep the text
  code. `--qr-display auto|text|kitty|iterm2|sixel` (`BRIDGE_QR_DISPLAY`,
  default `auto`) forces a rendering when detection guesses wrong, and every
  image is followed by the hint to rerun with `--qr-display text`.
  `--qr-invert` now changes only the text code; `--uri-only`, `--qr-out` and
  `--json` are unchanged.

- `relayer daemon-live` and `ackinacki-bridge withdraw` take the
  `LayerAnchorAppended` scan settings as arguments: `--bridge-deploy-block`
  (`BRIDGE_DEPLOY_BLOCK`), `--get-logs-chunk-blocks`
  (`BRIDGE_GET_LOGS_CHUNK_BLOCKS`; 0 = the default) and `--get-logs-pause-ms`
  (`BRIDGE_GET_LOGS_PAUSE_MS`), each also read from the environment
  variable of that name.
  `crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh` writes
  `BRIDGE_DEPLOY_BLOCK` next to the `BRIDGE_ADDRESS` it deploys (a commented
  placeholder when the receipt cannot be read; never fatal after the
  broadcast).
- **`deposit-relayer daemon` exports Prometheus metrics.** `--metrics-addr`
  (`DEPOSIT_RELAYER_METRICS_ADDR`, e.g. `127.0.0.1:9467`) serves the text
  format at `GET /metrics`, the same facade and histogram buckets as
  `relayer daemon-live --metrics-addr` on the AN→ETH side, so one scrape
  config fits both. Unset means no exporter, and the library's `metrics::*`
  calls are no-ops without one. Names are operator API, listed with their
  meaning in `deposit_relayer_daemon::metrics`:
  `deposit_relayer_ticks_total{outcome}` (finalized, already_finalized,
  not_yet_available, proof_failed, an_rejected, an_pending, skipped,
  unprovable, error),
  `deposit_relayer_stage_duration_seconds{stage}` (is_finalized, fetch_event,
  prove, submit, tick), `deposit_relayer_prover_stage_duration_seconds{example}`
  and `deposit_relayer_prover_stage_failures_total{example}` per
  `deposit-prover` subprocess, `deposit_relayer_eth_get_logs_total{outcome}`
  (ok, retry, error) and `deposit_relayer_eth_scanned_blocks_total` for the
  `eth_getLogs` cost, gauges `deposit_relayer_eth_safe_head_block`,
  `deposit_relayer_eth_scan_from_block`, `deposit_relayer_scan_done_through_block`,
  `deposit_relayer_eth_deposit_counter` (`depositCounter()` on Ethereum,
  polled once a minute; minus `last_finalized + 1` is the backlog),
  `deposit_relayer_target_deposit_id`, `deposit_relayer_last_finalized_deposit_id`,
  `deposit_relayer_last_finalized_timestamp_seconds`,
  `deposit_relayer_last_tick_timestamp_seconds`,
  `deposit_relayer_attempts_since_progress`, `deposit_relayer_parked_deposits`,
  `deposit_relayer_unprovable_deposits`,
  `deposit_relayer_backoff_seconds`, `deposit_relayer_build_info{version}` and
  `deposit_relayer_start_timestamp_seconds`. The in-process `RelayerMetrics`
  counters and the shutdown snapshot are unchanged.

- **`AckiNackiBridge` has `pause()` / `unpause()` again** (owner-only). While
  paused, `deposit`, `verifyBlock`, `applyBkSetUpdate` and `withdrawByProof`
  revert `BridgePaused`. AAVE management stays available so the owner can
  evacuate funds. Restored after it was dropped in #20; the AN-side
  `eccUSDCBridge.setPaused` is a separate control.
- **`eccUSDCBridge` can be stopped and restarted by its owner: `setPaused(bool)`,
  read back with `isPaused()`.** While it is paused, the two cross-chain entry
  points refuse with exit code **231** (`ERR_PAUSED`) before doing any work:
  `finalizeDeposit` (an inbound L1 deposit, rejected before the proof is even
  parsed, so a paused bridge spends no gas on verification) and
  `initiateWithdrawal` (an outbound burn, rejected before `tvm.accept()`, so
  the attached ECC bounces back to the sender untouched).

  Deliberately narrow — everything else stays available while the bridge is
  stopped: `confirmDeposit`, so a deposit already proven and vouched is still
  paid out instead of hanging half-finished; the TIP-3 inbound callback
  `onTransferReceived` and the owner mints; and every anchor, allowlist and
  light-client call, so the owner can keep the configuration current during the
  stop. Pausing is reversible and, like the allowlist and the anchor set, is
  **not** carried through `onCodeUpgrade`: an upgraded bridge starts unpaused.

  The bridge's code hash moves with this, `48d5c0ed…` → `68b17ae3…`, and the
  contract reports version `1.5.0`. A network takes it as a fresh zerostate or
  as an `updateCode` round on the bridge, not as an in-place patch; the voucher
  is unchanged and its artefacts are byte-identical. The light client changes
  separately, see the entry on light-client anchors under Fixed.

- **The Acki Nacki contracts now live in this repository, under `contracts/an/`.**
  `eccUSDCBridge`, `DepositVoucher` and `EthBeaconLightClient` with the
  `EthKeccak` library, all v1.4.0, moved here from acki-nacki into
  `contracts/an/exchange/` with the compiled `.tvc` / `.abi.json` that go
  into the zerostate, at the state of the `contracts/bridge` branch: the anchor
  surface (`setLightClientCode`, `deployLightClient`, the light-client writers,
  `disableOwnerAnchors`), the `ERR_UNKNOWN_BLOCK` gate on `finalizeDeposit`, and
  `ERR_ZERO_RECIPIENT` on both directions including `initiateWithdrawal`
  (audit WD-AN-07). The bridge's code hash is `48d5c0ed…`, which is what
  shellnet runs. acki-nacki no longer
  keeps a copy: it pins one commit of this repository and places the files
  into its own tree when a zerostate is generated, so a contract change made
  here reaches a network only after that pin is moved.
  `make -C contracts/an/exchange SOLD_0_80=<sold 0.80.0> SOLD_0_81=<sold 0.81.0>`
  rebuilds them, each contract with the compiler its tracked artefact was built
  with: `eccUSDCBridge` with 0.80.0, `DepositVoucher` and `EthBeaconLightClient`
  with 0.81.0. The build stops if either variable is unset or names a compiler
  of another version, since any other compiler changes the code hash.
- **CI pipeline `.woodpecker/an-contracts.yaml`** runs
  `scripts/check_voucher_abi_consistency.py` and
  `scripts/embed_deposit_vk_blob.py --check` on every pull request and on
  `main`.
- **The zerostate setup of the Acki Nacki bridge lives here too.**
  `contracts/an/zerostate_init.py` builds the data cell, upgrades the premined
  stub to the bridge's code, seeds the trusted L1 bridge, installs the
  light-client code and writes the account into the zerostate; acki-nacki
  places it with the contracts and calls it while generating one. A new setup
  call, a new parameter or a new storage field is now a change in this
  repository alone. `BRIDGE_ZS_L1_CHAIN_ID` and `BRIDGE_ZS_L1_BRIDGE` override
  the trusted L1 bridge a generated zerostate starts with, defaulting to
  `11155111` and `0xCdFd6Cef70F68d0849310cD970F8ef8F8E4b4fdb` (Sepolia).
  `contracts/an/zerostate/BridgeZerostateData.sol` is the contract that builds
  the cell — never deployed, executed in `tvm-debugger` — and
  `scripts/check_zerostate_data_encoder.py`, wired into
  `.woodpecker/an-contracts.yaml`, fails if it and
  `eccUSDCBridge.onCodeUpgrade` stop agreeing on the tuple.

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
  size drift, and warns from 90% of EIP-170 (none warn today; Primary and
  Fallback sit at 88%, layer hashes at 78%).
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
  unit is the live loop (no hardcoded `--dry-run --mock-prove`). Rotate
  of the committed fixture is on by default; a live period jump is
  `RotateRequired` — prove on n14 and `submit-rotate`. `--no-rotate`
  opts out of even the fixture path. The owner flip is **off** by default: only
  `daemon --flip-owner` runs it, after the first accepted `submitUpdate`, and
  the daemon refuses to start with the flag unless `AN_USDC_BRIDGE` and
  `AN_USDC_ABI_PATH` are set. The flip reads `getAnchorConfig()` on the bridge
  and sends nothing unless `lightClient` is `AN_LIGHT_CLIENT`; a failed read is
  retried after the next accepted update. Then it calls `disableOwnerAnchors`
  (skipped when `ownerAnchorsEnabled` is already false) and
  `disableOwnerRotation`; one-shot: `eth-lc-relayer flip-owner`. Do not flip
  yet: afterwards deposits from the allowlisted L2s and from the 31
  non-checkpoint blocks of an epoch fail `finalizeDeposit` with
  `ERR_UNKNOWN_BLOCK` (224). `deposit-relayer` walks past a failed id and
  retries it with backoff; `--skip-after-attempts` still parks one that
  keeps failing (`docs/eth-light-client.md` §3.5). `--no-flip-owner` is still accepted and
  changes nothing. Relayer keys must be the owner pubkey of both contracts. The
  slim ABIs in `crates/eth-light-client-relayer/abi/` follow the compiled
  contracts: `USDCBridge.abi.json` has `getAnchorConfig` and no
  `setLightClient`, and `EthBeaconLightClient.abi.json` has
  `setCommitteeCommitment`, which `set-committee` calls; point
  `AN_USDC_ABI_PATH` / `AN_LC_ABI_PATH` at these files. The light client is
  deployed from the bridge with `deployLightClient`; for `getAnchorConfig()`
  returning `lightClient = 0:<account>`, `AN_LIGHT_CLIENT` is
  `<account>::<account>`. tvm-sdk#284 co-deploys with this contract.
  Epoch ancestry **on-chain**: `EthBeaconLightClient.submitAncestry(bytes[]
  headerRlps)` keccak256-binds each execution header and walks `parentHash` to a
  proven checkpoint (≤ 31 parents), then pushes those hashes into
  `  USDCBridge._acceptedBlockHash`. The daemon fetches that chain after every
  accepted `submitUpdate` when `ETH_RPC_URL` is set (`--eth-rpc-url`) and runs
  `link_headers` locally; on-chain `submitAncestry` is `--submit-ancestry`
  (default **off**) because two headers already cost ~130 M gas against the
  10 M limit. It also calls `rePushAnchor` so a bounce before the light client
  was the sink writer is retried. Operator one-shot: `eth-lc-relayer submit-ancestry --eth-rpc-url … --checkpoint-hash
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

- **`.woodpecker/verifier_sources.yaml` checks that every committed verifier
  source compiles to its committed bytecode.** Nothing at run time ties the
  `.sol` a proof is checked against to the `.bin` the bridge deploys, so this
  pipeline does: on every pull request and push to `main` that touches
  `contracts/ethereum/verifiers/`, it downloads `solc 0.8.19` pinned by
  SHA-256 and runs `scripts/check_verifier_sources.sh`, which fails when a
  `.sol` does not compile to its `.bin` byte for byte, when either half of a
  pair is missing, or when a `.bin` exceeds EIP-170. Run it locally with
  `SOLC=/path/to/solc-0.8.19 scripts/check_verifier_sources.sh`.
- **`contracts/an/place.json` says which files acki-nacki places and where.**
  It lists every file that goes into an Acki Nacki tree with its destination
  there, and names the ones that stay here — among them this repository's
  copy of `ISubscriber.sol` (acki-nacki keeps its own original) and
  `zerostate/BridgeZerostateData.sol`, the encoder source whose compiled
  artefacts ship without it. acki-nacki now pins only a commit of this
  repository and follows the manifest, so a new contract or artefact is a
  change here plus a pin move there, with no file list to keep in step.
  `scripts/check_place_manifest.py`, wired into `.woodpecker/an-contracts.yaml`,
  fails when a file under `contracts/an/` appears in neither list, when a
  destination is not one of the few acki-nacki gives us (`contracts/exchange/`,
  `contracts/zerostate/`, the two `*_compiled/exchange/` folders and
  `contracts/scripts/bridge_*.py`, but never
  `contracts/scripts/bridge_contracts.py`, which is acki-nacki's own module and
  the one that does the placing), when two files claim one destination, or when
  the manifest tries to place `ISubscriber.sol` at all — acki-nacki keeps its
  own original of that interface at the destination our layout would imply, so
  placing our copy would silently overwrite it. acki-nacki enforces the same
  destination rule on its side; the manifest is followed, not trusted.

### Changed

- The ETH→AN deposit relayer no longer stalls every later deposit behind one
  that cannot be finalised. Later ids are tried first; a failed id stays in
  `outstanding_deposits` and is retried with backoff on idle ticks,
  round-robin, from its stored Deposit tx (or its block) so the log-scan
  cursor can keep moving. A pending submit (`ERR_PAUSED`, a confirmation
  timeout) goes into that set with backoff but does not count toward
  `--skip-after-attempts`. Parked ids stay parked. A `state.json` written by
  an earlier relayer that only has `attempts_since_progress` for the last
  failed id carries that count into the new outstanding entry, so
  `--skip-after-attempts` does not start over; that id has no stored tx, so
  the first retry starts at the `scan_done_through_block` from that file
  plus one (the earlier relayer left the cursor at the deposit's block
  minus one), not at `--from-block` and not at the live head.
  `finalize-one` is unchanged.

  Gauges that used to mean "the one stuck head-of-line id" now mean something
  else: `deposit_relayer_attempts_since_progress` follows the last failed
  outstanding id and resets when any later id finalizes;
  `deposit_relayer_last_finalized_deposit_id` is seeded from `state.json` and
  only goes up; `deposit_relayer_target_deposit_id` shows the retried id
  during an idle retry, then the forward id again. Alert on
  `deposit_relayer_outstanding_deposits`.

  Before running an older binary, move every `outstanding_deposits[].deposit_id`
  into `parked_deposit_ids`. An older binary ignores `outstanding_deposits`
  (no `deny_unknown_fields`) and `last_processed_deposit_id` is already above
  those ids, so it would neither retry nor park them.

- **A retried submission reuses its aggregated proof.** `daemon-live` ran
  `aggregate-proof` (~2 min, ~15 GB) again before every retry, so each
  refused send or missing receipt cost one more aggregation, and a rotation
  deferred until the next `verifyBlock` was aggregated on every tick. The
  calldata of the last bundle and of the last rotation is now kept with a
  digest of every input it was built from, and reused while the inputs
  match: `aggregated_source: same proof and inputs as the last attempt;
  reusing its aggregated calldata`. A contract rejection and a successful
  submission drop it, so a refused proof is aggregated anew.
- **The relayer no longer reports an ordinary wait between bundles as a
  stall.** `WARN block source still has no data; relayer is stalled` came
  after 16 polls without a new bundle, which at L2 (about 90 minutes between
  bundles) was most of every wait. The threshold now scales with the bundle
  stride, 16 polls per L1-sized stride (256 at L2, about four hours), and the
  warning reads `no new bundle for much longer than a bundle interval; check
  the prover and the chain`. Until then every 16th poll logs `INFO waiting
  for the next bundle`.
- **`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh` takes
  `BUNDLE_STRIDE` from `compute_bridge_anchors`.** `compute_bridge_anchors`
  prints `BUNDLE_STRIDE`, `AnchorMode::stride()` for its `--level`: the value
  `daemon-live` checks against `bundleStride()` at startup. The script
  deploys with it instead of the 1024 and 16384 it had written in, and
  ignores a `BUNDLE_STRIDE` set in the caller's shell.
- **A chain that stays below the relayer's last confirmed write is reported
  after 10 minutes.** `daemon-live` retries a `_rewound` signal as
  read-after-write lag (`WARN transient RPC read-after-write lag — will
  retry next tick`) for at most 10 minutes, then reports it as drift:
  `ERROR chain still below our last confirmed write — not RPC lag (a reorg
  that dropped our transaction?)`, counted in `tick_errors_total` and
  `history_drift_detected_total`, retried with backoff like every other
  drift. Before, it was retried silently and without limit, so a reorg that
  dropped the relayer's transaction never surfaced.
- **The shellnet L2 bridge was redeployed.** `AckiNackiBridge` is now
  `0xb733a57c393214254506bde7e9950a1b57e06fb4` (Sepolia block 11910592),
  seeded at shellnet seq_no 23887872 (`BRIDGE_BOOTSTRAP_SEQNO`) with
  `GENESIS_LAST_BK_SET_UPDATE_SEQNO=23886566`;
  `crates/bridge-prover-libraries/L2_config/env` now points at it instead
  of `0x8d9190666128ab897c5abd8c107239a197e08467`. The bridges deployed for
  the BK-rotation tests in between, `0x9f993772af91d9b1e61d210eaae903eb43655498`
  (Deploy #8) and `0x00e4b69d25f4526c768552ea217dad85e4b7963a`, are retired
  too. A relayer
  set up for the old address needs the new `BRIDGE_ADDRESS` and
  `BRIDGE_DEPLOY_BLOCK`, and a cold start with its `relayer-state.json`
  and `state/*.json` archived first (§3.5 of
  [the live relayer runbook](crates/bridge-relayer-daemon/docs/live_relayer_bridge_runbook.md)).
- **The bridge deploy and `daemon-live`'s cold start refuse a BK-set file
  that is not the chain's set at the seed.** `compute_bridge_anchors`
  (run by `crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`)
  and the daemon's bootstrap compare the file's Poseidon commitment with
  the one the seed block records (block-id tree leaf 2, or leaf 3 when the
  seed block is itself the last rotation at or below the seed) and stop
  with
  `BK-set commitment … does not match the chain's … at seed seq_no=…`.
  Before, a file built for an earlier height deployed without complaint,
  and the daemon failed only at the first rotation with
  `prover out of sync`. Build the file for the exact seed:
  `seed_prover_bk_set --cap-height <seed>`, then `jq .pubkeys_hex`.
- **`crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh`
  broadcasts legacy transactions at a fixed 2 gwei** (`--legacy
  --with-gas-price 2000000000`) with a 12× gas-limit estimate
  (`--gas-estimate-multiplier 1200`). The deploying
  account must cover the larger upfront reservation (~0.6 ETH for the
  whole bundle on Sepolia); on a congested network, raise the price in
  the script.
- **`scripts/install.sh` warns below 24 GB of RAM and swap, not 48 GB**, and
  the `ackinacki-bridge` docs give a withdrawal's measured peak: ~19 GB for a
  cold run on the test host, system-wide and sampled every 5 s; the CLI
  reached 9.5 GB RSS in Circuit 4, and the `aggregate-proof` it waits on
  12.6 GB while the CLI still held part of its own. They said ~40 GB, which no
  measurement backed.
- `ackinacki-bridge withdraw` exit 11 and exit 12 end with the AN burn's hash,
  the path of the withdrawal's record and the command that carries on (the same
  command with `--allow-retry`, which skips the burn and resumes at capture).
  Exit 11 also says when not to: a bounced burn emits no event, and the
  advanced runbook's Case 3a applies.
- `ackinacki-bridge withdraw` checks the `WithdrawalInitiated` event it
  captures against the withdrawal it is running — amount, recipient,
  destination chain, token (USDC) and sender multisig — before proving it. A
  mismatch refuses with exit 10 and lists what differs; the record is left as
  it was. Before, a resume whose record carried another withdrawal's burn hash
  (written in by hand during reconciliation) proved and paid out that other
  withdrawal and marked this record confirmed.
- `ackinacki-bridge withdraw` exit 13 names the `AckiNackiBridge` custom error
  a `withdrawByProof` dry run or submit reverted with, with its arguments
  (`NullifierAlreadyUsed(nullifier: 0x…)`, `BridgePaused()`,
  `WithdrawTreasuryShortfall(requested: …, available: …)`), and for the common
  ones what to do next. Before, it printed the node's raw text, which on public
  Sepolia RPCs reads "tried to decode error from abi but failed" followed by
  the selector.
- The AN→ETH relayer's startup read of the bridge (`daemon-live`, and the
  `ackinacki-bridge` CLI before the burn and after coverage) rebuilds the
  per-slot window heights from `LayerAnchorAppended` logs. The scan walks
  backwards from a pinned head in `BRIDGE_GET_LOGS_CHUNK_BLOCKS`-block
  `eth_getLogs` calls (default 2 000; Alchemy's free tier caps it at 10),
  `BRIDGE_GET_LOGS_PAUSE_MS` apart, and stops as soon as every window's
  entries are covered or at `BRIDGE_DEPLOY_BLOCK`; the stop point is the
  oldest entry still held by any window. Layer N of the windows is appended
  only at `128^N` boundaries, so a layer that is not full keeps its first
  entry and the walk reaches back to it (layer 3 fills only after about
  2.8 years), in practice nearly to the deploy block: about
  `(head - deploy_block) / span` calls, a few per day of bridge age on a
  2 000-block span and about 720 per day (three minutes a day at 4 calls/s)
  on a 10-block cap; a call failing with 429 / 5xx / a transport error is
  retried with backoff, a `-32600` / `-32602` response (how Alchemy rejects
  a span over its cap) is not. The snapshot is pinned to one block (the
  block's hash is compared before and after the read, so a head block
  replaced mid-read is re-read instead of mixing two forks, when the RPC
  observes the replacement) and every kept
  log is checked against its window slot (hash and height) and the
  window's `lastHeight`: a rejected newest span re-reads the snapshot a few
  times (six seconds apart, so the re-reads span a Sepolia slot); a
  mismatch (an append landing mid-read, a partial log set), fewer logs
  than entries (a backend one block behind omitting the newest append) or
  a replaced head block, found only once the walk is complete, once,
  counted on its own;
  after which a short log set fails naming `BRIDGE_DEPLOY_BLOCK` and the
  others surface as they are; on a span the RPC always rejects that is two
  extra short reads before the same refusal, and a `BRIDGE_DEPLOY_BLOCK`
  above the chain head is refused before any scan. The scan logs its range,
  progress and total; an
  unset deploy block is a warning. The CLI's coverage poll reads one scalar
  per round, reads the full snapshot once coverage is observed, and polls on
  if that snapshot still predates the target; an RPC error in a round is
  retried on the next one (up to ten rounds in a row, within the wait
  budget) instead of ending the run after the burn, the last error is part
  of the message when the wait gives up, and once `latest` has shown the
  target the wait allows a few rounds past the deadline for the snapshot to
  catch up.
- **The shellnet profile's `RPC_URL` is `https://rpc.sepolia.ethpandaops.io`**
  (was `https://ethereum-sepolia-rpc.publicnode.com`); the relayer-side
  `RPC_URL` in `crates/bridge-prover-libraries/shellnet.common` is the same
  endpoint (was publicnode too): Tenderly's public gateway, which has the
  history too, answered `daemon-live`'s `applyBkSetUpdate` sends with
  `-32005` (rate limit), even one at a time. The server template
  `scripts/ursus/bridge-relayer.env.example` points at it too. The
  window-heights read needs `eth_getLogs` back to the bridge's oldest held
  anchor; publicnode keeps only the newest ~10 000 blocks of logs and
  answers an empty set for older ones, so on it the read comes up short
  about a day and a half after the first anchor and every default-profile
  withdrawal is refused in preflight. `deposit` needs every receipt of a
  block and `eth_getRawTransactionByHash` from the same `RPC_URL`, and the
  raw transactions Tenderly's public gateway does not serve (`deposit` is
  refused in preflight on it); the EF DevOps endpoint serves both, with full log
  history, and takes a 10 000-block span. When a short log set is met and
  the deploy block itself answers with no logs, the error also says the RPC
  may not serve history that far back, and no re-read is spent on it; with
  no `BRIDGE_DEPLOY_BLOCK` at all a short log set is final too, since the
  re-read would walk to genesis again.
- **The pinned shellnet `BRIDGE_ADDRESS` in `config/bridge_config.shellnet`
  rotated to `0x5384f162aa1a80b7a03687e1a3188f67b1fffad5`** (the 2026-10-15
  deploy from `main` for the shellnet network started on 2026-10-14; its
  deploy block 11921366 is pinned beside it as `BRIDGE_DEPLOY_BLOCK`). The
  earlier deploys, `0xa1baf3f71eb9b146a3577c9d9d890f7a6932cb36` (2026-10-02,
  deploy block 11828971), `0x32b9e87acaa1ad7d61a81f93dd9d525f64ff4f38`
  (2026-09-29, deploy block 11807209) and
  `0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7`, were advanced from the
  previous network; they still answer every getter, but nobody advances
  them. Preflight refuses `0x0F4F…` (an older verifier stack) and, with the
  profile's `BRIDGE_DEPLOY_BLOCK`, the other two (their anchors predate that
  block, so the window-heights read comes up short); only a deploy block at
  or before the oldest anchor their windows still hold (their own deploy
  block, for one) gets a burn that then times out at stage 4b. All of them
  bind the same AN bridge account id; the rotation changes nothing else in
  the profile. Accounts from the previous network do not exist on the
  current one: a multisig used as `--from` before 2026-10-14 is refused in
  preflight with exit 2 (account not found) and has to be replaced. Acki
  Nacki trusts deposits from `0x5384…` and from none of the earlier deploys,
  so a deposit into one of them is not credited. A profile installed by
  `install.sh` from an earlier release, or copied by hand, keeps the old
  `BRIDGE_ADDRESS` and `RPC_URL`: change both and add `BRIDGE_DEPLOY_BLOCK`.
  Each deploy keeps its own treasury, and nothing moves from the old one to
  the new one.
- `ackinacki-bridge withdraw` runs the window-heights read in preflight, so a
  wrong `BRIDGE_DEPLOY_BLOCK`, an `eth_getLogs` span the RPC rejects or an RPC
  without log history refuses with exit 2 instead of failing after the burn
  and the coverage wait; preflight also sends the scan's first full-span
  `eth_getLogs` once, with the scan's retries, since the read itself sends
  none on a bridge without anchors and a clamped one on a bridge younger
  than the span.

- **`.woodpecker/release.yaml` builds, checks and publishes the deposit
  prover, and refuses a tag in more cases.**
  - The `build` step fails at once while `MIN_BRIDGE_VERSION` is unset (see
    Added), and builds the CLI with no cargo features.
  - A new first step, `ceremony`, carries `kzg_bn254_21.srs` over from the
    previous release. A tag whose previous release lacks it now fails,
    because the deposit SRS is derived from it; before, the release went out
    without the file. Upload it once by hand (`gh release upload <previous
    tag> kzg_bn254_21.srs`) and re-run the pipeline.
  - `build` also builds `deposit-prover`'s examples with that package's own
    `rust-toolchain.toml` (`nightly-2026-02-03`), which the build image has
    to be able to install.
  - A new step, `verify-deposit-prover`, runs before `publish`: it derives
    `kzg_params_18.srs`, unpacks the tarball outside the source tree, and
    checks the packaged prover with `scripts/check_deposit_prover_bundle.sh`
    — the Hermez [s]·G2, the verification key it derives against
    `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin` (the one
    the Acki Nacki bridge embeds), and a proof of the fixture deposit
    against its public inputs. Nothing is published if it fails. On a
    20-thread workstation it took about 2 minutes and 4 GB of RAM.
  - New secret `WC_PROJECT_ID`, with the `tag` event ticked: the
    WalletConnect Cloud project id compiled in as the default
    `--wc-project-id`. Without it the release has no default, and a
    WalletConnect deposit needs the flag or `BRIDGE_WC_PROJECT_ID`.

- **`bridge-relayer-daemon`'s withdraw scan parks a `proof_event_*.json` on
  proof-intrinsic `withdrawByProof` reverts instead of holding the queue on
  exponential backoff.** With the aggregator now binding the inner-circuit
  VK on-chain, a proof built against a rotated (or wrong) inner key makes
  the adapter's runtime digest guard return `false`, and the bridge reverts
  `WithdrawalProofRejected()` — every retry has the same fate. The scan
  loop now classifies the 4-byte revert selector: `WithdrawalProofRejected`,
  `WithdrawIdentityMismatch`, `DstChainIdMismatch`, `UnsupportedTokenId`,
  `RecipientHalfOutOfRange`, `InvalidRecipient`, `FieldElementOutOfRange`,
  `InvalidNumLayers` and `NullifierAlreadyUsed` are treated as permanent
  (the path is recorded in an in-memory set, counted under
  `parked_permanent`, and the scan continues to the next file). A restart
  retries the file. A rewrite of the same path is retried in-process when
  its length or mtime changes. `skipped_already_used` is only an
  already-used nullifier, and that log carries the nullifier, amount and
  recipient; a parked proof logs those fields on its own line. `UnknownAnchor` and `WithdrawTreasuryShortfall` stay on
  the backoff path, as do RPC-side failures and post-send confirmation
  errors. The `submit-withdraw` and `withdraw-e2e` CLI subcommands now
  label the revert as `(permanent)` or `(transient)` in the failure
  message.

- **`ackinacki-bridge withdraw` preflight now reads the withdrawal
  adapter's `vkDigest()` and compares it with word 23 of the reference
  `BridgeWithdrawalAggregatorVerifier_calldata.bin`.** A mismatch or an
  adapter that predates the inner-VK binding (`vkDigest()` reverts) is
  refused before the AN-side burn, exit `2` (`PreflightRefused`), with
  the `cast call <adapter> "vkDigest()(bytes32)"` remedy. Without the
  check the failure surfaces post-burn and post-anchor-wait as
  `WithdrawalProofRejected`. The check runs only when `--verifiers-dir`
  is passed (which is when the deployed-bytecode compare already runs);
  without it the run still passes the earlier stages and warns that both
  the bytecode check and the digest pin compare were skipped. A
  `_calldata.bin` that is not exactly 3 680 B is refused. 3 648 B is the
  pre-binding blob.

- The AN→ETH relayer applies `applyBkSetUpdate` even if the layer cursor
  is still behind this N, under the bundle-aware rule above: rotations
  before the next unverified bundle boundary, and the first one past it,
  go out back-to-back; the next one waits until that bundle is verified.
  It does not abort the tick. After an apply, `daemon-live` does **not**
  ack the live prover until the next bundle target (the driver's stride:
  1024 at L1, 16384 at L2) is above N, so the outgoing set can still
  sign `verifyBlock` for `seqNo <= N`. `daemon-bridge` does not apply
  rotations. AN must announce a rotation before the bundle at the same
  seq is proven — a bundle at N acked first bakes `last_seen = N` and
  the rotation becomes unsatisfiable.

- **The halo2 circuit crates are vendored under `crates/bridge-circuits/`;
  building the prover or the CLI no longer needs read access to a private
  repository.** The five crates (`attestation-bls-checker-circuit`,
  `historical-layer-hashes-movement-checker-circuit`,
  `bridge-event-prove-circuit`, `bridge-poseidon`, `bridge-test-data-gen`)
  used to live in gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits, pinned
  by revision (see the 0.2.0 "pinned by revision, not `branch = main`" note,
  which no longer applies). The `bridge-prover-libraries` sub-workspace
  declares the five circuit crates as path deps at its workspace level
  (`bridge-prover-libraries/Cargo.toml:50-54`), and the standalone
  `crates/bridge-snark-utils` package (also excluded from the root
  workspace, see the top-level `Cargo.toml` `exclude` list) declares
  them the same way; both reach across sub-workspace boundaries into
  `crates/bridge-circuits`, which is itself excluded from the root
  workspace so its gosh-fork halo2 backend does not clash with the
  root's.
  `bridge-relayer-daemon`'s own `Cargo.toml` has no direct path-dep on
  any circuit crate — it consumes them transitively through
  `bridge-event-witness` / `bridge-event-prover-lib`. The external
  `[patch]` block is gone, and a circuit edit plus its
  `EVENT_CIRCUIT_REVISION` bump land in the same PR.
  `bridge-evm-aggregator` never depended on these crates and is
  unaffected.

- **`tvm-sdk` moves from `v3.0.5.an` to `v3.0.6.an`** across the root
  workspace, `crates/bridge-prover-libraries`,
  `crates/deposit-relayer-daemon` and `crates/eth-light-client-relayer`.
  The three binaries that actually link `tvm_client` — `ackinacki-bridge`
  (inside `bridge-prover-libraries`), `deposit-relayer-daemon` and
  `eth-light-client-relayer` — all ship the new version.
  `eth-light-client-relayer` only pulls `tvm_client` in when built
  with `--features live-submit` (`dep:tvm_client` behind that feature
  in its `Cargo.toml`); without it the binary compiles without
  `tvm_client` at all.
  `bridge-relayer-daemon` does not link `tvm_client`, but it does pick
  up the bump: it consumes `tvm_block` transitively through
  `bridge-event-witness`, which pins `tvm_block` at the same tvm-sdk
  workspace tag. So no line in `bridge-relayer-daemon`'s own manifest
  changes, but its build now compiles against `tvm_block v3.0.6.an`.
  `dense-balanced-tree` also moves off a floating branch onto tag
  `v1.0.0`.

- **`scripts/check_voucher_abi_consistency.py` checks `contracts/an/` by
  default.** With no arguments it checks the sources in
  `contracts/an/exchange/` against the compiled ABIs in
  `contracts/an/0.80.0_compiled/exchange/` and
  `contracts/an/0.81.0_compiled/exchange/`. It does not check the ABI copies
  the tooling loads: `crates/bridge-prover-libraries/python/contracts/` was
  the previous default and is no longer checked, and
  `crates/ackinacki-bridge/abi/` was never covered. Those copies predate the
  `chainId` deposit identity and are known to be stale; pass them with
  `--compiled-bridge` / `--compiled-voucher` to see the drift.
  `--compiled DIR` is replaced by `--compiled-bridge FILE` and
  `--compiled-voucher FILE`, because the two artefacts sit in different
  folders, and the bridge source it reads is `eccUSDCBridge.sol`.

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
- `eth-lc-relayer daemon` auto-rotates only the committed fixture period;
  a live period jump is `RotateRequired` (prove on n14 and
  `submit-rotate`). `--no-rotate` is the shadow/laptop opt-out. The owner flip runs only with
  `--flip-owner` (see the `eth-lc-relayer` entry under Added). With
  `ETH_RPC_URL` each accepted `submitUpdate` then `rePushAnchor`s
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

- **The release bundle's `verifiers/` directory includes `*.sol`**, and
  `scripts/install.sh` installs them and reports a host without
  `BridgeWithdrawalAggregatorVerifier.sol` as incomplete. Run against a
  release published before this change, the installer instead warns that
  the bundle predates verifier sources and reports the verifier files as
  missing rather than installing an incomplete tree — so
  `scripts/install.sh` from `main` needs a release built from this change.
- **Stage 1 of `ackinacki-bridge withdraw` checks
  `BridgeWithdrawalAggregatorVerifier.sol` in `--verifiers-dir`** against the
  copy embedded in the build, exactly as it already checks the `.bin`, and
  `--allow-verifier-drift` now covers both files.
- **The relayer image's `IMAGE-SHA256SUMS` lists the four verifier `.sol`
  files** and no longer lists `bin/solc`; `preflight.sh` refuses a verifier
  lane whose `.sol` is missing.

### Fixed

- `deposit-prover/scripts/recanonicalize_fixture_headers.py` skips fixtures
  marked `fixture_kind: synthetic` in both check and write modes, so they do
  not interrupt header checks and updates for real chain witnesses.

- Frontend deposits use the validated Acki Nacki recipient entered in the form.
  The connected Ethereum address is used for USDC allowance checks. Invalid AN
  recipients are rejected before a deposit transaction is sent.

- The withdraw relayer proved a block whose attestation carried a signer
  more than once with `block_seq_no = 0`: the fetcher wrote the height into
  the assembled attestation bytes at an offset computed from the sum of the
  signer counts, while the layout, and the circuit, count entries (one per
  distinct signer). `AckiNackiBridge` rejected the proof with
  `AttestationProofRejected()`, the daemon aborted after three rejects and
  was restarted into the same loop. Shellnet block 1239860 (2026-10-19,
  three signers with count 3) stopped the relayer this way at
  `applyBkSetUpdate`; a bundle block with a repeated signer would have done
  the same at `verifyBlock`. The height is now written where the circuit
  reads it, a block whose `seq_no` is not the requested height is refused,
  and a proof whose height is not the block's is refused before it is sent.
  Upgrade the relayer (for the shellnet-l2 kit, rebuild the image); nothing
  on chain, in the keys or in the state changes, and the stuck block is
  proved again on the next start.
- `deposit-relayer` kills a prover that runs past its timeout instead of
  leaving it running, writing the `data/` key cache beside the retry the
  daemon starts after its backoff. `relayer prove-withdraw` does the same
  with `bridge-event-halo2-prover`, and passes a relative `--witness` or
  `--out-dir` to it as an absolute path: the prover runs in `--work-dir`, and
  resolved them there.
- An AAVE loss left `suppliedPrincipal` above the aToken balance with no way
  to bring it down: `accruedYield()` stayed zero for good, and
  `withdrawFromAave` kept reverting once the pool was empty. The book can now
  be written off (see Added), and payouts no longer depend on it. A pool that
  redeemed more than asked could also make `withdrawFromAave` and the
  `withdrawByProof` top-up revert with an arithmetic panic; the book now drops
  by the requested amount, floored at zero (ETH-28).

- `deposit-relayer` reported a deposit as finalized as soon as the bridge's
  own `finalizeDeposit` transaction exited 0. That transaction always exits 0
  once the proof verifies: the voucher deploy and the `confirmDeposit` mint
  run afterwards, in transactions of their own. A failed voucher deploy (the
  2026-07-02 outage, exit 9, when the bridge carried outdated voucher code)
  therefore counted as a success, and the relayer moved on with nothing
  minted. A resubmitted deposit also counted as a fresh finalization,
  because exit 51 surfaces in the voucher's transaction, never in the
  bridge's. The relayer now follows both hops and reports the outcome there:
  - voucher deploy exits 51: already finalized;
  - voucher deploy or `confirmDeposit` fails: rejected, with the reason;
  - either hop not executed within the 60-second confirm timeout: pending;
  - both hops succeed: finalized.

- `deposit-relayer` named `finalizeDeposit` errors after an older bridge.
  Exit 222 now reads `ERR_UNSUPPORTED_SRC_CHAIN`, and the fix it names is
  `setTrustedL1Bridge(chainId, l1Bridge, true)`. The 223 and 229 hints,
  for `setExpectedAnDappId` and `setMintCap`, are gone, because the
  deployed bridge has neither. Exits 204 (`ERR_ZERO_AMOUNT`) and 230
  (`ERR_ZERO_RECIPIENT`) are now named. Exit 231 (`ERR_PAUSED`) now counts
  as pending, not rejected, so pausing the bridge no longer runs deposits
  into `--skip-after-attempts` and parks them.

- **`daemon-live` no longer jams after a burst of BK rotations when a
  receipt read fails.** The relayer's per-tick comparison with its last
  snapshot of the bridge (Check B) rejected a second rotation while the
  previous one was ahead of the layer cursor
  (`last_bk_set_update_seq_no_uncovered`), although `applyBkSetUpdate`
  accepts it whenever the next bundle boundary is above the previous N.
  The relayer's own sends refreshed the snapshot and hid this, but a
  rotation that landed while its receipt read failed left every later tick
  failing with `chain drift`, and the startup audit refused a restart until
  `relayer-state.json` was edited by hand. Check B now applies the
  contract's rule, and the startup audit accepts the moves Check B accepts
  instead of requiring an exact match. It also no longer bounds how far the
  rotation cursor moves: rotations follow Acki Nacki's epochs, not bundles,
  so a rotation that landed without its receipt could be more than four
  bundles past the remembered one (after a deploy with a genesis cursor of
  0 the first one always is), and every tick and every restart failed with
  `last_bk_set_update_seq_no_jumped`.
- **A failed RPC call no longer stops `daemon-live`.** A `verifyBlock` or
  `applyBkSetUpdate` whose send the RPC refused (rate limit, connection,
  nonce) or whose receipt never came is logged as `… not confirmed — RPC
  failure, not counted toward the abort threshold` and retried; the next
  tick re-reads the chain, so a transaction that landed anyway is picked
  up. Ticks that find no bundle yet count in `polls_since_progress`, a new
  field of `relayer-state.json`, instead of `attempts_since_progress`.
  Before, the ~90-minute wait for an L2 bundle ran that counter far past
  the abort threshold of 3, so the first failed send after a wait stopped
  the daemon with `daemon: stuck — hard aborting`. Reverts the node
  simulates (`execution reverted`) and failed post-submit checks still
  count, and still stop the daemon at the third.
- **An RPC request that gets no answer no longer holds the relayer.** The
  Ethereum RPC client of every `relayer` command had no timeout, so a
  request on a connection that went silent waited until the OS dropped the
  connection. `daemon-live` once waited 9.5 minutes on one
  `eth_blockNumber`, and a BK rotation that had landed on Acki Nacki waited
  with it. A request now fails after 60 s (`BRIDGE_RPC_REQUEST_TIMEOUT_SECS`):
  the daemon logs `tick failed, will back off` and retries, and a send that
  times out is an RPC failure, not counted toward the abort threshold.
- **Check B allows four bundles of the daemon's own stride between two
  snapshots** (65 536 seq_nos at L2) instead of 4 096 at every level. One
  L2 bundle the relayer did not see land — its own `verifyBlock` whose
  receipt read failed, or a sibling relayer's — failed every later tick
  with `last_seen_block_seq_no_jumped`.
- **The relayer reads the bridge's state at one block.** The per-tick read
  and the startup audit take `eth_blockNumber` and pin all five view calls
  to it; on a load-balanced RPC the calls could land on backends at
  different heights and report a false `bk_set_commitment_without_update`.
- **`applyBkSetUpdate` waits at most 180 s for its receipt and is checked
  after it**, as `verifyBlock` already was. Before, a dropped transaction
  hung the daemon on the receipt indefinitely, and a transaction mined but
  reverted was taken as applied. Now the chain must hold the submitted N
  and commitment at the receipt's block, or the outcome is a revert
  (`post-submit drift: …`).
- `daemon-live` logged `ERROR chain monotonicity drift — halting` and
  failed its ticks seconds after a successful `applyBkSetUpdate` or
  `verifyBlock` tx on load-balanced public RPCs (observed on publicnode's
  Sepolia gateway). It did not exit: every failed tick was logged as
  `tick failed, will back off` and retried, counting toward
  `tick_errors_total` and `history_drift_detected_total`.
  The post-write snapshot used receipt-pinned reads, but the next tick's
  consistency check read `latest` through the balancer and sometimes
  landed on a backend that had not yet propagated the write — producing
  a `last_seen_block_seq_no_rewound` or `last_bk_set_update_seq_no_rewound`
  signal that the tick treated as a timeline divergence. Those
  two rewind signals are now classified as transient RPC
  read-after-write lag: the tick returns `NotYetAvailable` with a `WARN
  transient RPC read-after-write lag — will retry next tick` log, counts
  toward `not_yet_available_total` instead of `history_drift_detected_total`,
  and the daemon continues on the next backoff. Jumps, orphan commitments
  and uncovered rotations keep their fatal semantics — a stale backend
  serves an older snapshot, not a different timeline. Validated on the
  Sepolia Deploy #8 shellnet replay: five BK rotations and one bundle
  applied in 24 minutes with no false-positive drift errors.
- **A zero owner key can no longer be installed in `eccUSDCBridge` or
  `EthBeaconLightClient`.** Both gate their admin calls on
  `msg.pubkey() == _ownerPubkey`, and an unsigned external message carries
  `msg.pubkey() == 0`, so an owner key of zero let anyone through that gate —
  on the bridge that is `setPaused`, `setTrustedL1Bridge`,
  `setLightClientCode`, `setAcceptedBlockHash` and `updateCode`; on the light
  client the committee and code-upgrade calls. All three ways in now refuse
  it, each with a new error code — the constructor, `onCodeUpgrade` (so an
  upgrade whose migration cell carries a zero key aborts instead of opening
  the contract up) and `setPubkey`: `ERR_ZERO_PUBKEY` 232 on the bridge, 254
  on both light-client variants. Deployments and upgrades that pass a real key are
  unaffected; the zerostate already installs one read from a keyfile.
  The compiled bridge and exchange light client include these guards.
  For an existing deployment, install their rebuilt code with `updateCode`;
  the owner key and migration-cell layout are unchanged.
  `DepositVoucher` code is unchanged, so its replay-protection addresses
  stay the same.
- An `aggregate-proof` run that hits its timeout (`--prover-timeout-s` in
  `ackinacki-bridge withdraw` and `relayer withdraw-e2e`, a fixed 30 min in
  `relayer daemon-live` and `relayer prove-withdraw-shplonk`) is killed.
  Before, it kept running after the caller gave up — holding its memory and
  still writing the pk cache — beside the proof the next run started. Since a
  killed keygen starts over on the next run, the error of a command with
  `--prover-timeout-s` says to raise it rather than retry with the same
  budget. For scale: a cold withdrawal `aggregate-proof`, keygen included,
  took ~2 min on the test host.
- `aggregate-proof` logs to stderr, at `info` unless `RUST_LOG` says
  otherwise, and `ackinacki-bridge withdraw` and the relayer put each of its
  lines into their own log as it comes, prefixed `aggregate-proof:` and at
  the level it was logged with — among them the pk cache's notices: waiting
  for another keygen, a filesystem without locks, leftovers removed, a slot
  loaded or generated — and the lines of an error it prints, `Caused by:` and
  all, at `error`. They do not pass their own `RUST_LOG` on to it, and a
  `RUST_LOG` of yours must let `bridge_relayer_daemon=info` through to show
  its lines — `RUST_LOG=ackinacki_bridge=info` alone, as the 0.2.0 notes
  suggest, hides them. A failed or timed-out run's error quotes its error
  message — up to its backtrace — or, without one, its last 40 lines. Before,
  it logged only errors, to stdout, which the callers discarded, a failure
  quoted the whole of its stderr, and a timeout quoted nothing.
- `aggregate-proof` writes a new pk-cache slot under
  `<stem>.pk.partial-<pid>-<random>` /
  `<stem>.meta.json.partial-<pid>-<random>`, syncs each file to disk and renames
  it into place, so neither a keygen killed mid-write (a timeout, the OOM
  killer) nor a host crash just after it leaves a truncated `<stem>.pk` that
  every later run panicked on until it was deleted by hand. A write that fails —
  a full disk — removes its own partial file. A `.pk` without its `.meta.json`
  is regenerated rather than read. The `.meta.json` now records the key's length
  (`pk_bytes`) — a slot published before gets it once a run that can take the
  cache's lock, and owns the `.meta.json`, has read its key back whole — and a
  key is read in halo2's checked `RawBytes` format, which reads the same bytes,
  after its header's `k` is checked against the degree the `.meta.json` records.
  A key whose length or `k` is not the recorded one, that ends early, or whose
  wrong bytes are not field elements, or a `.meta.json` that is not whole, fails
  the run with `cached aggregator key … is damaged` (or `cache meta … is
  damaged`); one that cannot be read for another reason — a permission — says so
  without telling anyone to remove it. A key of the right length whose wrong
  bytes are field elements still reads, and the proof made with it fails the
  check the SDK runs on every proof it makes: the run fails with `making the
  aggregated proof failed (…)`, saying that the key read from the cache, or the
  ceremony file it names, is damaged — or, for a key the run generated itself,
  that the ceremony is damaged or keygen and prover disagree. In these cases the
  slot is set aside when it can be: its `.meta.json` is renamed to
  `<stem>.meta.json.damaged`, which unpublishes it, the next run that needs it
  removes both files and generates the key again, and the error says that
  removing them by hand frees the key's ~6.85 GB at once — preflight reserves
  room for the new key and cannot count the old one's. It waits for another
  `aggregate-proof` that holds the cache lock to finish first; with the
  `.meta.json` replaced since it was read, or a rename that fails, the slot
  stays published and the error names the two files to remove. `ackinacki-bridge
  withdraw` preflight names a set-aside slot with a warning, and, when the
  aggregator is to generate its key, refuses — with exit 2, or exit 10 on a run
  that resumes a recorded burn — a key without its `.meta.json` this user cannot
  remove, since stage 5 removes it first. Before, a key that could not be opened
  was regenerated over the published name without the lock, and a truncated one
  panicked. A process generating a key holds `<pk-cache>/.keygen.lock` (`flock`)
  until the slot is published, so cold keygens on one cache run one at a time
  and a process that waited loads the slot its predecessor published. Partial
  files are removed only by a holder of that lock, so never one another process,
  on the host or in a container sharing the cache, is still writing. A lock file
  another user created is opened read-only. Only a filesystem without `flock`
  lets a keygen run unlocked, and it then removes no partial file; any other
  failure to take the lock fails the run. With `--pk-cache-dir`,
  `aggregate-proof --slot-report <path>` writes the stem of the slot it used to
  `<path>` once its calldata is written. `ackinacki-bridge withdraw` preflight
  names any partial file in the pk cache before it measures free space, and
  refuses with exit 2 (exit 10 on a resume) a withdrawal slot whose key is not
  the length its `.meta.json` records or whose header is not a key's at the
  degree it records, whose `.meta.json` is not whole or not one the aggregator
  wrote (a field missing or of the wrong type), or that this user cannot read —
  the last with advice to fix its permissions, not to remove it.
- `docs/EVM-contracts-spec.md` documents the owner's pause: `pause()` /
  `unpause()`, the `paused` slot, the `Paused` / `Unpaused` events, the
  `BridgePaused` / `AlreadyInThatPauseState` errors, and that `deposit`,
  `verifyBlock`, `applyBkSetUpdate` and `withdrawByProof` revert while paused.
  It said the bridge had no pause switch. Its storage table is regenerated
  from `forge inspect`: `pendingOwner` (slot 6), `paused` (slot 7, offset 20)
  and `storedPrevBkSetCommitment` (slot 11) were missing, and the slots after
  them were off.
- Two `ackinacki-bridge withdraw` runs with one `--snark-dir` prove one at a
  time: stage 5 holds `<snark-dir>/.prove.lock`, and the second run logs that
  it waits, then reads the bridge's state again and re-checks the Circuit-4
  proving key before it proves, so the anchor it binds to is still in the
  window and the key is the one preflight verified. Preflight tries this
  lock without waiting — and the aggregator's `<pk-cache>/.keygen.lock` when
  the aggregator will generate its key (a file another user owns is opened
  read-only) — and refuses with exit 2 when one cannot be opened or locked —
  on NFS, a read-only lock file — instead of failing with exit 12 after the
  burn. A lock another run holds passes with a warning; the wait for
  `.keygen.lock` counts against `--prover-timeout-s`.
  Before, both runs wrote the snark directory's fixed-name files at once and
  one failed with exit 12 ("calldata instance … != public input …").
- `ackinacki-bridge withdraw` preflight reserves 7 GiB for the outer aggregator
  proving key instead of 1 GiB: the key a cold run writes into the pk cache is
  ~6.85 GB. A cold run now asks for ~10.7 GB free on the filesystem that holds
  `--params-dir` and `--pk-cache-dir` (one filesystem in the shipped profile)
  and refuses with exit 2 below that; before, a host with 4–9 GB free passed
  preflight and failed in stage 5, after the burn. `scripts/install.sh` now
  wants 13 GB free. A withdrawal whose aggregator key is already in the pk
  cache reserves nothing for it: after a proof, `withdraw` notes in
  `<pk-cache>/.withdraw-slot.json` the aggregator key the proof used, which
  `aggregate-proof` names in `<snark-dir>/aggregator-slot`
  (`--slot-report`), with the Circuit-4 key and the `aggregate-proof` it ran
  with as preflight verified them, and a later preflight that finds both
  digests unchanged and that key and its `.meta.json` still there skips the
  7 GiB (see Breaking Changes for what this asks of an upgrade). The first
  withdrawal after an upgrade reserves room again while
  the previous keys are still on disk: budget ~17 GB for a new
  `aggregate-proof`, ~20 GB when it also brings a new Circuit-4 key, or
  remove the previous `BridgeWithdrawalAggregatorVerifier__v3__*.pk` and its
  `.meta.json` first.
- The read of the bridge that rebuilds window heights scanned
  `LayerAnchorAppended` with no from/to block, so `eth_getLogs` defaulted
  both ends to `latest` and the read failed on any contract that already had
  history (`layer 1 has data_len=N but only 0 LayerAnchorAppended logs`;
  ETH-31): `daemon-live` could not start against such a contract, and
  `ackinacki-bridge withdraw`, which ran that read on every coverage poll,
  failed at stage 4b after the burn. The scan now walks the range; see
  Changed.
- `scripts/install.sh` now makes `<prefix>/withdraw-state` and
  `<prefix>/deposit-state` owner-only (0700), also over an earlier install
  that left them at the umask's mode, so `withdraw` and `deposit` no longer
  warn on every run that the state directory can be reached by more than
  its owner.
- `deposit-relayer daemon` lost deposits it had already seen. Its log-scan
  cursor in `state.json` (`scanned_through_block`) jumped to the confirmed
  head on every poll, whether the target `depositId` was found or not. A
  second deposit confirmed in the same poll window, and a deposit whose
  proof or `finalizeDeposit` had failed, were never looked for again: the
  daemon kept waiting as if they were not made yet, while the bridge's
  `depositCounter()` was already past them. A fresh start also skipped the
  `--from-block` block itself. Now each poll first reads `depositCounter()`
  at the confirmed head (`--confirmations` below the head):
  - while the next `depositId` is not made yet, the poll makes no
    `eth_getLogs` call and moves the cursor up to the confirmed head;
  - once it is made, the scan starts after the cursor and stops at the block
    that holds the deposit. The cursor moves after AN accepts the deposit,
    to the block before it, so a later deposit in the same block is still
    found. A failed deposit is retried from its stored tx, not from this
    cursor;
  - if `depositCounter()` says the deposit is made but no scanned block holds
    its log, the poll fails with an error naming the blocks it scanned: the
    RPC returned incomplete logs, or `--from-block` is above the deposit.
    `watch` and `prove-one` look deposits up the same way and fail with the
    same error where they used to report the deposit as not visible yet.

  The field is now `scan_done_through_block`. The old one is ignored and
  dropped on the next save, so `state.json` needs no editing. After the
  upgrade the first scan starts at `--from-block`, which must not be above
  the oldest deposit the daemon has yet to deliver. The RPC must answer
  `eth_call` at a block `--confirmations` below the head.
- `deposit-relayer daemon --skip-after-attempts` counted every poll that
  found no deposit as a failed attempt, so an idle daemon parked ids nobody
  had deposited yet, and the deposits that later took those ids needed a
  manual `finalize-one`. Waiting no longer counts; only a failed proof, an
  AN rejection or a submit still pending does. The attempt count an older
  daemon saved in `state.json` included those polls, so the first start
  after the upgrade resets it. The warning `no confirmed deposit yet; relayer
  is idle` is gone with it. The flag now also reads
  `SKIP_AFTER_ATTEMPTS`. The systemd unit does not pass
  `--skip-after-attempts`, so a `SKIP_AFTER_ATTEMPTS` line in
  `deposit-relayer.env` did nothing before and takes effect now: check it
  before restarting the daemon.
- `deposit-relayer` did not build against tvm-sdk `v3.0.6.an`: its
  `Cargo.toml` lacked the halo2 `[patch]` tables that pin `tvm_vm` to a
  single `halo2-axiom` (already present on `eth-light-client-relayer`),
  and `TvmAckiNacki::supports_dapp_id` called a `ClientContext` method
  removed in that tag. The probe was only a log line — `v3.0.6.an`
  `send_message` already rejects an empty `dapp_id`. `--an-graphql-url`
  / `AN_GRAPHQL_URL` must now name a host that serves GraphQL *and*
  `/v2/messages`. Rebuild `deposit-relayer` from this tree before
  `daemon` / `finalize-one`.
- **No block the light client admitted could finalize a deposit.**
  `EthBeaconLightClient` sent `USDCBridge` its stored anchor key — the block
  hash with each 16-byte half byte-reversed, `(LE(h[0..16]) << 128) |
  LE(h[16..32])` — while `finalizeDeposit` looks the block hash up in
  Ethereum byte order, the way the deposit proof carries it. Every
  light-client anchor missed, and once `disableOwnerAnchors` leaves the light
  client as the only writer, every `finalizeDeposit` fails with
  `ERR_UNKNOWN_BLOCK` (224). `acceptBlockHashFromLightClient` and
  `forgetBlockHashFromLightClient` now carry the Ethereum-order hash.
  Everything else keeps the stored key: `rePushAnchor` takes it, `getHead`,
  `HeadUpdated`, `CheckpointBackfilled` and `AncestryAccepted` report it, and
  `isProvenExecutionBlockHash` and the light client's own
  `isAcceptedBlockHash` take it. So the light client's `isAcceptedBlockHash`
  is not interchangeable with the bridge's: for the same block the bridge is
  asked the Ethereum-order hash. The ABI is unchanged.

  `contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.tvc` is rebuilt
  and reports version `1.4.1`; its code hash moves from `78905cf7…9ed532` to
  `314ac6b8…6092f5`. A network takes it with a fresh zerostate, every
  contract deployed from scratch, once acki-nacki's pin is moved. The
  standalone `contracts/an/EthBeaconLightClient.sol` has the same fix and
  reports version `0.1.1`; a light client deployed from it has to be deployed
  again from this source.

  A light client already deployed from the 1.4.0 `.tvc` is upgraded in place:
  its owner calls `updateCode` (present since 1.4.0) with the rebuilt code.
  The address, head, committee and proven set stay, and the bridge keeps
  accepting it as the writer. Anchors it pushed before the upgrade do not
  count; the daemon re-sends the checkpoint it proves next, and an older
  block still needed by a deposit gets `rePushAnchor` with its stored key
  (or, while owner anchors are enabled, `setAcceptedBlockHash` with the
  Ethereum-order hash). The words pushed before the upgrade stay in the
  bridge's anchor set; they match no block, so they admit nothing.
  `EthBeaconLightClient_encoding_and_gas_notes.patch` is regenerated against
  this copy: its `rePushAnchor` note is now in the source, the two remaining
  hunks still apply, and the code hash stays `314ac6b8…6092f5` with them
  applied.
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
- `applyBkSetUpdate` attestation `lastSeen` is the prove-time cursor
  (`attestationLastSeen`, strictly less than `blockSeqNo`). Passing the
  live cursor after `verifyBlock(N)` reverts
  `AttestationLastSeenNotBeforeSeqNo`. See Breaking.
- After `applyBkSetUpdate` the relayer pins the follow-up slot reads to
  the receipt block, same as `verifyBlock`. An unpinned `latest` on a
  load-balanced RPC could record a torn snapshot and halt the next tick
  as chain drift. Check B also rejects a `last_bk` rewind, a jump past
  `MAX_FORWARD_GAP`, and a second rotation while the previous N is
  still ahead of `last_seen`.
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
- Spec §7.3: `applyBkSetUpdate` attestation `lastSeen` is the prove-time
  cursor. `storedLastBkSetUpdateSeqNo` selects the set and gates the
  next rotation.

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
  `78905cf7…9ed532` to `812f2b9f…da6dab`. Ancestry's residual limit: see Known
  issues.

- `submitAncestry` and `rePushAnchor` were rejected by the light client
  (compute phase, exit 252) because two byte orders were in play. The step
  circuit splits a hash with `node_hi_lo` — each 16-byte half read
  little-endian — so `submitUpdate` keys an anchor as
  `(LE(h[0..16]) << 128) | LE(h[16..32])`. Keccak in the VM returns
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
- **`scripts/production_preflight.sh` runs its relayer step through the
  prover workspace.** The step ran `cargo test` inside
  `crates/bridge-relayer-daemon`, where cargo cannot read the manifest on its
  own, so the script aborted there after the Foundry gates and never printed
  `RESULT:`. `scripts/shellnet_e2e.sh` stopped at the same point, because it
  starts with the preflight. The step now runs the relayer tests from
  `crates/bridge-prover-libraries`, the way `make relayer-test` does. Fixed the
  same way: `make relayer-fmt`, which `make pre-push` runs, and the
  `verify-fixture` step of `scripts/shellnet_e2e.sh`, which could not start the
  relayer and reported every run as
  `verify-fixture failed (deploy bridge first or check anchors)`. That step also
  passes its fixtures directory as an absolute path now: from a relative one the
  relayer cannot find the R15 calldata in `contracts/ethereum/verifiers/` and
  looks for legacy Groth16 fixtures instead. The preflight step still fails for
  as long as that workspace does not build against its pinned circuit revision.
- **`make deploy-local` runs `script/DeployTestBridge.s.sol`**, the local and
  testnet smoke-test deployment; the `script/Deploy.s.sol` it named does not
  exist. It reads `PRIVATE_KEY` from the environment or from
  `contracts/ethereum/.env`. `make dev-setup` now creates that file from
  `contracts/ethereum/.env.example` when it is missing, instead of silently
  failing to copy a root `.env.example` that does not exist. `make audit` runs
  `cargo audit` only; the `forge audit` it also called is not a Foundry command.
- **`make setup` (`setup.sh`) no longer runs `forge init --force` in
  `contracts/ethereum`.** On a fresh clone, where the gitignored `lib/` is
  absent, it did, and left Foundry's template `src/Counter.sol`,
  `script/Counter.s.sol`, `test/Counter.t.sol` and a `README.md` in the
  project, where they were built and tested with it. It now installs the
  Solidity dependencies the way CI does — `npm install` and `forge-std` — and
  stops with an error when `npm` is missing, since `poseidon-solidity` comes
  from npm. It also installs the pinned Rust toolchains from
  `rust-toolchain.toml` and `deposit-prover/rust-toolchain.toml` instead of
  switching the global default to the latest nightly, keeps an existing
  `.git/hooks/pre-commit`, and no longer creates empty crate directories, a
  `test/integration/` directory or a root `.env.example`; the template for
  the deploy scripts is `contracts/ethereum/.env.example`.
- **`deposit-prover/download_trusted_setup.sh` no longer calls the Hermez SRS
  a test-only fallback.** It told operators that production deposit proofs need
  an Acki Nacki chain-ceremony SRS in `params/kzg_bn254_18.srs` and that proofs
  keyed on Hermez are rejected on chain. The reverse is true: the
  `ZKHALO2VERIFYWITHVK` opcode embeds the Hermez `[s]·G2`, and the deposit
  prover loads only the Hermez `data/kzg_params_18.srs` this script downloads.
  The script now checks that file's `[s]·G2` and refuses one from any other
  ceremony. A failed download is reported as such instead of as a corrupted
  file — the default source currently answers HTTP 403.

### Known issues

- **The FIFO compaction walk is not measured against a full proven set.**
  `_evictExpired` re-queues every still-live hash it examines, which advances
  the queue head, so the loop runs its full examination cap on every
  `submitUpdate` and `submitAncestry` whether anything has expired or not.
  With ancestry on, the proven set grows from one hash per epoch to one per
  execution block — roughly 2.6M entries over a year instead of ~82K — and
  each examination is a dictionary read, delete and write whose cost grows
  with that depth. The numbers in Breaking Changes were measured on an
  essentially empty map. Before mainnet this wants measuring against a
  populated one, together with the eviction lag it implies: the bridge forgets
  a hash only when the tail passes it, so `finalizeDeposit` can still accept a
  deposit whose block is older than a year by as much as one full rotation of
  the queue. The year in "deposits older than a year cannot `finalizeDeposit`"
  is the light client's own `_isLive`, not the bridge's map.

- **A genuine committee hand-off can be submitted early and freeze the head.**
  Permissionless, repeatable once per period, and present in the deployed
  1.4.1; see the `submitRotate` entry in Breaking Changes. The fix belongs in
  the rotate circuit, which has to bind the hand-off to a slot.

- **A deposit is only covered once both checkpoints bracketing its block are
  proven and within a span of each other.** Gas is no longer the obstacle —
  the `KECCAK256` instruction brought a full 33-header span to 1,976,017 gas
  against the 10M per-transaction limit (see Breaking Changes) — and a
  checkpoint the daemon skipped can be late-registered through `submitUpdate`,
  which closes the gap without the owner key — but the daemon never does it by
  itself, so today those blocks need `setAcceptedBlockHash` unless an operator
  late-registers the checkpoint by hand. What is left is ordering: until the
  skipped checkpoint is late-registered and the span sent, a deposit in those
  blocks fails `finalizeDeposit` with `ERR_UNKNOWN_BLOCK` (224), and on a
  bridge with `disableOwnerAnchors` already called there is no owner fallback
  to bridge the wait. `submitRotate` and `rePushAnchor` are unaffected; so are
  withdrawals. The earlier software-keccak measurements that made ancestry
  impossible outright (one permutation 12.93M gas, a real 642-byte Sepolia
  header 64.68M, a 32-header walk ≈2.07G) are kept here for the record:
  shellnet 2026-09-11, re-measured in tvm-debugger 3.0.6 on 2026-09-13 and on
  a real VM 2026-09-14.

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
  hash verified unchanged at `78905cf7…9ed532`, then at `314ac6b8…6092f5` after
  the QC-AN-13 rebuild) — and the gate was rewritten to
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

- **`solc 0.8.19` is no longer a prerequisite of `ackinacki-bridge withdraw`
  or of the relayer.** Stage 1 no longer probes the compiler or its version.
  `scripts/install.sh` no longer downloads it and `BRIDGE_SOLC_URL` is gone;
  the closing `export PATH` line is still printed, but only for the CLI. The
  relayer image no longer contains `/opt/gosh-relayer/bin/solc`, the
  `solc_bin` build context and `SOLC_BIN_DIR` are gone from
  `deploy/shellnet-l2/compose.yaml` and `compose.env.example` — delete
  `SOLC_BIN_DIR` from your `.env` — and the image preflight no longer checks
  the compiler version. An installed `solc` can stay; nothing on these paths
  uses it. Regenerating verifiers (`export-inner-aggregator`,
  `export-spike-artifacts`) still needs `solc 0.8.19`.
- `make generate-proof`, `make test-integration` and `make generate-verifier`,
  with `scripts/regenerate_verifier.sh`. They ran a `generate-proof` binary,
  an `eth-frontend` integration test and a `generate-verifier` binary, none of
  which exists, so each failed on every run. Production verifiers are
  regenerated with `export-inner-aggregator` in `crates/bridge-evm-aggregator`.
- `test_poseidon.sh` and `contracts/ethereum/test/generate_test_proof.sh`,
  which ran the same missing `generate-proof` binary.
- The `cargo run` fallbacks of `eth-lc-relayer` (`export_step_vk_blob`,
  `rotate_tree_n8`), `deposit-relayer` (`fetch_deposit_data`,
  `export_vk_blob`, `export_blake2b_proof`) and `relayer` (`aggregate-proof`,
  `bridge-event-halo2-prover`), and with them the `--aggregator-dir` /
  `--bridge-prover-libraries-dir` flags of `relayer` and their
  `BRIDGE_AGGREGATOR_DIR`, `AGGREGATOR_DIR` and `BRIDGE_PROVER_LIBRARIES_DIR`.
  See Breaking Changes.

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
