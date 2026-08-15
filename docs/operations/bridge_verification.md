# Acki Nacki Bridge — Verification Guide (v2)

> **v2 update (2026-05-10).** Rewritten for the four-circuit architecture. Sections §5 and §6
> have been retargeted from the retired `LayerHashBridge.sol` (deleted in Phase 4.2) to
> `AckiNackiBridge.verifyBlock`. New §6.5 enumerates cross-circuit invariants (CC-#).
> Read `docs/four_circuit_architecture.md` first for context.
>
> **v2.1 update (2026-05-17, Phase 4.3).** The DEP-# invariants (§4) have been rewritten:
> the legacy refund-style `withdraw(...)` together with the ETH-side `Groth16DepositVerifier`
> + `IAckiNackiVerifier` chain were retired (Decision Log 2026-05-17 in
> `docs/an_partner_integration_plan.md`). Deposit-event verification now happens **on the
> AN side** via the future `VERHALO2SHPLONK` TVM opcode. Until that opcode lands, deposit
> verification runs off-chain at the AN-side relayer. Old DEP-3 / DEP-5 / DEP-6 (which talked
> about double-spend nullifiers, recipient, oracle on the ETH side) are reframed in §4 as
> AN-side responsibilities (DEP-N-# series), and the ETH-side rows are pruned to the surface
> that actually still exists in `AckiNackiBridge.sol`.

This article is the operational checklist for proving that the bridge is correct: every deposit on Ethereum maps to at most one credit on Acki Nacki, every Acki Nacki block update reflects a real BLS-attested block whose `block_id` is bound across two ZK proofs, and no off-path actor can forge state. It complements two narrower documents:

- `aave_integration.md` — verifies the AAVE V3 yield bolt-on.
- `layer_hashes_circuit_audit.md` — audits the partner's Halo2 circuits.

This doc is the **end-to-end view**. Every section follows the pattern: *what the property is → why it holds → how to verify it ran correctly today*.

---

## 1. Scope

**In scope** (everything the bridge contracts and ZK pipeline are responsible for):

- ETH → AN: `AckiNackiBridge.deposit()` (ETH side), the Halo2 deposit-prover proof (off-chain), and the future `VERHALO2SHPLONK`-based AN-side `TokenBridge.finalizeDeposit(...)` + nullifier. The legacy refund-style `withdraw(...)` was retired in Phase 4.3.
- AN → ETH: `AckiNackiBridge.verifyBlock()` and the tuple of two cross-circuit-bound ZK proofs (Circuit 1A or 1B + Circuit 2).
- BK-set commitment lifecycle: rotation runs today through the interim `applyBkSetUpdate` (attestation + SHA-256 opening of the `(L2, L3)` leaf pair, §6); Circuit 3 (`bkSetUpdateProof`) is still owed for a statement about the rotation's contents. The legacy 7-day timelocked owner fallback (`LayerHashBridge.proposeBkSetCommitment`) was **retired in Phase 4.2** and no owner path replaced it.
- Block-hash oracle (`AxiomBlockHeaderOracle` + the EVM `blockhash()` opcode) — currently unused by the public surface; preserved for a future burn-proof / ETH-side withdrawal flow.
- Cross-cutting properties: ZK verifier integrity, reentrancy, access control, fork resistance.
- AAVE V3 integration: covered in detail in `docs/aave_integration.md`; this doc only summarises how it interacts with the rest.

**Out of scope** (verified elsewhere):

- The internal soundness of the partner's Halo2 circuits (see the audit). We treat the per-circuit gnark-wrapped Groth16 verifier as the trusted boundary.
- Acki Nacki node-level consensus and BK economic security.
- Off-chain relayer correctness (it can be byzantine — a wrong relayer cannot forge state, only stall).

---

## 2. Bridge at a Glance (v2)

```
            Ethereum                                          Acki Nacki
            ────────                                          ──────────
deposit()  ─►  Deposit event  ─►  deposit-prover (Halo2)  ─►  AN-side TokenBridge
                                  ─► (no on-chain ETH-side ZK adapter)
                                  ─► AN-side VERHALO2SHPLONK opcode (planned)
                                  ─► tokens minted on AN, depositId nullified

verifyBlock(finType, attestationProof, layerHashesProof,    ◄── AN consensus
            blockId, bkSetCommitment,                          (Primary ≥ 2/3
            blockSeqNo, numLayers,                              or Fallback > 1/2)
            layerHashes, prevMaxLevelLayerHash)
        ◄── 1A / 1B + 2 (256 B each, gnark Groth16)
            cross-bound by blockId & bkSetCommitment
            chain-anchored to storedPrevMaxLevelLayerHash

verifyBkSetUpdate(...)                                       ◄── Phase 1.C, planned
        ◄── Circuit 3 proves (oldCommit, newCommit) hand-off    (Halo2 stub upstream)

(Future, post-Phase 4.3) burn-based ETH-side withdrawal will be designed alongside a
burn-proof circuit; the legacy v1 refund-style `withdraw(depositId, ...)` is gone.
```

The single trust anchor is now:

- `AckiNackiBridge.sol` — custodies user ETH (with optional AAVE yield), AND anchors Acki Nacki state via the `verifyBlock` surface. Phase 4.2 removed the legacy `LayerHashBridge.sol` and folded its responsibilities into this one contract. Phase 4.3 retired the legacy refund-style `withdraw()` and the associated ETH-side deposit-verifier chain.

Plumbing (current AN→ETH state): `PrimaryVerifier`, `FallbackVerifier`, `LayerHashesMovementVerifier` (each calling its own `*Groth16VerifierGenerated.sol`). `AxiomBlockHeaderOracle` is in the tree but currently unused by the public surface. All AN→ETH adapters sit between `AckiNackiBridge` and the proof artefacts.

---

## 3. Verification Levels

| Level | What it answers | Cost |
|---|---|---|
| L0 — code review | Are the invariants present in source? | minutes |
| L1 — `forge build` + `forge fmt --check` | Does it compile and conform to style? | seconds |
| L2 — `forge test` (mocks only) | Do unit/fuzz/E2E tests pass? | ~5 s — 1 min |
| L3 — real-proof E2E | Do real Halo2 → Groth16 → on-chain proofs verify? | already cached: < 1 min; from scratch: ~33 min keygen+proving |
| L4 — fork test (recommended pre-deploy) | Does the bridge cooperate with real mainnet AAVE / Axiom? | minutes per scenario |
| L5 — post-deployment `cast` checks | Were the immutables wired correctly on-chain? | seconds |
| L6 — runtime monitoring | Has any invariant drifted? | continuous |

Run **L0–L3** for every PR. Run **L4–L5** for every deployment. Run **L6** continuously in production.

---

## 4. ETH → AN Deposit (Phase 4.3 rewrite)

### 4.1 What must hold

ETH-side (live in `AckiNackiBridge.sol`):

| Property | Statement |
|---|---|
| **DEP-1** | Every successful `deposit()` emits a `Deposit(depositId, sender, amount, anWorkchain, anAccount, timestamp)` with a unique monotonic `depositId`; `anAccount == 0` reverts (`InvalidAnAccount`). |
| **DEP-2** | `MAX_DEPOSIT_AMOUNT = type(uint64).max` is enforced; `amount == 0` reverts (`InvalidAmount`). |
| **DEP-3** | `treasuryBalance` is incremented by exactly the `amount` pulled via `usdc.transferFrom` on every successful `deposit()`. |
| **DEP-4** | `deposit()` is `nonReentrant`; no external calls are made inside it (yield routing to AAVE is owner-triggered separately via `supplyToAave`). |

AN-side (planned, lands with the future `VERHALO2SHPLONK` opcode + `TokenBridge.finalizeDeposit`):

| Property | Statement |
|---|---|
| **DEP-N-1** | A successful `finalizeDeposit(halo2Proof, publicInputs, vk)` requires the Halo2 SHPLONK proof to verify natively under the immutable VK, binding `(depositId, sender, amount, bridgeAddr, anWorkchain, anAccount, blockHash, promiseCommit)` to a real `Deposit` event in the receipt trie of an Ethereum block. |
| **DEP-N-2** | `publicInputs[3] == ETH_BRIDGE_ADDRESS_FR` — wrong-bridge proofs revert. |
| **DEP-N-3** | The per-`depositId` nullifier in `TokenBridge` is set before any token mint; replay reverts. |
| **DEP-N-4** | `finalizeDeposit` credits a deposit only if the proof-bound source block hash sits in the on-chain anchor set `_acceptedBlockHash[chainId]`. Admission is off-proof: the block must be canonical at its number on an independent node and ≥ 64 confirmations deep. |
| **DEP-N-5** | One source deposit mints at most once, and deposits on different chains never share a voucher: the replay identity is `(srcChainId, depositId, contractAddr, dappId)`, and all three places that compute it agree. |
| **DEP-N-6** | Per chain, cumulative minted amount stays within `_mintCapByChain[chainId]` when that cap is non-zero (bound on damage; overshoot possible by the amount of concurrently in-flight deposits). |
| **DEP-N-5** | The AN recipient is a **proven** public input (`anWorkchain`, `anAccountHigh`, `anAccountLow`): `finalizeDeposit` credits `anWorkchain:(anAccountHigh<<128 \| anAccountLow)` reconstructed from the proof, never an EVM address or a relayer-supplied hint. The circuit `constrain_equal`s these to the RLP-parsed `Deposit` event data words. |

### 4.2 Why each property holds

ETH-side:

- **DEP-1**: `depositCounter++` is `uint256` and only increments. At 1 deposit per second it would take ~10⁷⁰ years to overflow. There is no decrement path.
- **DEP-2**: explicit `require(msg.value > 0 && msg.value <= MAX_DEPOSIT_AMOUNT, ...)` at the top of `deposit()`.
- **DEP-3**: `treasuryBalance += msg.value` is the only mutation in `deposit()`; there is no admin path that decrements it without a balanced AAVE supply / withdraw.
- **DEP-4**: `nonReentrant` modifier; the function body has no `call`/`transfer`/`send`. Yield routing is an owner-only separate flow (`supplyToAave`).

AN-side (post-`VERHALO2SHPLONK`):

- **DEP-N-1**: the Halo2 SHPLONK verifier inside the TVM opcode accepts only proofs whose transcript matches `vk` and whose public inputs match the supplied vector. The circuit's internal constraints chain receipt RLP, MPT inclusion, log selector + topics + data, block-hash binding, and the keccak coprocessor commitment.
- **DEP-N-2**: explicit `require(publicInputs[3] == ETH_BRIDGE_ADDRESS_FR, "wrong bridge contract");` in `TokenBridge.finalizeDeposit`.
- **DEP-N-3**: `nullifier[depositId] = true` is set before `_mintTo(...)`; second call reverts.
- **DEP-N-4**: half contract, half trust assumption. The contract half is `require(_acceptedBlockHash[f.chainId][_parseBlockHash(publicInputs)], ERR_UNKNOWN_BLOCK)` in `USDCBridge.finalizeDeposit` (`acki-nacki` `7992ce26`), mirroring `AckiNackiBridge._knownAnchors` in the AN→ETH direction. No circuit can supply the other half — "this header is canonical" is a statement about Ethereum consensus, not about the witness (BC-D01) — so the anchor set is populated from outside the proof, by a key. Two writers exist: the owner (`setAcceptedBlockHash`) and a threshold of attesters (`attestBlockHash`, `1fb5b28c`). **Which one is the actual trust root is a deployment fact, not a code fact**: while `getAttesterConfig().ownerAnchorsEnabled` is true it is the owner key alone regardless of how many attesters are registered, and `disableOwnerAnchors()` (one-way) is what makes it M-of-N. Either way the writer can mint by admitting a hash from a chain that never existed; an ETH light client is the long-run target for L1, and L2s stay on attesters until someone verifies their settlement to L1. `scripts/deposit_anchor_params.py --verify` enforces the independence + confirmation-depth obligations before printing the call arguments. Full analysis: `docs/reviews/deposit_circuit_audit_2026-08-03.md` §1.

- **DEP-N-5** (new 2026-08-04, `acki-nacki` `21a781e7`): the anti-replay identity behind the deterministic `DepositVoucher` address is `(srcChainId, depositId, contractAddr, dappId)`. `srcChainId` is load-bearing rather than decorative: without it, two allowlisted chains sharing a bridge address (CREATE2, or the same deployer nonce) collide on their per-chain `depositId` counters, and the second chain's deposit is silently swallowed as a replay — funds in, nothing minted, no refund path. Checked mechanically across all three copies of the signature by `scripts/check_voucher_abi_consistency.py`, because a divergence between bridge and voucher aborts the voucher constructor on cell underflow (`exit_code 9`) rather than failing to compile.

- **DEP-N-6** (new 2026-08-04, `acki-nacki` `993af815`): `_mintedByChain[chainId] <= _mintCapByChain[chainId]` for every chain with a non-zero cap — a bound on damage, not a soundness invariant. Enforced in `finalizeDeposit` (so an over-cap deposit reverts while staying retryable) and tallied in `confirmDeposit` (so replays consume no headroom), which means N in-flight deposits can overshoot by their combined amount. Read it as "a forgery cannot drain more than this per chain", not as an exact ceiling.
- **DEP-N-5**: the deposit circuit (`deposit-prover/src/circuit_v2.rs`) exposes `anWorkchain`/`anAccountHigh`/`anAccountLow` as public inputs #4–#6 and constrains them equal to the RLP-parsed `Deposit` data words 1–2 (the same Phase-0/Phase-1 binding used for `amount`). `TokenBridge.finalizeDeposit` therefore credits a destination that is part of the proof, not trusted from the relayer (landed 2026-06-02; `num_instance` 7→10).

### 4.3 How to verify

#### L0 — code review

Open `contracts/ethereum/src/AckiNackiBridge.sol` and confirm by inspection:

```bash
grep -n "depositCounter\|treasuryBalance\|MAX_DEPOSIT_AMOUNT\|emit Deposit" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

Confirm there is **no** `function withdraw(` and **no** `IAckiNackiVerifier` import (both retired in Phase 4.3):

```bash
! grep -E "function withdraw\(|IAckiNackiVerifier" contracts/ethereum/src/AckiNackiBridge.sol
```

#### L2 — tests

```bash
cd contracts/ethereum
forge test --match-contract "FuzzAckiNackiBridgeDepositTest|AckiNackiBridgeAaveTest" -vv
```

Expected ETH-side mappings:

| Property | Tests that enforce it |
|---|---|
| DEP-1 | `testDeposit`, `testDepositMultiple`, `testDepositCounterIncrement`, `testFuzz_MultipleDepositsInvariant` (in `FuzzAckiNackiBridgeDepositTest`) |
| DEP-2 | `testFuzz_DepositAmountInvariants`, `testFuzz_DepositInvalidAmountReverts` |
| DEP-3 | `testFuzz_DepositAmountInvariants` (treasury invariant) |
| DEP-4 | `AckiNackiBridgeAaveTest::*` — exercises that yield routing is a separate owner-only path |

AN-side DEP-N-# are out-of-scope for the Foundry suite; they will be enforced by `tvm-sdk` tests once the opcode lands and by AN-side `TokenBridge` tests once that contract exists. Track under `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 + Phase 8.

#### L3 — real Halo2 proof (deposit-prover, AN-side off-chain)

```bash
cd deposit-prover
cargo test --release -- --nocapture test_real_data
```

This runs the real Halo2 round-trip (deposit-prover Halo2 SHPLONK prove + verify). Until `VERHALO2SHPLONK` lands, this is the closest thing to "real proof" verification of the ETH→AN deposit path.

#### L4 — fork test (block hash oracle)

```bash
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest
```

Currently exercises Axiom V2 Core against a recent mainnet block; preserved for the future burn-proof flow.

#### L5 — post-deployment

```bash
cast call $BRIDGE "blockHeaderOracle()(address)"        # preserved but unused by public surface
cast call $BRIDGE "depositCounter()(uint256)"
cast call $BRIDGE "treasuryBalance()(uint256)"
cast call $BRIDGE "MAX_DEPOSIT_AMOUNT()(uint256)"        # 100000000000000000000 (100 ether)
```

(The legacy `verifier()` getter is gone — Phase 4.3.)

---

## 5. AN → ETH Block Updates (`verifyBlock`)

### 5.1 What must hold

| Property | Statement |
|---|---|
| **LH-1** | Every successful `verifyBlock(...)` is preceded by **two** Groth16 proofs: one attestation proof (Circuit 1A or 1B, depending on `finType`) and one layer-hashes-movement proof (Circuit 2). |
| **LH-2** | `bkSetCommitment` (the argument flowing into both verifier calls) equals the bridge's stored `storedBkSetCommitment` — proofs cannot be back-dated to a stale committee. |
| **LH-3** | `prevMaxLevelLayerHash` (the argument flowing into Circuit 2) equals the bridge's `expectedPrevAnchor(numLayers)` — the **chain anchor** derived from the per-layer rolling windows via `pick = min(numLayers, highestActiveLayer)`. For the very first call after deployment this falls back to the immutable `storedPrevMaxLevelLayerHash` genesis seed (typically 0). Storage v2.0 (2026-08-04): the flat mutable `storedPrevMaxLevelLayerHash` was removed; `expectedPrevAnchor` sources from `_layerWindows[pick]` instead. |
| **LH-4** | `numLayers ∈ [1, MAX_LAYER_HASHES]` (i.e., 1..10) — out-of-range counts revert with `InvalidNumLayers`. |
| **LH-5** | `layerHashes[i] == 0` for every `i ≥ numLayers`. The bridge will revert with `LayerHashTailNonZero(i)` — the unused tail must not carry silent garbage. |
| **LH-6** | `blockSeqNo > storedLastSeenBlockSeqNo` — strict monotonicity. Replay attempts revert with `BlockSeqNoNotMonotonic`. |
| **LH-7** | Each verifier adapter (`PrimaryVerifier`, `FallbackVerifier`, `LayerHashesMovementVerifier`) returns `false` if `proof.length != 256`, and never reverts on invalid proofs — it normalises gnark reverts to `false` via try/catch so `verifyBlock` produces a clean `AttestationProofRejected` / `LayerHashesProofRejected` revert. |
| **LH-8** | Storage v2.0 (2026-08-04): after a successful call, `_layerWindows[L]` gets `layerHashes[L-1]` appended for every non-zero `L ∈ [1, numLayers]` (rolling window semantics); `storedLastSeenBlockSeqNo = blockSeqNo`. There is no admin path that bypasses this. The flat `storedNumLayers` / `storedLayerHashes` / mutable `storedPrevMaxLevelLayerHash` writes are gone — indexers observe per-layer heads via `getLatestPerLayer()` and query the next-block anchor via `expectedPrevAnchor(numLayers)`. |
| **LH-9** | If any of the three verifier slots is `address(0)` at construction, `verifyBlock` reverts with `VerifyBlockDisabled`. The deposit/AAVE surface remains fully functional in that mode. |

### 5.2 Why each property holds

- **LH-1**: `verifyBlock` directly calls both verifier adapters; the verifier addresses are `immutable` and set in the constructor's `VerifyBlockConfig`. There is no setter that can replace them post-deployment.
- **LH-2**: `bkSetCommitment != storedBkSetCommitment ⇒ revert BkSetCommitmentMismatch`. The user-supplied `bkSetCommitment` is the same value passed to **both** verifier calls — the partner's circuits commit to it as a public input (offset 96 of the envelope tree), so a wrong value would also fail the gnark pairing.
- **LH-3**: explicit `prevMaxLevelLayerHash != _expectedPrevAnchor(numLayers) ⇒ revert PrevAnchorMismatch`. Storage v2.0 (2026-08-04): `_expectedPrevAnchor` picks the head of `_layerWindows[min(numLayers, highestActiveLayer)]`; if `highestActiveLayer == 0` (very first call after deployment) it falls back to the immutable `storedPrevMaxLevelLayerHash` genesis seed.
- **LH-4**: explicit `if (numLayers == 0 || numLayers > MAX_LAYER_HASHES) revert InvalidNumLayers(numLayers)`.
- **LH-5**: explicit `for (i = numLayers; i < MAX_LAYER_HASHES; i++) if (layerHashes[i] != 0) revert LayerHashTailNonZero(i)` — guards against silent garbage in unused slots.
- **LH-6**: explicit `blockSeqNo <= storedLastSeenBlockSeqNo ⇒ revert BlockSeqNoNotMonotonic`.
- **LH-7**: each adapter normalises a failing verifier call to `false` rather than bubbling a revert. The production SHPLONK adapters (`PrimaryAggregatorVerifier.sol`, `FallbackAggregatorVerifier.sol`, `LayerHashesAggregatorVerifier.sol`) wrap the aggregator-Yul `staticcall`; the retained 1A/2 gnark Groth16 test adapters (`PrimaryVerifier.sol`, `LayerHashesMovementVerifier.sol`) wrap `groth16Verifier.verifyProof(...)` in try/catch. (The 1B `FallbackVerifier.sol` Groth16 adapter was retired 2026-06-22.)
- **LH-8**: state writes happen *only* after both verifiers return `true` and all anchor checks pass. CEI-clean: there are no external calls between the writes and `BlockVerified` emission.
- **LH-9**: feature gate at the top of `verifyBlock`: any zero verifier address ⇒ revert.

### 5.3 How to verify

#### L0 — read the function

`AckiNackiBridge.verifyBlock` is ~85 lines. Verify in order (top-down, no skips):

1. Feature-gate the three verifier slots (LH-9).
2. Shape & range checks: `numLayers` in `[1, MAX_LAYER_HASHES]` (LH-4); `layerHashes` tail (LH-5).
3. Anchor checks against stored state: `bkSetCommitment` (LH-2), `blockSeqNo` (LH-6), `prevMaxLevelLayerHash` (LH-3).
4. Crypto: route to `primaryVerifier` or `fallbackVerifier` based on `finType`, then call `layerHashesVerifier`. Both must return `true` (LH-1, LH-7).
5. Effects (CEI): commit `storedLastSeenBlockSeqNo` and call `_appendLayerHashes(numLayers, layerHashes, blockSeqNo)` to push each non-zero `layerHashes[L-1]` into `_layerWindows[L]` (LH-8). Storage v2.0 (2026-08-04): the flat `storedNumLayers` / `storedLayerHashes` / mutable `storedPrevMaxLevelLayerHash` writes were removed.
6. Emit `BlockVerified(blockId, blockSeqNo, finType, numLayers)`.

```bash
grep -n "verifyBlock\|stored\(BkSet\|LastSeen\|NumLayers\|LayerHashes\|PrevMaxLevel\)" \
  contracts/ethereum/src/AckiNackiBridge.sol
```

#### L2 — tests

```bash
cd contracts/ethereum
forge test --match-contract "AckiNackiBridgeVerifyBlockTest|AckiNackiBridgeRelayerLoopTest|PrimaryVerifierTest|FallbackVerifierTest|LayerHashesMovementVerifierTest" -vv
```

| Property | Tests |
|---|---|
| LH-1 | `AckiNackiBridgeVerifyBlockTest::testHappyPathPrimary`, `…::testHappyPathFallback` |
| LH-2 | `…::testRevertOnBkSetCommitmentMismatch`, `…::testRevertOnTamperedBkSetInProof` |
| LH-3 | `…::testRevertOnPrevAnchorMismatch`, `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_anchorMismatch_reverts` |
| LH-4 | `…::testRevertOnZeroNumLayers`, `…::testRevertOnNumLayersAboveMax` |
| LH-5 | `…::testRevertOnLayerHashTailNonZero` |
| LH-6 | `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_replaySameSeqNo_reverts`, `…::test_relayerLoop_lowerSeqNo_reverts` |
| LH-7 | `PrimaryVerifierTest::testVerifyInvalidProofLength`, `…::testVerifyRevertingGroth16`; same for Fallback / LayerHashesMovement |
| LH-8 | `AckiNackiBridgeRelayerLoopTest::test_relayerLoop_advancesAcrossTenBlocks`, `…::test_relayerLoop_restartRecoversFromState` |
| LH-9 | `AckiNackiBridgeVerifyBlockTest::testRevertWhenVerifyBlockDisabled` |

#### L3 — real proofs (single-block bound fixture)

```bash
forge test --match-test "testHappyPathPrimary" -vv
```

Drives `verifyBlock` end-to-end with a real bound proof set generated by:

```bash
cargo run -p bridge-snark-utils --bin export-bound-block-proofs --release
```

The export binary writes `bound_scenario.json` plus `proof_*.bin` into `crates/bridge-snark-utils/exports/`; the Foundry test loads them as hardcoded fixtures (regenerated on demand). Multi-block real-proof coverage (the legacy `LayerHashE2ETest` analogue) is deferred to **Phase 5.3**.

#### L5 — post-deployment

```bash
cast call $BRIDGE "primaryVerifier()(address)"
cast call $BRIDGE "fallbackVerifier()(address)"
cast call $BRIDGE "layerHashesVerifier()(address)"
cast call $BRIDGE "storedBkSetCommitment()(uint256)"
cast call $BRIDGE "storedLastSeenBlockSeqNo()(uint64)"
# Storage v2.0 (2026-08-04): `storedNumLayers` was removed; use
# `getLatestPerLayer()` (see below) — the highest non-zero entry
# is the current `highestActiveLayer`.
# The `storedPrevMaxLevelLayerHash()` getter still exists but is the
# immutable genesis seed; for the next-block anchor use
# `expectedPrevAnchor(numLayers)`.
cast call $BRIDGE "storedPrevMaxLevelLayerHash()(uint256)"   # immutable genesis seed
cast call $BRIDGE "expectedPrevAnchor(uint8)(uint256)" $NUM_LAYERS
cast call $BRIDGE "MAX_LAYER_HASHES()(uint256)"   # 10
cast call $BRIDGE "getLatestPerLayer()(uint256[10])"   # replaces getStoredLayerHashes()
```

#### L6 — monitoring invariants

```
# Storage v2.0 (2026-08-04): invariants restated over the per-layer window model.
getLatestPerLayer()[L-1] == last-non-zero layerHashes[L-1] observed in verifyBlock so far  # rolling head
expectedPrevAnchor(numLayers)(t) == layerHashes[pick-1] from most recent verifyBlock       # per-layer anchor
prevMaxLevelLayerHash_in_event_t == expectedPrevAnchor(numLayers_t)(t-1)                    # event continuity
storedLastSeenBlockSeqNo(t) > storedLastSeenBlockSeqNo(t-1)                                  # strict monotonicity
```

A relayer/monitor that sees a `BlockVerified` event with a `prevMaxLevelLayerHash` parameter that doesn't match the prior `expectedPrevAnchor(numLayers)` should alert — it would indicate a state-machine break.

---

## 6. BK Set Rotation (Phase 1.C — pending)

### 6.1 Status

The legacy `LayerHashBridge.rotateBkSet` (ZK-proven via a 2-input Halo2 stub) **and** its 7-day timelocked owner fallback (`proposeBkSetCommitment` / `executeBkSetCommitment` / `cancelBkSetCommitment`) were retired in **Phase 4.2** (2026-05-10) along with the rest of `LayerHashBridge.sol`.

The v2 plan is **Phase 1.C**: extend `verifyBlock` (or add a sibling `verifyBkSetUpdate`) with an optional `bkSetUpdateProof` argument that, on a successful Circuit 3 pairing, advances `storedBkSetCommitment` from old → new in the same transaction. Until Phase 1.C ships:

- An **interim** `applyBkSetUpdate` has since shipped: it takes a Circuit 1A/1B attestation plus a three-sibling SHA-256 opening of the `(L2, L3)` leaf pair in the 16-leaf, depth-4 block-id tree, and advances `storedBkSetCommitment` when the fold reproduces `blockId`. It is permissionless and needs no Circuit 3 — the rotation is authorised by the attested block itself, not by a dedicated proof. So `storedBkSetCommitment` is no longer immutable post-deployment, and BK-set rotation no longer requires a redeployment.
- What Circuit 3 still buys is a statement about the rotation's *contents* (that the new committee legitimately succeeds the old one). `applyBkSetUpdate` only shows that the attested block commits to this pair of commitments at those tree positions.
- No owner path was reintroduced: the "trusted owner can flip the committee" assumption that the legacy `LayerHashBridge.proposeBkSetCommitment` carried stays retired.

### 6.2 What must hold (target invariants for Phase 1.C)

The following list is **forward-looking** and will be re-verified once Circuit 3 is wired:

| Property | Statement (target) |
|---|---|
| **BK-1** | `verifyBkSetUpdate(proof, oldCommitment, newCommitment)` succeeds only if Circuit 3's gnark verifier returns `true` AND `oldCommitment == storedBkSetCommitment`. |
| **BK-2** | If `bkSetUpdateVerifier == address(0)`, the call reverts (the slot is reserved but not yet filled). |
| **BK-3** | `verifyBkSetUpdate` is permissionless — anyone can submit a valid proof. |
| **BK-4** | After success, `storedBkSetCommitment = newCommitment` and a `BkSetCommitmentRotated(oldCommitment, newCommitment)` event is emitted. |
| **BK-5** | The transition is single-step: there is no "pending"/"executed" two-phase flow (the v1 timelock was a workaround for the absence of Circuit 3). |

The one invariant below is **not** forward-looking — it constrains `applyBkSetUpdate` as shipped, and it is the kind that fails silently in the direction of "nothing works" rather than "anything passes":

| Property | Statement (holds today) |
|---|---|
| **BK-6** | `blockId` means the same thing to both of its consumers inside `applyBkSetUpdate`: the canonical `Fr` image of the block-id tree root. The attestation adapter compares it byte-for-byte against an instance read out of the proof, which is always `< r`; the fold produces a raw SHA-256 root, of which only `r / 2^256 = 18.9%` are. The contract therefore reduces the root (`% BN254_R`) before comparing, and the relayer sends the reduced value on both this path and `verifyBlock`. |

- **BK-6**: worth stating because the failure is not a security hole but a dead entry point — without the reduction the two consumers are unsatisfiable at once for ~81% of rotations, and the mismatch surfaces as `AttestationProofRejected` or `BkUpdateMerkleMismatch` depending on which convention the caller picked. It stayed invisible for a while because the mock verifiers accepted any argument; they now mirror the adapter and reject non-canonical inputs, so the suite fails if either side drifts back. Contract side: `AckiNackiBridge.applyBkSetUpdate`. Relayer side: `types::block_id_to_field` / `withdrawal::hash_hex_to_block_id_fr`. Note the older claim in this tree that "the on-chain verifier auto-reduces via `mod(calldataload, f_q)`" was true of the direct Yul verifier and stopped being true with the R15 aggregator adapters, which compare before they pair.

### 6.3 How to verify (today)

`AckiNackiBridgeApplyBkSetUpdateTest` (11 tests) covers the shipped interim surface: the depth-4 fold against a vector computed independently in Python, rejection of the legacy depth-3 root, the `Fr` reduction in both directions (BK-6), replay, non-monotonic sequence numbers, stale old commitment, and a two-rotation chain. For the Circuit 3 target invariants above, the closest thing at HEAD is the negative path in `AckiNackiBridgeVerifyBlockTest::testRevertOnBkSetCommitmentMismatch`, which confirms that `verifyBlock` rejects any proof signed by a different committee.

```bash
cd contracts/ethereum && forge test --match-contract ApplyBkSetUpdate -vv
```

```bash
cast call $BRIDGE "storedBkSetCommitment()(uint256)"   # static between deployments
```

### 6.4 Why this is safe in the interim

The deposit surface and the layer-hash advancement surface remain operational. The only
operational impact of "no rotation" is that if the AN-side BK set rotates, the bridge must
be redeployed with a fresh `genesisBkSetCommitment`. This is acceptable for testnet /
shellnet operation; production launch will gate on Phase 1.C.

---

## 6.5 Cross-Circuit Invariants (CC-#)

These are the v2-specific invariants binding two independent ZK proofs into a single block update. See `docs/four_circuit_architecture.md` §4 for the detailed mechanism.

| Property | Statement |
|---|---|
| **CC-1** | `blockId` is the same value committed by Circuit 1A/1B (PI[0]) and Circuit 2 (PI[0]). The bridge passes one argument to both verifier calls — any mismatch ⇒ at least one `verifyProof` returns `false` ⇒ `verifyBlock` reverts. |
| **CC-2** | `bkSetCommitment` is the same value committed by Circuit 1A/1B (PI[1]) and Circuit 2 (PI[1]). Same enforcement mechanism as CC-1. |
| **CC-3** | `bkSetCommitment == storedBkSetCommitment` at the contract level (binds the proofs to the *currently active* committee, not just to "some" committee). |
| **CC-4** | `blockSeqNo` is committed by Circuit 1A/1B (PI[2]) but not by Circuit 2; relayer-side discipline + the partner's `bridge-test-data-gen` enforce coherent values at proof-generation time. |
| **CC-5** | Strict monotonicity at the contract: `blockSeqNo > storedLastSeenBlockSeqNo`. Prevents replay across blocks. |
| **CC-6** | Chain continuity at the contract: `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`. Prevents fork injection between consecutive blocks. |
| **CC-7** | No silent garbage: `layerHashes[i] == 0` for `i ≥ numLayers`. Prevents an attacker from sneaking values into unused slots that would survive future calls. |

### 6.5.1 How to verify

CC-1 / CC-2 / CC-4 are **circuit-level** invariants and have no Solidity-side tests; they are
enforced inside the partner's circuits and are exercised by the happy-path bound-proof tests
(`testHappyPathPrimary`, `testHappyPathFallback`). A bound-proof generator that produces
inconsistent `blockId` between Circuit 1A and Circuit 2 would cause `testHappyPathPrimary` to
fail — that's the regression check.

CC-3 / CC-5 / CC-6 / CC-7 are **bridge-level** invariants enforced before either gnark call.
Their negative tests (one per invariant) live in `AckiNackiBridgeVerifyBlockTest` and
`AckiNackiBridgeRelayerLoopTest`:

```bash
forge test --match-test "testRevertOnBkSetCommitmentMismatch|testRevertOnPrevAnchorMismatch|testRevertOnLayerHashTailNonZero|test_relayerLoop_replaySameSeqNo_reverts|test_relayerLoop_lowerSeqNo_reverts|test_relayerLoop_anchorMismatch_reverts" -vv
```

---

## 7. Block Hash Oracle (Anti-Fork)

### 7.1 What must hold

| Property | Statement |
|---|---|
| **OR-1** | For blocks within the last 256 of `block.number`, `getBlockHash(n)` returns the value of the EVM `blockhash(n)` opcode — i.e., the hash of the canonical chain the contract is executing on. |
| **OR-2** | For blocks beyond 256 ago, `getBlockHash(n)` reverts with `"Use verifyBlockHash() for historical blocks"`. The historical path requires a witness verified by `AxiomV2Core.isBlockHashValid`. |
| **OR-3** | `getBlockHash(n)` for `n >= block.number` reverts (`BlockNotYetMined`). |
| **OR-4** | The Axiom V2 Core address is set at construction time and is `immutable`. |

### 7.2 Why each property holds

Direct from `AxiomBlockHeaderOracle.sol`. The two-tier design intentionally fails closed: if neither tier can produce a hash, the call reverts. The oracle is currently unused by the public surface (the only consumer used to be the legacy `withdraw()` retired in Phase 4.3); it is preserved as the building block for the future burn-proof ETH-side withdrawal flow.

### 7.3 Fork resistance reasoning

If two Ethereum chains exist (mainnet vs an attacker's fork):

- The bridge contract is deployed on **one specific chain** — there is no cross-chain bytecode sharing.
- `blockhash()` returns hashes from the chain the EVM is executing on. On the mainnet bridge instance, only mainnet hashes are available.
- Axiom V2 Core's update proofs are themselves chained back to a trusted root that lives on the same chain. A fork-specific Axiom would only know fork-specific block hashes.

So a "fork attack" only succeeds if the attacker can publish state on the **same Ethereum** the bridge is on — i.e., a 51% reorg of mainnet itself. That is a layer-1 consensus attack, not a bridge-specific vulnerability.

### 7.4 How to verify

#### L2 — tests

```bash
forge test --match-contract AxiomBlockHeaderOracleTest -vv
```

16 tests cover all four tiers (recent, historical, future, malformed) including event emission.

#### L4 — fork test

```bash
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv
```

Confirms the contract behaves correctly against the real `AxiomV2Core` deployment.

#### L5 — post-deployment

```bash
cast call $ORACLE "axiomV2Core()(address)"
# expected mainnet: 0x69963768F8407dE501029680dE46945F838Fc98B
cast call $ORACLE "MAX_BLOCKHASH_AGE()(uint256)"   # 256
cast call $ORACLE "getBlockHash(uint256)" $((BLOCK_NOW - 1))   # should return non-zero
```

---

## 8. ZK Verifier Plumbing

### 8.1 What must hold

| Property | Statement |
|---|---|
| **ZK-1** | Production wires all three circuits to the R15 SHPLONK aggregator Yul (`PrimaryAggregatorVerifier.bin`, `FallbackAggregatorVerifier.bin`, `LayerHashesAggregatorVerifier.bin`), each `snark-verifier-sdk` output not subsequently edited. The retained 1A/2 gnark Groth16 test verifiers (`PrimaryGroth16VerifierGenerated`, `LayerHashesGroth16VerifierGenerated`) are likewise auto-generated; the 1B `FallbackGroth16VerifierGenerated` was deleted when Circuit 1B moved to SHPLONK. |
| **ZK-2** | Each adapter normalises a failing verifier call to `false`: the SHPLONK adapters (`PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier`) check the Yul `staticcall` result; the retained 1A/2 gnark adapters (`PrimaryVerifier`, `LayerHashesMovementVerifier`) wrap `verifyProof` in `try/catch`. |
| **ZK-3** | The adapter never builds a public-input vector larger than the circuit allows; layout matches the gnark VK. (Circuit 1A/1B: 4 inputs; Circuit 2: 14 inputs. The deposit-prover Halo2 SHPLONK path's **12 public inputs** — Track-2 chain-binding, 2026-07-23 — are consumed natively on the AN side, not on Ethereum.) |
| **ZK-4** | Adapter constructors reject `address(0)` for the underlying Groth16 verifier. |
| **ZK-5** | Each adapter rejects proofs of the wrong byte length (256 B for the v2 attestation/layer-hashes path). |

### 8.2 How to verify

#### L0 — diff against gnark output

```bash
diff -u contracts/ethereum/src/LayerHashesGroth16VerifierGenerated.sol \
  <(cd crates/bridge-snark-utils/gnark-wrappers/circuit-2 && ./circuit-2 setup ../../proofs/.../halo2_proof.json | tee /dev/stderr)
```

This is a one-shot manual check whenever the corresponding circuit (or its `circuit.go` `Define`) changes. The same applies to `PrimaryGroth16VerifierGenerated.sol` (circuit-1a). (Circuit 1B no longer has a gnark Groth16 verifier — it uses the SHPLONK aggregator `.bin`; regenerate via `scripts/n14_r15_proving_run.sh`.)

#### L2 — fuzz tests

```bash
forge test --match-contract "FuzzHalo2VerifierTest" -vv
```

These exercise the verifier with thousands of malformed/random calldata payloads — every one must be rejected. Coverage:

- Random proof bytes
- Single-byte mutations
- Truncated calldata
- Inputs above the BN254 field modulus
- Generator-point identity proofs (would otherwise pass the trivial check)

#### L2 — adapter sanity

```bash
forge test --match-test "testVerifyInvalidProofLength|testVerifyRevertingGroth16|testConstructorRejectsZeroAddress" -vv
```

Exercises ZK-2, ZK-3, ZK-4 across all three adapters.

---

## 9. Reentrancy & Access Control

### 9.1 What must hold

| Property | Statement |
|---|---|
| **AC-1** | `AckiNackiBridge.deposit` is `nonReentrant`. (The legacy `withdraw()` was also `nonReentrant`; it was retired in Phase 4.3.) |
| **AC-2** | All AAVE-management functions on `AckiNackiBridge` are `onlyOwner` and `nonReentrant`. |
| **AC-3** | `AckiNackiBridge.verifyBlock` is permissionless and `nonReentrant` (no modifier other than the proof checks). |
| **AC-4** | `transferOwnership`, `setYieldRecipient`, `setLiquidReserveBps`, `setAaveEnabled`, `harvestYield`, `emergencyWithdrawAll` are `onlyOwner`. |
| **AC-5** | The verifier slots (`primaryVerifier`, `fallbackVerifier`, `layerHashesVerifier`, `verifier`, `blockHeaderOracle`, `aavePool`, `wethGateway`, `aWETH`) are `immutable` — there is no setter that can replace them post-deployment. |
| **AC-6** | All external interactions follow CEI: state mutation precedes external call. |

### 9.2 How to verify

```bash
grep -n "modifier\|onlyOwner\|nonReentrant\|immutable" contracts/ethereum/src/AckiNackiBridge.sol
```

Cross-reference each external function with the matrix above.

Tests:

```bash
forge test --match-test "OnlyOwner|NotOwner|nonReentrant|Reentrancy|Unauthorized" -vv
```

CEI by inspection: in `AckiNackiBridge.verifyBlock`, the two gnark verifier calls are `view`-style externals returning bool, and storage writes (`storedLastSeenBlockSeqNo`, `storedPrevMaxLevelLayerHash`) follow both successful verifications (effects come after both interactions return). `deposit()` has no external call at all. The owner-only AAVE routing functions (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`) wrap external calls under `nonReentrant` and update `suppliedPrincipal` after the interaction (acceptable because `suppliedPrincipal` is not consulted to gate the call). Legacy `withdraw()`'s CEI (`processedDeposits` set before `_pullFromAave` and `recipient.transfer`) is no longer in scope — the function was retired in Phase 4.3.

---

## 10. Acki Nacki Side Fork Resistance

### 10.1 What must hold

| Property | Statement |
|---|---|
| **FORK-1** | A proof must be signed by a BLS aggregate of the **current** BK set committee (committed via `storedBkSetCommitment`). |
| **FORK-2** | The aggregate must reach the BFT threshold (`3·signers ≥ 2·n` for primary) — this is enforced inside the Halo2 circuit. |
| **FORK-3** | Each layer-hash update is anchored to the previous top-level hash — an attacker cannot inject a discontinuous state. |
| **FORK-4** | The BK set commitment is `uint256` Poseidon hash of the sorted (signer_index, x-coordinate) tuples — collisions require breaking Poseidon. |

### 10.2 Why this is enough

If an attacker forks Acki Nacki and tries to push a fake block update to Ethereum:

1. They need **two** Groth16 proofs (Circuit 1A *or* 1B + Circuit 2). The attestation circuit requires ≥2/3 BLS signatures (Primary) or >1/2 (Fallback) from BKs whose Poseidon commitment matches `storedBkSetCommitment`.
2. If they control fewer than 1/3 of the BK set, they can't reach the Primary threshold; if they control fewer than 1/2, they can't reach the Fallback threshold either.
3. If they control more than 2/3, they're effectively the legitimate majority — that is a consensus failure, not a bridge bug.
4. Even a successful pair of proofs must satisfy the chain anchor (`prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash`) AND strict monotonicity (`blockSeqNo > storedLastSeenBlockSeqNo`). So the only "fork" they could inject is one that *continues* from current state with strictly increasing seqno — i.e., adversarial blocks they finalised themselves with majority control.
5. They cannot mix-and-match proofs from different blocks: `blockId` is bound across both circuits via the 8-leaf envelope hash tree (offset 48), and the bridge passes a single `blockId` argument into both verifier calls (CC-1).

### 10.3 Where to inspect

- Threshold logic: `gosh-bls-verification` crate (audited; see `docs/layer_hashes_circuit_audit.md`).
- BK set commitment derivation: same crate, sorts by signer index then commits with Poseidon T=3 RATE=2.
- Chain anchor + monotonicity: `AckiNackiBridge.verifyBlock` (the lines that revert with `PrevAnchorMismatch` / `BlockSeqNoNotMonotonic`).
- Cross-circuit binding: `docs/four_circuit_architecture.md` §2 (envelope hash tree leaves), §4 (CC-1..CC-7).

---

## 11. Reproducible End-to-End Verification Recipe

Below is a single ordered recipe a reviewer can run start-to-finish.

```bash
# ─── L1 + L2 ────────────────────────────────────────────────────
cd contracts/ethereum

forge build               # must compile with no errors
forge fmt --check         # must be clean
forge test                # 135 tests across 15 suites, all green

# ─── L3 — single-block real-proof bound test ──────────────────
forge test --match-test "testHappyPathPrimary|testHappyPathFallback" -vv
# Drives verifyBlock with a real bound proof set generated by:
#   cargo run -p bridge-snark-utils --bin export-bound-block-proofs --release
# Multi-block real-proof E2E is deferred to Phase 5.3 (relayer + Anvil).

# ─── L0 — sanity grep for invariants ──────────────────────────
grep -n "treasuryBalance\|emit Deposit\|MAX_DEPOSIT_AMOUNT" src/AckiNackiBridge.sol
grep -n "verifyBlock\|stored\(BkSet\|LastSeen\|NumLayers\|LayerHashes\|PrevMaxLevel\)" \
  src/AckiNackiBridge.sol

# ─── L4 — fork tests (recommended pre-deploy) ─────────────────
forge test --fork-url $ETH_RPC_URL --match-contract AxiomBlockHeaderOracleTest -vvv

# ─── L5 — post-deployment cast checks ─────────────────────────
# Run these after `forge script` deployment (see DeployRealBridge.s.sol).
cast call $BRIDGE  "verifier()(address)"                         # deposit verifier
cast call $BRIDGE  "blockHeaderOracle()(address)"
cast call $BRIDGE  "owner()(address)"
cast call $BRIDGE  "aaveEnabled()(bool)"                         # true if USE_AAVE=true
cast call $BRIDGE  "primaryVerifier()(address)"                  # AN→ETH adapters
cast call $BRIDGE  "fallbackVerifier()(address)"
cast call $BRIDGE  "layerHashesVerifier()(address)"
cast call $BRIDGE  "storedBkSetCommitment()(uint256)"
cast call $BRIDGE  "storedLastSeenBlockSeqNo()(uint64)"
# Storage v2.0 (2026-08-04): `storedNumLayers()` removed; use
# `getLatestPerLayer()` and scan for the highest non-zero entry.
cast call $BRIDGE  "storedPrevMaxLevelLayerHash()(uint256)"     # immutable genesis seed
cast call $BRIDGE  "expectedPrevAnchor(uint8)(uint256)" $NUM_LAYERS
cast call $BRIDGE  "MAX_LAYER_HASHES()(uint256)"                 # 10
cast call $ORACLE  "axiomV2Core()(address)"

# ─── L6 — monitoring queries (run continuously) ───────────────
# 1. Solvency (AAVE-aware)
echo "treasury  = $(cast call $BRIDGE 'treasuryBalance()(uint256)')"
echo "balance   = $(cast balance $BRIDGE)"
echo "aWETHbal  = $(cast call $aWETH 'balanceOf(address)(uint256)' $BRIDGE)"
# Invariant: balance + aWETHbal >= treasury

# 2. AN→ETH chain integrity (poll every block)
# Storage v2.0 (2026-08-04): observe the per-layer window heads and the
# per-numLayers anchor pick — the flat `getStoredLayerHashes()` cache is gone.
cast call $BRIDGE "getLatestPerLayer()(uint256[10])"
cast call $BRIDGE "expectedPrevAnchor(uint8)(uint256)" $NUM_LAYERS
# Invariant: prevMaxLevelLayerHash in each new BlockVerified event must equal
# expectedPrevAnchor(numLayers) at the time of the prior call. Strict
# monotonicity on storedLastSeenBlockSeqNo is the second invariant — alert
# on any drop.

# 3. No genesis drift
cast call $BRIDGE "storedBkSetCommitment()(uint256)"
# Must equal the deployment-time genesisBkSetCommitment until Phase 1.C
# (Circuit 3) ships — there is no rotation path in v2 today.
```

---

## 12. What This Does *Not* Prove

To be explicit about the trust boundary:

| Outside this guide | Where it lives |
|---|---|
| Soundness of the Halo2 circuits (1A, 1B, 2, and 3 once it lands) | `docs/layer_hashes_circuit_audit.md`, partner audits, `docs/four_circuit_architecture.md` §3 |
| BLS12-381 G2 subgroup gap (audit FORK-2 / BLS-1) | open audit finding; carries over to v2's Circuit 1A/1B (same `gosh-bls-verification` chip); will be addressed in next circuit revision |
| Trusted setup of the KZG SRS for Halo2 | community-generated `kzg_bn254_19.srs` shared across all four circuits; verify checksum on download |
| Trusted setup of the gnark Groth16 wrappers (one per AN→ETH circuit) | wrapping circuits live under `crates/bridge-snark-utils/gnark-wrappers/{circuit-1a,circuit-1b,circuit-2}/`. As of 2026-05-17 all three `Define`s are no-op identity stubs (R15) — tracked under `docs/an_partner_integration_plan.md` Phase 8 R&D. Mainnet `v2.0.0` is gated on closing this. The deposit-side gnark wrapper was retired in Phase 4.3 — there is no longer an ETH-side ZK adapter for the deposit direction. |
| Acki Nacki BFT economic security | not a bridge concern; the bridge inherits whatever finality AN provides via the Primary ≥ 2/3 / Fallback > 1/2 thresholds |
| Off-chain relayer liveness / censorship | a malicious relayer can stall but cannot forge state; multiple competing relayers are sufficient |
| Phase 1.C BK-set rotation **circuit** (Circuit 3) | not yet shipped; the interim `applyBkSetUpdate` rotates the commitment against an attested block's Merkle tree, but proves nothing about the succession itself |

When any of these change, this doc must be revisited and the affected sections updated.

---

## 13. Cross-References

- `docs/four_circuit_architecture.md` — canonical v2 architectural overview (envelope hash tree, public-input layouts, CC-#).
- `docs/an_partner_integration_plan.md` — live phase-by-phase roadmap (Phase 4.1/4.2/5.1 done; 5.2/5.3/1.C pending).
- `docs/integration_analysis.md` — architecture analysis, contract ↔ circuit mapping (v2-updated §3 and §5).
- `docs/integration_plan.md` — historical milestones M0–M9 (M7–M9 superseded by `an_partner_integration_plan.md`).
- `docs/layer_hashes_circuit_audit.md` — line-by-line legacy circuit audit (BLS-1 / FORK-2 still apply to v2's Circuit 1A/1B).
- `docs/aave_integration.md` — yield bolt-on verification (companion to §4 above).
- `docs/proof_metrics_report.md` — proof sizes, key sizes, gas costs per circuit.
