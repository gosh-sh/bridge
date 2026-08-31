# eth-light-client-prover

Trustless Ethereum beacon-chain **light client** for the ETH→AN deposit path.
Produces a BN254 Halo2 SHPLONK proof (verified natively on Acki Nacki by
`ZKHALO2VERIFYWITHVK`) that advances a sync-committee light-client head and marks
finalized Ethereum **execution block hashes** canonical, so `finalizeDeposit`
accepts a deposit only if its proven `blockHash` was finalized in-circuit.

Removes the trusted-attester MVP (`_acceptedBlockHash` / `setAcceptedL1Block`).

- Full work plan: [`../docs/eth_light_client_trustless_deposit_plan.md`](../docs/eth_light_client_trustless_deposit_plan.md)
- **M0 spec:** [`docs/m0_spec.md`](docs/m0_spec.md) — frozen public-input layout,
  gindex/fork table, constants, weak-subjectivity anchor policy.
- **M1 notes:** [`docs/m1_notes.md`](docs/m1_notes.md) — BLS-core
  reuse decision, API map, Ethereum deltas (DST/compressed-sig/threshold), subgroup gap.
- **M2 notes:** [`docs/m2_notes.md`](docs/m2_notes.md) — SSZ
  merkleization / `signing_root` / Merkle-branch primitives, gindex validated on
  real data, the 512-committee rotate/step cost decision, M2→M3 seams.
- **M3 notes (this milestone):** [`docs/m3_notes.md`](docs/m3_notes.md) — the
  step circuit (BLS + SSZ + finality in one) + the in-circuit G2 subgroup check
  (closes BLS-1/FORK-2) + M3-exec execution `block_hash` binding
  (`htr(ExecutionPayloadHeader)` + `execution_branch`); real-data MockProver; rotate seam.
- **M4 real proof / SRS:** [`docs/m4_real_proof_and_srs.md`](docs/m4_real_proof_and_srs.md) —
  fused step at k=19, real keygen/prove/verify, column-width shape probe.
- **M5 VkBlob:** [`docs/m5_vkblob.md`](docs/m5_vkblob.md) — production Base-v1
  `VkBlob` keyed on **Hermez** (the ceremony the opcode embeds), emitted + verified
  through the exact `ZKHALO2VERIFYWITHVK` read+SHPLONK path (`fixtures/step_vkblob/`),
  plus the end-to-end `tvm_vm` opcode-acceptance test.
- **M5 AN contract:** [`docs/m5_eth_beacon_light_client.md`](docs/m5_eth_beacon_light_client.md)
  — `EthBeaconLightClient.sol` (verifier + head + canonical-anchor push into the
  deposit path); the on-AN consumer of the step VkBlob.
- **M5 rotate join:** [`docs/m5_rotate_join.md`](docs/m5_rotate_join.md) —
  `submitRotate` + the one-way `disableOwnerRotation()` that make the committee
  chain trustless from the checkpoint; R2 trust model, PI layout, and the
  rotate-VkBlob hardware blocker (k≈26 SSZ committee root > 125 GB).
- **M6 recursive rotate:** [`docs/m6_rotate_recursive.md`](docs/m6_rotate_recursive.md)
  — how the committee SHA root is split into fitting shard proofs + a cheap
  snark-verifier aggregation (exact balanced-tree decomposition, 2-level Poseidon
  commitment, R2 BLS-as-recursion-input, k/memory budget); brick #1 (sharding),
  brick #2 (N=8 aggregation shell fits n14 @ k=23), and brick #3 (gosh→snark-verifier
  Poseidon transcript bridge — real Hermez-k20 shard proof, self-verified) are green.
- Fixtures: [`fixtures/`](fixtures/) (real `LightClientUpdate` / `finality_update`)
  captured via [`scripts/fetch_lc_fixtures.sh`](scripts/fetch_lc_fixtures.sh).

## Status

