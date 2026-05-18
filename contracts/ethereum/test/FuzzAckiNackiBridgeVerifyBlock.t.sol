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

/// @title FuzzAckiNackiBridgeVerifyBlockTest
/// @notice Property-based coverage for the input-validation invariants of
///         `AckiNackiBridge.verifyBlock`. Mirrors the deterministic
///         `AckiNackiBridgeVerifyBlockTest` revert-path cases but lets Foundry
///         pick the offending value at random so we don't only ever exercise
///         a single boundary point per invariant.
///
/// The fuzz cases here only cover **pre-crypto** checks (the cheap ones the
/// contract runs before reaching the Groth16 verifiers): `VerifyBlockDisabled`,
/// `InvalidNumLayers`, `LayerHashTailNonZero`, `BkSetCommitmentMismatch`,
/// `BlockSeqNoNotMonotonic`, `PrevAnchorMismatch`. Cryptographic rejection
/// paths (`AttestationProofRejected`, `LayerHashesProofRejected`) are out of
/// scope here — they need real Halo2 proofs and are already covered with
/// targeted negatives in `AckiNackiBridgeVerifyBlockTest`.
///
/// Reuses the same bound scenario (Circuit 1A + Circuit 2 against one synthetic
/// block) so that "real proofs, one fuzz-perturbed input" stays meaningful —
/// the perturbed input causes a revert *before* the contract ever calls into a
/// verifier, leaving the actual proof bytes untouched.
contract FuzzAckiNackiBridgeVerifyBlockTest is Test {
    // ─── Bound scenario public inputs (same as the deterministic suite) ──
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

    bytes internal constant PROOF_PRIMARY =
        hex"250e54dc356d7a3e75a001445d8e68161026769c256e24ee2a98989fe3b053cf0cab97566d6da19f08ecc0f80f8b4fc07c2237c66bde30d86772a27a28e0c36d0d3040adb45f17b39c3770752fb313e6c5197fd62888e53f234488f5d2345c05018bb679a901cf6e7941fa51eca8eae4fb096550384f09a5daf5056d3626797a1f366f5b7d5a8092bc38f3733682929023b13aa4cb5acf9d012f2454f08b13b52e397c247d57ad5f7428dad9b1138b16194591e9c3083b11146220258df92c3518ec536d04b52fbdc6d68f3e68b8e9f99fd489ee76e698d1bf00c6d1def9d01403dfc6047d2fdd625739e75cd08063cb5502e04e39c74d667ed44c5290bdda7b";

    bytes internal constant PROOF_LAYER_HASHES =
        hex"1f5fa2faf1f9332e6794fa01ff88ee2e8f1638633573b152f13e922bce2964ce255e3654cd95b3d59487fa7287da197f6cbd4a905f3f2cb3d92ad4dd32c9682e09316a5373289b332399f5db8bfe0e9537778f3a7b1f17a092d2047423eba1863012f5546d7730e009b0aa1dca6252252b033944e201fdc22ad7401a4713b45d2669e67b2c106c75bf765fe78a6a9cfbfb949b5ac16fd7cc91448677e3be07f709077287569a62617c398085a233da6b2598bdc0e9fcb3cf653ced49a9d808ed1ea221ef5fedff54bdb7f6bc1595857c6d00cffc0b416acff8db68d075ff78a206cb9d3d5717ecab99303bbe4ac69eb4b08ea4da4d1e1e4057ebe09db8b0bf98";

    // ─── Test rig ─────────────────────────────────────────────────────────
    AckiNackiBridge internal bridge;
    AckiNackiBridge internal disabledBridge;
    MockBlockHeaderOracle internal oracle;
    PrimaryGroth16VerifierGenerated internal primaryGroth16;
    PrimaryVerifier internal primaryVerifier;
    LayerHashesGroth16VerifierGenerated internal layerHashesGroth16;
    LayerHashesMovementVerifier internal layerHashesVerifier;
    MockFallbackVerifier internal fallbackVerifier;

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
            VerifyBlockConfigLib.disabledBridgeEvent()
        );

        disabledBridge = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent()
        );
    }

    function _layerHashes() internal pure returns (uint256[10] memory arr) {
        arr[0] = LAYER_HASH_0;
        arr[1] = LAYER_HASH_1;
        arr[2] = LAYER_HASH_2;
        arr[3] = LAYER_HASH_3;
        arr[4] = LAYER_HASH_4;
    }

    // ────────────────────────────────────────────────────────────────────
    // Invariant: `verifyBlock` on a bridge with one or more zero verifier
    // slots always reverts with `VerifyBlockDisabled`, regardless of inputs.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_disabledBridge_alwaysReverts(
        uint8 finTypeRaw,
        uint256 blockId,
        uint256 bkSet,
        uint64 seqNo,
        uint8 numLayers,
        uint256 prevHash,
        uint256 layerSeed
    ) public {
        AckiNackiBridge.FinalizationType finType =
            AckiNackiBridge.FinalizationType(uint8(bound(finTypeRaw, 0, 1)));

        uint256[10] memory hashes;
        for (uint256 i = 0; i < 10; i++) {
            hashes[i] = uint256(keccak256(abi.encode(layerSeed, i)));
        }

        vm.expectRevert(AckiNackiBridge.VerifyBlockDisabled.selector);
        disabledBridge.verifyBlock(
            finType,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            blockId,
            bkSet,
            seqNo,
            numLayers,
            hashes,
            prevHash
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Invariant: `numLayers ∉ [1, MAX_LAYER_HASHES=10]` always reverts with
    // `InvalidNumLayers(numLayers)` *before* any crypto work happens.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_invalidNumLayers_reverts(uint8 numLayers) public {
        vm.assume(numLayers == 0 || numLayers > 10);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.InvalidNumLayers.selector, numLayers)
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            BLOCK_SEQ_NO,
            numLayers,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Invariant: any non-zero element in `layerHashes[numLayers..10]`
    // reverts with `LayerHashTailNonZero(firstBadIndex)`. The contract
    // scans left-to-right, so the first non-zero slot ≥ numLayers wins.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_layerTailNonZero_reverts(uint8 slotRaw, uint256 phantom) public {
        // Restrict slot to a tail position [NUM_LAYERS .. 9].
        uint256 slot = bound(slotRaw, NUM_LAYERS, 9);
        vm.assume(phantom != 0);

        uint256[10] memory hashes = _layerHashes();
        // Clear any tail noise so `slot` is provably the first non-zero
        // tail slot (the helper only fills [0..NUM_LAYERS-1]).
        for (uint256 i = NUM_LAYERS; i < 10; i++) {
            hashes[i] = 0;
        }
        hashes[slot] = phantom;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.LayerHashTailNonZero.selector, slot));
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
    // Invariant: any `bkSetCommitment` other than `storedBkSetCommitment`
    // reverts with `BkSetCommitmentMismatch(supplied, stored)`.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_bkSetMismatch_reverts(uint256 bkSet) public {
        vm.assume(bkSet != BK_SET_POSEIDON);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkSetCommitmentMismatch.selector, bkSet, BK_SET_POSEIDON
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            bkSet,
            BLOCK_SEQ_NO,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Invariant: any `blockSeqNo <= storedLastSeenBlockSeqNo` reverts with
    // `BlockSeqNoNotMonotonic(supplied, stored)`. On a fresh bridge
    // `storedLastSeenBlockSeqNo == 0`, so the offending value space is the
    // single u64 zero — fuzz it as a regression check anyway, and then run
    // a second case after one successful verify so the stored anchor moves
    // to 1 and the value space widens to {0, 1}.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_seqNoNotMonotonic_genesis_reverts(uint64 seqNo) public {
        vm.assume(seqNo == 0);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BlockSeqNoNotMonotonic.selector, seqNo, uint64(0)
            )
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            seqNo,
            NUM_LAYERS,
            _layerHashes(),
            PREV_MAX_LEVEL_LAYER_HASH
        );
    }

    function testFuzz_seqNoNotMonotonic_afterAdvance_reverts(uint64 seqNoRaw) public {
        // First do one successful verifyBlock so stored advances to BLOCK_SEQ_NO=1.
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
        uint64 stored = bridge.storedLastSeenBlockSeqNo();
        assertEq(stored, BLOCK_SEQ_NO, "precondition: stored == BLOCK_SEQ_NO after advance");

        // Pick any seqNo in [0, stored] — all such values must trip monotonicity.
        uint64 seqNo = uint64(bound(seqNoRaw, 0, stored));

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BlockSeqNoNotMonotonic.selector, seqNo, stored)
        );
        // Use LAYER_HASH_4 as the new prev anchor (set by the previous verify).
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            PROOF_PRIMARY,
            PROOF_LAYER_HASHES,
            BLOCK_ID,
            BK_SET_POSEIDON,
            seqNo,
            NUM_LAYERS,
            _layerHashes(),
            LAYER_HASH_4
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Invariant: any `prevMaxLevelLayerHash != storedPrevMaxLevelLayerHash`
    // reverts with `PrevAnchorMismatch(supplied, stored)`. To reach this
    // check, the supplied (bkSet, seqNo) must already be valid — the fuzz
    // input perturbs only the prev-anchor field.
    // ────────────────────────────────────────────────────────────────────
    function testFuzz_prevAnchorMismatch_reverts(uint256 prevHash) public {
        vm.assume(prevHash != PREV_MAX_LEVEL_LAYER_HASH);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.PrevAnchorMismatch.selector, prevHash, PREV_MAX_LEVEL_LAYER_HASH
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
            prevHash
        );
    }
}
