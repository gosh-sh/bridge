// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IBridgeWithdrawalFinalVerifier.sol";
import "../src/IBridgeMultiHopVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockBridgeWithdrawalFinalVerifier.sol";
import "./mocks/MockBridgeMultiHopVerifier.sol";
import "./mocks/MockERC20.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";

/// @title AckiNackiBridgeWithdrawByProofBundleTest
/// @notice Multi-thread `withdrawByProofBundle` gate — mirrors the pure-Rust
///         reference `bridge_event_prove_circuit::bundle_verifier::verify_bundle`
///         (see [`crates/bridge-circuits/bridge-event-prove-circuit/src/bundle_verifier.rs`]).
///
///         All snark verification is mocked away here so this suite exercises
///         only the on-chain acceptance gate: constructor wiring,
///         same-thread and cross-thread happy paths (n = 0, 1, N_BUNDLE_MAX),
///         and one revert per custom error introduced by the bundle path.
contract AckiNackiBridgeWithdrawByProofBundleTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal legacyWithdrawalVerifier;
    MockBridgeWithdrawalFinalVerifier internal finalVerifier;
    MockBridgeMultiHopVerifier internal multiHopVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xC10C40001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    address internal constant RECIPIENT =
        address(0x1111111111111111111111111111111111111111);
    address internal funder = address(0xF00D);

    /// @dev The L1 anchor recorded by `setUp`'s seed `verifyBlock`.
    uint256 internal seedAnchor;

    event WithdrawalByProofExecuted(
        uint256 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 indexed tokenId,
        address submitter
    );

    // ─────────────────────────────────────────────────────────────────────
    // Setup
    // ─────────────────────────────────────────────────────────────────────

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        legacyWithdrawalVerifier = new MockBridgeWithdrawalVerifier();
        finalVerifier = new MockBridgeWithdrawalFinalVerifier();
        multiHopVerifier = new MockBridgeMultiHopVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        legacyWithdrawalVerifier.setShouldAccept(true);
        finalVerifier.setShouldAccept(true);
        multiHopVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdrawBundle(
                IBridgeWithdrawalVerifier(address(legacyWithdrawalVerifier)),
                IBridgeWithdrawalFinalVerifier(address(finalVerifier)),
                IBridgeMultiHopVerifier(address(multiHopVerifier)),
                DAPP_FR,
                ACC_FR
            )
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 50 * UsdcTestLib.UNIT);
        seedAnchor = _seedFirstBlock();
    }

    function _seedFirstBlock() internal returns (uint256 l1Anchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("bundle-seed-layer", i))));
        }
        l1Anchor = layers[0];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("bundle-seed-att")),
            abi.encodePacked(keccak256("bundle-seed-lh")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & RECIPIENT_HALF_MASK;
    }

    /// @dev Build a 13-slot FinalProof PI vector wired to the seeded anchor.
    ///      `xBlockId` and `yBlockId` control the same-thread vs cross-thread
    ///      shape.
    function _finalPub(
        uint256 amount,
        uint256 nullifier,
        uint256 xBlockId,
        uint256 yBlockId
    ) internal view returns (uint256[] memory pub) {
        (uint256 hi, uint256 lo) = _split(RECIPIENT);
        pub = new uint256[](13);
        pub[0] = 0;            // tokenId
        pub[1] = amount;
        pub[2] = hi;
        pub[3] = lo;
        pub[4] = block.chainid;
        pub[5] = Bn254FrLib.toFr(uint256(keccak256("bundle-sender-acc")));
        pub[6] = DAPP_FR;
        pub[7] = ACC_FR;
        pub[8] = nullifier;
        pub[9] = seedAnchor;
        pub[10] = 1;           // anchorLayer
        pub[11] = xBlockId;
        pub[12] = yBlockId;
    }

    /// @dev Build a 2-slot hop PI vector.
    function _hopPub(uint256 startBlockId, uint256 endBlockId)
        internal
        pure
        returns (uint256[] memory pub)
    {
        pub = new uint256[](2);
        pub[0] = startBlockId;
        pub[1] = endBlockId;
    }

    function _dummyProof() internal pure returns (bytes memory) {
        return abi.encodePacked(keccak256("dummy-bundle-proof"));
    }

    /// @dev Build an `n`-hop chain of proofs and matching PI vectors that
    ///      walks `x → ... → y` through the sequence `chain[0..=n]` where
    ///      `chain[0] == x` and `chain[n] == y`. Every intermediate id is
    ///      derived deterministically from `seed`.
    function _linearHopChain(uint256 x, uint256 y, uint256 n, string memory seed)
        internal
        pure
        returns (uint256[][] memory hopPubs, bytes[] memory hopProofs)
    {
        require(n > 0, "n>0 required");
        uint256[] memory chain = new uint256[](n + 1);
        chain[0] = x;
        chain[n] = y;
        for (uint256 i = 1; i < n; i++) {
            chain[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode(seed, i))));
        }

        hopPubs = new uint256[][](n);
        hopProofs = new bytes[](n);
        for (uint256 i = 0; i < n; i++) {
            hopPubs[i] = new uint256[](2);
            hopPubs[i][0] = chain[i];
            hopPubs[i][1] = chain[i + 1];
            hopProofs[i] = abi.encodePacked(keccak256(abi.encode("hop-proof", seed, i)));
        }
    }

    function _emptyHops()
        internal
        pure
        returns (uint256[][] memory hopPubs, bytes[] memory hopProofs)
    {
        hopPubs = new uint256[][](0);
        hopProofs = new bytes[](0);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Constructor wiring
    // ─────────────────────────────────────────────────────────────────────

    function test_constructor_bothBundleVerifiersStored() public view {
        assertEq(address(bridge.bridgeWithdrawalFinalVerifier()), address(finalVerifier));
        assertEq(address(bridge.bridgeMultiHopVerifier()), address(multiHopVerifier));
    }

    function test_constructor_partialBundleWiring_finalOnly_reverts() public {
        AckiNackiBridge.BridgeWithdrawConfig memory bw = VerifyBlockConfigLib.withWithdraw(
            IBridgeWithdrawalVerifier(address(legacyWithdrawalVerifier)), DAPP_FR, ACC_FR
        );
        bw.withdrawalFinalVerifier = IBridgeWithdrawalFinalVerifier(address(finalVerifier));
        // multiHopVerifier stays address(0)
        vm.expectRevert(AckiNackiBridge.PartialBundleWiring.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            bw
        );
    }

    function test_constructor_partialBundleWiring_multiHopOnly_reverts() public {
        AckiNackiBridge.BridgeWithdrawConfig memory bw = VerifyBlockConfigLib.withWithdraw(
            IBridgeWithdrawalVerifier(address(legacyWithdrawalVerifier)), DAPP_FR, ACC_FR
        );
        bw.multiHopVerifier = IBridgeMultiHopVerifier(address(multiHopVerifier));
        // withdrawalFinalVerifier stays address(0)
        vm.expectRevert(AckiNackiBridge.PartialBundleWiring.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            bw
        );
    }

    function test_constructor_bundleWithoutLegacyWithdrawal_reverts() public {
        AckiNackiBridge.BridgeWithdrawConfig memory bw = VerifyBlockConfigLib.disabledWithdraw();
        bw.withdrawalFinalVerifier = IBridgeWithdrawalFinalVerifier(address(finalVerifier));
        bw.multiHopVerifier = IBridgeMultiHopVerifier(address(multiHopVerifier));
        vm.expectRevert(AckiNackiBridge.BundleRequiresLegacyWithdrawal.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            bw
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Green paths (bundle length 0, 1, N_BUNDLE_MAX)
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_sameThread_n0_happyPath() public {
        uint256 amount = 3 * UsdcTestLib.UNIT;
        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256("bundle-null-n0")));
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("same-thread-block")));

        uint256[] memory finalPub = _finalPub(amount, nullifier, blockId, blockId);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();

        uint256 recipientBefore = usdc.balanceOf(RECIPIENT);
        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.expectEmit(true, true, true, true);
        emit WithdrawalByProofExecuted(nullifier, RECIPIENT, amount, 0, address(this));

        bool ok = bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
        assertTrue(ok);
        assertEq(usdc.balanceOf(RECIPIENT), recipientBefore + amount);
        assertEq(bridge.treasuryBalance(), treasuryBefore - amount);
        assertTrue(bridge.isNullifierUsed(nullifier));
    }

    function test_bundle_singleHop_n1_happyPath() public {
        uint256 amount = 2 * UsdcTestLib.UNIT;
        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256("bundle-null-n1")));
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("x-block-n1")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("y-block-n1")));

        uint256[] memory finalPub = _finalPub(amount, nullifier, x, y);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _linearHopChain(x, y, 1, "n1");

        bool ok = bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
        assertTrue(ok);
        assertTrue(bridge.isNullifierUsed(nullifier));
    }

    function test_bundle_maxHops_nMax_happyPath() public {
        uint256 amount = 4 * UsdcTestLib.UNIT;
        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256("bundle-null-nMax")));
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("x-block-nMax")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("y-block-nMax")));

        uint256[] memory finalPub = _finalPub(amount, nullifier, x, y);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) =
            _linearHopChain(x, y, bridge.N_BUNDLE_MAX(), "nMax");

        bool ok = bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
        assertTrue(ok);
        assertTrue(bridge.isNullifierUsed(nullifier));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Constructor-disabled gate
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_disabled_reverts() public {
        // Fresh bridge without either bundle verifier wired.
        AckiNackiBridge bare = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(legacyWithdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        uint256[] memory finalPub = _finalPub(1, 1, 1, 1);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(AckiNackiBridge.WithdrawByProofBundleDisabled.selector);
        bare.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Structural sanity reverts
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_finalPublicInputsBadLength_reverts() public {
        uint256[] memory shortPub = new uint256[](12); // one shy of 13
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.FinalPublicInputsBadLength.selector, uint256(12), uint256(13)
            )
        );
        bridge.withdrawByProofBundle(shortPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_hopPublicInputsHopProofsLengthMismatch_reverts() public {
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("mismatch-lengths")));
        uint256[] memory finalPub = _finalPub(1, 1, blockId, blockId);
        uint256[][] memory hopPubs = new uint256[][](1);
        hopPubs[0] = _hopPub(blockId, blockId);
        bytes[] memory hopProofs = new bytes[](2);
        hopProofs[0] = _dummyProof();
        hopProofs[1] = _dummyProof();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HopPublicInputsHopProofsLengthMismatch.selector,
                uint256(1),
                uint256(2)
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_hopBundleLengthOverflow_reverts() public {
        uint256 amount = 1;
        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256("overflow-null")));
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("overflow-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("overflow-y")));
        uint256 nMax = bridge.N_BUNDLE_MAX();
        uint256 tooMany = nMax + 1;
        uint256[] memory finalPub = _finalPub(amount, nullifier, x, y);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) =
            _linearHopChain(x, y, tooMany, "overflow");
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HopBundleLengthOverflow.selector, tooMany, nMax
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_hopPublicInputsBadLength_reverts() public {
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("bad-hop-len")));
        uint256[] memory finalPub = _finalPub(1, 1, blockId, blockId);
        uint256[][] memory hopPubs = new uint256[][](1);
        hopPubs[0] = new uint256[](3); // must be 2
        hopPubs[0][0] = blockId;
        hopPubs[0][1] = blockId;
        hopPubs[0][2] = 0;
        bytes[] memory hopProofs = new bytes[](1);
        hopProofs[0] = _dummyProof();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.HopPublicInputsBadLength.selector,
                uint256(0),
                uint256(3),
                uint256(2)
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bundle-continuity reverts
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_sameThreadEndpointsMismatch_reverts() public {
        // n = 0 but X != Y.
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("mismatch-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("mismatch-y")));
        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(AckiNackiBridge.SameThreadEndpointsMismatch.selector);
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_sameThreadRequiresEmptyHopChain_reverts() public {
        // X == Y but hopCount > 0 — cross-thread bundle offered for a
        // same-thread event.
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("cycle-block")));
        uint256[] memory finalPub = _finalPub(1, 1, blockId, blockId);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) =
            _linearHopChain(blockId, blockId, 1, "cycle");
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.SameThreadRequiresEmptyHopChain.selector, uint256(1)
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_hopChainHeadMismatch_reverts() public {
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("head-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("head-y")));
        uint256 wrongStart = Bn254FrLib.toFr(uint256(keccak256("wrong-start")));

        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        uint256[][] memory hopPubs = new uint256[][](1);
        hopPubs[0] = _hopPub(wrongStart, y);
        bytes[] memory hopProofs = new bytes[](1);
        hopProofs[0] = _dummyProof();

        vm.expectRevert(AckiNackiBridge.HopChainHeadMismatch.selector);
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_adjacentHopBlockIdMismatch_reverts() public {
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("adj-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("adj-y")));
        uint256 mid = Bn254FrLib.toFr(uint256(keccak256("adj-mid")));
        uint256 broken = Bn254FrLib.toFr(uint256(keccak256("adj-broken")));

        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        uint256[][] memory hopPubs = new uint256[][](2);
        hopPubs[0] = _hopPub(x, mid);
        hopPubs[1] = _hopPub(broken, y); // start != mid → gap at index 0
        bytes[] memory hopProofs = new bytes[](2);
        hopProofs[0] = _dummyProof();
        hopProofs[1] = _dummyProof();

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.AdjacentHopBlockIdMismatch.selector, uint256(0)
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_hopChainTailMismatch_reverts() public {
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("tail-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("tail-y")));
        uint256 wrongEnd = Bn254FrLib.toFr(uint256(keccak256("wrong-end")));

        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        uint256[][] memory hopPubs = new uint256[][](1);
        hopPubs[0] = _hopPub(x, wrongEnd);
        bytes[] memory hopProofs = new bytes[](1);
        hopProofs[0] = _dummyProof();

        vm.expectRevert(AckiNackiBridge.HopChainTailMismatch.selector);
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Canonicity + shape reverts (bundle-specific slots)
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_nonCanonicalXBlockId_reverts() public {
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("non-canon")));
        uint256[] memory finalPub = _finalPub(1, 1, blockId, blockId);
        finalPub[11] = Bn254FrLib.R; // xBlockId now == R (non-canonical)
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.FieldElementOutOfRange.selector, Bn254FrLib.R
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_nonCanonicalHopEndpoint_reverts() public {
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("canon-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("canon-y")));
        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        uint256[][] memory hopPubs = new uint256[][](1);
        hopPubs[0] = _hopPub(x, y);
        hopPubs[0][0] = Bn254FrLib.R; // hopStartBlockId non-canonical
        bytes[] memory hopProofs = new bytes[](1);
        hopProofs[0] = _dummyProof();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.FieldElementOutOfRange.selector, Bn254FrLib.R
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Crypto reverts (mock crypto rejection)
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_finalProofRejected_reverts() public {
        finalVerifier.setShouldAccept(false);
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("reject-final")));
        uint256[] memory finalPub = _finalPub(1, 1, blockId, blockId);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();
        vm.expectRevert(AckiNackiBridge.WithdrawalProofRejected.selector);
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    function test_bundle_multiHopProofRejected_reverts() public {
        uint256 x = Bn254FrLib.toFr(uint256(keccak256("reject-mh-x")));
        uint256 y = Bn254FrLib.toFr(uint256(keccak256("reject-mh-y")));
        uint256 mid = Bn254FrLib.toFr(uint256(keccak256("reject-mh-mid")));

        uint256[] memory finalPub = _finalPub(1, 1, x, y);
        uint256[][] memory hopPubs = new uint256[][](2);
        hopPubs[0] = _hopPub(x, mid);
        hopPubs[1] = _hopPub(mid, y);
        bytes[] memory hopProofs = new bytes[](2);
        hopProofs[0] = _dummyProof();
        hopProofs[1] = _dummyProof();

        // Reject the second hop; the first passes.
        multiHopVerifier.setRejectFor(mid, y);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.MultiHopProofRejected.selector, uint256(1)
            )
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Replay
    // ─────────────────────────────────────────────────────────────────────

    function test_bundle_replayRejected() public {
        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256("replay-null")));
        uint256 blockId = Bn254FrLib.toFr(uint256(keccak256("replay-block")));
        uint256[] memory finalPub = _finalPub(1 * UsdcTestLib.UNIT, nullifier, blockId, blockId);
        (uint256[][] memory hopPubs, bytes[] memory hopProofs) = _emptyHops();

        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, nullifier)
        );
        bridge.withdrawByProofBundle(finalPub, _dummyProof(), hopPubs, hopProofs);
    }
}
