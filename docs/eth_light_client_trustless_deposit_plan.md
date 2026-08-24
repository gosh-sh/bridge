# Trustless Ethereum light client for ETH→AN deposits — work plan

**Status:** proposed · **Created:** 2026-08-20 · **Owner:** Sergey (ZK) · **Direction:** ETH → Acki Nacki (deposit)

> Supersedes the trusted-attester MVP (`_acceptedBlockHash` / `setAcceptedL1Block`).
> The team has **not** accepted an attester/oracle trust assumption; this plan removes
> it entirely by proving Ethereum finality in-circuit.

---

## 1. Problem

The deposit circuit (`deposit-prover/src/circuit_v2.rs`) proves:

- a `Deposit` log sits in a receipt, the receipt sits under `receiptsRoot`,
- the receipt/tx bind `chainId` (PI[4]) and the emitting `contractAddress` (PI[3]),
- the block header hashes to `blockHashHigh‖blockHashLow` (PI[9]/PI[10]).

It does **not** prove that this `blockHash` belongs to Ethereum's **canonical, finalized**
chain. A valid Halo2 proof over a **self-fabricated** header + receipt trie (attacker-authored,
carrying a `Deposit` log that names a trusted bridge address) passes `ZKHALO2VERIFYWITHVK`
and `finalizeDeposit` — **mint-from-nothing**. Canonicity is currently an out-of-circuit trust
input. This plan makes it an **in-circuit** statement.

## 2. Goal & non-goals

**Goal.** A Halo2/BN254 SHPLONK proof, verified natively on AN by the existing
`ZKHALO2VERIFYWITHVK` opcode, that advances a trustless Ethereum beacon-chain **light-client
head** and marks finalized **execution block hashes** canonical. `finalizeDeposit` then only
accepts a deposit whose proven `blockHash` is one the light client already finalized.

**Non-goals.**
- Not a full Casper-FFG validator-set client (~1M validators — infeasible in-circuit). We use the
  **Altair sync-committee** protocol (512 signers), the industry-standard zk light-client base
  (Succinct Telepathy/Helios, Electron, etc.).
- No new TVM opcode — the light-client proof is another BN254 SHPLONK proof; only a new **VkBlob**
  is registered on AN.
- Does not change the AN→ETH direction (already committee-verified via BK BLS).

## 3. Trust model — what remains, what becomes trustless

**Irreducible genesis anchors** (set once at AN-contract deploy = weak-subjectivity checkpoint):

1. `genesis_validators_root` (32 B) — pins the chain identity, feeds the signing domain.
2. Initial `current_sync_committee_root` (SSZ hash-tree-root of the 512-pubkey committee) at a
   recent finalized period.
3. Fork schedule (fork_version per epoch) — needed for the domain and for fork-versioned gindices.

Everything after the anchor is **trustless**: each accepted update is a ZK proof that the *current*
committee (already trusted transitively from genesis) signed a beacon header, that this header is
finalized, that it carries the given execution block hash, and (on period boundaries) that it
attests the *next* committee.

**Residual, documented, non-trust properties:**
- **Weak subjectivity / long-range.** If the light client falls behind > ~1 sync-committee period
  (~27 h) with no update, it cannot safely catch up (standard light-client limitation). → relayer
  **liveness SLA + monitoring**; a manual re-anchor procedure to a fresh WS checkpoint.
- **Sync-committee honest-supermajority** for the *signing* period (same assumption Ethereum's own
  light clients make). We require ≥ 2/3 participation in-circuit.
