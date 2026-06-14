# Acki Nacki Bridge — Agent Context

## Project Overview

Cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/) (TVM-based, multi-threaded blockchain). The bridge enables deposits on Ethereum to be proven on Acki Nacki, and Acki Nacki state (layer hashes) to be verified on Ethereum — both via ZK proofs.

## Git remotes

| Remote | URL | Role |
|--------|-----|------|
| `origin` | `git@vcs.modus-ponens.com:ton/acki-nacki-bridge.git` | **Canonical** — CI, merge target `main` |
| `github` | `git@github.com:gosh-sh/bridge-EVM.git` | GitHub mirror — open PRs here |

```bash
# one-time local setup (if `github` is missing or wrong)
git remote add github git@github.com:gosh-sh/bridge-EVM.git
# or fix an existing remote:
git remote set-url github git@github.com:gosh-sh/bridge-EVM.git

git push origin main
git push github main
```

## Repository Layout

```
acki-nacki-bridge/          ← this repo (Ethereum side + integration)
├── contracts/ethereum/     ← Solidity (Foundry): bridge contract, verifiers, oracles
├── crates/
│   ├── acki-nacki-interface/  ← Rust traits + mock for AN node communication; live `BkSetClient` + stateful `BkSetTracker` against AN-node REST `/v2/bk_set{,_update}`
│   └── eth-frontend/          ← Rust Ethereum client (alloy-rs; migrated 2026-05-17 from ethers-rs)
├── deposit-prover/         ← Rust Halo2 circuit: proves Ethereum deposit events. Halo2 SHPLONK proof is consumed natively on the AN side (no gnark wrapper — retired in Phase 4.3 2026-05-17).
├── crates/bridge-prover-orchestrator/  ← Wraps the partner's 4-circuit pipeline (Halo2 1A/1B/2[/3]) for prover/relayer use
│   └── gnark-wrappers/     ← Go modules per circuit (circuit-1a, circuit-1b, circuit-2[, circuit-3]) producing 256-byte Groth16 proofs (AN→ETH side only; EIP-170 forces gnark wrap on this direction)
├── crates/bridge-relayer-daemon/       ← Phase 5.1 relayer skeleton (AN→ETH direction): Relayer::tick() / run_loop() + BlockSource/BridgeClient traits + abigen!-generated AckiNackiBridge bindings + state.json persistence + CLI
├── crates/deposit-relayer-daemon/      ← EVM→AN deposit relayer (mirror of bridge-relayer-daemon): listen for `Deposit` events (EthLogSource over alloy) → generate the AN-consumable Halo2 proof triple (SubprocessProofGenerator over deposit-prover) → submit to `TokenBridge.finalizeDeposit` (AnSubmitter). `AnConfig` drives the live `BkSetClient` (read-side endpoints wired; `finalizeDeposit` write gated on the upstream `IAckiNacki`/tvm-sdk client). `deposit-relayer` CLI: watch / prove-one / an-preflight / daemon
├── crates/bridge-evm-aggregator/       ← R15 / M2 spike (standalone cargo workspace): snark-verifier-sdk → AggregationCircuit → Yul EVM verifier. ~13 KB bytecode @ K=21, well under EIP-170. Trivial inner circuit (`a*b==c`) until partner ships Circuit 4 (M4)
│
│   The orchestrator's `poseidon_transcript.rs` (M3, 2026-05-27) is the Poseidon Fiat–Shamir transcript that bridges these two cargo trees — it produces proofs in a flavour the aggregator can consume.
├── poseidon-proof/         ← Rust Halo2 circuit with Blake2b transcript (Poseidon commitments)
├── frontend/               ← WASM frontend (excluded from workspace)
├── scripts/                ← Shell scripts for verifier generation, deployment, partner-pack assembly
├── docs/                   ← Architecture docs, audit reports, integration plan
├── e2e_test_data/          ← Test fixtures for end-to-end tests
├── e2e_attack_test_data/   ← Negative test fixtures
├── params/                 ← SRS parameters (KZG trusted setup)
├── Makefile                ← Entry point: make setup/build/test/deploy
├── setup.sh                ← One-time dependency install (Foundry, Go, Rust)
├── test.sh / test_e2e.sh   ← Test runners
└── .gitlab-ci.yml          ← CI pipeline
```

## Sibling Repositories (under ../  relative to this repo)

| Repo | Description |
|------|-------------|
| `layer-hashes-update-halo2-circuit` | Partner's Halo2 circuit proving AN block attestation + layer hash updates |
| `gosh-zk-snark-halo2-utils` | Generic keygen/prove/verify utilities with Blake2b transcript (SHPLONK) |
| `gosh-halo2-crypto-lib` | Crypto chips: SHA-256, BLS12-381 verification, dense balanced Merkle tree |
| `halo2-lib-zkevm-sha256-and-bls12-381` | Gosh fork of axiom-crypto/halo2-lib adding BLS12-381 support to halo2-ecc |
| `acki-nacki` | Acki Nacki node implementation |
| `tvm-sdk` | TVM SDK — opcode `ZKHALO2VERIFYWITHVK`, `tvm_vm` Halo2 verifier. **Shellnet pin:** branch `full_dex_and_bridge_test_with_final_halo2_circuit` (see § Shellnet E2E below). Local path: `../tvm-sdk` |
| `bk-set-stub` | Minimal stub for `bk-set-change-verifier-halo2-circuit-with-better-sha256` (enables halo2-utils build without access to that private repo) |

### `circuit-data-exporter` (on `bridge_halo2_tests` branch of `acki-nacki`)

Lives at `acki-nacki/helpers/circuit_data_exporter` on the **`bridge_halo2_tests`** branch (not on `main`).
Queries a live AN node via GraphQL, fetches a key block, extracts layer hashes, generates a synthetic BLS attestation, and writes a JSON fixture for the circuit tests.

```bash
# Fetch the branch locally
cd ../acki-nacki && git fetch origin bridge_halo2_tests

# Build (needs to be in workspace or built standalone with node deps)
cargo build -p circuit-data-exporter

# Usage
cargo run -p circuit-data-exporter -- \
    --network http://127.0.0.1:8080 \
    --height 32 \
    --bk-set-size 5 \
    --num-prev-chain-steps 1 \
    --output circuit_test_data.json
```

Output: `circuit_test_data_L{layers}_H{height}_prevH{prev}_S{steps}.json` — the same format consumed by `test_real_data.rs` and `test_layer_hashes_d3.rs`.

**Requirements**: `--height` must be a layer-N key block (H % W^N == 0 for N≥1). For `small-window` (W=2): heights 2, 4, 8, 16, 32, …

### Acki Nacki Testnet

- **Node API**: `http://94.156.178.19:8600` (port 8600; HTTPS/443 is firewalled)
- **Working endpoints**: `/v2/bk_set`, `/v2/bk_set_update` (no auth required)
- **GraphQL**: NOT publicly exposed (gql-server is a separate binary; would need local setup)
- **Testnet status**: Not ready for E2E testing (as of Apr 2026)
- **Local 5-node cluster**: `cd ../acki-nacki/nock && docker-compose build && docker-compose up -d`
  - Node0 API: `http://127.0.0.1:11000`
  - Requires building with `history_proofs` feature for layer hash data

## ZK Proof Pipeline

### Deposit Flow (Ethereum → Acki Nacki)

