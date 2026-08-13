// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsAaveTest
/// @notice Phase D — F-TR-3 principal vs yield isolation under interleaved ops.
contract InvariantsAaveTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;
    AaveHandler internal handler;

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

        handler = new AaveHandler(bridge, usdc, aUSDC, pool, address(this));
        targetContract(address(handler));
    }

    /// @dev INV: TR-3
    function invariant_TR3_suppliedPrincipalLeqAUSDC() public view {
        assertLe(bridge.suppliedPrincipal(), bridge.aUsdcBalance(), "TR-3 principal book");
    }

    /// @dev INV: TR-3 / A4-INV-2 — harvest never drives aUSDC below principal book.
    function invariant_TR3_accruedYieldNonNegative() public view {
        assertEq(bridge.accruedYield(), _accrued(), "accruedYield consistent");
    }

    function _accrued() internal view returns (uint256) {
        uint256 bal = bridge.aUsdcBalance();
        uint256 p = bridge.suppliedPrincipal();
        return bal > p ? bal - p : 0;
    }
}