- **Finality lag.** "Finalized" = 2 epochs (~12.8 min) behind head. Deposits confirm with that
  inherent latency (acceptable; today's attester used confirmation depth anyway).

## 4. Architecture

Three moving parts, mirroring how real zk light clients are deployed:

```
 Beacon node ──LightClientUpdate──▶ eth-light-client-prover (Halo2/BN254)
   (/eth/v1/beacon/light_client/*)        │ proof + public inputs
                                           ▼
                        AN: EthBeaconLightClient (tvm-solidity)
                        ZKHALO2VERIFYWITHVK(vk, PI, proof)
                          advances finalized head + committee,
                          records finalized exec blockHash canonical
                                           │
 deposit-prover proof ──────────────────▶ finalizeDeposit:
                        require blockHash ∈ light-client-finalized set
```

**Design choice — separate header-oracle circuit, two proofs cross-checked on-contract**
(vs one monolithic circuit or in-circuit aggregation):

- The light-client update circuit is expensive and cadence-driven (~1 update / finality period,
  or per-deposit at most a few/hour). Coupling it to every deposit would re-prove BLS on each
  deposit — wasteful.
- Keep `deposit-prover` ~unchanged; the AN contract binds `deposit.blockHash` to the light-client's
  finalized set. This **replaces the attester write path** (`setAcceptedL1Block`) with a ZK write
  path — same `_acceptedL1Block`-style gate, now trustless.
- In-circuit aggregation (via `bridge-evm-aggregator`/snark-verifier) is a **later optimization**
  (M-agg), not required for correctness.

## 5. Light-client update circuit — spec

Input = one beacon `LightClientUpdate` (Altair+; Capella/Deneb/Electra header shapes):

```
attested_header            (BeaconBlockHeader + ExecutionPayloadHeader + execution_branch)
finalized_header           (same shape)
finality_branch            (Merkle branch: finalized root ∈ attested.state_root)
next_sync_committee         + next_sync_committee_branch   (period-rotation hop, optional per update)
sync_aggregate             (sync_committee_bits[512] + BLS G2 signature)
signature_slot
```

**Constraints (all in BN254 circuit; BLS12-381 ops emulated via halo2-ecc):**

1. **Committee selection.** For each set bit in `sync_committee_bits`, select the pubkey from the
   **active committee** whose SSZ root == on-chain `current_sync_committee_root` (bind the 512 G1
   pubkeys to that root via SHA-256 hash-tree-root). Enforce `popcount(bits) ≥ ⌈2·512/3⌉ = 342`.
2. **Aggregate pubkey.** G1 sum of selected pubkeys → `agg_pk`.
3. **Signing root + domain.** `domain = compute_domain(DOMAIN_SYNC_COMMITTEE=0x07000000,
   fork_version(signature_slot), genesis_validators_root)`; `signing_root =
   hash_tree_root(attested_header.beacon) mixed with domain`. (SHA-256 chip.)
4. **BLS verify.** `e(agg_pk, hash_to_curve_G2(signing_root)) == e(G1, signature)` — reuse the
   gosh BLS12-381 chip (`halo2-lib-zkevm-sha256-and-bls12-381`, RFC-9380 hash-to-curve).
   **Add G1/G2 subgroup checks** (open audit finding BLS-1/FORK-2 — must not carry over here).
5. **Finality branch.** SSZ Merkle-verify `hash_tree_root(finalized_header.beacon)` ∈
   `attested_header.beacon.state_root` at fork-versioned `FINALIZED_ROOT_GINDEX`.
6. **Execution branch.** SSZ Merkle-verify `finalized_header.execution.block_hash` (the L1
   `blockHash` deposits reference) ∈ `finalized_header.beacon` at fork-versioned
   `EXECUTION_PAYLOAD_GINDEX`.
7. **Committee rotation (period boundary).** If update advances the period, SSZ Merkle-verify
   `next_sync_committee` ∈ `attested_header.beacon.state_root` at `NEXT_SYNC_COMMITTEE_GINDEX`;
   output `next_sync_committee_root`.
8. **Monotonicity guard** (enforced on-contract, echoed as PI): `finalized_slot >
   stored_finalized_slot`.

**Public inputs / outputs (VkBlob-driven, exact count TBD in M1):**

```
[ genesis_validators_root_hi/lo,            # anchor echo (contract pins)
  active_sync_committee_root_hi/lo,         # must == on-chain current root
  next_sync_committee_root_hi/lo,           # 0 if no rotation this update
  finalized_slot,
  finalized_exec_block_hash_hi/lo,          # the canonical L1 blockHash to record
  participation ]                           # optional, for policy
```

**Fork-awareness.** Gindices for finality/next-committee/execution branches shift across
Altair→Bellatrix→Capella→Deneb→Electra. Circuit takes `fork_version` and selects the correct
gindex set; support at least the **current mainnet fork + the next scheduled fork** and the
transition. (Design item; do not hardcode a single fork.)

## 6. Reused assets (do not rebuild)

| Need | Asset |
|---|---|
| BLS12-381 pairing, hash-to-curve (RFC 9380) | `halo2-lib-zkevm-sha256-and-bls12-381` (gosh fork of halo2-lib) + `gosh-halo2-crypto-lib/bls-verification` |
| SHA-256 (SSZ hash-tree-root, branches) | `gosh-halo2-crypto-lib/sha256-chip` (or axiom-eth keccak/sha) |
| Poseidon (internal commitments) | `bridge-poseidon` / crypto-lib |
| BN254 Halo2 SHPLONK + VkBlob export | `deposit-prover` prover/export tooling (`export_vk_blob.rs`, `export_blake2b_proof.rs`) |
| Native AN verify | existing `ZKHALO2VERIFYWITHVK` (0xC7 0x4A) — **no opcode change**, new VkBlob only |
| SRS | chain KZG ceremony (`params/kzg_bn254_*.srs`); downsize as in `deposit-prover/examples/downsize_srs.rs` |
| Aggregation (optional M-agg) | `crates/bridge-evm-aggregator` (snark-verifier) |

**Note:** all BLS12-381 arithmetic is **non-native** inside the BN254 circuit (halo2-ecc). Expect
K ≈ 21–22, multi-minute prove, large PK — plan HW (n14) accordingly.

## 7. On-AN contract (`EthBeaconLightClient.sol`, tvm-solidity)

State: `genesisValidatorsRoot`, `currentSyncCommitteeRoot`, `nextSyncCommitteeRoot`,
`finalizedSlot`, `finalizedExecBlockHash`, `mapping(uint chainId => mapping(uint blockHash => bool)) _finalizedL1Block`, `VK_BLOB_LIGHTCLIENT`.

- `applyUpdate(vk, publicInputs, proof)`: `ZKHALO2VERIFYWITHVK` → check `active_root ==
  currentSyncCommitteeRoot`, `finalized_slot > finalizedSlot`, `genesis_root` matches; on success
  advance head, record `_finalizedL1Block[1][exec_block_hash]=true`, rotate committee if
  `next_root != 0`.
- `finalizeDeposit` binding: **replace** `require(_acceptedL1Block[chainId][blockHash])`
  (attester) with `require(EthBeaconLightClient._finalizedL1Block[chainId][blockHash])`. Delete
  `setAcceptedL1Block`/`setL1Attester`/`_l1AttesterPubkey`.
- Genesis anchors set in constructor / one-shot `initAnchor(...)` (weak-subjectivity checkpoint).

## 8. Relayer (`eth-light-client-relayer`, or extend `deposit-relayer-daemon`)

- Poll a beacon node: `/eth/v1/beacon/light_client/finality_update` and `.../updates?start_period`.
- Drive `eth-light-client-prover` (subprocess, like `SubprocessProofGenerator`) → BN254 proof +
  PI → `EthBeaconLightClient.applyUpdate`.
- Cadence: at least once per finality period boundary (committee rotation) + on demand so any
  deposit's target block gets finalized promptly. Idempotent (skip if `finalized_slot` not
  advanced), backoff, state.json cursor — same shape as existing daemons.