1. User calls `AckiNackiBridge.deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)` on Ethereum → emits `Deposit(depositId, sender, amount, anWorkchain, anAccount, timestamp)`. The AN destination is supplied explicitly because an EVM address is not a valid AN recipient (different address systems); `anAccount==0` reverts (`InvalidAnAccount`). Event sig `keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")` = `0x8d5d0606…3d37ee`.
2. `deposit-prover` (Halo2, K=20) proves the event was emitted: Receipt RLP, MPT inclusion, log matching, block hash binding.
3. Keccak coprocessor handles SHA3/keccak256 via Poseidon promise commitments (~500× savings).
4. **AN side verifies the Halo2 SHPLONK proof natively** via the `ZKHALO2VERIFYWITHVK` TVM opcode (LANDED 2026-05-22 on `tvm-sdk` via PR #240, dispatch byte `0xC7 0x4A`; integration reference: `docs/zkhalo2verifywithvk_reference.md`; design memo: `docs/zk_halo2_an_side_design.md`). Partner's parallel `ZKHALO2VERIFY` (hard-coded DarkDex VK, dispatch byte `0xC7 0x49`) lives on the same `tvm-sdk` branch `serhii/node-3406-vergrth16-with-vk`; our WithVK sibling adds caller-supplied VK so per-deployment bridge circuits can be verified. 10 public inputs: `[depositId, sender, amount, contractAddress, anWorkchain, anAccountHigh, anAccountLow, blockHashHigh, blockHashLow, promiseCommit]`. **AN-recipient binding (LANDED 2026-06-02)**: `deposit` carries `int8 anWorkchain` + `bytes32 anAccount` (the AN destination — an EVM address can't be an AN recipient; `anAccount==0` reverts). The destination is **bound inside the proof**: `deposit-prover/src/circuit_v2.rs` parses the two new `Deposit` data words (word 1 = sign-extended workchain, word 2 = account) in Phase 1 and `constrain_equal`s them to the Phase-0 public instances `anWorkchain` (full 32-byte word) + `anAccountHigh`/`anAccountLow` (the account's two 16-byte halves), bumping `num_instance()` 7→10. The `ZKHALO2VERIFYWITHVK` opcode is VK-driven (it reads the instance count from the VkBlob, so 10 inputs verify with no opcode change); `TokenBridge.finalizeDeposit` builds the public-inputs cell from the same 10 scalars and reconstructs the recipient as `anWorkchain:(anAccountHigh<<128 | anAccountLow)`. The `deposit-relayer-daemon` decodes 10 inputs (`DepositPublicInputs`), `check_binds_to` verifies the proven account matches the on-chain event, and `encode_finalize_deposit` forwards the 10 scalars + proof (no out-of-circuit destination side-channel). **No on-chain ETH-side verifier**: the legacy `IAckiNackiVerifier` / `Groth16{Verifier,DepositVerifier}` chain + the deposit-prover's `gnark-wrapper/` were retired in Phase 4.3 (2026-05-17) — see Decision Log.

### Layer Hash Flow (Acki Nacki → Ethereum)

1. AN blocks contain layer hashes in BTreeMap data structure (bincode-serialized)
2. Block Keepers (BK) sign attestations using BLS12-381 aggregate signatures
3. `layer-hashes-update-halo2-circuit` (Halo2, K=19, BN254 Fr) proves:
   - SHA-256 binding of block data to attestation envelope hash
   - Primary target type constraint
   - Layer hash extraction at correct BTreeMap offsets
   - Poseidon dense balanced Merkle tree chain continuity
   - BLS12-381 committee signature verification (≥2/3 threshold)
   - BK set Poseidon commitment
4. The legacy single-circuit 13-input pipeline (`layer-hashes-update-halo2-circuit` with `gnark-wrapper/`) was retired by Phase 4.2 (2026-05-10) in favour of the four-circuit architecture below.
5. **v2 (current)**: per-circuit Halo2 + gnark Groth16 wrappers (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/`) — public inputs split into Circuit 1A/1B (4 inputs: `[block_id, bk_set_poseidon, block_seq_no, last_seen]`) and Circuit 2 (14 inputs: `[block_id, bk_set_poseidon, num_layers, layer_hash[0..10], prev_max_level_layer_hash]`). Consumed on-chain by `PrimaryVerifier.sol`/`FallbackVerifier.sol`/`LayerHashesMovementVerifier.sol` → `*Groth16VerifierGenerated.sol`.

## Solidity Contracts (Foundry)

| Contract | Purpose |
|----------|---------|
| `AckiNackiBridge.sol` | Main bridge: `deposit()`, **AAVE V3 yield integration**, AN→ETH state via `verifyBlock(finType, 1A-or-1B proof, Circuit-2 proof, …)` enforcing cross-circuit `block_id`/`bk_set_poseidon` agreement + monotonic `block_seq_no` + Poseidon chain anchor (Phase 4.1). AN→ETH payout via `withdrawByProof(proof, pub)` against Circuit 4 (single-final-root layout, partner branch `circuit4-single-final-root`) — verifies a 10-PI proof, checks `pub.finalRoot ∈ _knownAnchors` (set populated by `verifyBlock`), enforces `dstChainId == block.chainid`, identity match, 80-bit recipient halves, `tokenId == 0` (native ETH), nullifier replay protection, then pays out reconstructed recipient. Legacy refund-style `withdraw()` + `IAckiNackiVerifier` chain retired in Phase 4.3 (2026-05-17); the Phase A `verifyEvent` + `_layerWindow[100]` scaffold and Phase B 110-PI withdrawal were retired in Phase 4.4 (2026-05-26) once the partner shipped the unified single-final-root v3 circuit. |
| `IBlockHeaderOracle.sol` | Interface for Ethereum block hash oracle |
| `AxiomBlockHeaderOracle.sol` | Axiom-based block hash oracle implementation |
| `Halo2Verifier.sol` | Direct Halo2 SHPLONK verifier (Yul-based, for testing) |
| `Blake2bHalo2Verifier.sol` | Blake2b-transcript Halo2 verifier |
| `Blake2bTranscript.sol` / `Blake2bChallengeComputer.sol` | On-chain Blake2b transcript replay |
| `MockBlockHeaderOracle.sol` | Mock oracle for testing |
| `IPrimaryVerifier.sol` / `PrimaryVerifier.sol` | Bridge-side adapter for Circuit 1A (Primary attestation) — 4 public inputs `[blockId, bkSetCommitment, blockSeqNo, lastSeenBlockSeqNo]` (renamed from `envelopeHash → blockId` 2026-05-10 per partner offset shift) |
| `IPrimaryGroth16Verifier.sol` / `PrimaryGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier interface + impl |
| `IFallbackVerifier.sol` / `FallbackVerifier.sol` | Bridge-side adapter for Circuit 1B (Fallback attestation) — same 4-input shape as 1A |
| `IFallbackGroth16Verifier.sol` / `FallbackGroth16VerifierGenerated.sol` | Gnark-generated 4-input Groth16 verifier for Fallback (separate VK from 1A) |
| `ILayerHashesMovementVerifier.sol` / `LayerHashesMovementVerifier.sol` | Bridge-side adapter for Circuit 2 (Layer Hashes Movement) — 14 public inputs `[blockId, bkSetCommitment, numLayers, layerHashes[0..10], prevMaxLevelLayerHash]` |
| `ILayerHashesGroth16Verifier.sol` / `LayerHashesGroth16VerifierGenerated.sol` | Gnark-generated 14-input Groth16 verifier for Circuit 2 |
| `IBridgeWithdrawalVerifier.sol` / `BridgeWithdrawalVerifier.sol` | Bridge-side adapter for **Circuit 4 (Bridge Withdrawal, single-final-root)** — 10 public inputs `[tokenId, amount, recipientHi, recipientLo, dstChainId, senderAccFr, dappFr, accFr, nullifier, finalRoot]`. Used by `AckiNackiBridge.withdrawByProof()` (the AN→ETH payout path). Mock-tested only; real on-chain verifier (`BridgeWithdrawalAggregatorVerifier.sol`, Yul-based) pending **R15 milestones M3–M7** (snark-verifier aggregator path; M1 + M2 closed 2026-05-26 + 2026-05-27 — see `docs/r15_snark_verifier_roadmap.md` and `crates/bridge-evm-aggregator/README.md`). Layout per partner branch `circuit4-single-final-root` (`bridge_event_prove_circuit.rs` `PUB_*` constants). The `finalRoot` is verified off-circuit against the bridge's `_knownAnchors` set (populated on every `verifyBlock`); the v1/v2 `layerHashes[100]` private-index design is retired. |
| `IBridgeWithdrawalGroth16Verifier.sol` | Interface for the future gnark-generated 10-input Groth16 verifier (Circuit 4 single-final-root) |
| `IAavePool.sol` | Minimal AAVE V3 Pool interface (`supply` / `withdraw` / `getReserveData`) |
| `IWrappedTokenGatewayV3.sol` | AAVE V3 ETH⇄WETH gateway interface (`depositETH` / `withdrawETH`) |
| `IERC20.sol` | Trimmed ERC-20 interface for aWETH custody |

Test mocks (under `test/mocks/`): `MockAave.sol` (`MockAWETH`, `MockAavePool`, `MockWETHGateway`) for the AAVE path without forking mainnet; `MockPrimaryVerifier.sol` / `MockFallbackVerifier.sol` / `MockLayerHashesMovementVerifier.sol` for driving `AckiNackiBridge.verifyBlock` through many synthetic blocks without re-running ZK proof generation (real verifiers covered end-to-end by `AckiNackiBridgeVerifyBlock.t.sol`); `MockBridgeWithdrawalVerifier.sol` for `withdrawByProof` tests with optional **strict-pub mode** (pins expected `WithdrawalPublicInputs` byte-for-byte so a passing test is itself proof the bridge forwarded the right bytes).

Build: `cd contracts/ethereum && forge build`
Test: `cd contracts/ethereum && forge test`

## Rust Workspace

**Workspace members** (in `Cargo.toml`): `crates/eth-frontend`, `crates/acki-nacki-interface`
**Excluded** (separate dependency trees): `deposit-prover`, `frontend`, `poseidon-proof`, `layer-hashes-prover`, `crates/bridge-prover-orchestrator`, `crates/bridge-relayer-daemon`, `crates/deposit-relayer-daemon`

- `acki-nacki-interface`: Async traits (`IAckiNacki`, `TransactionSender`) + mock implementations, plus a **live REST client** `BkSetClient` against the AN node's `/v2/bk_set` and `/v2/bk_set_update` endpoints (probed working against `http://94.156.178.19:8600` on 2026-05-18). Returns typed `BkSetResponse` / `BkSetUpdateResponse` and a `signer_index → 48-byte BLS pubkey` map ready for `bridge-prover-orchestrator::generate_fallback_proof`. The crate also ships a stateful `BkSetTracker` that polls `/v2/bk_set_update`, caches the last snapshot, and surfaces structured `BkSetChange` events (`FirstObservation` / `Unchanged` / `MembershipChanged { added, removed, pubkey_mutations }`) — the primitive the relayer will use in Phase 5.2 to decide when a Circuit 3 rotation proof is needed. Live tests are `#[ignore]`-gated (`cargo test -p acki-nacki-interface --test live_bk_set -- --ignored`).
- `eth-frontend`: Ethereum client using alloy-rs (migrated 2026-05-17 from ethers-rs). Interacts with bridge contracts.
- `bridge-prover-orchestrator`: Phase 1.A/1.B prover wiring — wraps the partner's halo2 Circuit 1A/1B/2 with `KeyManager`/`generate_*_proof`/`verify_*_proof` helpers, plus `bound_test_data` for cross-circuit-bound test scenarios and `export-bound-block-proofs` binary used by Phase 4 fixtures. Since R15/M3 (2026-05-27) also exports `poseidon_transcript::{PoseidonRead, PoseidonWrite}` and `generate_fallback_proof_with_transcript(.., TranscriptKind::{Blake2b, Poseidon})` — Blake2b stays the AN-side default for `ZKHALO2VERIFYWITHVK`; Poseidon is the ETH-side inner-SNARK flavour the `crates/bridge-evm-aggregator/` aggregator consumes.
- `bridge-relayer-daemon`: Phase 5.1 relayer skeleton — `Relayer::tick()`/`run_loop()` with `BlockSource` + `BridgeClient` traits (`EthBridgeClient` over `abigen!`-bindings; `MockBridgeClient`/`InMemoryBlockSource`/`FixturesBlockSource` for tests), atomic `state.json` persistence, `relayer` CLI binary. 13 unit tests cover the loop, state machine, restart-from-anchor recovery.
- `deposit-relayer-daemon`: **EVM→AN deposit relayer** (mirror of `bridge-relayer-daemon` for the deposit direction). Three trait seams keep the loop testable and let the heavy / not-yet-built pieces swap independently: `source` (`DepositSource` async trait + `EthLogSource` — alloy `eth_getLogs` with 10-block chunking + 429 retry/backoff, confirmation-gated, `BRIDGE_DEPLOY_BLOCK` env when `--from-block 0` — + `fetch_deposit_from_receipt` fast path for known tx hashes + `InMemoryDepositSource` for tests), `prover` (`ProofGenerator` async trait + `SubprocessProofGenerator` invoking `deposit-prover`'s `fetch_deposit_data`→`export_vk_blob`→`export_blake2b_proof` examples out-of-process + `MockProofGenerator`), `submitter` (`AnSubmitter` async trait + `AnInterfaceSubmitter` over `acki_nacki_interface::IAckiNacki` with interim `encode_finalize_deposit` + `MockAnSubmitter` mirroring the `usedDepositIds` nullifier), plus `relayer` (`Relayer::tick()`/`run_loop()`), `daemon` (`BackoffConfig`/`RelayerMetrics`/`run_until_shutdown` with SIGINT/SIGTERM), `state` (atomic `state.json`, `depositId` cursor), and `an_config` (`AnConfig` — holds AN `node_url`/`token_bridge`/`sender`, builds the live `BkSetClient`, and `preflight()`s `GET /v2/bk_set`). CLI binary `deposit-relayer` with `watch` / `prove-one` / `an-preflight` / `daemon`. **Production discovery** (`daemon`, `prove-one` without `--tx-hash`): `eth_getLogs` filtered by `depositId` → block-global `logIndex` mapped to receipt-local index via `receipt_log_index_from_block_log` (fixture-tested). **Operator fast path** (`prove-one --tx-hash --log-index`): skips `eth_getLogs` when the deposit tx is already known — does **not** substitute for production-path sign-off. **32 lib tests** + **1 fixture integration test** (`tests/log_index_mapping.rs`, Sepolia `depositId=0`) + **2 `#[ignore]` live tests** (`live_an_preflight`, `live_log_discovery`). Env template: `scripts/ursus/deposit-relayer.env.example` (`BRIDGE_DEPLOY_BLOCK=11025180` for shellnet Sepolia bridge). Read-side AN endpoints are wired/verifiable today (incl. against the local cluster `http://127.0.0.1:11000`); the `finalizeDeposit` write stays gated on shellnet `USDCBridge` VkBlob redeploy + partner node rebuild.
- `deposit-prover`: Standalone Halo2 circuit crate. Uses axiom-crypto's halo2-lib (different from partner's gosh fork).
- `poseidon-proof`: Halo2 circuit with Blake2b transcript for Poseidon commitment proofs.

## Partner's Circuit Details

### `layer-hashes-update-halo2-circuit`

**Build**: `cargo build --features small-window` (uses LAYER_TREE_DEPTH=2 for testing)
**Test**: `cargo test --features small-window -- "mock"` for mock prover; real prover takes ~26 min

**Key feature flags**: `small-window` (depth=2, window=4 for testing), default is production depth=8

**Known build issue**: `state_to_bytes` in `gosh-sha256-chip` is private but called by the main circuit. Requires `pub` fix.

**Circuit parameters** (small-window): K=19, 80 advice columns, 2 lookup advice columns, lookup_bits=18

**Test fixtures**: `circuit/tests/fixtures/circuit_test_data_*.json` — real AN node data

### `gosh-halo2-crypto-lib`

Three crates, all using the Gosh fork of halo2-lib:
- **`sha256-chip`**: Standard SHA-256 with range-checked mod-2^32 arithmetic. Correct.
- **`bls-verification`**: BLS12-381 aggregate signature verification. Threshold: Primary (3s≥2n), Fallback (2s>n). Uses `load_private_g*_unchecked` + manual `check_is_on_curve`. No G2 subgroup check.
- **`dense-balanced-tree`**: Poseidon Merkle chain verification. MAX_CHAIN_LEN=11. Uses 3-chunk encoding (c0, c1, c2) with range checks.

### `halo2-lib-zkevm-sha256-and-bls12-381`

Gosh fork of axiom-crypto/halo2-lib. Single commit. Adds `bls12_381` module to halo2-ecc. Core halo2-base is unchanged. Pairing equation: `e(-g1, sig) · e(pk, H(m)) = 1`. Hash-to-curve follows RFC 9380.

### `gosh-zk-snark-halo2-utils`

- `keygen.rs`: `generate_and_save_keys` — runs keygen_vk/keygen_pk, saves VK + PK + config
- `proof.rs`: `Proof::create_for_circuit` / `verify_with_vk` — Blake2bWrite + ProverSHPLONK
- `io.rs`: Universal PK/VK deserialization using BaseCircuitBuilder

**Test flow for layer-hashes (depth=3, small-window)**:
1. `test_layer_hashes_keygen_d3` — generates PK (~5.3GB), VK (11KB), config in `keys/` (~11 min)
2. `test_layer_hashes_prove_and_verify_all_fixtures_d3` — proves all 4 fixtures with same key, verifies each (~22 min total)
3. Keys are reusable for any fixture from a W=4 node with blocks ≤ 4096 bytes

**Patching**: `Cargo.toml` needs `[patch]` sections pointing to local sibling repos (halo2-lib, layer-hashes-circuit, crypto-lib, bk-set-stub) to build without network access to private GitHub repos.

## Key Technical Concepts

### Proof Systems
- **Halo2** (KZG + SHPLONK): Used by both deposit-prover and layer-hashes circuit. BN254 scalar field.
- **Groth16** (BN254): Final on-chain verifier format. Wrapped from Halo2 via gnark.
- **Blake2b transcript**: Fiat-Shamir transcript used by partner's circuits (vs Poseidon/Keccak in axiom's).

### Crypto Primitives
- **Keccak256**: Ethereum hashing. Offloaded to coprocessor circuit in deposit-prover.
- **SHA-256**: Used in partner's circuit for block data hashing.
- **Poseidon**: In-circuit-friendly hash. Used for BK set commitment, dense Merkle tree, keccak coprocessor promises.
- **BLS12-381**: Acki Nacki Block Keeper signatures. Aggregate signature scheme.
- **Dense Balanced Tree**: Acki Nacki's Merkle tree variant using 3-chunk Poseidon hashing.

### Acki Nacki Blockchain
- TVM-based, multi-threaded (up to 512 threads per workchain)
- **Block Keepers (BK)**: Validate and attest blocks with BLS12-381 signatures
- **Block Managers (BM)**: Propose blocks
- **Epochs**: BK set rotates; each epoch has a new committee
- **Layer hashes**: Per-layer state commitments stored in dense balanced Merkle trees
- **Bincode serialization**: AN uses bincode for on-chain data structures. The circuit has hardcoded offsets for parsing.

### Bincode Layout Constants (in the circuit)
These are tightly coupled to the AN node's serialization and will break if the node changes format:
- `ENVELOPE_HASH_REL_OFFSET`, `TARGET_TYPE_REL_OFFSET`
- `BTREE_ENTRY_SIZE`, `BTREE_KEY_SIZE`, `BTREE_VALUE_SIZE`
- `MAX_BLOCK_DATA_BYTES = 4096`, `MAX_PADDED_BYTES = 4160`

## Open Audit Findings (Medium Severity)

1. **BLS-1 / FORK-2**: No G2 subgroup check in BLS12-381 path. `load_private_g2_unchecked` skips both on-curve and subgroup checks. Calling code adds on-curve but not subgroup. G2 cofactor ≠ 1.
2. **D-1**: No semantic BTreeMap validation in circuit. Layer hash extraction trusts that BLS-attested data is correctly formatted. Protocol-level trust assumption.
3. **L-1**: Bincode layout constants are fragile across AN node releases. Need CI integration tests.

## Partner Prover Bug Found (2026-05-10, GOSH stand)

While running Alina's two-circuit live-stand experiment (`acki-nacki/latest_an_to_eth_bridge_test_lightweight` + `acki-nacki-to-eth-bridge-halo2-prover/test_both_circuits_lightweight`) on the GOSH server, Circuit 1a verified consistently but Circuit 2 was rejected for every live key block (`primary=true, layer=false`).

**Root cause**: `bridge-prover-lib/src/keys.rs::ensure_layer_keys` builds the reference circuit used for VK/PK keygen with tree depth 3:

```rust
let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 3);
```

The accompanying comment said `WINDOW_SIZE=4 → 6 leaves → pad to 8 → depth=3`, which was correct for the now-superseded `HISTORY_PROOF_WINDOW_SIZE=4`. The lightweight branch uses `HISTORY_PROOF_WINDOW_SIZE=8`, so real layer trees have `2 + 8 = 10` leaves padded to 16, **depth 4**. As a result, every `DenseChainLink` produced by `real_chain_builder` carries `siblings.len() == 4`, while the VK/PK were committed for `siblings.len() == 3`. The witness still satisfies the constraint system (MockProver passes), but `halo2::verify_proof` rejects every proof because the polynomial shape no longer matches the VK.

**Diagnostic isolation** (in `bridge-prover-lib/tests/`):
- `diagnostic_l1_block16.rs` — rebuilds the layer-1 Poseidon tree for live block 16 from blocks 8..15 leaves and confirms the prover's tree reconstruction matches the node's `history_proofs[1].root_hash` byte-for-byte (`fb3461c9...d175252c`). Tree math is fine.
- `diagnostic_mockprover_block16.rs` — runs `MockProver` with the EXACT witness the live prover constructs (real preimage, real siblings, real chain). All constraints satisfied.
- `diagnostic_chain_depth.rs` — proves the discrepancy: real chain links have `siblings.len()=4`, keygen reference has `siblings.len()=3`.

**Fix**: change the keygen reference depth to 4 (and update the stale comment):

```diff
- // Tree depth must match real trees: WINDOW_SIZE=4 → 6 leaves → pad to 8 → depth=3.
- let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 3);
+ // Tree depth must match real trees: WINDOW_SIZE=8 → 10 leaves → pad to 16 → depth=4.
+ let chain_data = bridge_test_data_gen::layer_hashes::generate_layer_hash_chain_with_depth(3, 2, 4);
```

**Verification (live)**: with the patch, after deleting cached `params/layer_*.bin` + `params/layer_config_params.json` and `state/*.json`, the prover regenerated keys (`base_circuit_params: k=17, num_advice_per_phase=[18], lookup_bits=Some(16)`) and successfully proved + verified Circuit 2 for blocks 16 and 24:

```
block 16: Circuit 1a VERIFIED (11.3ms), Circuit 2 VERIFIED (9.1ms)  → BOTH VERIFIED OK
block 24: Circuit 1a VERIFIED (17.5ms), Circuit 2 VERIFIED (11.0ms) → BOTH VERIFIED OK
```

Three diagnostic tests are checked into the prover repo so this regression is catchable in CI without needing a live node only for the depth check (`diagnostic_chain_depth` is the smallest reproducer).

**Long-term remediation suggestion (for the partner)**: derive the keygen depth from `HISTORY_WINDOW_SIZE` rather than hardcoding it, so the reference circuit shape automatically tracks the runtime constant.

### Resolution upstream (2026-05-11, Alina)

Alina confirmed the diagnosis and pushed the long-term remediation across all three repos. The actual story turned out to be a *duplicate-constant* drift, not a Window-= 8 production setting: `HISTORY_PROOF_WINDOW_SIZE` was defined in two places inside `acki-nacki` (`node/src/types/history_proof.rs` = 8, `node/libs/node-block-client/src/history_proof.rs` = 4). Her local node had been built from a cached Docker image where `node/src/types/history_proof.rs` = 8 while the prover daemon used the lightweight crate's `= 4` definition — masking the desync for several full E2E runs.

The chain of three coordinated commits:

- `acki-nacki@8c54dd7c` — single source of truth lives in `node/libs/node-block-client/src/history_proof.rs` (value reverted to `= 4` for fast E2E coverage); `node/src/types/history_proof.rs` now does `pub use node_block_client::history_proof::HISTORY_PROOF_WINDOW_SIZE`. Old `8` and production `128` preserved as comments.
- `acki-nacki-to-eth-bridge-halo2-prover@4cdc932 + c8495f9 + 24dffa1` — `bridge-prover-daemon` no longer carries its own `HISTORY_WINDOW_SIZE = …` literal; it imports `node_block_client::history_proof::HISTORY_PROOF_WINDOW_SIZE` at compile time. `ensure_layer_keys` computes `REF_TREE_DEPTH = (W + 2).next_power_of_two().trailing_zeros() as usize` — exactly the recommendation above. For the current `W = 4` this is depth `3`, bit-identical to the pre-fix literal, so cached VK/PK survive the patch with no keygen rerun.
- `acki-nacki-to-eth-bridge-halo2-circuits@55a22b9` — `historical-layer-hashes-movement-checker-circuit/tests/real_prover.rs` aligned to `TREE_DEPTH = 3`, README table now lists all three rows (W=4 → depth 3, W=8 → 4, W=128 → 8), `.gitignore` covers `test_cache_real_prover/`.

She independently re-verified end-to-end on her lightweight stand: 16/16 key blocks (heights 8..68) BOTH VERIFIED OK at `~3:20`/block steady state. Her note for future testing: stay on `W = 4` to maximise configuration coverage per unit time; bump to `8` only for the mid-size sanity sweep, and to `128` for production. The class of bug ("daemon literal drifts from node constant") is now eliminated at the type-system level — the prover crate physically depends on `node-block-client`, and the node crate physically re-exports the same constant.

### Independent re-verification on our stand (2026-05-11)

To close the loop we rebuilt the lightweight stand from scratch against Alina's unified `W = 4` ribosome (no Docker cache for `ackinacki-bridge-lite:latest`, fresh `params/layer_*.bin`, empty `state/`) and ran the prover+verifier pair against it. Six consecutive key blocks from the freshly bootstrapped chain all came through clean:

```
block  8: Circuit 1a VERIFIED (9.8 ms),  Circuit 2 VERIFIED (10.1 ms)  → BOTH VERIFIED OK  (3:32 e2e)
block 12: Circuit 1a VERIFIED (11.3 ms), Circuit 2 VERIFIED (9.7 ms)   → BOTH VERIFIED OK  (3:27 e2e)
block 16: Circuit 1a VERIFIED (8.8 ms),  Circuit 2 VERIFIED (10.0 ms)  → BOTH VERIFIED OK  (3:23 e2e)
block 20: Circuit 1a VERIFIED (15.2 ms), Circuit 2 VERIFIED (12.4 ms)  → BOTH VERIFIED OK  (3:28 e2e)
block 24: Circuit 1a VERIFIED (9.7 ms),  Circuit 2 VERIFIED (10.6 ms)  → BOTH VERIFIED OK  (3:25 e2e)
block 28: Circuit 1a VERIFIED (13.0 ms), Circuit 2 VERIFIED (8.1 ms)   → BOTH VERIFIED OK  (3:24 e2e)
```

Steady-state ~3:25/block end-to-end (prove+verify), matching Alina's reported ~3:20/block within noise. Halo2 verify time stays at ~10 ms/circuit. The class of bug is empirically closed on our stand too.

## Build & Test Commands

```bash
# This repo — main workspace
make setup          # One-time: install Foundry, Go, Rust toolchain
make build          # Build all (Rust workspace + Solidity)
make test           # Run all tests
make build-solidity # Solidity only

# Solidity contracts (132 tests across 14 suites, all green; +4 opt-in AAVE fork tests)
cd contracts/ethereum && forge build
cd contracts/ethereum && forge test                                                      # full suite
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeAaveTest" -vv       # AAVE mock subset (20 tests)

# AAVE V3 mainnet-fork tests (opt-in, requires RPC + `fork` profile for evm_version=shanghai
# because AAVE V3 aWETH bytecode uses PUSH0). Without FORK_URL the suite skips cleanly.
FOUNDRY_PROFILE=fork FORK_URL=https://ethereum-rpc.publicnode.com \
    forge test --match-contract AckiNackiBridgeAaveForkTest -vv         # 4 fork tests against real V3 Pool/Gateway/aWETH
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeVerifyBlock" -vv    # Phase 4 verifyBlock (17 tests)
cd contracts/ethereum && forge test --match-contract "AckiNackiBridgeRelayerLoop" -vv    # Phase 5.1 relayer loop (6 tests)
cd contracts/ethereum && forge test --match-contract "(Primary|Fallback|LayerHashesMovement)Verifier" -vv  # Per-circuit Groth16 adapters

# Relayer skeleton (Phase 5.1, standalone)
cd crates/bridge-relayer-daemon && cargo test                                            # 29 unit tests (13 baseline + 6 sentry + 5 guarded + 5 daemon)
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- --help                     # CLI surface
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- sentry-watch --ticks 5     # poll AN testnet, print BK-set events
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- smoke-fixture \
    --fixtures-dir ./fixtures --rpc-url ... --bridge-address ... \
    --an-node-url http://94.156.178.19:8600                                              # smoke run wrapped in SentryGuardedRelayer
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- verify-fixture \
    --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
    --rpc-url ... --bridge-address ...                                                   # read-only pre-flight (no key, exits non-zero on mismatch)
cd crates/bridge-relayer-daemon && cargo run --bin relayer -- daemon \
    --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
    --rpc-url ... --bridge-address ... --private-key ... \
    --backoff-initial-secs 2 --backoff-max-secs 60 --backoff-multiplier 2                # long-running operator entry (B5)
cd crates/bridge-relayer-daemon && cargo test --test live_bk_set_sentry -- --ignored     # live BK-set sentry against AN testnet

# Deposit relayer (EVM→AN direction, standalone)
cd crates/deposit-relayer-daemon && cargo test                                           # 32 lib + 1 fixture (+2 ignored live)
cd crates/deposit-relayer-daemon && cargo test --test log_index_mapping                  # block logIndex → receipt-local index (Sepolia fixture)
cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- --help            # CLI surface
cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- \
    watch --rpc-url <SEPOLIA_RPC> --bridge-address 0x... --from-block <DEPLOY_BLOCK> --start 0 --count 16
cd crates/deposit-relayer-daemon && BRIDGE_DEPLOY_BLOCK=11025180 cargo run --bin deposit-relayer -- \
    prove-one --rpc-url <SEPOLIA_RPC> --bridge-address 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
    --deposit-id 0 --deposit-prover-dir ../../deposit-prover --out-dir ./out             # production path: eth_getLogs discovery
cd crates/deposit-relayer-daemon && AN_DAPP_ID=0x1a1a1a1a1a cargo run --bin deposit-relayer -- \
    prove-one --rpc-url <SEPOLIA_RPC> --bridge-address 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
    --deposit-id 0 --tx-hash 0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf \
    --log-index 2 --deposit-prover-dir ../../deposit-prover --out-dir ./out               # operator fast path (known tx; skips getLogs)
cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- \
    an-preflight --an-node-url http://127.0.0.1:11000                                    # probe AN read endpoints (/v2/bk_set)
cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- \
    daemon --rpc-url <SEPOLIA_RPC> --bridge-address 0x... --from-block <DEPLOY_BLOCK> \
    --deposit-prover-dir ../../deposit-prover --an-node-url http://127.0.0.1:11000 \
    --dry-run                                                                            # listen→prove→submit loop; --dry-run until shellnet VK redeploy
cd crates/deposit-relayer-daemon && AN_NODE_URL=http://127.0.0.1:11000 \
    cargo test --test live_an_preflight -- --ignored                                     # live AN preflight
cd crates/deposit-relayer-daemon && BRIDGE_DEPLOY_BLOCK=11025180 SEPOLIA_RPC_URL=<RPC> \
    cargo test --test live_log_discovery -- --ignored --nocapture                        # live eth_getLogs discovery (production path)

# Cross-circuit-bound proof generation (Phase 4.1 fixture builder)
cd crates/bridge-prover-orchestrator
cargo run --bin export-bound-block-proofs --release        # writes proofs/bound/{primary,layer-hashes}/*
cd gnark-wrappers/circuit-1a && ./circuit-1a prove ../../proofs/bound/primary/halo2_proof.json
cd ../circuit-2                && ./circuit-2 prove ../../proofs/bound/layer-hashes/halo2_proof.json
```

## CI Pipeline (`.gitlab-ci.yml`)

| Stage | Jobs |
|------|------|
| `setup` | `setup:rust` (`cargo fetch --locked`), `setup:foundry` (npm + `forge install forge-std`) |
| `build` | `build:rust:debug`, `build:rust:release` (workspace only), **`build:rust:relayer`** (the relayer crate is excluded from the main workspace and has its own `Cargo.lock`; this job catches what `--workspace` skips, added 2026-05-18), `build:solidity` |
| `test` | `test:rust` (workspace), **`test:rust:relayer`**, `test:solidity` (forge), `test:solidity:coverage`, `lint:rust:fmt`, `lint:rust:clippy`, **`lint:rust:relayer:{fmt,clippy}`**, `lint:solidity:fmt` |
| `security` | `security:rust:audit` (`cargo audit` hard-gating; `--locked` cargo-audit install, RUSTSEC fail = pipeline fail; `main` + MR only), `security:solidity:slither` (allow_failure) |
| `deploy` | `docs:rust`, `docs:solidity`, manual `deploy:testnet`/`deploy:mainnet` placeholders |

`bridge-prover-orchestrator` and `deposit-prover` are **not** yet in CI (they pull halo2 deps that take minutes to build); their `cargo test` happens only locally. Tracking as future A2.

### Shipping docs / code snapshots to partners (off-tree zip / tar.gz)

For one-off bundles to partners who don't have GitLab access (Alina, Serhii et al.):

```bash
# Define what goes in the pack:
$EDITOR scripts/partner_packs/<topic>_for_<recipient>.manifest

# Build (zip + tar.gz + per-archive .sha256, all auto-gitignored):
scripts/build_partner_pack.sh <topic>_for_<recipient>
```

The script reads the manifest (one repo-relative path per line; supports `src => dest` renames; everything below a `---` line becomes the README footer), assembles `<topic>_for_<recipient>_<today>/` with a provenance-stamped `README.md` (repo URL + commit SHA + auto-generated TOC), a `MANIFEST.sha256` (per-file hashes inside the pack), and drops `<...>.zip` + `<...>.tar.gz` + matching `.sha256` files at the repo root. All filenames match `/*_for_*.{zip,tar.gz,sha256}` in `.gitignore` so the artefacts never accidentally get committed.

Two manifests already live under `scripts/partner_packs/`:

- `circuit4_for_alina.manifest` — the Q-C4-1..6 + decoded layout + integration plan §3 Phase 8/9 bundle.
- `halo2_tvm_for_serhii.manifest` — `ZKHALO2VERIFYWITHVK` wire-format pack (design memo + `Halo2TvmBundle` reference impl + green round-trip test).

`scripts/partner_packs/_template.manifest` is the starter template.

### Reproducing CI locally before pushing

Run `make pre-push` before any non-trivial push — it mirrors every job CI runs and catches the two failure modes that the default `make test` doesn't:

1. **`vm.assume` rejection-cap trips** (pipeline #5741, fix `13d59431`): a fuzz test with `vm.assume(seqNo == 0)` rejects 2^64 − 1 of 2^64 inputs, blowing past Foundry's 65 536-rejected-inputs cap. The default `forge test` may happen to seed past it; CI's seed often doesn't. **Lesson**: if the constrained value space has < ~5 % of total inputs, demote to a regular unit test or use `bound(rawVal, lo, hi)` to project the seed into the valid range.
2. **`Stack too deep` under coverage** (pipeline #5744, fix `b63a4d3`): `forge coverage` disables the optimizer + viaIR for accurate coverage, so functions with > 16 live local stack slots fail to compile in the coverage profile even though `forge build` happily inlines them. **Lesson**: keep deploy-script `run()` lean — use scope blocks `{}` to drop dead locals, extract helpers, or pack multi-arg calls into a memory `struct`.

`make pre-push` runs: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the same for the relayer crate, `forge fmt --check`, `forge test`, **`forge coverage --report summary`** (this is the key one), `cargo test --workspace --locked`, and `cargo test` inside the relayer crate. ~3 min total on a warm cache.

## Integration Status

**Architecture v2 (4-circuit, since Phase 1.A 2026-05-06 onward)**: AN→ETH state lives directly on `AckiNackiBridge.sol` (`verifyBlock`); the legacy single-circuit pipeline (`LayerHashBridge.sol`, `LayerHashVerifier.sol`, the `bk-set-rotation-prover/` design, and the `layer-hashes-prover/` crate with its 13-input Groth16 wrapper) was retired by Phase 4.2 on 2026-05-10. Per-circuit Groth16 adapters (`PrimaryVerifier.sol`, `FallbackVerifier.sol`, `LayerHashesMovementVerifier.sol`) and the bound proof toolchain (`crates/bridge-prover-orchestrator/`) now own the surface that used to be split across the legacy crate + `LayerHashBridge`.

**Phase 4.1 (`AckiNackiBridge.verifyBlock`, completed 2026-05-10, additive)**:
- `AckiNackiBridge.sol` extended with AN→ETH state (`storedBkSetCommitment`, `storedLastSeenBlockSeqNo`, `storedNumLayers`, `storedLayerHashes[10]`, `storedPrevMaxLevelLayerHash`) + 3 immutable verifier slots + `verifyBlock(finType, attestationProof, layerHashesProof, blockId, bkSetCommitment, blockSeqNo, numLayers, layerHashes[10], prevMaxLevelLayerHash)` permissionless entry point.
- Cross-circuit + monotonicity + chain-anchor invariants enforced (`BkSetCommitmentMismatch`, `BlockSeqNoNotMonotonic`, `PrevAnchorMismatch`, `InvalidNumLayers`, `LayerHashTailNonZero`, `AttestationProofRejected`, `LayerHashesProofRejected`).
- Constructor takes a `VerifyBlockConfig` struct as 6th argument; passing `VerifyBlockConfigLib.disabled()` disables `verifyBlock` (legacy deployments).
- Cross-circuit-bound test data: `crates/bridge-prover-orchestrator/src/bound_test_data.rs` wraps the partner's `generate_bridge_test_data` so Circuit 1A's primary attestation BLS-bytes and Circuit 2's layer-hashes preimage + Merkle siblings hash to the **same** `block_id` (the 8-leaf envelope tree root) and share `bk_set_poseidon`.
- New binary `cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release` produces both proofs from one scenario in ~3.5 min wall time (uses cached 1A K=20 + Circuit 2 K=17 keys; gnark setup also cached).
- 17 new Foundry tests in `AckiNackiBridgeVerifyBlock.t.sol` — real bound 1A + 2 Groth16 proofs go end-to-end through both adapters → on-chain `*Groth16VerifierGenerated.sol`. Per-block verify cost: ~700 k gas (well under the 1 M target). Mock fallback path covered by `test/mocks/MockFallbackVerifier.sol`.
- 178/178 Foundry tests green at landing.

**Phase 5.1 (relayer skeleton, completed 2026-05-10)**:
- New standalone crate `crates/bridge-relayer-daemon/` (excluded from workspace, like `bridge-prover-orchestrator`). Modules: `types` (`AnBlockData`, `FinalizationType`, `MAX_LAYER_HASHES = 10`, structural validation), `bridge` (`BridgeClient` async trait + `EthBridgeClient` over `abigen!`-bindings — with inherent `dry_run_block` for `eth_call`-based pre-flight simulation — + `MockBridgeClient` mirroring the on-chain state machine byte-for-byte for unit tests, returning `DryRunOutcome::{WouldSucceed,WouldRevert}`), `source` (`BlockSource` async trait + `InMemoryBlockSource` + `FixturesBlockSource` reading Phase 4.1 bound proof artefacts), `state` (atomic `state.json` persistence with write-temp-then-rename), `relayer` (`Relayer::tick()` + `Relayer::run_loop(max_ticks, should_stop)`), `daemon` (`BackoffConfig`, `RelayerMetrics`, `run_until_shutdown` with SIGINT/SIGTERM-aware exit), and a CLI binary `relayer` with four subcommands: **`smoke-fixture`** (one-shot submission), **`sentry-watch`** (read-only BK-set poller), **`verify-fixture`** (read-only pre-flight against a wired bridge — cheap anchor checks + by default an `eth_call` simulation of `verifyBlock(...)` that catches bad proofs as well as anchor mismatches; `--no-simulate` skips the eth_call; added 2026-05-20), **`daemon`** (long-running operator entry with backoff + metrics + sentry guard).
- 13 Rust unit tests cover the loop end-to-end against `MockBridgeClient` + `InMemoryBlockSource`: 5 sequential blocks (mixed Primary/Fallback), `NotYetAvailable` recovery, verifier-rejection path, restart-from-persisted-state, `run_loop`'s `should_stop` semantics.
- 6 new Foundry tests in `AckiNackiBridgeRelayerLoop.t.sol` drive `verifyBlock` through 10 sequential synthetic blocks with the new `MockPrimaryVerifier`/`MockFallbackVerifier`/`MockLayerHashesMovementVerifier` mocks: asserts `storedLastSeenBlockSeqNo`/`storedNumLayers`/`storedLayerHashes[..]`/`storedPrevMaxLevelLayerHash` after each step, plus restart-reads-anchor, replay-reverts, fast-forward-permitted, attestation-rejection-state-untouched (CEI), anchor-mismatch-reverts negatives.
- 184/184 Foundry tests green at landing (178 baseline + 6 new). Phase 5.2 (`LiveBlockSource` over partner GraphQL/BOC + halo2+gnark inside the relayer) blocked on Q1 + Q2; Phase 5.3 (10-block shellnet acceptance) blocked on Q1.

**Phase 4.2 (legacy demolition, completed 2026-05-10)**:
- Deleted 8 legacy Solidity sources: `LayerHashBridge.sol`, `LayerHashVerifier.sol`, `LayerHashGroth16Verifier.sol`, `LayerHashGroth16VerifierGenerated.sol`, `ILayerHashVerifier.sol`, `BkSetRotationVerifier.sol`, `BkSetRotationGroth16Verifier.sol`, `IBkSetRotationVerifier.sol`. The single-circuit 13-input flow plus its old BK-rotation surface are gone — `AckiNackiBridge.verifyBlock` (Phase 4.1) is the only AN→ETH path; the future BK-set update will plug into it as an optional fourth proof argument once Phase 1.C / 3.4 land.
- Deleted 2 legacy Foundry test files: `LayerHashBridge.t.sol` (35 tests across `LayerHashBridgeTest` + `LayerHashVerifierTest` + `BkSetRotationVerifierTest`) and `LayerHashE2E.t.sol` (14 real-proof tests across 4 fixtures). Net Foundry suite: 184 → **135 tests** across 15 suites; coverage of every surviving invariant is preserved by `AckiNackiBridgeVerifyBlock.t.sol` (17), `AckiNackiBridgeRelayerLoop.t.sol` (6), `LayerHashesMovementVerifier.t.sol` (10), `PrimaryVerifier.t.sol` (8), `FallbackVerifier.t.sol` (8). Real-proof multi-fixture E2E coverage will be re-introduced by Phase 5.3 (relayer-driven, multi-block from shellnet).
- Deleted 2 legacy Rust trees: `layer-hashes-prover/` (the standalone single-circuit Halo2 → 13-input Groth16 wrapping pipeline) and `bk-set-rotation-prover/` (circuit spec + Go gnark wrapper for the old 2-input rotation design). Workspace `Cargo.toml` no longer excludes `layer-hashes-prover`.
- Lingering doc references in `docs/integration_plan.md` / `docs/verifying_an_proof.md` are intentionally left unchanged — they describe the legacy architecture that's now superseded by `docs/an_partner_integration_plan.md`. A pointer in `docs/integration_plan.md` already marks M7–M9 as superseded.

**Phase 4.3 — Legacy deposit-verifier demolition (2026-05-17)**:
- Removed the legacy v1 ETH-side deposit-verifier chain: deleted `IAckiNackiVerifier.sol`, `Groth16Verifier.sol` (gnark-generated for deposit-prover), `Groth16DepositVerifier.sol`, `DummyVerifier.sol`, `AckiNackiBridgeV2.t.sol`, the `FuzzGroth16{Verifier,DepositVerifier}Test` contracts, and the withdraw-flow fuzz tests.
- Surgically removed from `AckiNackiBridge.sol`: `verifier` storage slot, `processedDeposits` mapping, `event Withdrawal`, four errors (`InvalidProof`, `DepositAlreadyProcessed`, `InvalidVerifier`, `InvalidBlockHash`), the `withdraw(...)` function, the `isDepositProcessed(...)` view, and the `_verifier` ctor parameter. Ctor is now 5-args (was 6).
- Deleted the deposit-prover gnark-wrapper toolchain: `deposit-prover/gnark-wrapper/` (Go module + R1CS + keys + generated `Groth16Verifier.sol`), `deposit-prover/src/groth16_wrapper/` (Rust JSON adapter), and 11 deposit-prover/top-level shell scripts that wired the Sepolia E2E.
- Trimmed `crates/eth-frontend/src/contract.rs` ABI to deposit + read-only views; deleted `crates/eth-frontend/src/withdrawal.rs` stub.
- Net Foundry suite: 135 → **109 tests across 12 suites** (−26 tests, −3 suites); `forge build` clean, `forge test` all green; `cargo check --workspace --all-targets` clean.
- Rationale: AN-side will verify the deposit-prover's Halo2 SHPLONK proof natively via the future `VERHALO2SHPLONK` TVM opcode (no EIP-170 on AN side, so the gnark wrapper is unnecessary). The AN→ETH gnark wrappers under `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-{1a,1b,2}/` are **not affected** — EIP-170 still forces a wrapper on that side. See Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`.

**AAVE V3 Yield Integration (completed)**:
- `AckiNackiBridge` extended with optional AAVE V3 wiring (pool + WETH gateway + aWETH).
- New owner role manages routing only — **cannot touch user principal**.
- `supplyToAave(amount)` routes idle ETH to AAVE; `withdrawFromAave` and `emergencyWithdrawAll` pull funds back (owner-only).
- Configurable `liquidReserveBps` (default 10%, capped at 50%) governs how much of treasury stays liquid as ETH.
- Yield isolated from principal: `accruedYield() = aWETH.balanceOf(bridge) - suppliedPrincipal`. `harvestYield(amount)` forwards to a separate `yieldRecipient`.
- Reentrancy guard on all mutating functions; CEI preserved throughout.
- Mainnet addresses hardcoded in `script/DeployRealBridge.s.sol`; opt-in via `USE_AAVE=true`.
- See `docs/aave_integration.md` for design + correctness verification protocol. (Note: doc may still mention the legacy user-facing `withdraw()` auto-pull behaviour, which was retired in Phase 4.3 along with the rest of the legacy withdraw flow; the owner-only `withdrawFromAave` / `emergencyWithdrawAll` paths are unaffected.)

**Test counts (Foundry, 15 suites, all green; +1 fork suite gated by `FORK_URL`)**:

| Suite | Count |
|------|------|
| `AckiNackiBridgeAaveTest` (AAVE; owner-only top-up + yield) | 20 |
| `AckiNackiBridgeVerifyBlockTest` (Phase 4 AN→ETH, real bound 1A+2 proofs + invariants) | 17 |
| `FuzzAckiNackiBridgeVerifyBlockTest` (Phase 4 input-validation invariants, 6 fuzz × 256 runs + 1 unit) | 7 |
| `AckiNackiBridgeRelayerLoopTest` (Phase 5.1 — 10-block loop with mock verifiers) | 6 |
| `AckiNackiBridgeWithdrawByProofTest` (Circuit 4 single-final-root — `withdrawByProof` + nullifier mapping + anchor lookup + recipient split + treasury shortfall + strict-pub plumbing) | 24 |
| `AckiNackiBridgePauseTest` (global pause / unpause — owner-only, blocks deposit / verifyBlock / withdrawByProof; AAVE management remains available) | 12 |
| `AxiomBlockHeaderOracleTest` | 16 |
| `Blake2bHalo2VerifierTest` | 7 |
| `KeccakHalo2VerifierTest` | 1 |
| `Halo2PoseidonVerifierTest` | 7 |
| `FuzzAckiNackiBridgeDepositTest` (deposit fuzz only) | 3 |
| `FuzzHalo2VerifierTest` | 6 |
| `FallbackVerifierTest` (Circuit 1B, real gnark proof) | 8 |
| `PrimaryVerifierTest` (Circuit 1A, real gnark proof) | 8 |
| `LayerHashesMovementVerifierTest` (Circuit 2, real gnark proof) | 10 |
| **Total Foundry** | **152** |

**Rust tests** (excluded crates, run with `cargo test` per crate):

| Crate | Count | Notes |
|------|------|------|
| `bridge-relayer-daemon` | 29 | state persistence (2), `BlockSource` (2), `MockBridgeClient` (4), `Relayer` loop end-to-end (5), `BkSetSentry` Bootstrapped/Quiet/RotationDetected classification + metrics counters + `run_until_stop` orchestration (6), `SentryGuardedRelayer` rotation-pause + manual-resume + pass-through + error-propagation (5), **daemon** exponential-backoff + shutdown-aware sleep + `RelayerMetrics` atomic counters + `BackoffConfig::bump` cap (5) |
| `bridge-relayer-daemon` (live) | 1 | `#[ignore]`-gated `live_sentry_bootstraps_then_quiet_or_rotation` — two-tick sequence against the public AN testnet `/v2/bk_set_update` |
| `deposit-relayer-daemon` (EVM→AN) | 32 | lib: `types`/`state`/`source` (incl. `resolve_from_block`, 429 retry, log-index mapping) round-trips, `MockProofGenerator`/`SubprocessProverConfig`, `submitter` nullifier + `encode/decode_finalize_deposit`, `Relayer` loop (in-order / nullifier-skip / proof-failure / AN-rejection / restart / `run_loop`), **daemon** backoff + shutdown, `AnConfig`, `RelayerMetrics` |
| `deposit-relayer-daemon` (integration) | 1 | `log_index_mapping` — Sepolia `depositId=0` fixture: block `logIndex` 271 → receipt position 2 |
| `deposit-relayer-daemon` (live) | 2 | `#[ignore]` `live_an_preflight_succeeds` (`GET /v2/bk_set`); `live_eth_log_source_finds_deposit_id0` (production `eth_getLogs` discovery; needs `BRIDGE_DEPLOY_BLOCK` + paid RPC) |

### Shellnet E2E — EVM↔AN deposit path (updated 2026-06-13)

**Partner updates (2026-06-11 chat)**

| Topic | Detail |
|-------|--------|
| **AN→ETH shellnet test** | Unified in `acki-nacki-to-eth-bridge-halo2-prover/python/generate_withdrawals_with_live_event_proving.py` (local node + shellnet); runbook: `TECHNICAL_README.md` in same repo |
| **Fork pins (use gosh-sh, not SergeSPb)** | `halo2-lib-zkevm-sha256-and-bls12-381` → `github.com/gosh-sh/…` branch `bump-halo2-lib-v0.4.1`; `axiom-eth` → `github.com/gosh-sh/…` branch `gosh-stable-rlcmanager-assignment` (Alina cherry-picked + repointed tvm-sdk `Cargo.toml`) |
| **`ZKHALO2VERIFYWITHVK` ABI** | **Frozen Variant A: 3 stack operands** (`vk_cell`, `public_inputs_cell`, `proof_cell`). Merge `c249fb11` accidentally reverted to 1-operand `Halo2TvmBundle`; **fixed `1b9502cd` (2026-06-13)** per Alina/Serhii |
| **tvm-sdk release tag** | Alina pointed to `v3.0.0.an`; branch tip for shellnet: `full_dex_and_bridge_test_with_final_halo2_circuit` @ `1b9502cd` |

**Direction status**

| Direction | Status | Notes |
|-----------|--------|-------|
| **AN→ETH** | ✅ Green (2026-06) | Sepolia bridge `verifyBlock` + `withdrawByProof` mined on Ursus stand |
| **ETH→AN** | 🔴 Blocked on shellnet VK | Relayer on `ubuntu@ursus-tools.dev` proves Sepolia deposits; `finalizeDeposit` fails at `ZKHALO2VERIFYWITHVK` because deployed `USDCBridge` embeds wrong VK |

**Why shellnet rejects real deposit proofs today**

| Layer | Shellnet now | Required |
|-------|--------------|----------|
| **USDCBridge `VK_BLOB`** | Circuit 1B fallback, Base v1, **4 PI**, 6308 B | Deposit RLC VkBlob v2, **11 PI**, 3597 B |
| **`finalizeDeposit`** | `_buildPublicInputs` returns 4 scalars | Must passthrough all 11 proof-binding fields (see below) |
| **AN node `tvm_vm`** | Base-only or pre-merge opcode | RLC reader (`circuit_shape=1`, `read_rlc_vk`) + nightly `gosh` feature |
| **Smoke that works** | `fallback_vk_blob.bin` (4 PI) via `acki-nacki/tests/exchange/test_usdcbridge_finalize.py` | Proves relayer keys / gas / ABI — **not** deposit proofs |

**11 public inputs** (canonical for `deposit-prover` / `deposit-relayer-daemon` / shellnet redeploy; 11 × 32 B LE `Fr`):

```
[0] depositId  [1] sender  [2] amount  [3] contractAddress
[4] dappIdHigh  [5] dappIdLow  [6] anAccountHigh  [7] anAccountLow
[8] blockHashHigh  [9] blockHashLow  [10] promiseCommit
```

Partner checklist (full redeploy steps): `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`. VK gap analysis: `docs/deposit_finalize_vk_gap_2026-05-28.md`.

#### `tvm-sdk` merge + ABI fix (2026-06-11 → 2026-06-13)

Merged `pruvendo/deposit-rlc-vkblob-v2` (`027610d7`) into `full_dex_and_bridge_test_with_final_halo2_circuit`. **No new opcode** — same `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`); merge adds VkBlob **v2** RLC reader inside the **3-operand** handler.

| Item | Value |
|------|-------|
| **Branch** | `full_dex_and_bridge_test_with_final_halo2_circuit` |
| **Tip (2026-06-13)** | `1b9502cd` — **restore 3-operand ABI** + VkBlob v2 RLC + deposit 10×11-PI tests green |
| **Prior** | `c0aca1e0` (Alina: gosh-sh fork pins); `95055e85` (W128 VK + deposit tests); `c249fb11` (RLC merge — **introduced 1-operand regression**) |
| **GitHub PR** | [#251](https://github.com/tvmlabs/tvm-sdk/pull/251) OPEN, CI ✅, `MERGEABLE`, blocked on `REVIEW_REQUIRED` |
| **Merge to `main`?** | Nice-to-have; **not required for shellnet** — `acki-nacki` `Cargo.toml` pins the branch |
| **CI verify** | `cd ../tvm-sdk && cargo test -p tvm_vm --features gosh` (141 pass, 3 `#[ignore]` fallback positive) |

**Assembly (frozen Variant A):**

```text
PUSHREF vk_cell              ← VkBlob (v1 Base or v2 Rlc)
PUSHREF public_inputs_cell   ← N × 32 B LE Fr (no header)
PUSHREF proof_cell           ← raw SHPLONK bytes
ZKHALO2VERIFYWITHVK
```

`Halo2TvmBundle` (single self-describing byte stream) remains in `zk_halo2_with_vk_bundle.rs` for producer round-trip tests only — **not** consumed by the opcode handler.

Post-merge fixes: `95055e85` restored W=128 embedded VK for legacy `ZKHALO2VERIFY`; `1b9502cd` reverted the accidental 1-operand ABI from `c249fb11` while keeping RLC `circuit_shape=1` reader. Circuit 1B fallback **positive** integration tests are `#[ignore]` pending fixture regen after gosh halo2-lib bump (negative ABI tests still run).

**Superseded branches (do not pin for deposit E2E):**

- `halo2_circuit_with_vk` — **does not exist on GitHub** (was a doc typo)
- `serhii/node-3406-vergrth16-with-vk` — Groth16-era umbrella; deposit path is native Halo2 only
- `pruvendo/deposit-rlc-vkblob-v2` — merged into `full_dex`; tip `027610d7` (2026-05-30)

#### VK / proof fixtures in `tvm-sdk` (`tvm_vm/halo2_test_data/`)

| File | Size | Shape | PI count | Use |
|------|------|-------|----------|-----|
| `deposit_10proofs/deposit_vk_blob.bin` | 3597 B | VkBlob v2 RLC | **11** | **Target for USDCBridge redeploy** — byte-identical to `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin` (SHA-256 `147efe14…068abaf`) |
| `deposit_10proofs/proof_00..09/{public_inputs,proof}.bin` | 352 B + ~8 KB each | — | 11 | Unit tests `test_zkhalo2_with_vk_deposit_10_real_proofs` — **no** `input.json` or `.srs` here (producer-only; sync via `scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`) |
| `deposit_rlc_vk_blob.bin` | 3725 B | VkBlob v1 | 7 | Older RLC smoke (`round_trip_deposit_rlc_*` tests) — **not** the production 11-PI layout |
| `fallback_vk_blob.bin` | 6308 B | Base v1 | 4 | **Currently on shellnet** — Circuit 1B fallback; copy also in `crates/bridge-prover-orchestrator/fixtures/circuit_1b_fallback/` |
| `dark_dex_w128_L{0,1,2}_*.bin` | — | — | — | Legacy `ZKHALO2VERIFY` opcode only (different KZG ceremony than deposit) |

#### E2E readiness checklist (ETH→AN)

```
[✅] tvm-sdk @1b9502cd: 3-operand ABI + RLC reader + deposit_10proofs (141 tests pass)
[✅] deposit-prover: 11-PI RLC proofs + VkBlob export
[✅] deposit-relayer: prove Sepolia deposits (production + fast-path CLI; fixture covers log-index mapping)
[✅] AN→ETH withdraw path on Sepolia (orthogonal)
[⏳] tvm-sdk PR #251 merge to main (optional; needs 1 GitHub approving review)
[🔴] acki-nacki: USDCBridge.sol — embed 11-PI VkBlob + extend finalizeDeposit + redeploy .tvc
[🔴] shellnet: rebuild AN nodes from tvm-sdk @1b9502cd (--features gosh, nightly toolchain)
[🔴] SRS alignment: deposit proofs keyed on chain ceremony (kzg_bn254_19.srs → k=18), not Hermez
[🔴] deposit-relayer: live finalizeDeposit (remove --dry-run once contract + IAckiNacki send land)
[⏳] Circuit 1B fallback WITHVK fixtures: regen via bridge-prover `EXPORT_HALO2_FIXTURE_DIR` (positive tests #[ignore])
```

**Partner message template** (after tvm-sdk push): branch tip SHA, `deposit_vk_blob.bin` SHA-256, `cargo test -p tvm_vm test_zkhalo2_with_vk --features gosh`, pointer to `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`.

#### Ursus operator host (`ubuntu@ursus-tools.dev`, updated 2026-06-11)

Shellnet E2E operator box. SSH: `ssh ubuntu@ursus-tools.dev` (passwordless from dev machine). Root layout: `/home/ubuntu/bridge-e2e/`. Live manifest: `DEPLOYED_VERSION.json` on the host.

| Path | Role |
|------|------|
| `acki-nacki-bridge/` | rsync'd from local (no git on host); **tip `15ea747`** (2026-06-11) |
| `acki-nacki-to-eth-bridge-halo2-prover/` | partner AN→ETH prover + `proofs/` |
| `bin/deposit-relayer` | EVM→AN daemon (rebuilt 2026-06-11) |
| `bin/relayer` | AN→ETH `verifyBlock` / `withdrawByProof` CLI |
| `config/deposit-relayer.env` | Sepolia + shellnet GraphQL + `AN_SENDER` msig |
| `config/bridge-relayer.env` | Sepolia E2E bridge `0x58a1…043d` (AN→ETH) |
| `tvm-sdk/` | `v3.0.0.an` tag build for `tvm-cli` (prover python tests) |

**systemd**

| Unit | State | Purpose |
|------|-------|---------|
| `deposit-relayer.service` | **active** | EVM→AN loop → `finalizeDeposit` on shellnet |
| `bridge-relayer.service` | inactive | long-running AN→ETH (manual `relayer` CLI used for E2E) |

**Redeploy recipe** (from dev machine — build on ursus for GLIBC safety):

```bash
# sync source
rsync -avz --delete --exclude target --exclude .git \
  acki-nacki-bridge/ ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/acki-nacki-bridge/

# rebuild + install
ssh ubuntu@ursus-tools.dev '
  source ~/.cargo/env
  cd /home/ubuntu/bridge-e2e/acki-nacki-bridge/crates/deposit-relayer-daemon
  cargo build --release --locked && install -m 755 target/release/deposit-relayer /home/ubuntu/bridge-e2e/bin/
  cd ../bridge-relayer-daemon
  cargo build --release --locked && install -m 755 target/release/relayer /home/ubuntu/bridge-e2e/bin/
  sudo systemctl restart deposit-relayer.service
'
```

Unit + env templates: `scripts/ursus/deposit-relayer.{service,env.example}`, `scripts/ursus/bridge-relayer.{service,env.example}`. Set `BRIDGE_DEPLOY_BLOCK` in deposit-relayer env (not genesis). AN→ETH wiring: `docs/shellnet_an_eth_relayer_wiring.md`.

**GitHub (bridge-EVM):** active docs/integration PR [#5](https://github.com/gosh-sh/bridge-EVM/pull/5) (`pruvendo/shellnet-e2e-landing` → `main`).

**Key config (deposit, non-secret)**

- Sepolia bridge (deposits): `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`
- Sepolia E2E bridge (withdraw): `0x58a1c8d22a79a91db6e7448a7d64d59ad4dc043d`
- Shellnet GraphQL: `https://shellnet.ackinacki.org/graphql`
- `AN_SENDER`: `20c2db9c…::20c2db9c…` (relayer msig, deployed 2026-06)
- RPC: Alchemy Sepolia (switched from publicnode 2026-06-11 — was hitting HTTP 429)

**Remaining (Phase 5.2/5.3, 6, 7)**:
- Phase 5.2: `LiveBlockSource` impl over partner's `gql_client` + `boc_parser` + relayer-side halo2+gnark (blocked on Q1 + Q2).
- Phase 5.3: 10-block shellnet acceptance (Anvil + live AN node) — blocked on Q1. Re-introduces real-proof multi-block coverage that retired with the legacy E2E suite in Phase 4.2.
- Phase 1.C / Phase 3.4: BK-set update circuit (partner stub `bk-set-change-verifier-halo2-circuit`); on landing, `verifyBlock` grows an optional fourth proof argument.
- Phase 6: real `acki-nacki-interface` implementation (currently mock only).
- AAVE: mainnet fork tests landed 2026-05-17 in `AckiNackiBridgeAaveFork.t.sol` (4 tests: constructor wiring, supply+withdraw round-trip, 1-year yield accrual + harvest, emergency exit). Opt-in via `FOUNDRY_PROFILE=fork FORK_URL=...`. Default `forge test` skips them. Validates real V3 `Pool` + `WrappedTokenGatewayV3` + `aWETH` ABI assumptions against the mock-based `AckiNackiBridgeAaveTest`.
- Production LAYER_TREE_DEPTH=8 testing.

### Canonical v2 doc set (Phase 7 done, 2026-05-10)

- `docs/four_circuit_architecture.md` — **canonical v2 entry point**: envelope-hash leaf table, per-circuit public-input layouts, cross-circuit binding (CC-#), state machine, gas table.
- `docs/audit_trail_v2.md` — **v1 → v2 trust delta**: 7 reduced assumptions, 12 retained, the cross-circuit soundness argument, the pre-tag sign-off checklist.
- `docs/an_partner_integration_plan.md` — **active integration plan** against the partner's four-circuit architecture (`acki-nacki-to-eth-bridge-halo2-circuits` + `acki-nacki-to-eth-bridge-halo2-prover` sibling repos).
- `docs/integration_analysis.md` — architecture analysis with §3 / §5 rewritten for v2 (deposit/AN-platform sections still original).
- `docs/bridge_verification.md` — invariant labels (DEP-#, **LH-#** v2, **BK-#** Phase 1.C placeholder, OR-#, AC-#, FORK-#, **CC-#** new in v2, ZK-#) + reproducible run recipe.
- `docs/manual_verification_runbook.md` — hands-on review (Phase D bound proof, Phase F `verifyBlock` walk, Phase G Phase-1.C placeholder, Phase J ≥ 30 attack scenarios incl. CC-1..CC-7).
- `docs/verifying_an_proof.md` — per-circuit (1A/1B/2) verification flow, V1–V5 stages.
- `docs/verifying_eth_proof_on_an.md` — deposit-side flow. **Rewritten 2026-05-17 after Phase 4.3 demolition**: the ETH-side Groth16 path was retired and the verification is now described as a pure AN-side native Halo2 SHPLONK check via the proposed `ZKHALO2VERIFYWITHVK` TVM opcode.
- `docs/zk_halo2_an_side_design.md` — design memo for the AN-side `ZKHALO2VERIFYWITHVK` opcode: gap analysis vs. partner's `ZKHALO2VERIFY` (hard-coded DarkDex VK), stack ABI / gas model / per-VK cache, six-phase roadmap. Companion real-impl branch in `tvm-sdk`: `serhii/verhalo2shplonk-real-impl` (rebased onto `serhii/node-3406-vergrth16-with-vk`; 5 round-trip tests green against DarkDex W=8 L0 fixture). **Wire-format Q-WIRE-1..5 + Q-NAME-1 FROZEN 2026-05-22** (table in §4): Blake2b transcript (`transcript_kind=0x00`), verifier-only `ParamsKZG<Bn256>` built at runtime from 3 globally-embedded points (no on-disk SRS file), strict 32-byte LE Fr public inputs, self-describing `Halo2TvmBundle` (magic `b"HALO2TVM"`, format_version `0x01`), 1-operand stack ABI, opcode at `0xC7 0x4A`. Created 2026-05-17, frozen + implemented 2026-05-22.
- `docs/zkhalo2verifywithvk_reference.md` — integration reference for the **landed** `ZKHALO2VERIFYWITHVK` opcode: stack ABI, full `Halo2TvmBundle` byte layout (header / chunks / sizing table), producer-side Rust snippet against `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`, KZG embedding scheme, per-VK cache semantics, gas placeholder + re-bench TODO, test surface inventory (consumer-side + producer-side + assembler), Solidity-on-TVM call-site sketch for `TokenBridge.finalizeDeposit`, and an 8-row failure-mode cheat sheet keyed on caller-visible symptoms. Cross-references to PR #240 / PR #242 and to every file in both repos that touches the opcode. Created 2026-05-22. **§15 added 2026-05-29**: VkBlob **v2** `circuit_shape` byte (0=Base `BaseCircuitParams`/`BaseCircuitBuilder`, 1=Rlc `EthCircuitParams`/`EthCircuitImpl<Fr,Noop>`) so the opcode can verify deposit-prover RLC proofs (not just BaseCircuitBuilder/DarkDex). **Consumer implementation (2026-06-11):** merged on `tvm-sdk` branch `full_dex_and_bridge_test_with_final_halo2_circuit` @ `95055e85` (PR #251); requires nightly `gosh` feature + unified halo2 backend patches. See `docs/deposit_finalize_vk_gap_2026-05-28.md` and § Shellnet E2E above.
- `docs/circuit_4_open_questions.md` — Phase A vs Phase B split for Circuit 4 (`bridge-event-prove-circuit`). Phase A landed in this commit (`AckiNackiBridge.verifyEvent`, `_layerWindow`, `BridgeEventVerifier`, 16 mock-based Foundry tests, gnark wrapper skeleton at `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/`). Phase B (real `withdraw()`) gated on five circuit-side design questions Q-CIRC4-1..5. New 2026-05-17.
- `docs/an_partner_questions_circuit4_2026-05-17.md` — open-question pack for Alina specific to Circuit 4 (instance binding, ephemeral roots, witness export, multi-token, withdrawal flow). Companion to `docs/circuit_4_open_questions.md`.
- `docs/reviews/alina_review_pack_2026-05-18.md` — metadata for the out-of-tree review pack delivered to Alina (`dist/alina_review_pack_2026-05-18.tar.gz`, 46 files / 165 KB / SHA-256 pinned): three AN→ETH circuit VKs + sample Blake2b-SHPLONK proofs + integration glue + retired `deposit-prover/` source + 1A↔2 bound-scenario artefacts. Lists Q1..Q7 queued for her.
- `docs/aave_integration.md` — AAVE yield integration (orthogonal to four-circuit surface).
- `docs/layer_hashes_circuit_audit.md` — Phase 0 partner-circuit audit (still applies — chips reused by Circuit 1A/1B/2).
- `docs/legacy/verifying_an_proof_v1.md` — the retired single-circuit walkthrough, kept for reproducibility of legacy proofs.
- `docs/integration_plan.md` — historical M0–M9; M7–M9 banner-marked as superseded by `an_partner_integration_plan.md`.
