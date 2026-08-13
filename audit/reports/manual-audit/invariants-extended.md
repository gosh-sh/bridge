# Extended invariants (manual-audit overlay)

Proposed invariants from the manual-audit workstreams. **Canonical tracker for A1/A2/A4 test mapping:** `audit/reports/invariants-extended.md`. This file holds **A3 (withdraw)** detail.

## §A3 — withdrawByProof & nullifiers (WD-*)

AN→ETH fund exit. `withdrawByProof(proof, pub)` releases `pub.amount` USDC to the
proof-bound `recipient` exactly once. Circuit 4 soundness is out of scope; these
invariants pin the **contract-side** enforcement.

### A3.1 What must hold

| ID | Invariant |
|----|-----------|
| **WD-1** | Feature gate: `withdrawByProof` reverts `WithdrawByProofDisabled` when `bridgeWithdrawalVerifier == address(0)`. |
| **WD-2** | Identity binding: reverts `WithdrawIdentityMismatch` unless `pub.dappFr == bridgeWithdrawalDappFr && pub.accFr == bridgeWithdrawalAccFr`. Both are immutable, set at construction; `accFr` must be non-zero when the verifier is wired (`InvalidBridgeWithdrawalIdentity`). |
| **WD-3** | Chain binding: reverts `DstChainIdMismatch` unless `pub.dstChainId == block.chainid`, or the scoped alias holds (`altDstChainId != 0 && altDstHostChainId != 0 && block.chainid == altDstHostChainId && pub.dstChainId == altDstChainId`). |
| **WD-4** | Token binding: reverts `UnsupportedTokenId` unless `pub.tokenId == 0` or (`altTokenId != 0 && pub.tokenId == altTokenId`). |
| **WD-5** | Recipient well-formedness: `pub.recipientHi ≤ 2^80−1` and `pub.recipientLo ≤ 2^80−1` (`RecipientHalfOutOfRange`); the payout address is exactly `address(uint160((recipientHi << 80) | recipientLo))` with no truncation or overflow. |
| **WD-6** | Anchor membership: reverts `UnknownAnchor` unless `pub.finalRoot` is present in layer-1's rolling window (`WITHDRAW_ANCHOR_LAYER = 1`). Before any `verifyBlock`, the window is empty ⇒ every `finalRoot` (including 0) fails. |
| **WD-7** | Replay protection: each `nullifier` pays out at most once; a second submission reverts `NullifierAlreadyUsed`. `isNullifierUsed` reflects the mapping. |
| **WD-8** | CEI + reentrancy: `_nullifiers[bytes32(nullifier)] = true` and `treasuryBalance -= amount` are committed *before* any external call (AAVE pull, `usdc.transfer`); the function is `nonReentrant`. |
| **WD-9** | Treasury solvency: reverts `WithdrawTreasuryShortfall` when `amount > treasuryBalance`; payout is all-or-nothing (no partial pay). AAVE top-up pulls at most `suppliedPrincipal` and reverts on shortfall. |
| **WD-10** | Pause: `whenNotPaused` — a paused bridge reverts `BridgePaused`. |
| **WD-11** | Input forwarding: all 10 public inputs are passed byte-for-byte to `verifyWithdrawal`; a `false` return or revert surfaces as `WithdrawalProofRejected` and leaves treasury + nullifier state untouched. |
| **WD-12** | Payout target: funds go to the proof-reconstructed `recipient`, never to `msg.sender`; submission is permissionless (MEV-neutral). |

### A3.2 How to verify

| ID | Test(s) |
|----|---------|
| WD-1 | `test_withdrawByProof_disabled_reverts`, `test_constructor_withdrawDisabledByDefault` |
| WD-2 | `test_withdrawByProof_wrongDappFr_reverts`, `test_withdrawByProof_wrongAccFr_reverts`, `test_constructor_withdrawEnabledWithoutAccFr_reverts` |
| WD-3 | `test_withdrawByProof_wrongDstChainId_reverts`, `test_withdrawByProof_mainnetDstChainId_reverts_on_arbitrum`, `test_withdrawByProof_sepoliaPayoutCannotReplayOnArbitrum`, `test_withdrawByProof_shellnetAliasAcceptedOnSepoliaOnly` |
| WD-4 | `test_withdrawByProof_unsupportedTokenId_reverts`, shellnet alias test (`tokenId = 3`) |
| WD-5 | `test_withdrawByProof_recipientHiOutOfRange_reverts`, `test_withdrawByProof_recipientLoOutOfRange_reverts`, `test_withdrawByProof_recipientReconstructsCorrectly`, `test_withdrawByProof_zeroRecipientWorks` |
| WD-6 | `test_withdrawByProof_unknownAnchor_reverts`, `test_withdrawByProof_anchorRecordedByLaterVerifyBlock_isAccepted`, `test_setUp_recordsSeedAnchor`, `AckiNackiBridgeWithdrawByProofOrder2Test::test_withdrawOnL1Anchor_succeedsWhenNumLayersIsTwo` |
| WD-7 | `test_withdrawByProof_replayRejected`, `test_isNullifierUsed_initiallyFalse` |
| WD-8 | `test_withdrawByProof_verifierRejects_revertsAndDoesNotMutateState`, `test_withdrawByProof_happyPath_transfersAndMarksNullifier` |
| WD-9 | `test_withdrawByProof_treasuryShortfall_reverts` |
| WD-10 | `AckiNackiBridgePause.t.sol` (withdrawByProof paused ⇒ `BridgePaused`) |
| WD-11 | `test_withdrawByProof_forwardsPublicInputsByteForByte`, production suite `AckiNackiBridgeProductionWithdrawByProof.t.sol` (`_mismatchedPub_returnsFalse`, `_tamperedProof_reverts`) |
| WD-12 | `test_withdrawByProof_happyPath_transfersAndMarksNullifier` (payout to `RECIPIENT`, submitter `address(this)`) |

### A3.3 Open items (see questions.md)

- **WD-Q1 / A3-01** — bounded L1 anchor window (128 entries) can evict a `finalRoot`; liveness depends on re-prove-against-newer-anchor capability. Pinned by `audit/spec/ethereum/WithdrawAnchorEviction.t.sol`.
- **WD-Q2 / A3-02** — `address(0)` recipient is accepted on-chain but unpayable on real USDC.
- **WD-Q3 / A3-03** — the Groth16 adapter (`BridgeWithdrawalVerifier.sol`) is an R15 identity stub; needs a deployment invariant that only the SHPLONK aggregator adapter is wired in production.
- **WD-Q4 / A3-05** — `WITHDRAW_ANCHOR_LAYER` hard-coded to 1.
