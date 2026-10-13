// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/DemoAavePool.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/VerifyBlockConfigLib.sol";

/// @title DemoAavePoolForkTest
/// @notice **Live Sepolia fork**: the `DEMO_AAVE=true` stand — a bridge on Circle's Sepolia
///         USDC wired to a `DemoAavePool` — run against the real FiatToken.
///
/// ```bash
/// FOUNDRY_PROFILE=fork FORK_URL=https://ethereum-sepolia-rpc.publicnode.com \
///   forge test --match-contract DemoAavePoolForkTest -vv
/// ```
contract DemoAavePoolForkTest is Test {
    address constant CIRCLE_USDC = 0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238;

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
    }

    function test_fork_circleUsdc_supplyAccrueHarvestUnwind() public {
        IERC20 usdc = IERC20(CIRCLE_USDC);
        address user = makeAddr("user");
        address sink = makeAddr("sink");

        DemoAavePool pool = new DemoAavePool(CIRCLE_USDC, address(this));
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            CIRCLE_USDC,
            address(pool),
            address(pool.aToken()),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        bridge.setYieldRecipient(sink);

        deal(CIRCLE_USDC, user, 100e6);
        deal(CIRCLE_USDC, address(this), 5e6);
        vm.startPrank(user);
        usdc.approve(address(bridge), 100e6);
        bridge.deposit(100e6, 0, bytes32(uint256(1)));
        vm.stopPrank();

        bridge.supplyToAave(type(uint256).max);
        assertEq(usdc.balanceOf(address(pool)), 90e6, "90% supplied");

        usdc.approve(address(pool), 5e6);
        pool.accrueYield(address(bridge), 5e6);
        bridge.harvestYield(bridge.accruedYield());
        assertEq(usdc.balanceOf(sink), 5e6, "yield reached the recipient");

        bridge.withdrawFromAave(type(uint256).max);
        assertEq(usdc.balanceOf(address(bridge)), 100e6, "principal back on the bridge");
        assertEq(usdc.balanceOf(address(pool)), 0, "pool empty");
    }
}
