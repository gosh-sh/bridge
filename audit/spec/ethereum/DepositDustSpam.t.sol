// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositDustSpamTest
/// @notice TD-33 — dust spam: min `amount=1` per tx; aggregate TVL uncapped (QC-A1-1 / A1-F2).
/// @dev Per-tx cap only (`MAX_DEPOSIT_AMOUNT`); no global dust throttle on L1.
contract DepositDustSpamTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

    /// Smallest allowed deposit (1 base unit = 1e-6 USDC with 6 decimals).
    uint256 internal constant DUST = 1;

    function setUp() public {
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc = new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_td33_n_dust_deposits_counter_and_treasury() public {
        uint256 n = 40;
        address user = address(0xD00D);

        vm.startPrank(user);
        usdc.mint(user, n);
        usdc.approve(address(bridge), n);
        for (uint256 i = 0; i < n; i++) {
            bridge.deposit(DUST, int8(0), bytes32(uint256(0xABCD + i)));
        }
        vm.stopPrank();

        assertEq(bridge.depositCounter(), n, "TD-33: counter");
        assertEq(bridge.treasuryBalance(), n, "TD-33: treasury += N dust units");
        assertEq(usdc.balanceOf(address(bridge)), n, "custody");
    }

    function test_td33_many_dust_aggregate_tvl_uncapped_qc() public {
        uint256 n = 50;
        uint256 expected = n * DUST;

        for (uint256 i = 0; i < n; i++) {
            address user = address(uint160(0x2000 + i));
            UsdcTestLib.depositUsdc(vm, usdc, bridge, user, DUST);
        }

        assertEq(bridge.depositCounter(), n);
        assertEq(bridge.treasuryBalance(), expected, "QC-A1-1/A1-F2: aggregate dust uncapped");
        assertGt(bridge.MAX_DEPOSIT_AMOUNT(), expected, "per-tx cap does not limit aggregate");
    }

    function test_td33_dust_one_unit_is_minimum_not_zero() public {
        address user = address(0xBEEF);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, DUST);
        assertEq(bridge.treasuryBalance(), DUST);

        vm.startPrank(user);
        usdc.approve(address(bridge), DUST);
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.deposit(0, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }
}
