// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title OwnerPrincipalTest
/// @notice Phase C / A1+A4 — A4-INV-3 / SWC-105: owner paths cannot reduce treasuryBalance.
contract OwnerPrincipalTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    address internal user = address(0xA1);
    address internal deputy = address(0xB0B);

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 20 * UsdcTestLib.UNIT);
    }

    function test_ownerOperations_neverReduceTreasuryBalance() public {
        uint256 treasuryStart = bridge.treasuryBalance();
        assertEq(treasuryStart, 20 * UsdcTestLib.UNIT);

        bridge.supplyToAave(type(uint256).max);
        assertEq(bridge.treasuryBalance(), treasuryStart, "supplyToAave");

        aUSDC.accrueYield(address(bridge), 500_000);
        usdc.mint(address(pool), 500_000);

        bridge.harvestYield(200_000);
        assertEq(bridge.treasuryBalance(), treasuryStart, "harvestYield");

        bridge.withdrawFromAave(1 * UsdcTestLib.UNIT);
        assertEq(bridge.treasuryBalance(), treasuryStart, "withdrawFromAave");

        bridge.setLiquidReserveBps(2_000);
        assertEq(bridge.treasuryBalance(), treasuryStart, "setLiquidReserveBps");

        bridge.setYieldRecipient(deputy);
        assertEq(bridge.treasuryBalance(), treasuryStart, "setYieldRecipient");

        bridge.setAaveEnabled(false);
        assertEq(bridge.treasuryBalance(), treasuryStart, "setAaveEnabled");

        bridge.setAaveEnabled(true);
        bridge.emergencyWithdrawAll();
        assertEq(bridge.treasuryBalance(), treasuryStart, "emergencyWithdrawAll");

        bridge.transferOwnership(deputy);
        vm.prank(deputy);
        assertEq(bridge.treasuryBalance(), treasuryStart, "transferOwnership");
    }
}