- Liveness SLA + alert if lag approaches 1 period (WS safety).

## 9. Deposit-side changes (minimal)

- `deposit-prover`: **no circuit change** (already exposes `blockHashHigh/Low`). Optionally also
  expose the beacon `finalized_slot`/state-root binding if we later want single-proof aggregation.
- `deposit-relayer-daemon`: before submit, ensure the target block is light-client-finalized
  (poll `EthBeaconLightClient`), else wait/trigger a light-client update. Decode unchanged.

## 10. Phasing, deliverables, effort

Estimates assume 1 strong ZK engineer reusing the existing chips; parallelizable across 2.

| Phase | Deliverable | Est. |
|---|---|---|
| **M0 — spec & fixtures** | Freeze PI layout, gindex/fork table, WS-anchor policy. Capture real `LightClientUpdate` fixtures (mainnet + Holešky/Hoodi) via beacon REST + consensus-spec-tests vectors. | 1–1.5 wk |
| **M1 — BLS core** | In-circuit committee selection + 2/3 popcount + G1 aggregate + BLS12-381 pairing + hash-to-curve, **with subgroup checks**. MockProver green on real committee/signature. | 3–4 wk |
| **M2 — SSZ + branches** | SHA-256 hash-tree-root, signing-root+domain, finality/execution/next-committee Merkle branches (fork-versioned gindices). | 2–3 wk |
| **M3 — full update circuit** | Wire M1+M2 into one `EthCircuitInstructions` circuit; public IO; real-prover verify on fixtures; VkBlob export; SRS alignment (chain ceremony). | 2 wk |
| **M4 — AN contract** | `EthBeaconLightClient.sol` + `applyUpdate` + `finalizeDeposit` binding; register VkBlob; unit/integration tests (tvm-cli, mirror `test_usdcbridge_*`). | 2–3 wk |
| **M5 — relayer** | `eth-light-client-relayer`: beacon fetch → prove → submit; cadence/liveness; state persistence; `--dry-run`. | 2 wk |
| **M6 — E2E on testnet** | Holešky/Hoodi beacon → shellnet: advance head across ≥ 2 committee rotations; a real deposit finalizes only after its block is light-client-finalized; negative test (fabricated block rejected). | 2 wk |
| **M-agg (optional)** | Aggregate light-client + deposit into one proof (snark-verifier) if we want atomic single-proof deposits. | 3–4 wk |
| **M-audit** | Internal + external audit of the new circuit + contract; close BLS subgroup finding. | 3–4 wk |

