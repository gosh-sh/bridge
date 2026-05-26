// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/PrimaryVerifier.sol";
import "../src/PrimaryGroth16VerifierGenerated.sol";
import "../src/LayerHashesMovementVerifier.sol";
import "../src/LayerHashesGroth16VerifierGenerated.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockFallbackVerifier.sol";

/// @title AckiNackiBridgeVerifyBlockTest
/// @notice Phase 4 — End-to-end tests for `AckiNackiBridge.verifyBlock`.
///
/// Exercises the AN→ETH state machine using a *cross-circuit-bound* synthetic
/// scenario: one block whose Primary attestation (Circuit 1A) and layer-hashes
/// movement (Circuit 2) proofs share `block_id` and `bk_set_poseidon`. The
/// fixtures below were produced once by:
///
/// ```
/// cargo run -p bridge-prover-orchestrator --bin export-bound-block-proofs --release
/// cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a && ./circuit-1a prove ../../proofs/bound/primary/halo2_proof.json
/// cd ../circuit-2                                                && ./circuit-2 prove ../../proofs/bound/layer-hashes/halo2_proof.json
/// ```
///
/// Scenario parameters: `bk_set_size=10`, `num_layers=5`, `num_chain_steps=3`,
/// `block_seq_no=1`, `last_seen=0`. The 5 active layer hashes are committed to
/// the bridge state; the 5 inactive slots stay zero. The new chain anchor
/// after this block is `LAYER_HASH_4`.
///
/// The Fallback path uses a configurable `MockFallbackVerifier` because
/// re-running Circuit 1B keygen+prove against a bound scenario is a separate
/// expensive ceremony; the real `FallbackVerifier` itself is independently
/// covered in `FallbackVerifier.t.sol`.
contract AckiNackiBridgeVerifyBlockTest is Test {
    // ─── Bound scenario public inputs (shared across both proofs) ─────────
    uint256 internal constant BLOCK_ID =
        20322406600520462375758092294219252768443831515974743492093184796387680257654;
    uint256 internal constant BK_SET_POSEIDON =
        12131685556123040013370001299379043353566086228347545885725494982201367766590;
    uint64 internal constant BLOCK_SEQ_NO = 1;
    uint8 internal constant NUM_LAYERS = 5;
    uint256 internal constant LAYER_HASH_0 =
        5597430690756036137200850231008677009392496962870610452683763762890050825836;
    uint256 internal constant LAYER_HASH_1 =
        4796295659576234412953062246444144483845500275027135395768740734699524433822;
    uint256 internal constant LAYER_HASH_2 =
        13612500885536344885003699466285227870961224105616882696060905946370081269316;
    uint256 internal constant LAYER_HASH_3 =
        765644099832424376728543246025162156692809455668879575312335534521837264383;
    uint256 internal constant LAYER_HASH_4 =
        11258276999963302388849994778928696710880810094012346484732501452243136409891;
    uint256 internal constant PREV_MAX_LEVEL_LAYER_HASH =
        428496435308882603647236216064516127915391730646288063657718627407097714118;

    // ─── Bound Groth16 proof bytes (1A) ───────────────────────────────────
    bytes internal constant PROOF_PRIMARY =
        hex"250e54dc356d7a3e75a001445d8e68161026769c256e24ee2a98989fe3b053cf0cab97566d6da19f08ecc0f80f8b4fc07c2237c66bde30d86772a27a28e0c36d0d3040adb45f17b39c3770752fb313e6c5197fd62888e53f234488f5d2345c05018bb679a901cf6e7941fa51eca8eae4fb096550384f09a5daf5056d3626797a1f366f5b7d5a8092bc38f3733682929023b13aa4cb5acf9d012f2454f08b13b52e397c247d57ad5f7428dad9b1138b16194591e9c3083b11146220258df92c3518ec536d04b52fbdc6d68f3e68b8e9f99fd489ee76e698d1bf00c6d1def9d01403dfc6047d2fdd625739e75cd08063cb5502e04e39c74d667ed44c5290bdda7b";

    // ─── Bound Groth16 proof bytes (Circuit 2 layer-hashes) ───────────────
    bytes internal constant PROOF_LAYER_HASHES =
        hex"1f5fa2faf1f9332e6794fa01ff88ee2e8f1638633573b152f13e922bce2964ce255e3654cd95b3d59487fa7287da197f6cbd4a905f3f2cb3d92ad4dd32c9682e09316a5373289b332399f5db8bfe0e9537778f3a7b1f17a092d2047423eba1863012f5546d7730e009b0aa1dca6252252b033944e201fdc22ad7401a4713b45d2669e67b2c106c75bf765fe78a6a9cfbfb949b5ac16fd7cc91448677e3be07f709077287569a62617c398085a233da6b2598bdc0e9fcb3cf653ced49a9d808ed1ea221ef5fedff54bdb7f6bc1595857c6d00cffc0b416acff8db68d075ff78a206cb9d3d5717ecab99303bbe4ac69eb4b08ea4da4d1e1e4057ebe09db8b0bf98";

    // ─── Test rig ─────────────────────────────────────────────────────────
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    PrimaryGroth16VerifierGenerated internal primaryGroth16;
    PrimaryVerifier internal primaryVerifier;
    LayerHashesGroth16VerifierGenerated internal layerHashesGroth16;
    LayerHashesMovementVerifier internal layerHashesVerifier;
    MockFallbackVerifier internal fallbackVerifier;

    event BlockVerified(
        uint256 indexed blockId,
        uint64 indexed blockSeqNo,
        AckiNackiBridge.FinalizationType finType,
        uint8 numLayers
    );

    function setUp() public {
        oracle = new MockBlockHeaderOracle();

        primaryGroth16 = new PrimaryGroth16VerifierGenerated();
        primaryVerifier = new PrimaryVerifier(address(primaryGroth16));
        layerHashesGroth16 = new LayerHashesGroth16VerifierGenerated();
        layerHashesVerifier = new LayerHashesMovementVerifier(address(layerHashesGroth16));
        fallbackVerifier = new MockFallbackVerifier();

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primaryVerifier)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layerHashesVerifier)),
            BK_SET_POSEIDON,
            PREV_MAX_LEVEL_LAYER_HASH
        );

        bridge = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            vb,
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Helpers
    // ────────────────────────────────────────────────────────────────────

    function _layerHashes() internal pure returns (uint256[10] memory arr) {
        arr[0] = LAYER_HASH_0;
        arr[1] = LAYER_HASH_1;
        arr[2] = LAYER_HASH_2;
        arr[3] = LAYER_HASH_3;
        arr[4] = LAYER_HASH_4;
    }

    function _verifyBlockPrimary() internal {
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

    // ────────────────────────────────────────────────────────────────────
    // Genesis state
    // ────────────────────────────────────────────────────────────────────

    function test_genesisState_seededFromConstructor() public view {
        assertEq(
            bridge.storedBkSetCommitment(),
            BK_SET_POSEIDON,
            "BK-set commitment seeded at construction"
        );
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            PREV_MAX_LEVEL_LAYER_HASH,
            "chain anchor seeded at construction"
        );
        assertEq(bridge.storedLastSeenBlockSeqNo(), 0, "no blocks verified yet");
        assertEq(bridge.storedNumLayers(), 0, "no layers stored yet");
    }

    // ────────────────────────────────────────────────────────────────────
    // Positive: real bound Primary + LayerHashes proofs verify and
    // advance the on-chain state.
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_primary_bound_succeeds_andUpdatesState() public {
        vm.expectEmit(true, true, false, true, address(bridge));
        emit BlockVerified(
            BLOCK_ID, BLOCK_SEQ_NO, AckiNackiBridge.FinalizationType.Primary, NUM_LAYERS
        );

        _verifyBlockPrimary();

        assertEq(bridge.storedLastSeenBlockSeqNo(), BLOCK_SEQ_NO, "seqno advanced");
        assertEq(bridge.storedNumLayers(), NUM_LAYERS, "numLayers stored");

        uint256[10] memory expected = _layerHashes();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(bridge.storedLayerHashes(i), expected[i], "layer hash slot stored");
        }
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            LAYER_HASH_4,
            "chain anchor advanced to new top of chain"
        );
        assertEq(bridge.storedBkSetCommitment(), BK_SET_POSEIDON, "BK set unchanged (no rotation)");
    }

    function test_verifyBlock_storedLayerHashes_view_matches_individual_slots() public {
        _verifyBlockPrimary();
        uint256[10] memory storedView = bridge.getStoredLayerHashes();
        uint256[10] memory expected = _layerHashes();
        for (uint256 i = 0; i < 10; i++) {
            assertEq(storedView[i], expected[i], "view returns same as per-slot");
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Positive: Fallback finalization path (mocked verifier returns true)
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_fallback_routesToFallbackVerifier() public {
        fallbackVerifier.setShouldAccept(true);

        vm.expectEmit(true, true, false, true, address(bridge));
        emit BlockVerified(
            BLOCK_ID, BLOCK_SEQ_NO, AckiNackiBridge.FinalizationType.Fallback, NUM_LAYERS
        );

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Fallback,
            PROOF_PRIMARY, // attestationProof bytes are forwarded verbatim
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), BLOCK_SEQ_NO, "fallback advanced state");
    }

    function test_verifyBlock_fallback_rejected_byMock_reverts() public {
        // shouldAccept defaults to false → fallbackVerifier returns false.
        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Fallback,
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

    // ────────────────────────────────────────────────────────────────────
    // Negative: shape / range
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_numLayersZero_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.InvalidNumLayers.selector, uint8(0)));
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            0,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_numLayersAboveCap_reverts() public {
        // numLayers param is uint8; 11 is > MAX_LAYER_HASHES (10).
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.InvalidNumLayers.selector, uint8(11))
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            11,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_layerTailNonZero_reverts() public {
        uint256[10] memory hashes = _layerHashes();
        hashes[7] = uint256(keccak256("phantom-layer")); // tail (>= numLayers=5) must be 0
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.LayerHashTailNonZero.selector, uint256(7))
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            hashes,
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Negative: anchor & monotonic invariants
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_bkSetMismatch_reverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkSetCommitmentMismatch.selector,
                BK_SET_POSEIDON ^ 1,
                BK_SET_POSEIDON
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON ^ 1,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_seqNoNotMonotonic_reverts() public {
        // Genesis storedLastSeen = 0, supplying blockSeqNo = 0 must fail.
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BlockSeqNoNotMonotonic.selector, uint64(0), uint64(0)
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            0,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_replayAfterSuccess_reverts() public {
        _verifyBlockPrimary();

        // Now storedLastSeen = BLOCK_SEQ_NO. Re-submitting must revert via
        // monotonicity (the proof would also revert via prev-anchor, but the
        // monotonic check fires first).
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BlockSeqNoNotMonotonic.selector, BLOCK_SEQ_NO, BLOCK_SEQ_NO
            )
        );
        _verifyBlockPrimary();
    }

    function test_verifyBlock_prevAnchorMismatch_reverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.PrevAnchorMismatch.selector,
                PREV_MAX_LEVEL_LAYER_HASH ^ 1,
                PREV_MAX_LEVEL_LAYER_HASH
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH ^ 1
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Negative: cryptographic rejection (proof tampering / wrong public inputs)
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_tamperedAttestationProof_reverts() public {
        bytes memory tampered = PROOF_PRIMARY;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xFF);
        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            tampered,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_tamperedLayerHashesProof_reverts() public {
        bytes memory tampered = PROOF_LAYER_HASHES;
        tampered[80] = bytes1(uint8(tampered[80]) ^ 0xFF);
        vm.expectRevert(AckiNackiBridge.LayerHashesProofRejected.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            tampered,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function test_verifyBlock_blockIdMismatchAcrossProofs_reverts() public {
        // Supplying a wrong blockId. Both verifiers will be fed the wrong
        // value as a public input, so whichever runs first rejects. The
        // attestation verifier runs first → AttestationProofRejected.
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

    // ────────────────────────────────────────────────────────────────────
    // VerifyBlockDisabled (one-of-three-zero verifiers)
    // ────────────────────────────────────────────────────────────────────

    function test_verifyBlock_disabled_revertsOnFreshBridge() public {
        AckiNackiBridge disabled = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        vm.expectRevert(AckiNackiBridge.VerifyBlockDisabled.selector);
        disabled.verifyBlock(
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

    function test_verifyBlock_partiallyWired_revertsAsDisabled() public {
        // Only primary wired; fallback + layer-hashes both zero.
        AckiNackiBridge partialBridge = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(0)),
                ILayerHashesMovementVerifier(address(0)),
                BK_SET_POSEIDON,
                PREV_MAX_LEVEL_LAYER_HASH
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        vm.expectRevert(AckiNackiBridge.VerifyBlockDisabled.selector);
        partialBridge.verifyBlock(
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
}

