// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsOwnerTest
/// @notice Phase D / F-A4-1 — A4-INV-1 owner ops never reduce treasuryBalance.
contract InvariantsOwnerTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;
    OwnerOpsHandler internal handler;

    uint256 internal treasuryBaseline;

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

        address user = address(0xA1);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 20 * UsdcTestLib.UNIT);
        treasuryBaseline = bridge.treasuryBalance();

        aUSDC.accrueYield(address(bridge), 1_000_000);
        usdc.mint(address(pool), 1_000_000);

        handler = new OwnerOpsHandler(bridge, aUSDC, address(this));
        targetContract(address(handler));
    }

    /// @dev INV: A4-INV-1
    function invariant_A4INV1_ownerOpsNeverReduceTreasury() public view {
        assertGe(bridge.treasuryBalance(), treasuryBaseline, "A4-INV-1 treasury");
    }

    /// @dev INV: A4-INV-1 (solvency companion)
    function invariant_A4INV1_totalAssetsCoversTreasury() public view {
        assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "totalAssets >= treasury");
    }
}
