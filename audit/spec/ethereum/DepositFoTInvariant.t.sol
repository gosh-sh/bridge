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
/// @notice TD-23 — TR-1 under fee-on-transfer token assumptions (QC-A1-2 / DEP-FOT-ASSUME).
/// @dev INV: TR-1 (`liquid + principal >= treasuryBalance`) is **not** applicable to FoT tokens;
///      stateful campaign uses inverted invariant documenting the solvency gap after deposits.
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

    function _tr1Holds() internal view returns (bool) {
        return _custody() >= bridge.treasuryBalance();
    }

    /// @dev TD-23 inverted invariant — passes once FoT deposits create TR-1 gap (QC, not BC).
    function invariant_TR1_FoT_solvency_gap_after_deposit() public view {
        if (handler.fotDepositOps() == 0) return;
        assertLt(_custody(), bridge.treasuryBalance(), "TD-23 FoT TR-1 gap");
    }

    /// @dev Nominal ledger still tracks `deposit(amount)` — TR-2-style ghost cross-check.
    function invariant_TR2_nominal_ghost_matches_treasury() public view {
        if (handler.fotDepositOps() == 0) return;
        assertEq(bridge.treasuryBalance(), handler.ghostDeposited(), "TD-23 nominal ledger");
    }

    /// @dev Unit smoke — treasuryBalance vs actual custody after one FoT deposit.
    function test_TD23_unit_smoke_treasury_overstates_custody() public {
        uint256 amount = 100 * UsdcTestLib.UNIT;
        address user = address(0xF023);

        fot.mint(user, amount);
        vm.startPrank(user);
        fot.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), amount, "ledger credits nominal");
        assertLt(fot.balanceOf(address(bridge)), amount, "custody net-of-fee");
        assertFalse(_tr1Holds(), "TR-1 must not hold");
    }

    /// @dev Handler path — TR-1 formula fails after stateful deposit selector.
    function test_TD23_handler_deposit_TR1_formula_fails() public {
        handler.depositFoT(UsdcTestLib.UNIT);
        assertGt(handler.fotDepositOps(), 0);
        assertFalse(_tr1Holds(), "TR-1 broken after handler FoT deposit");
        assertEq(handler.ghostCustodyReceived(), fot.balanceOf(address(bridge)));
    }

    /// @dev Fee envelope 1% — gap proportional to fee bps.
    function test_TD23_fee_one_percent_TR1_gap() public {
        _assertFoTGapAtFeeBps(100);
    }

    /// @dev Fee envelope 10% — larger gap.
    function test_TD23_fee_ten_percent_TR1_gap() public {
        _assertFoTGapAtFeeBps(1_000);
    }

    function _assertFoTGapAtFeeBps(uint256 feeBps) internal {
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
        b.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        uint256 custody = token.balanceOf(address(b));
        assertEq(b.treasuryBalance(), amount);
        assertLt(custody, amount);
        assertEq(amount - custody, (amount * feeBps) / 10_000, "gap equals fee");
        assertLt(custody + b.suppliedPrincipal(), b.treasuryBalance());
    }
}