| Milestone | State |
|---|---|
| M0 — spec & fixtures | done |
| M1 — BLS core (committee select + 2/3 + G1 aggregate + pairing + hash-to-curve) | **green** (n14 MockProver: 512-signer aggregate @ k=22, ~66 s) |
| M2 — SSZ + Merkle branches (finality / execution / next-committee) | **green** (n14 MockProver: header htr / signing_root / real finality branch @ k≤20, 4 tests 36.7 s) |
| G2 subgroup hardening (BLS-1 / FORK-2) | **green** (ψ-endomorphism check; n14 MockProver accepts valid / rejects non-subgroup @ k=18, 1.6 s) |
| M3 — step circuit (BLS+SSZ+finality, subgroup-bound) | **green** (base circuit was k=22/110 s; the `step_mock_prover` test now measures the **fused** circuit — see M4-fusion row) |
| M3-rotate — committee↔anchor (Poseidon commit + SSZ rotate proof) | **anchor + commitment green** (native `htr(SyncCommittee)`+`next_sync_committee_branch` @ gindex 87 reconstruct live `state_root`; in-circuit Poseidon commitment == native `pse_poseidon` @ k20; full-512 in-circuit SSZ root is memory-bound >125 GB — see m3_notes). Step-side decode-bind + PI equality → M4 fusion |
| M3-exec — execution `block_hash` binding (`execution_branch`) | **green** (n14 MockProver: `htr(ExecutionPayloadHeader)` + `execution_branch` @ gindex 25 reconstruct real finalized+attested `body_root`, block_hash bound @ k=20, 78 s; off-circuit twin validated on live data) |
| M4-fusion brick #1 — compressed-G1 decode-bind (bytes↔point) | **green** (n14 MockProver: binds 4 real pubkeys, rejects flipped-x / negated-y @ k=18, 0.5 s; lexicographic sign `y>(p−1)/2` validated on 512 live pubkeys) |
| M4-fusion — step folds decode-bind ×512 + Poseidon commitment + exec block_hash into one proof | **green** (n14 MockProver: real headers + real finality/execution branches + valid 512-committee, one `verify_step` @ k=23, ~183 s, ~35.5 GB; exec branch reuses the finality `body_root` cell) |
| M4 — PI instance wiring (8 public inputs on a real instance column) | **green** (`pack_step_instances` → `STEP_INSTANCE_LEN=8`; fast `step_instances_layout_and_binding` pins order + hi/lo endianness + per-position binding @ k=12, 8 tamper-rejected; full circuit exposes them @ k=23) |
| M4 — real SHPLONK proof + SRS fit | **green** (n14: fused step configures at **k=19 / 132 advice cols / lookup_bits=18**, real keygen+prove+verify: VK 17 KB, proof 40 KB, verify 20 ms — see `docs/m4_real_proof_and_srs.md`. NB: the opcode is keyed on **Hermez** (K≤28), so there is no ceremony ceiling — see M5) |
| M5 — production VkBlob emit (Hermez SRS) + opcode-faithful round-trip | **green** (n14: real Base-v1 `VkBlob` 17 573 B keyed on Hermez k=19 (tau-preserving downsize of Hermez k=20); reparsed + verified through the **exact** `ZKHALO2VERIFYWITHVK` Base read + SHPLONK path against Hermez `verifier_params` — PASS; artifacts in `fixtures/step_vkblob/`, see `docs/m5_vkblob.md`) |
| M5 — tvm-sdk opcode fixture (end-to-end acceptance) | **green** (`tvm-sdk/tvm_vm/halo2_test_data/step_light_client/` + `round_trip_step_light_client_real_proof_returns_true`: real proof → `Ok(true)` through the actual `execute_zkhalo2_verify_with_vk` handler; 4/4 tests pass. `scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh`) |
| M5 — AN contract `EthBeaconLightClient` + deposit anchor wiring | **source landed** (`acki-nacki/contracts/exchange/EthBeaconLightClient.sol` embeds `VK_BLOB`; `submitUpdate` verifies via `gosh.zkhalo2VerifyWithVK`, committee-commitment gate, monotonic head, pushes anchor into `USDCBridge.acceptBlockHashFromLightClient` for `finalizeDeposit`; compile/deploy on operator Mac — see `docs/m5_eth_beacon_light_client.md`) |
| M5 — rotate ↔ step join (trustless committee chain) | **contract wired** (`submitRotate(proof, publicInputs)` = only permissionless writer of `_currentCommittee`; 3 PIs current/next commitment + monotonic period; `disableOwnerRotation()` one-way drops owner off the path). ⛔ `ROTATE_VK_BLOB` emit **hardware-blocked** monolithically (SSZ committee root ~1023 SHA-256, k≈26 > 125 GB) → M6 recursion below. See `docs/m5_rotate_join.md` |
| M6 — recursive rotate brick #1 (committee SHA-root sharding) | **green** (n14 MockProver, 3 tests 246.8 s: `committee_subtree_root` shard @ k20 fits ~127 SHA; cheap recompose top @ k18; sharded root == monolithic `native_sync_committee_root` byte-for-byte incl. **live mainnet** committee). Escapes the 125 GB wall by proving 8×64-pubkey subtrees + a cheap snark-verifier aggregation. See `docs/m6_rotate_recursive.md` |
| M6 — recursive rotate brick #2 (N-way aggregation shell fits) | **green** (n14: `crates/bridge-evm-aggregator/tests/shard_aggregation_probe.rs` — N=8 inner SHPLONK snarks aggregate in one `AggregationCircuit` at **k_outer=23, ~8 min (482 s), RAM well within 125 GB**; ~17.3 M advice cells, 20 outer instances under `VerifierUniversality::Full`). Answers the aggregation-mechanics + outer-k budget decoupled from the gosh→snark-verifier transcript bridge (next brick). See `docs/m6_rotate_recursive.md` § probe |
| M6 — recursive rotate brick #3 (gosh→snark-verifier transcript bridge) | **green** (`src/poseidon_transcript.rs` vendored `PoseidonWrite`/`PoseidonRead`, byte-identical to snark-verifier-sdk; `tests/poseidon_transcript_prove.rs` real `create_proof`+`verify_proof`+tamper @ k8; `examples/export_shard_snark.rs` on **n14: Hermez k=20 shard proof, keygen 203+151 s, Poseidon prove 212 s → 25 920 B, self-verify PASS**, writes `shard_{vk,config,proof,instances}` in the exact `export_poseidon_snark` shape). Unblocks feeding real shard proofs into `bridge-evm-aggregator`. See `docs/m6_rotate_recursive.md` |
| M5 — relayer | **landed** (`crates/eth-light-client-relayer`, `eth-lc-relayer` CLI). Default shadow loop `--dry-run --mock-prove`. Live prove = subprocess `export_step_vk_blob` (`FINALITY_UPDATE_PATH`). Auto-rotate off until tvm-sdk#284. Does **not** flip `finalizeDeposit`. |
| M-audit | not started — scope includes the opcode-side KZG-accumulator decider (tvm-sdk#284), not just the circuit + contract |
| M6 — testnet E2E | not started |

Standalone crate (own `[workspace]` + `[patch]`), excluded from the main Cargo
workspace and CI's `cargo test --workspace`, same as `deposit-prover`. Build/test on n14
(`docs/m1_notes.md` §7).
