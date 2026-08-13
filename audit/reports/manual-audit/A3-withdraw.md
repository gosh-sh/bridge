# Manual audit — A3: withdrawByProof & nullifiers

**Workstream:** A3
**Subagent model:** Fable 5
**Files:** `AckiNackiBridge.sol` (`withdrawByProof`, `_nullifiers`, `_isKnownLayerAnchor`, `_reconstructRecipient`, USDC payout), `BridgeWithdrawalVerifier.sol`, `BridgeWithdrawalAggregatorVerifier.sol`, `IBridgeWithdrawalVerifier.sol`, `MockBridgeWithdrawalVerifier.sol`
**Invariants:** custom WD-1..WD-12 (see `invariants-extended.md` §A3)
**Scope note:** Circuit 4 (`bridge-event-prove-circuit`) soundness is **OUT OF SCOPE**. This report audits only the *contract-side enforcement* of anchors, nullifiers, recipient, amount, chain/token binding and treasury.

## Summary

The `withdrawByProof` payout path is the AN→ETH fund exit. Contract-side enforcement is **solid**: correct
Checks-Effects-Interactions ordering (nullifier + treasury mutated before any transfer), `nonReentrant`, `whenNotPaused`,
immutable identity pinning, exact 160-bit recipient reconstruction with per-half range checks, per-contract nullifier
replay protection, chain-id binding with a tightly scoped shellnet alias, and fail-closed behaviour before any
`verifyBlock` has recorded an anchor.

**No BC (production bug with direct fund loss) was found in the contract enforcement.** All identified issues are
liveness / policy / verifier-soundness concerns, classified QC or INFO. The single most important caveat (A3-03) is
adjacent to scope: the Groth16 adapter `BridgeWithdrawalVerifier.sol` is an R15 **identity stub** that does not verify
the Halo2 proof — it is a deployable code path in the audited file set even though production wires the SHPLONK
aggregator adapter instead.

- **BC:** 0
- **QC:** 4 (A3-01, A3-02, A3-03, A3-05)
- **OK / INFO:** 1 (A3-04) + positives below

## Checklist

- [x] `finalRoot ∈ knownAnchors` — enforced via `_isKnownLayerAnchor(WITHDRAW_ANCHOR_LAYER=1, pub.finalRoot)`; fails closed (reverts `UnknownAnchor`) before any `verifyBlock`. Window is bounded → eviction concern (A3-01).
- [x] Nullifier replay protection — `_nullifiers[bytes32(pub.nullifier)]` checked pre-verify, set in Effects before transfer; `nonReentrant`. Per-contract, so cross-chain replay is also blocked by scope + dstChainId. Sound.
- [x] `dstChainId == block.chainid` — enforced, plus a correctly *scoped* alias (`altDstChainId` honoured only when `block.chainid == altDstHostChainId`, both non-zero). Production sets alts to 0 → exact match only. Sound.
- [x] Recipient hi/lo → address reconstruction — each half range-checked `≤ 2^80−1` (`RecipientHalfOutOfRange`); `address(uint160((hi<<80)|lo))` is an exact, non-truncating 160-bit recombination. `address(0)` is accepted (A3-02).
- [x] Amount vs treasury / USDC transfer — `amount > treasuryBalance` reverts `WithdrawTreasuryShortfall`; no partial pay; AAVE top-up pull is bounded and reverts on shortfall. Sound.
- [x] Treasury shortfall behaviour — reverts atomically; nullifier NOT consumed on revert (verified by `test_..._verifierRejects_revertsAndDoesNotMutateState` and shortfall test). Sound.
- [x] Public input forwarding to verifier — all 10 PIs forwarded verbatim; aggregator adapter re-checks re-exposed instances [12..21]; verifier `false`/revert ⇒ `WithdrawalProofRejected`, no state mutation. Sound (but see A3-03 for the stub adapter).
- [x] Pause / no-anchor-yet paths — `whenNotPaused` (tested in `AckiNackiBridgePause.t.sol`); before any `verifyBlock`, every `finalRoot` (incl. 0) fails the anchor check. Sound.
- [x] tokenId assumptions — only `tokenId == 0` (or scoped `altTokenId`) accepted; `amount` treated as 6-decimal USDC unconditionally (A3-05 coupling note).
- [x] CEI: nullifier set before transfer — yes; `_nullifiers[k]=true; treasuryBalance-=amount;` precede the AAVE pull and `usdc.transfer`. Sound.
- [x] `withdrawByProof` permissionless — payout targets the proof-bound `recipient`, never `msg.sender`; front-running is MEV-neutral (griefer only burns gas). Positive.

## Findings

