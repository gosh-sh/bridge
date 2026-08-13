// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockBridgeWithdrawalVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositIsolationTest
/// @notice Phase C / A1 — DEP-6: deposit must not mutate verifyBlock / withdraw state.
contract DepositIsolationTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    address internal user = address(0xF00D);

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
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
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        _seedVerifyBlock();
    }

    function _seedVerifyBlock() internal {
        uint256[10] memory layers;
        layers[0] = 0xA1;
        layers[1] = 0xA2;
        layers[2] = 0xA3;

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"aa",
            hex"bb",
            0x1001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    function test_deposit_doesNotMutateVerifyBlockOrWithdrawState() public {
        uint64 seqBefore = bridge.storedLastSeenBlockSeqNo();
        uint256 bkBefore = bridge.storedBkSetCommitment();
        uint256 anchorBefore = bridge.storedPrevMaxLevelLayerHash();
        uint256 l1Before = bridge.getLatestPerLayer()[0];
        bool anchorKnownBefore = bridge.isKnownLayerAnchor(1, l1Before);
        uint256 nullifierProbe = 0xCAFE;
        assertFalse(bridge.isNullifierUsed(nullifierProbe), "pre: nullifier unused");

        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 5 * UsdcTestLib.UNIT);

        assertEq(bridge.storedLastSeenBlockSeqNo(), seqBefore, "seq unchanged");
        assertEq(bridge.storedBkSetCommitment(), bkBefore, "bkSet unchanged");
        assertEq(bridge.storedPrevMaxLevelLayerHash(), anchorBefore, "flat anchor unchanged");
        assertEq(bridge.getLatestPerLayer()[0], l1Before, "layer hash unchanged");
        assertEq(bridge.isKnownLayerAnchor(1, l1Before), anchorKnownBefore, "window unchanged");
        assertFalse(bridge.isNullifierUsed(nullifierProbe), "nullifier map untouched");
        assertEq(bridge.treasuryBalance(), 5 * UsdcTestLib.UNIT, "only treasury ledger moved");
    }
}
