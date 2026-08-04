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
contract AckiNackiBridgeApplyBkSetUpdateTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primary;

    uint256 internal constant L2 = 0xA11CE;
    uint256 internal constant L3 = 0xB0B;
    uint64 internal constant SEQ = 42;

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
        (uint256 blockId, bytes32 h01, bytes32 h4_7, bytes32 h8_15) = _merkleWitness(L2, L3);

        vm.expectEmit(true, true, true, true);
        emit BkSetUpdated(L2, L3, SEQ);

        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            SEQ,
            L2,
            L3,
            h01,
            h4_7,
            h8_15
        );

        assertEq(bridge.storedBkSetCommitment(), L3);
        assertEq(bridge.storedLastBkSetUpdateSeqNo(), SEQ);
    }

    function test_applyBkSetUpdate_revertsOnMerkleMismatch() public {
        (uint256 blockId,,,) = _merkleWitness(L2, L3);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.BkUpdateMerkleMismatch.selector, blockId, blockId + 1)
        );
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId + 1,
            SEQ,
            L2,
            L3,
            bytes32(uint256(0x1234)),
            bytes32(uint256(0x5678)),
            bytes32(uint256(0x9abc))
        );
    }

    function test_applyBkSetUpdate_revertsWhenAttestationRejected() public {
        primary.setShouldAccept(false);
        (uint256 blockId, bytes32 h01, bytes32 h4_7, bytes32 h8_15) = _merkleWitness(L2, L3);

        vm.expectRevert(AckiNackiBridge.AttestationProofRejected.selector);
        bridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            blockId,
            SEQ,
            L2,
            L3,
            h01,
            h4_7,
            h8_15
        );
    }

    /// @dev Depth-4 / 16-leaf block-id tree with (l2, l3) at leaf positions 2 and 3.
    ///      Arbitrary sibling literals — the test only checks that the
    ///      contract reconstructs the same root the helper does.
    ///
    ///      Must mirror the on-chain fold: L2 and L3 are numeric `uint256` Fr
    ///      scalars, hashed by the AN side (bridge-prover-lib
    ///      `block_id_tree.rs:30-31`) as canonical little-endian
    ///      `Fr::to_repr()` bytes. Byte-reverse here so the expected root
    ///      matches what `applyBkSetUpdate` now computes via `_frToLeBytes`.
    function _merkleWitness(uint256 l2, uint256 l3)
        internal
        pure
        returns (uint256 blockId, bytes32 h01, bytes32 h4_7, bytes32 h8_15)
    {
        bytes32 h23 = sha256(abi.encodePacked(_toLe(l2), _toLe(l3)));
        h01 = bytes32(uint256(0x1234));
        bytes32 h0_3 = sha256(abi.encodePacked(h01, h23));
        h4_7 = bytes32(uint256(0x5678));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, h4_7));
        h8_15 = bytes32(uint256(0x9abc));
        blockId = uint256(sha256(abi.encodePacked(h0_7, h8_15)));
    }

    /// @dev Test-local copy of `AckiNackiBridge._frToLeBytes`. Byte-reverses
    ///      a numeric `uint256` into its 32-byte little-endian Fr::to_repr()
    ///      form.
    function _toLe(uint256 v) internal pure returns (bytes32 out) {
        bytes32 be = bytes32(v);
        assembly {
            for { let i := 0 } lt(i, 32) { i := add(i, 1) } {
                out := or(out, shl(mul(8, i), byte(i, be)))
            }
        }
    }

    /// @notice Pinned oracle: expected root for L2=1, L3=2 with fixed
    ///         siblings, computed independently with Python's `hashlib` (the
    ///         same SHA-256 primitive `sha2::Sha256` uses on the AN side in
    ///         `bridge-prover-lib/src/block_id_tree.rs::sha256_combine`). If
    ///         this test drifts, the on-chain fold no longer matches the AN
    ///         convention — see `_frToLeBytes` doc on `AckiNackiBridge.sol`.
    ///
    ///         Reproduce with:
    ///             python3 -c '
    ///             import hashlib
    ///             def sha(a,b): return hashlib.sha256(a+b).digest()
    ///             L2=(1).to_bytes(32,"little"); L3=(2).to_bytes(32,"little")
    ///             h01=(0x11).to_bytes(32,"big"); h4_7=(0x22).to_bytes(32,"big")
    ///             h8_15=(0x33).to_bytes(32,"big")
    ///             h23=sha(L2,L3); h0_3=sha(h01,h23); h0_7=sha(h0_3,h4_7)
    ///             print(sha(h0_7,h8_15).hex())'
    function test_applyBkSetUpdate_pinnedVector_matchesRustSideFold() public {
        uint256 pinnedL2 = 1;
        uint256 pinnedL3 = 2;
        bytes32 pinnedH01   = bytes32(uint256(0x11));
        bytes32 pinnedH4_7  = bytes32(uint256(0x22));
        bytes32 pinnedH8_15 = bytes32(uint256(0x33));
        uint256 expectedRoot =
            0x8f698b0396c584252ab3a26b426baeb1349d473c21d81463e3c2552b795de3de;

        // Rewind the bridge to a state where pinnedL2 is the accepted "old"
        // commitment. Redeploy so we bypass the fixture-wide `L = 0xA11CE`.
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier pinnedPrimary = new MockPrimaryVerifier();
        MockFallbackVerifier pinnedFallback = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier pinnedLayer = new MockLayerHashesMovementVerifier();
        pinnedPrimary.setShouldAccept(true);
        pinnedFallback.setShouldAccept(true);
        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(pinnedPrimary)),
            IFallbackVerifier(address(pinnedFallback)),
            ILayerHashesMovementVerifier(address(pinnedLayer)),
            pinnedL2,
            0
        );
        AckiNackiBridge pinnedBridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            vb,
            VerifyBlockConfigLib.disabledWithdraw()
        );

        // If the pinned root doesn't match the contract's fold, the call
        // reverts BkUpdateMerkleMismatch(expected, expected+1); pass ==
        // proof that _frToLeBytes matches sha256_combine of Fr::to_repr()
        // bytes on the AN side.
        pinnedBridge.applyBkSetUpdate(
            AckiNackiBridge.FinalizationType.Primary,
            hex"00",
            expectedRoot,
            SEQ,
            pinnedL2,
            pinnedL3,
            pinnedH01,
            pinnedH4_7,
            pinnedH8_15
        );

        assertEq(pinnedBridge.storedBkSetCommitment(), pinnedL3);
        assertEq(pinnedBridge.storedLastBkSetUpdateSeqNo(), SEQ);
    }
}