| ID | Sev | BC/QC/OK | Summary | PoC |
|----|-----|----------|---------|-----|
| A3-01 | Medium | QC | L1 anchor window is a 128-entry (`HISTORY_PROOF_WINDOW`) circular buffer. A `finalRoot` older than the last 128 `verifyBlock`s is evicted and `withdrawByProof` reverts `UnknownAnchor` forever. Liveness depends on the prover being able to re-bind an already-emitted withdrawal event to a *newer* anchor. If it cannot, a slow relayer / delayed submission strands funds. | Needs partner confirmation (re-prove capability) — see questions.md WD-Q1 |
| A3-02 | Low | QC | `recipient == address(0)` is accepted by the contract (`test_withdrawByProof_zeroRecipientWorks` pins this). Real USDC reverts transfers to `address(0)`, so the whole tx reverts, the nullifier is never consumed, and that specific withdrawal event becomes permanently unpayable. Not fund loss (funds stay in treasury) but a stuck-event edge case. | questions.md WD-Q2 |
| A3-03 | High | QC (known / Phase 8) | `BridgeWithdrawalVerifier.sol` (Groth16 adapter) is an **R15 identity stub**: per its own NatSpec it forwards the 10 PIs to `verifyProof` but the underlying gnark wrapper does **not** verify the Halo2 SHPLONK proof. If this adapter were ever wired into a live deployment, any 256-byte proof binding the 10 PIs would pass and fully drain the treasury (bounded only by per-tx `amount ≤ treasuryBalance`). Production deploy scripts wire `BridgeWithdrawalAggregatorVerifier` (real SHPLONK) instead. Needs an explicit deployment invariant that the stub adapter is never used on mainnet. | questions.md WD-Q3 |
| A3-04 | Info | OK | `pub.amount == 0` is accepted: a zero-value payout consumes a nullifier and emits `WithdrawalByProofExecuted` with `amount = 0`. Harmless (no funds move) but under-documented. | — |
| A3-05 | Info | QC | `WITHDRAW_ANCHOR_LAYER` is hard-coded to `1`, coupling the contract to the partner witness builder's `layer_idx = 0`. If the partner ever anchors on a different layer, every withdrawal fails closed (`UnknownAnchor`) — safe (no fund loss) but a full liveness halt. The code comments a future `anchorLayer` PI slot [10]. | questions.md WD-Q4 |

### Positives (no action; documented for the invariant registry)

- **CEI is correct and tested**: state is committed before the external transfer; `test_withdrawByProof_verifierRejects_revertsAndDoesNotMutateState` proves atomic rollback.
- **Defence-in-depth identity check**: `WithdrawIdentityMismatch` surfaces before the opaque `WithdrawalProofRejected`, even though the verifier also binds `(dappFr, accFr)`.
- **Cheap-checks-first gas ordering**: identity → chain → token → recipient range → nullifier → anchor all precede the expensive crypto verify.
- **Chain-id alias is tightly scoped** (`altDstHostChainId` gate) so a shellnet logical-chain proof cannot replay on mainnet/Arbitrum (`test_withdrawByProof_sepoliaPayoutCannotReplayOnArbitrum`, `test_withdrawByProof_mainnetDstChainId_reverts_on_arbitrum`).
- **Fail-closed anchor membership** before any `verifyBlock` (`test_withdrawByProof_unknownAnchor_reverts`).

## New invariants proposed

See `audit/reports/manual-audit/invariants-extended.md` §A3 for the full WD-1..WD-12 statements + test mapping. Summary:

| ID | Statement |
|----|-----------|
| WD-1 | `withdrawByProof` reverts `WithdrawByProofDisabled` unless `bridgeWithdrawalVerifier != address(0)`. |
| WD-2 | Reverts `WithdrawIdentityMismatch` unless `pub.dappFr == bridgeWithdrawalDappFr && pub.accFr == bridgeWithdrawalAccFr`. |
| WD-3 | Reverts `DstChainIdMismatch` unless `pub.dstChainId == block.chainid` or the scoped `altDstChainId` alias matches. |
| WD-4 | Reverts `UnsupportedTokenId` unless `pub.tokenId == 0` or the scoped `altTokenId`. |
| WD-5 | `recipientHi, recipientLo ≤ 2^80−1`; payout address is exactly `(hi<<80)|lo` (no truncation). |
| WD-6 | Reverts `UnknownAnchor` unless `pub.finalRoot` is in layer-1's rolling window; always reverts before any `verifyBlock`. |
| WD-7 | A nullifier pays out at most once; replay reverts `NullifierAlreadyUsed`. |
| WD-8 | CEI: `_nullifiers[k]=true` and `treasuryBalance -= amount` execute before any external call; `nonReentrant`. |
| WD-9 | `amount ≤ treasuryBalance` (else revert); payouts are all-or-nothing (no partial pay). |
| WD-10 | `whenNotPaused`: paused ⇒ revert `BridgePaused`. |
| WD-11 | All 10 public inputs are forwarded byte-for-byte to the verifier; a `false`/revert leaves all state untouched. |
| WD-12 | Payout is sent to the proof-bound `recipient`, independent of `msg.sender` (permissionless submission). |
