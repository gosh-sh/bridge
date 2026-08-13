// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./helpers/BoundVerifyBlockHarness.sol";

/// @title CrossCircuitNegativeTest
/// @notice Phase E / E-03 — cross-circuit binding negatives with mock verifiers on main SHPLONK surface.
/// @dev CC-# tags mirror `docs/operations/bridge_verification.md`. Bound Groth16 proofs retired with main.
contract CrossCircuitNegativeTest is BoundVerifyBlockHarness {
    function setUp() public {
        setUpBoundVerifyBlockBridge();
    }

    /// @dev CC-1 / E-03: rejected attestation proof aborts verifyBlock.
    function test_CC1_attestationRejected_reverts() public {
        primaryVerifier.setShouldAccept(false);

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    /// @dev CC-3: caller-supplied `bkSetCommitment` must match genesis seed and proof binding.
    function test_CC3_bkSetCommitmentMismatch_reverts() public {
        uint256 wrongBk = BK_SET_POSEIDON ^ 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkSetCommitmentMismatch.selector, wrongBk, BK_SET_POSEIDON
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            wrongBk,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    /// @dev CC-7: layer verifier rejection surfaces as LayerHashesProofRejected.
    function test_CC7_layerVerifierRejected_reverts() public {
        layerHashesVerifier.setShouldAccept(false);

        vm.expectRevert(AckiNackiBridge.LayerHashesProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    /// @dev A2-INV-1 / CEI: failed verifyBlock must not advance stored seqNo or anchors.
    function test_CC_negativeVerifyBlock_stateUntouched() public {
        uint64 seqBefore = bridge.storedLastSeenBlockSeqNo();
        uint256 anchorBefore = bridge.storedPrevMaxLevelLayerHash();

        primaryVerifier.setShouldAccept(false);

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), seqBefore, "seqNo unchanged");
        assertEq(bridge.storedPrevMaxLevelLayerHash(), anchorBefore, "anchor unchanged");
        uint256[10] memory layers = bridge.getLatestPerLayer();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(layers[i], 0, "layers untouched");
        }
    }
}
