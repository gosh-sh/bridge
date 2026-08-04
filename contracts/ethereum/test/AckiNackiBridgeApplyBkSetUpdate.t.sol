// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeApplyBkSetUpdateTest
/// @notice Covers the BK-set rotation entry point against the canonical
///         **16-leaf, depth-4** block-id tree: three open siblings
///         (`h01` / `h4_7` / `h8_15`) and an LE `Fr` preimage for the L2/L3
///         leaf pair.
contract AckiNackiBridgeApplyBkSetUpdateTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    uint64 internal constant SEQ = 42;

    /// @dev BN254 scalar field order.
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    bytes32 internal constant SIB_H01 = bytes32(uint256(0x1234));
    bytes32 internal constant SIB_H4_7 = bytes32(uint256(0x5678));
    bytes32 internal constant SIB_H8_15 = bytes32(uint256(0x9ABC));

    event BkSetUpdated(uint256 indexed oldCommitment, uint256 indexed newCommitment, uint64 indexed blockSeqNo);

    function setUp() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();

        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primary)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layer)),
            L2,
            0
        );

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            vb,
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_applyBkSetUpdate_happyPath() public {
        uint256 blockId = _merkleRoot(L2, L3);

        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, SEQ);

        _apply(blockId, SEQ, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), SEQ);
    }

    /// @notice Pins the on-chain fold against a vector computed independently
    ///         of this contract (python `hashlib`, same construction as
    ///         `bridge-verifier-daemon`'s pre-flight fold):
    ///
    ///         ```
    ///         h23  = SHA256(LE(0xA11CE) ‖ LE(0xB0B))
    ///         h0_3 = SHA256(0x11..11 ‖ h23)
    ///         h0_7 = SHA256(h0_3 ‖ 0x22..22)
    ///         root = SHA256(h0_7 ‖ 0x33..33)
    ///         ```
    ///
    ///         Catches both regressions the 16-leaf migration can hide: a
    ///         fold at the wrong depth, and a big-endian L2/L3 preimage (which
    ///         would yield `0xd37275f3…` instead). This particular root happens
    ///         to be below the field order already, so the `Fr` reduction is a
    ///         no-op here and the vector stays a pure statement about the fold.
    function test_applyBkSetUpdate_matchesOffChainVector() public {
        bytes32 h01 =
            bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111));
        bytes32 h4_7 =
            bytes32(uint256(0x2222222222222222222222222222222222222222222222222222222222222222));
        bytes32 h8_15 =
            bytes32(uint256(0x3333333333333333333333333333333333333333333333333333333333333333));
        uint256 expectedRoot =
            uint256(0x28ee9f98c4d654e9e4b33712fa1652ecd9fecd2a591fac1370b1b51e2b65fba4);

        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            expectedRoot,
            SEQ,
            L2,
            L3,
            h01,
            h4_7,
            h8_15
        );

        assertEq(bridge.storedBkSetCommitment(), L3);
    }

    /// @notice A depth-3 (8-leaf) fold must no longer be accepted — the old
    ///         two-sibling root is not a valid `blockId` any more. Reduced into
    ///         `Fr` like any real argument would be, so this stays a statement
    ///         about the fold depth and not about field canonicality (which
    ///         `test_applyBkSetUpdate_rejectsUnreducedRoot` covers separately).
    function test_applyBkSetUpdate_rejectsLegacyDepth3Root() public {
        bytes32 h23 = sha256(abi.encodePacked(_le(L2), _le(L3)));
        uint256 legacyRoot = uint256(
            sha256(abi.encodePacked(sha256(abi.encodePacked(SIB_H01, h23)), SIB_H4_7))
        ) % R;
        uint256 expectedRoot = _merkleRoot(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkUpdateMerkleMismatch.selector, expectedRoot, legacyRoot
            )
        );
        _apply(legacyRoot, SEQ, L2, L3);
    }

    /// @notice `blockId` has two consumers inside `applyBkSetUpdate`: the
    ///         attestation adapter, which compares it byte-for-byte against a
    ///         public instance read out of the proof, and the SHA-256 fold. The
    ///         first can only ever match a canonical `Fr`; the second produces a
    ///         raw 256-bit hash, and only `R / 2^256 = 18.9%` of those are
    ///         canonical. Unless the contract reduces before comparing, no
    ///         single argument satisfies both for the other ~81% of rotations —
    ///         this fixture's root among them — and the entry point is dead.
    function test_applyBkSetUpdate_reducesRootIntoFieldBeforeComparing() public {
        uint256 rawRoot = _rawMerkleRoot(L2, L3);
        assertGe(rawRoot, R, "fixture must exercise the non-canonical root case");

        _apply(rawRoot % R, SEQ, L2, L3);

        assertEq(bridge.storedBkSetCommitment(), L3);
    }

    /// @notice The mirror of the above: the raw root is what the fold literally
    ///         produces, and it is still not a valid `blockId`. No circuit can
    ///         have committed to it, so the attestation gate rejects it before
    ///         the fold is even reached.
    function test_applyBkSetUpdate_rejectsUnreducedRoot() public {
        uint256 rawRoot = _rawMerkleRoot(L2, L3);
        assertGe(rawRoot, R, "fixture must exercise the non-canonical root case");

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        _apply(rawRoot, SEQ, L2, L3);
    }

    function test_applyBkSetUpdate_revertsOnMerkleMismatch() public {
        uint256 blockId = _merkleRoot(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.BkUpdateMerkleMismatch.selector, blockId, blockId + 1
            )
        );
        _apply(blockId + 1, SEQ, L2, L3);
    }

    function test_applyBkSetUpdate_revertsWhenAttestationRejected() public {
        primary.setShouldAccept(false);
        uint256 blockId = _merkleRoot(L2, L3);

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        _apply(blockId, SEQ, L2, L3);
    }

    function test_applyBkSetUpdate_revertsOnStaleOldCommitment() public {
        uint256 wrongL2 = L2 + 1;
        uint256 blockId = _merkleRoot(wrongL2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.StaleBkSetCommitment.selector, wrongL2, L2)
        );
        _apply(blockId, SEQ, wrongL2, L3);
    }

    function test_applyBkSetUpdate_revertsOnReplay() public {
        uint256 blockId = _merkleRoot(L2, L3);
        _apply(blockId, SEQ, L2, L3);

        // Second submission of the same rotation: the commitment has moved on,
        // so the stale-commitment guard fires before the seq-no guard.
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.StaleBkSetCommitment.selector, L2, L3)
        );
        _apply(blockId, SEQ, L2, L3);
    }

    function test_applyBkSetUpdate_revertsOnNonMonotonicSeqNo() public {
        uint256 l4 = 0xC0FFEE;
        uint256 secondBlockId = _merkleRoot(L3, l4);

        _apply(_merkleRoot(L2, L3), SEQ, L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkUpdateSeqNoNotMonotonic.selector, SEQ, SEQ)
        );
        _apply(secondBlockId, SEQ, L3, l4);
    }

    function test_applyBkSetUpdate_chainsTwoRotations() public {
        uint256 l4 = 0xC0FFEE;

        _apply(_merkleRoot(L2, L3), SEQ, L2, L3);
        _apply(_merkleRoot(L3, l4), SEQ + 1, L3, l4);

        assertEq(bridge.storedBkSetCommitment(), l4);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), SEQ + 1);
    }

    function _apply(uint256 blockId, uint64 seqNo, uint256 oldL2, uint256 newL3) internal {
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            seqNo,
            oldL2,
            newL3,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    /// @dev Depth-4 fold of the L2/L3 pair up to the block-id root, mirroring
    ///      `BlockIdMerkleTree::siblings_for_l2_l3` on the prover side, reduced
    ///      into BN254 `Fr` the way the contract and the circuit both see it.
    function _merkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        return _rawMerkleRoot(l2, l3) % R;
    }

    /// @dev The unreduced SHA-256 root, before it is mapped into `Fr`. Only the
    ///      tests that care about the difference use this.
    function _rawMerkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        bytes32 h23 = sha256(abi.encodePacked(_le(l2), _le(l3)));
        bytes32 h0_3 = sha256(abi.encodePacked(SIB_H01, h23));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, SIB_H4_7));
        return uint256(sha256(abi.encodePacked(h0_7, SIB_H8_15)));
    }

    /// @dev Little-endian `Fr::to_repr()` image, matching the bytes the AN node
    ///      hashes into leaves L2/L3.
    function _le(uint256 value) internal pure returns (bytes32) {
        uint256 reversed;
        for (uint256 i = 0; i < 32; i++) {
            reversed = (reversed << 8) | (value & 0xff);
            value >>= 8;
        }
        return bytes32(reversed);
    }
}
