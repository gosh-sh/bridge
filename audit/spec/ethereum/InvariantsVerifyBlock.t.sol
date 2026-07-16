// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsVerifyBlockTest
/// @notice Phase D — F-VB-1 / LH-6 strict monotonic blockSeqNo under mock verifyBlock loop.
contract InvariantsVerifyBlockTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    VerifyBlockHandler internal handler;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;

    function setUp() public {
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _seedBlock1();
        handler = new VerifyBlockHandler(bridge, BK_SET, 3, 2);
        targetContract(address(handler));
    }

    function _seedBlock1() internal {
        uint256[10] memory layers;
        layers[0] = 0xC1;
        layers[1] = 0xC2;
        layers[2] = 0xC3;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"11",
            hex"22",
            0x6001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: LH-6 — stored seq equals last successful submission.
    function invariant_LH6_blockSeqNoMonotonic() public view {
        assertEq(bridge.storedLastSeenBlockSeqNo(), handler.nextSeq() - 1, "LH-6 seq cursor");
    }
}
