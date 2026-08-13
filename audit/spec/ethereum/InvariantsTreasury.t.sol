// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

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
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsTreasuryTest
/// @notice Phase D — F-TR-1 / F-TR-2 solvency + treasury conservation.
contract InvariantsTreasuryTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;
    TreasuryHandler internal handler;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    uint256 internal seedAnchor;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawal)), DAPP_FR, ACC_FR
            )
        );

        seedAnchor = _seedVerifyBlock();
        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 100 * UsdcTestLib.UNIT);

        handler = new TreasuryHandler(
            bridge,
            usdc,
            address(this),
            seedAnchor,
            DAPP_FR,
            ACC_FR,
            RECIPIENT,
            100 * UsdcTestLib.UNIT
        );

        targetContract(address(handler));
    }

    function _seedVerifyBlock() internal returns (uint256 l1) {
        uint256[10] memory layers;
        layers[0] = 0xA1;
        layers[1] = 0xA2;
        layers[2] = 0xA3;
        l1 = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"aa",
            hex"bb",
            0x8001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: TR-1
    function invariant_TR1_solvency() public view {
        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 principal = bridge.suppliedPrincipal();
        assertGe(liquid + principal, bridge.treasuryBalance(), "TR-1 solvency");
    }

    /// @dev INV: TR-2
    function invariant_TR2_treasuryConservation() public view {
        assertEq(
            bridge.treasuryBalance(),
            handler.ghostDeposited() - handler.ghostWithdrawn(),
            "TR-2 conservation"
        );
    }

    /// @dev INV: A4-INV-1 (partial)
    function invariant_totalAssetsCoversTreasury() public view {
        assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "totalAssets >= treasury");
    }
}
