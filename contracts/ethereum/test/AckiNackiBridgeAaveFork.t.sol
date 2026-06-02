// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IERC20.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";

/// @title AckiNackiBridgeAaveForkTest
/// @notice **Live Sepolia fork** tests for the AAVE V3 USDC yield integration.
///
/// Exercises the real AAVE V3 Sepolia Pool + USDC market (underlying + aUSDC).
/// Opt-in via `FORK_URL`; defaults to Sepolia (chainid 11155111).
///
/// ```bash
/// FORK_URL=https://ethereum-sepolia-rpc.publicnode.com \
///   forge test --match-contract AckiNackiBridgeAaveForkTest -vv
/// ```
contract AckiNackiBridgeAaveForkTest is Test {
    address constant AAVE_V3_POOL = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant USDC = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;
    address constant aUSDC = 0x16dA4541aD1807f4443d92D26044C1147406EB80;
    address constant AAVE_FAUCET = 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D;

    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    IERC20 internal usdc;

    address internal owner = address(this);
    address internal user1 = address(0xA1);
    address internal yieldSink = address(0xBEEF);

    uint256 internal constant DRIFT_TOLERANCE = 10;

    function setUp() public {
        string memory rpc = vm.envOr("FORK_URL", string(""));
        if (bytes(rpc).length == 0) {
            console.log("FORK_URL not set; skipping fork tests");
            vm.skip(true);
            return;
        }

        uint256 forkBlock = vm.envOr("FORK_BLOCK", uint256(0));
        if (forkBlock > 0) {
            vm.createSelectFork(rpc, forkBlock);
        } else {
            vm.createSelectFork(rpc);
        }
        require(block.chainid == 11155111, "fork must be Sepolia (chainid=11155111)");

        oracle = new MockBlockHeaderOracle();
        usdc = IERC20(USDC);

        bridge = new AckiNackiBridge(
            address(oracle),
            USDC,
            AAVE_V3_POOL,
            aUSDC,
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _fundUserFromFaucet(user1, 200 * UsdcTestLib.UNIT);
    }

    function _fundUserFromFaucet(address to, uint256 amount) internal {
        (bool ok,) = AAVE_FAUCET.call(
            abi.encodeWithSignature("mint(address,address,uint256)", USDC, to, amount)
        );
        require(ok, "faucet mint failed");
    }

    function _approveAndDeposit(address user, uint256 amount) internal {
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_fork_constructorWiresRealAave() public view {
        assertEq(address(bridge.usdc()), USDC, "usdc wired");
        assertEq(address(bridge.aavePool()), AAVE_V3_POOL, "pool wired");
        assertEq(address(bridge.aUSDC()), aUSDC, "aUSDC wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default");
    }

    function test_fork_supplyAndWithdrawRoundTrip() public {
        uint256 depositAmount = 10 * UsdcTestLib.UNIT;
        uint256 supplyAmount = 9 * UsdcTestLib.UNIT;

        uint256 usdcStart = usdc.balanceOf(address(bridge));
        uint256 aUsdcStart = bridge.aUsdcBalance();

        _approveAndDeposit(user1, depositAmount);
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury == deposit");
        assertEq(usdc.balanceOf(address(bridge)) - usdcStart, depositAmount, "bridge USDC grew");

        bridge.supplyToAave(type(uint256).max);

        assertApproxEqAbs(
            bridge.suppliedPrincipal(), supplyAmount, DRIFT_TOLERANCE, "principal ~= 9 USDC"
        );
        assertEq(
            usdc.balanceOf(address(bridge)) - usdcStart,
            depositAmount - bridge.suppliedPrincipal(),
            "bridge USDC delta == reserve"
        );
        assertApproxEqAbs(
            bridge.aUsdcBalance() - aUsdcStart,
            supplyAmount,
            DRIFT_TOLERANCE,
            "aUSDC minted ~= supplied"
        );

        uint256 usdcBefore = usdc.balanceOf(address(bridge));
        uint256 aUsdcBefore = bridge.aUsdcBalance();
        uint256 principalBefore = bridge.suppliedPrincipal();

        bridge.withdrawFromAave(3 * UsdcTestLib.UNIT);

        assertEq(
            bridge.suppliedPrincipal(),
            principalBefore - 3 * UsdcTestLib.UNIT,
            "principal decremented"
        );
        assertEq(
            usdc.balanceOf(address(bridge)) - usdcBefore, 3 * UsdcTestLib.UNIT, "received 3 USDC"
        );
        assertApproxEqAbs(
            aUsdcBefore - bridge.aUsdcBalance(),
            3 * UsdcTestLib.UNIT,
            DRIFT_TOLERANCE,
            "aUSDC burned"
        );
    }

    function test_fork_yieldAccruesAfterTimeWarp() public {
        _approveAndDeposit(user1, 100 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 principal = bridge.suppliedPrincipal();
        assertApproxEqAbs(principal, 90 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "principal ~= 90 USDC");

        uint256 aUsdcBefore = bridge.aUsdcBalance();

        vm.warp(block.timestamp + 365 days);
        vm.roll(block.number + 1);

        _approveAndDeposit(user1, 1 * UsdcTestLib.UNIT);
        bridge.supplyToAave(500_000);

        uint256 yieldA = bridge.accruedYield();
        assertGt(yieldA, 10_000, "expected yield on 90 USDC over a year");

        assertGt(bridge.aUsdcBalance(), aUsdcBefore + 500_000, "aUSDC includes new supply + yield");

        bridge.setYieldRecipient(yieldSink);
        uint256 sinkBefore = usdc.balanceOf(yieldSink);
        uint256 principalBeforeHarvest = bridge.suppliedPrincipal();

        bridge.harvestYield(yieldA / 2);

        assertApproxEqAbs(
            usdc.balanceOf(yieldSink) - sinkBefore, yieldA / 2, DRIFT_TOLERANCE, "harvest delivered"
        );
        assertEq(
            bridge.suppliedPrincipal(), principalBeforeHarvest, "harvest must not touch principal"
        );
    }

    function test_fork_emergencyWithdrawAllPullsAaveDown() public {
        _approveAndDeposit(user1, 10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 suppliedAtStart = bridge.suppliedPrincipal();
        assertGt(suppliedAtStart, 0, "supplied something");

        uint256 usdcBefore = usdc.balanceOf(address(bridge));
        uint256 aUsdcBefore = bridge.aUsdcBalance();

        bridge.emergencyWithdrawAll();

        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        assertGe(aUsdcBefore - bridge.aUsdcBalance(), suppliedAtStart - DRIFT_TOLERANCE);
        assertGe(usdc.balanceOf(address(bridge)) - usdcBefore, suppliedAtStart - DRIFT_TOLERANCE);
    }
}