**Critical path (correctness-complete, attester removed):** M0→M6 ≈ **3–4 months** solo, ~**2–3
months** with two engineers. M-agg and hardening extend from there.

## 11. Testing

- **Vectors:** ethereum/consensus-spec-tests `light_client/*` (deterministic, fork-tagged) — the
  gold source for gindices/domain across forks.
- **Real fixtures:** captured mainnet + testnet `LightClientUpdate`s (including a period-rotation
  update and a non-rotation finality update).
- **MockProver** per component (M1/M2), **real-prover** end-to-end (M3).
- **Negative:** wrong committee root, < 2/3 participation, tampered branch, non-subgroup pubkey,
  self-fabricated exec header (must fail on-contract binding).
- **Contract:** `applyUpdate` monotonicity/rotation/replay; `finalizeDeposit` rejects
  non-finalized blockHash.
- **CI:** add the new crate to a nightly/heavy job (like deposit-prover, out of the fast lane).

## 12. Risks & open questions

- **Prover cost.** Non-native BLS12-381 over BN254 is the dominant cost (K≈21–22, minutes, big PK).
  Mitigation: reuse gosh chips, prove on n14, cache PK, batch multiple finality updates rarely.
- **Fork transitions.** Gindices/header shape change per fork; must support the active fork + next.
  Open: how aggressively to pre-support Electra/Fulu gindices.
- **G1/G2 subgroup checks** (audit BLS-1/FORK-2): mandatory here — a wrong-subgroup pubkey/sig
  could forge. Must be constrained, not assumed.
- **Weak subjectivity / liveness.** Relayer must not fall > 1 period behind. Open: automated
  re-anchor UX + who signs the WS checkpoint bump (governance).
- **Two-proof vs aggregate.** MVP = contract cross-check (blockHash ∈ finalized set). Open: whether
  UX/atomicity justifies M-agg.
- **SRS/opcode.** Confirm VkBlob v2 RLC path handles the new PI count (VK-driven, expected fine);
  re-bench opcode gas for the larger proof.
- **`circuit_shape`** (Base vs Rlc): light-client circuit uses SHA-256+BLS; decide RlcCircuitBuilder
  (shape=1) vs BaseCircuitBuilder (shape=0) for the VkBlob header.

## 13. Rollout / migration from the attester MVP

1. Ship M0–M5 behind an unused contract (no deposit binding yet).
2. Run the relayer in shadow: advance the light-client head on shellnet, compare its finalized set
   against the attester's `_acceptedL1Block` (should agree).
3. Flip `finalizeDeposit` to require the light-client set; keep attester as break-glass one release.
4. Remove attester surface (`setAcceptedL1Block`/`setL1Attester`/`_l1AttesterPubkey`) entirely.

## 14. References

- Ethereum Altair light-client spec: `specs/altair/light-client/*` (sync-committee protocol,
  gindices, `compute_domain`, `hash_tree_root`).
- consensus-spec-tests `tests/.../light_client/`.
- Existing prior art: Succinct Telepathy / SP1-Helios, Electron zk light client.
- In-repo: `deposit-prover/src/circuit_v2.rs` (12-PI layout, `blockHashHigh/Low` at 9/10),
  `docs/zkhalo2verifywithvk_reference.md`, `docs/bridge_deposit_chain_binding_fix_proposal_2026_07_20.md`,
  AN-side `USDCBridge`/`eccUSDCBridge` `_acceptedBlockHash` (attester MVP being replaced).
