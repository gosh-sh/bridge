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
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title FuzzPauseMatrixTest
/// @notice Phase D — F-PS-1 / A4-INV-4: pause blocks user ops; owner AAVE ops still work.
contract FuzzPauseMatrixTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal seedAnchor;
    address internal funder = address(0xF00D);

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

        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 20 * UsdcTestLib.UNIT);
        seedAnchor = _seedVerifyBlock();
    }

    function _seedVerifyBlock() internal returns (uint256 l1) {
        uint256[10] memory layers;
        layers[0] = 0xD1;
        layers[1] = 0xD2;
        layers[2] = 0xD3;
        l1 = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"ee",
            hex"ff",
            0xA001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: PS-1 — user entrypoint index: 0=deposit, 1=verifyBlock, 2=withdraw, 3=unpaused deposit ok
    function testFuzz_pauseMatrix_userOpsBlocked(uint8 entrypoint) public {
        entrypoint = uint8(bound(entrypoint, 0, 2));

        bridge.pause();
        assertTrue(bridge.paused(), "paused");

        if (entrypoint == 0) {
            vm.startPrank(funder);
            usdc.approve(address(bridge), 1);
            vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
            bridge.deposit(1, int8(0), bytes32(uint256(uint160(funder))));
            vm.stopPrank();
        } else if (entrypoint == 1) {
            uint256[10] memory layers;
            layers[0] = 0xE1;
            uint256 anchor = bridge.expectedPrevAnchor(1);
            vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
            bridge.verifyBlock(
                AckiNackiBridge.FinalizationType.Primary,
                hex"01",
                hex"02",
                0xA002,
                BK_SET,
                2,
                1,
                layers,
                anchor
            );
        } else {
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
                .WithdrawalPublicInputs({
                tokenId: 0,
                amount: 1,
                recipientHi: 0,
                recipientLo: uint256(uint160(address(0x1111))),
                dstChainId: block.chainid,
                senderAccFr: 1,
                dappFr: DAPP_FR,
                accFr: ACC_FR,
                nullifier: 0x9999,
                finalRoot: seedAnchor
            });
            vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
            bridge.withdrawByProof(hex"00", pub);
        }
    }

    /// @dev INV: PS-1 / A4-INV-4 — owner AAVE supply works while paused.
    function testFuzz_pauseMatrix_ownerSupplyStillWorks(uint256 reserveSeed) public {
        uint256 bps = bound(reserveSeed, 0, bridge.MAX_LIQUID_RESERVE_BPS());
        bridge.setLiquidReserveBps(bps);
        bridge.pause();

        uint256 treasuryBefore = bridge.treasuryBalance();
        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.treasuryBalance(), treasuryBefore, "treasury unchanged under supply");
        assertTrue(bridge.suppliedPrincipal() > 0 || bps >= bridge.BPS_DENOMINATOR(), "supplied or all liquid reserved");
    }
}
