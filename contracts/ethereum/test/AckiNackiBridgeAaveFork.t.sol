// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IERC20.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdtTestLib.sol";

/// @title AckiNackiBridgeAaveForkTest
/// @notice **Live Sepolia fork** tests for the AAVE V3 USDT yield integration.
///
/// Exercises the real AAVE V3 Sepolia Pool + USDT market (underlying + aUSDT).
/// Opt-in via `FORK_URL`; defaults to Sepolia (chainid 11155111).
///
/// ```bash
/// FORK_URL=https://ethereum-sepolia-rpc.publicnode.com \
///   forge test --match-contract AckiNackiBridgeAaveForkTest -vv
/// ```
contract AckiNackiBridgeAaveForkTest is Test {
    address constant AAVE_V3_POOL = 0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951;
    address constant USDT = 0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0;
    address constant aUSDT = 0xAF0F6e8b0Dc5c913bbF4d14c22B4E78Dd14310B6;
    address constant AAVE_FAUCET = 0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D;

    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    IERC20 internal usdt;

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
        usdt = IERC20(USDT);

        bridge = new AckiNackiBridge(
            address(oracle),
            USDT,
            AAVE_V3_POOL,
            aUSDT,
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _fundUserFromFaucet(user1, 200 * UsdtTestLib.UNIT);
    }

    function _fundUserFromFaucet(address to, uint256 amount) internal {
        (bool ok,) = AAVE_FAUCET.call(
            abi.encodeWithSignature("mint(address,address,uint256)", USDT, to, amount)
        );
        require(ok, "faucet mint failed");
    }

    function _approveAndDeposit(address user, uint256 amount) internal {
        vm.startPrank(user);
        usdt.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_fork_constructorWiresRealAave() public view {
        assertEq(address(bridge.usdt()), USDT, "usdt wired");
        assertEq(address(bridge.aavePool()), AAVE_V3_POOL, "pool wired");
        assertEq(address(bridge.aUSDT()), aUSDT, "aUSDT wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default");
    }

    function test_fork_supplyAndWithdrawRoundTrip() public {
        uint256 depositAmount = 10 * UsdtTestLib.UNIT;
        uint256 supplyAmount = 9 * UsdtTestLib.UNIT;

        uint256 usdtStart = usdt.balanceOf(address(bridge));
        uint256 aUsdtStart = bridge.aUsdtBalance();

        _approveAndDeposit(user1, depositAmount);
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury == deposit");
        assertEq(usdt.balanceOf(address(bridge)) - usdtStart, depositAmount, "bridge USDT grew");

        bridge.supplyToAave(type(uint256).max);

        assertApproxEqAbs(
            bridge.suppliedPrincipal(), supplyAmount, DRIFT_TOLERANCE, "principal ~= 9 USDT"
        );
        assertEq(
            usdt.balanceOf(address(bridge)) - usdtStart,
            depositAmount - bridge.suppliedPrincipal(),
            "bridge USDT delta == reserve"
        );
        assertApproxEqAbs(
            bridge.aUsdtBalance() - aUsdtStart,
            supplyAmount,
            DRIFT_TOLERANCE,
            "aUSDT minted ~= supplied"
        );

        uint256 usdtBefore = usdt.balanceOf(address(bridge));
        uint256 aUsdtBefore = bridge.aUsdtBalance();
        uint256 principalBefore = bridge.suppliedPrincipal();

        bridge.withdrawFromAave(3 * UsdtTestLib.UNIT);

        assertEq(
            bridge.suppliedPrincipal(),
            principalBefore - 3 * UsdtTestLib.UNIT,
            "principal decremented"
        );
        assertEq(
            usdt.balanceOf(address(bridge)) - usdtBefore, 3 * UsdtTestLib.UNIT, "received 3 USDT"
        );
        assertApproxEqAbs(
            aUsdtBefore - bridge.aUsdtBalance(),
            3 * UsdtTestLib.UNIT,
            DRIFT_TOLERANCE,
            "aUSDT burned"
        );
    }

    function test_fork_yieldAccruesAfterTimeWarp() public {
        _approveAndDeposit(user1, 100 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 principal = bridge.suppliedPrincipal();
        assertApproxEqAbs(principal, 90 * UsdtTestLib.UNIT, DRIFT_TOLERANCE, "principal ~= 90 USDT");

        uint256 aUsdtBefore = bridge.aUsdtBalance();

        vm.warp(block.timestamp + 365 days);
        vm.roll(block.number + 1);

        _approveAndDeposit(user1, 1 * UsdtTestLib.UNIT);
        bridge.supplyToAave(500_000);

        uint256 yieldA = bridge.accruedYield();
        assertGt(yieldA, 10_000, "expected yield on 90 USDT over a year");

        assertGt(bridge.aUsdtBalance(), aUsdtBefore + 500_000, "aUSDT includes new supply + yield");

        bridge.setYieldRecipient(yieldSink);
        uint256 sinkBefore = usdt.balanceOf(yieldSink);
        uint256 principalBeforeHarvest = bridge.suppliedPrincipal();

        bridge.harvestYield(yieldA / 2);

        assertApproxEqAbs(
            usdt.balanceOf(yieldSink) - sinkBefore, yieldA / 2, DRIFT_TOLERANCE, "harvest delivered"
        );
        assertEq(
            bridge.suppliedPrincipal(), principalBeforeHarvest, "harvest must not touch principal"
        );
    }

    function test_fork_emergencyWithdrawAllPullsAaveDown() public {
        _approveAndDeposit(user1, 10 * UsdtTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 suppliedAtStart = bridge.suppliedPrincipal();
        assertGt(suppliedAtStart, 0, "supplied something");

        uint256 usdtBefore = usdt.balanceOf(address(bridge));
        uint256 aUsdtBefore = bridge.aUsdtBalance();

        bridge.emergencyWithdrawAll();

        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        assertGe(aUsdtBefore - bridge.aUsdtBalance(), suppliedAtStart - DRIFT_TOLERANCE);
        assertGe(usdt.balanceOf(address(bridge)) - usdtBefore, suppliedAtStart - DRIFT_TOLERANCE);
    }
}
