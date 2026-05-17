// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/VerifyBlockConfigLib.sol";

/// @title AckiNackiBridgeAaveForkTest
/// @notice **Live mainnet fork** tests for the AAVE V3 yield integration.
///
/// Complements the mock-based `AckiNackiBridgeAaveTest` (which covers the
/// bridge's own bookkeeping with a stub Pool/Gateway/aWETH) by exercising
/// the *real* AAVE V3 mainnet contracts: production `Pool`, production
/// `WrappedTokenGatewayV3`, production `aWETH`. Catches regressions in
/// AAVE-side ABI assumptions and confirms that real-world ETH ↔ aWETH
/// routing actually settles in our balance accounting.
///
/// ## Design notes
///
/// All ETH/aWETH balance assertions are formulated as **deltas around a
/// pre-action snapshot**, because the deterministic CREATE address the
/// bridge gets deployed to on a mainnet fork may carry tiny dust from
/// the live chain state. Absolute-balance asserts would be brittle for
/// no real reason; we care about correctness of the *transfer*, not the
/// starting value.
///
/// We also avoid touching `aWETH` directly. AAVE V3's `ATokenInstance`
/// view dispatch tripped a `NotActivated` revert when called from a
/// freshly-deployed contract on `https://ethereum-rpc.publicnode.com`
/// during development; the bridge's own `aWethBalance()` wrapper goes
/// through the same proxy with the same arguments and works fine, so
/// that's what we use.
///
/// ## Opt-in
///
/// `vm.skip(true)` when `FORK_URL` is unset — default `forge test` stays
/// hermetic. Run with:
///
/// ```bash
/// FORK_URL=https://ethereum-rpc.publicnode.com \
///   forge test --match-contract AckiNackiBridgeAaveForkTest -vv
/// ```
///
/// Pin a specific block for fully deterministic test data:
///
/// ```bash
/// FORK_URL=...  FORK_BLOCK=20100000 \
///   forge test --match-contract AckiNackiBridgeAaveForkTest -vv
/// ```
///
/// ## Mainnet addresses (from `bgd-labs/aave-address-book`, mirrored in
/// `script/DeployRealBridge.s.sol`)
///
/// | Component        | Address                                      |
/// |------------------|----------------------------------------------|
/// | AAVE V3 Pool     | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
/// | WETH Gateway V3  | 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C |
/// | aWETH            | 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 |
contract AckiNackiBridgeAaveForkTest is Test {
    address constant AAVE_V3_POOL = 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2;
    address constant AAVE_V3_WETH_GATEWAY = 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C;
    address constant aWETH = 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8;

    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;

    address internal owner = address(this);
    address internal user1 = address(0xA1);
    address internal yieldSink = address(0xBEEF);

    /// AAVE rounds in liquidity-index fixed-point. Single-block drift on a
    /// 10 ether supply is < 1 wei observed; we allow 10 wei to absorb noise
    /// without masking real bugs.
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
        require(block.chainid == 1, "fork must be Ethereum mainnet (chainid=1)");

        oracle = new MockBlockHeaderOracle();

        bridge = new AckiNackiBridge(
            address(oracle),
            AAVE_V3_POOL,
            AAVE_V3_WETH_GATEWAY,
            aWETH,
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent()
        );

        vm.deal(user1, 200 ether);
    }

    // -----------------------------------------------------------------
    // Constructor wiring
    // -----------------------------------------------------------------

    function test_fork_constructorWiresRealAave() public view {
        assertEq(address(bridge.aavePool()), AAVE_V3_POOL, "pool wired");
        assertEq(address(bridge.wethGateway()), AAVE_V3_WETH_GATEWAY, "gateway wired");
        assertEq(address(bridge.aWETH()), aWETH, "aWETH wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default");
        // Bridge approved the gateway during construction (verified in the
        // setUp trace by the `Approval(owner: bridge, spender: gateway,
        // value: max)` event on the aWETH proxy). We don't assert against
        // `aWETH.allowance` directly here because AAVE V3 view dispatch
        // doesn't behave consistently across RPC providers; the round-trip
        // tests below would fail if the approval hadn't actually landed.
    }

    // -----------------------------------------------------------------
    // Round-trip: deposit → supplyToAave → withdrawFromAave
    // -----------------------------------------------------------------

    function test_fork_supplyAndWithdrawRoundTrip() public {
        uint256 depositAmount = 10 ether;
        uint256 supplyAmount = 9 ether; // 1 ether stays as 10% liquid reserve

        uint256 ethStart = address(bridge).balance;
        uint256 aWethStart = bridge.aWethBalance();

        // Step 1: user deposits.
        vm.prank(user1);
        bridge.deposit{ value: depositAmount }();
        assertEq(bridge.treasuryBalance(), depositAmount, "treasury == deposit");
        assertEq(
            address(bridge).balance - ethStart, depositAmount, "bridge ETH grew by depositAmount"
        );

        // Step 2: supply maximum allowed (respects 10% reserve).
        // `suppliedPrincipal` is computed against `address(this).balance`
        // at the time of the call, which may include unrelated dust at
        // the bridge's deterministic CREATE address on a mainnet fork
        // (we've observed 1 wei). We assert with a small tolerance.
        bridge.supplyToAave(type(uint256).max);

        assertApproxEqAbs(
            bridge.suppliedPrincipal(), supplyAmount, DRIFT_TOLERANCE, "principal ~= 9 ETH"
        );
        assertEq(
            address(bridge).balance - ethStart,
            depositAmount - bridge.suppliedPrincipal(),
            "bridge ETH delta == reserve"
        );
        assertApproxEqAbs(
            bridge.aWethBalance() - aWethStart,
            supplyAmount,
            DRIFT_TOLERANCE,
            "aWETH minted ~= ETH supplied"
        );

        // Step 3: pull 3 ETH back from real AAVE.
        uint256 ethBefore = address(bridge).balance;
        uint256 aWethBefore = bridge.aWethBalance();

        uint256 principalBefore = bridge.suppliedPrincipal();
        bridge.withdrawFromAave(3 ether);

        assertEq(bridge.suppliedPrincipal(), principalBefore - 3 ether, "principal decremented");
        assertEq(address(bridge).balance - ethBefore, 3 ether, "bridge received exactly 3 ETH");
        assertApproxEqAbs(
            aWethBefore - bridge.aWethBalance(), 3 ether, DRIFT_TOLERANCE, "aWETH burned ~= 3 ETH"
        );
    }

    // -----------------------------------------------------------------
    // Yield accrual against real liquidity index
    // -----------------------------------------------------------------

    function test_fork_yieldAccruesAfterTimeWarp() public {
        vm.prank(user1);
        bridge.deposit{ value: 100 ether }();
        bridge.supplyToAave(type(uint256).max); // 90 ether at 10% reserve
        uint256 principal = bridge.suppliedPrincipal();
        assertApproxEqAbs(principal, 90 ether, DRIFT_TOLERANCE, "principal ~= 90 ETH");

        uint256 aWethBefore = bridge.aWethBalance();

        // Warp ~ 1 year. AAVE V3 ETH supply APY is historically 1–3%; a
        // year of compounding on 90 ETH should yield well over 0.1 ETH
        // even at the lower end. We use loose bounds to stay robust
        // against the live index.
        vm.warp(block.timestamp + 365 days);
        vm.roll(block.number + 1);

        // Force AAVE to re-index on next interaction (its `balanceOf`
        // uses the cached `liquidityIndex` from the latest mutating call
        // against the reserve). A tiny additional supply pokes the
        // index forward without significantly affecting the yield math.
        vm.prank(user1);
        bridge.deposit{ value: 1 ether }();
        bridge.supplyToAave(0.5 ether);

        uint256 yieldA = bridge.accruedYield();
        // 0.1 ETH = 0.11% of 90 ETH — extremely conservative lower bound.
        assertGt(yieldA, 0.1 ether, "expected > 0.1 ETH yield on 90 ETH over a year");

        // After supply, aWETH balance must have grown by at least the new
        // 0.5 ETH principal contribution + accrued yield.
        assertGt(
            bridge.aWethBalance(),
            aWethBefore + 0.5 ether,
            "aWETH balance includes new supply + yield"
        );

        // Harvest half of the yield to a separate recipient.
        bridge.setYieldRecipient(yieldSink);
        uint256 sinkBefore = yieldSink.balance;
        uint256 principalBeforeHarvest = bridge.suppliedPrincipal();

        bridge.harvestYield(yieldA / 2);

        assertApproxEqAbs(
            yieldSink.balance - sinkBefore,
            yieldA / 2,
            DRIFT_TOLERANCE,
            "harvest delivered ~ yield/2"
        );
        assertEq(
            bridge.suppliedPrincipal(), principalBeforeHarvest, "harvest must not touch principal"
        );
    }

    // -----------------------------------------------------------------
    // Emergency exit
    // -----------------------------------------------------------------

    function test_fork_emergencyWithdrawAllPullsAaveDown() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);
        uint256 suppliedAtStart = bridge.suppliedPrincipal();
        assertGt(suppliedAtStart, 0, "supplied something");

        uint256 ethBefore = address(bridge).balance;
        uint256 aWethBefore = bridge.aWethBalance();

        bridge.emergencyWithdrawAll();

        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        // aWETH balance should have dropped by at least the principal.
        assertGe(aWethBefore - bridge.aWethBalance(), suppliedAtStart - DRIFT_TOLERANCE);
        // Bridge got at least the principal back as ETH.
        assertGe(
            address(bridge).balance - ethBefore,
            suppliedAtStart - DRIFT_TOLERANCE,
            "bridge received >= supplied principal back"
        );
    }
}
