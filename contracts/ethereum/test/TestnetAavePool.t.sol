// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/testnet/SepoliaAaveUsdcShim.sol";
import "../src/testnet/TestnetAavePool.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockERC20.sol";
import { MockAaveFaucet } from "./SepoliaAaveUsdcShim.t.sol";

/// @title TestnetAavePoolTest
/// @notice The bridge's AAVE module runs unchanged against `TestnetAavePool`:
///         a shimmed deposit is supplied, interest accrues, yield is harvested
///         and principal comes back.
contract TestnetAavePoolTest is Test {
    uint256 internal constant AMOUNT = 2_000_000;

    MockERC20 internal circleUsdc;
    MockERC20 internal faucetUsdc;
    MockAaveFaucet internal faucet;
    TestnetAavePool internal pool;
    TestnetAToken internal aToken;
    AckiNackiBridge internal bridge;
    SepoliaAaveUsdcShim internal shim;

    address internal yieldSink = address(0x5EED);

    function setUp() public {
        circleUsdc = new MockERC20("Circle USDC", "USDC", 6);
        faucetUsdc = new MockERC20("Faucet USDC", "USDC", 6);
        faucet = new MockAaveFaucet();
        pool = new TestnetAavePool(address(faucetUsdc), address(faucet));
        aToken = pool.aToken();
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(faucetUsdc),
            address(pool),
            address(aToken),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        bridge.setYieldRecipient(yieldSink);
        vm.chainId(11_155_111);
        shim = new SepoliaAaveUsdcShim(address(bridge), address(circleUsdc), address(faucet));

        circleUsdc.mint(address(this), AMOUNT);
        circleUsdc.approve(address(shim), AMOUNT);
        shim.deposit(AMOUNT, bytes32(uint256(0xabc)));
    }

    function test_supplyAccrueHarvestWithdraw() public {
        bridge.supplyToAave(type(uint256).max);
        uint256 supplied = AMOUNT - (AMOUNT * bridge.liquidReserveBps()) / 10_000;
        assertEq(aToken.balanceOf(address(bridge)), supplied);
        assertEq(bridge.suppliedPrincipal(), supplied);
        assertEq(faucetUsdc.balanceOf(address(pool)), supplied);

        pool.accrue(address(bridge), 1_234);
        assertEq(bridge.accruedYield(), 1_234);

        bridge.harvestYield(1_234);
        assertEq(faucetUsdc.balanceOf(yieldSink), 1_234);
        assertEq(bridge.accruedYield(), 0);

        bridge.withdrawFromAave(type(uint256).max);
        assertEq(aToken.balanceOf(address(bridge)), 0);
        assertEq(bridge.suppliedPrincipal(), 0);
        assertEq(faucetUsdc.balanceOf(address(bridge)), AMOUNT);
        assertEq(bridge.treasuryBalance(), AMOUNT);
    }

    function test_reserveData_namesTheAToken() public {
        assertEq(pool.getReserveData(address(faucetUsdc)).aTokenAddress, address(aToken));
        vm.expectRevert(
            abi.encodeWithSelector(TestnetAavePool.WrongAsset.selector, address(circleUsdc))
        );
        pool.getReserveData(address(circleUsdc));
    }

    function test_accrue_isOwnerOnly() public {
        vm.prank(address(0xBAD));
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        pool.accrue(address(bridge), 1);
    }

    function test_aToken_isMintedAndBurnedOnlyByThePool() public {
        vm.expectRevert(TestnetAToken.NotPool.selector);
        aToken.mint(address(this), 1);
        vm.expectRevert(TestnetAToken.NotPool.selector);
        aToken.burn(address(bridge), 1);
    }

    function test_withdraw_aboveBalanceReverts() public {
        bridge.supplyToAave(type(uint256).max);
        vm.expectRevert(abi.encodeWithSelector(TestnetAavePool.InsufficientATokens.selector, 0));
        pool.withdraw(address(faucetUsdc), 1, address(this));
    }
}
