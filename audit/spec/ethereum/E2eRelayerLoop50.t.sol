// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/Bn254FrLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title E2eRelayerLoop50Test
/// @notice Phase E / E-04 — 50-block relayer acceptance (extends main-tree 10-block loop).
contract E2eRelayerLoop50Test is Test {
    uint256 internal constant NUM_BLOCKS = 50;
    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 5;
    uint256 internal constant FIRST_BLOCK_ID = 0xB10C0001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;

    event BlockVerified(
        uint256 indexed blockId,
        uint64 indexed blockSeqNo,
        AckiNackiBridge.FinalizationType finType,
        uint8 numLayers
    );

    function setUp() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            IPrimaryVerifier(address(primaryVerifier)),
            IFallbackVerifier(address(fallbackVerifier)),
            ILayerHashesMovementVerifier(address(layerHashesVerifier)),
            BK_SET,
            GENESIS_PREV_ANCHOR
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

    function _layersFor(uint256 blockIdx) internal pure returns (uint256[10] memory arr) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            arr[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("layer", blockIdx, i))));
        }
    }

    function _proofBytes(uint256 blockIdx, string memory tag) internal pure returns (bytes memory) {
        return abi.encodePacked(keccak256(abi.encode("proof", blockIdx, tag)));
    }

    function _currentAnchor() internal view returns (uint256) {
        return bridge.expectedPrevAnchor(ACTIVE_LAYERS);
    }

    function _submit(uint256 blockIdx, AckiNackiBridge.FinalizationType finType)
        internal
        returns (uint256[10] memory layers)
    {
        layers = _layersFor(blockIdx);
        uint256 blockId = FIRST_BLOCK_ID + blockIdx - 1;
        uint64 seqNo = FIRST_SEQ_NO + uint64(blockIdx) - 1;

        vm.expectEmit(true, true, false, true);
        emit BlockVerified(blockId, seqNo, finType, ACTIVE_LAYERS);

        bridge.verifyBlock(
            finType,
            _proofBytes(blockIdx, "att"),
            _proofBytes(blockIdx, "lh"),
            blockId,
            BK_SET,
            seqNo,
            ACTIVE_LAYERS,
            layers,
            _currentAnchor()
        );
    }

    /// @dev E-04: relayer drives 50 sequential blocks; on-chain cursor advances exactly 50.
    function test_E04_relayerLoop_50Blocks_mixedFinTypes_advancesState() public {
        for (uint256 idx = 1; idx <= NUM_BLOCKS; idx++) {
            AckiNackiBridge.FinalizationType ft = (idx % 3 == 0)
                ? AckiNackiBridge.FinalizationType.Fallback
                : AckiNackiBridge.FinalizationType.Primary;

            uint256[10] memory layers = _submit(idx, ft);

            assertEq(bridge.storedLastSeenBlockSeqNo(), uint64(idx), "seqNo");
            uint256[10] memory latest = bridge.getLatestPerLayer();
            for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
                assertEq(latest[i], layers[i], "layer head");
            }
            for (uint256 i = ACTIVE_LAYERS; i < 10; i++) {
                assertEq(latest[i], 0, "tail-zero");
            }
        }

        assertEq(bridge.storedLastSeenBlockSeqNo(), uint64(NUM_BLOCKS), "final seqNo");
        assertEq(bridge.storedBkSetCommitment(), BK_SET, "BK set stable");
    }
}
