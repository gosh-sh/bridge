// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/FeeOnTransferERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title DepositFoTInvariantTest
/// @notice TD-23 / ETH-11 — FoT deposits fail closed (`TransferAmountMismatch`).
/// @dev TR-1 holds because a non-exact custody delta cannot credit `treasuryBalance`.
contract DepositFoTInvariantTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    FeeOnTransferERC20 internal fot;
    FoTTreasuryHandler internal handler;

    uint256 internal constant FOT_FEE_BPS = 500; // 5% — mid-range in 1–10% envelope
    uint256 internal constant FOT_MIN_DEPOSIT = 20; // ceil(10000 / 500) — non-zero fee at 5%

    function setUp() public {
        fot = new FeeOnTransferERC20("FUSDC", "FUSDC", 6, FOT_FEE_BPS);
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(fot),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        handler = new FoTTreasuryHandler(bridge, fot, 0, FOT_MIN_DEPOSIT);
        targetContract(address(handler));
    }

    function _custody() internal view returns (uint256) {
        return fot.balanceOf(address(bridge)) + bridge.suppliedPrincipal();
    }

    /// @dev ETH-11: FoT attempts revert; ledger and custody stay empty.
    function invariant_TR1_FoT_deposit_does_not_credit() public view {
        assertEq(bridge.treasuryBalance(), 0, "ETH-11 FoT must not credit treasury");
        assertEq(_custody(), 0, "ETH-11 FoT must not leave custody");
    }

    function test_TD23_unit_smoke_fot_deposit_reverts() public {
        uint256 amount = 100 * UsdcTestLib.UNIT;
        address user = address(0xF023);

        fot.mint(user, amount);
        vm.startPrank(user);
        fot.approve(address(bridge), amount);
        vm.expectRevert(AckiNackiBridge.TransferAmountMismatch.selector);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(fot.balanceOf(address(bridge)), 0);
    }

    function test_TD23_handler_deposit_reverts_and_TR1_holds() public {
        handler.depositFoT(UsdcTestLib.UNIT);
        assertGt(handler.fotDepositOps(), 0);
        assertEq(bridge.treasuryBalance(), 0);
        assertEq(_custody(), 0);
    }

    function test_TD23_fee_one_percent_reverts() public {
        _assertFoTRevertsAtFeeBps(100);
    }

    function test_TD23_fee_ten_percent_reverts() public {
        _assertFoTRevertsAtFeeBps(1_000);
    }

    function _assertFoTRevertsAtFeeBps(uint256 feeBps) internal {
        FeeOnTransferERC20 token = new FeeOnTransferERC20("F", "F", 6, feeBps);
        AckiNackiBridge b = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(token),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        address user = address(0xFEE);
        uint256 amount = 50 * UsdcTestLib.UNIT;

        token.mint(user, amount);
        vm.startPrank(user);
        token.approve(address(b), amount);
        vm.expectRevert(AckiNackiBridge.TransferAmountMismatch.selector);
        b.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(b.treasuryBalance(), 0);
        assertEq(token.balanceOf(address(b)), 0);
    }
}
