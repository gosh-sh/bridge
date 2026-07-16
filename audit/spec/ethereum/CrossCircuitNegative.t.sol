// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./helpers/BoundVerifyBlockHarness.sol";

/// @title CrossCircuitNegativeTest
/// @notice Phase E / E-03 — cross-circuit binding negatives with real bound Groth16 proofs.
/// @dev CC-# tags mirror `docs/four_circuit_architecture.md` cross-circuit invariants.
contract CrossCircuitNegativeTest is BoundVerifyBlockHarness {
    function setUp() public {
        setUpBoundVerifyBlockBridge();
    }

    /// @dev CC-1 / E-03: tampered `blockId` breaks attestation public-input binding.
    function test_CC1_blockIdMismatch_revertsAttestationFirst() public {
        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID ^ 1,
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

    /// @dev CC-7: layer-hash calldata tampered while proofs stay bound → layer verifier rejects.
    function test_CC7_tamperedLayerHashCalldata_revertsLayerProof() public {
        uint256[10] memory tampered = _layerHashes();
        tampered[0] ^= 1;

        vm.expectRevert(AckiNackiBridge.LayerHashesProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            tampered,
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    /// @dev A2-INV-1 / CEI: failed verifyBlock must not advance stored seqNo or anchors.
    function test_CC_negativeVerifyBlock_stateUntouched() public {
        uint64 seqBefore = bridge.storedLastSeenBlockSeqNo();
        uint256 anchorBefore = bridge.storedPrevMaxLevelLayerHash();

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID ^ 1,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), seqBefore, "seqNo unchanged");
        assertEq(bridge.storedPrevMaxLevelLayerHash(), anchorBefore, "anchor unchanged");
        assertEq(bridge.storedNumLayers(), 0, "layers untouched");
    }
}
