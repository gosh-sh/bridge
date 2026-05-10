// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/IAckiNackiVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./mocks/MockAave.sol";
import "./helpers/VerifyBlockConfigLib.sol";

/// @notice Trivial always-passing verifier for AAVE behaviour tests.
contract PermissiveVerifier is IAckiNackiVerifier {
    uint256 private constant PUBLIC_INPUTS_COUNT = 6;

    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        pure
        override
        returns (bool, bytes32)
    {
        if (proof.length == 0 || publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }
        return (true, bytes32(publicInputs[0]));
    }

    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}

/// @title AckiNackiBridgeAaveTest
/// @notice Exercises the AAVE integration surface of AckiNackiBridge.
contract AckiNackiBridgeAaveTest is Test {
    AckiNackiBridge internal bridge;
    PermissiveVerifier internal verifier;
    MockBlockHeaderOracle internal oracle;

    MockAWETH internal aWETH;
    MockAavePool internal pool;
    MockWETHGateway internal gateway;

    address internal owner = address(this);
    address internal user1 = address(0xA1);
    address internal user2 = address(0xA2);
    address internal yieldSink = address(0xBEEF);

    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event WithdrawnFromAave(uint256 amountRequested, uint256 amountReceived);
    event YieldHarvested(address indexed recipient, uint256 amount);
    event EmergencyWithdrawAll(uint256 amount);
    event AaveEnabledSet(bool enabled);

    function setUp() public {
        verifier = new PermissiveVerifier();
        oracle = new MockBlockHeaderOracle();

        aWETH = new MockAWETH();
        pool = new MockAavePool(address(aWETH));
        gateway = new MockWETHGateway(address(pool), address(aWETH));

        bridge = new AckiNackiBridge(
            address(verifier),
            address(oracle),
            address(pool),
            address(gateway),
            address(aWETH),
            VerifyBlockConfigLib.disabled()
        );

        vm.deal(user1, 200 ether);
        vm.deal(user2, 200 ether);
    }

    // -----------------------------------------------------------------
    // Constructor / config
    // -----------------------------------------------------------------

    function test_constructor_wiresAaveAndApprovesGateway() public view {
        assertEq(address(bridge.aavePool()), address(pool), "pool wired");
        assertEq(address(bridge.wethGateway()), address(gateway), "gateway wired");
        assertEq(address(bridge.aWETH()), address(aWETH), "aWETH wired");
        assertTrue(bridge.aaveEnabled(), "aave enabled by default when wired");
        assertEq(
            aWETH.allowance(address(bridge), address(gateway)),
            type(uint256).max,
            "gateway pre-approved"
        );
        assertEq(bridge.owner(), owner, "owner");
        assertEq(bridge.yieldRecipient(), owner, "yield recipient defaults to owner");
        assertEq(bridge.liquidReserveBps(), 1_000, "default 10% reserve");
    }

    function test_constructor_partialAaveWiringReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidAaveAddress.selector);
        new AckiNackiBridge(
            address(verifier),
            address(oracle),
            address(pool),
            address(0),
            address(aWETH),
            VerifyBlockConfigLib.disabled()
        );
    }

    function test_constructor_noAaveIsLegal() public {
        AckiNackiBridge plain = new AckiNackiBridge(
            address(verifier),
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled()
        );
        assertFalse(plain.aaveEnabled(), "aave disabled when no addresses");
        assertEq(address(plain.aavePool()), address(0));
    }

    // -----------------------------------------------------------------
    // supplyToAave
    // -----------------------------------------------------------------

    function test_supplyToAave_respectsLiquidReserve() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();

        // default reserve is 10% → 1 ether must stay liquid
        vm.expectEmit(false, false, false, true);
        emit SuppliedToAave(9 ether, 9 ether);
        bridge.supplyToAave(type(uint256).max);

        assertEq(bridge.suppliedPrincipal(), 9 ether, "principal");
        assertEq(bridge.aWethBalance(), 9 ether, "aWETH balance");
        assertEq(address(bridge).balance, 1 ether, "liquid reserve kept");
        assertEq(bridge.treasuryBalance(), 10 ether, "treasury unchanged");
    }

    function test_supplyToAave_explicitAmount() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(5 ether);
        assertEq(bridge.suppliedPrincipal(), 5 ether);
        assertEq(address(bridge).balance, 5 ether);
    }

    function test_supplyToAave_onlyOwner() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_whenDisabledReverts() public {
        bridge.setAaveEnabled(false);
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        vm.expectRevert(AckiNackiBridge.AaveDisabled.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_nothingToSupplyReverts() public {
        // No deposits yet → reserve=0, available=0
        vm.expectRevert(AckiNackiBridge.NothingToSupply.selector);
        bridge.supplyToAave(type(uint256).max);
    }

    function test_supplyToAave_amountExceedsAvailableReverts() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.supplyToAave(10 ether); // only 9 ether is supplyable after 10% reserve
    }

    // -----------------------------------------------------------------
    // withdraw auto-pulls from AAVE
    // -----------------------------------------------------------------

    function test_withdraw_pullsShortfallFromAave() public {
        // Deposit 10, supply 9 (1 ether liquid), then withdraw 5.
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        uint256 withdrawAmount = 5 ether;
        (uint256 blockNumber, bytes memory proof) = _makeWithdrawContext();

        uint256 balBefore = user1.balance;
        bridge.withdraw(payable(user1), withdrawAmount, 0, blockNumber, proof);

        assertEq(user1.balance, balBefore + withdrawAmount, "recipient paid");
        assertEq(bridge.treasuryBalance(), 5 ether, "treasury decremented");
        // 5 ether paid: 1 liquid + 4 from AAVE → principal 5 left.
        assertEq(bridge.suppliedPrincipal(), 5 ether, "principal drawn down");
        assertEq(bridge.aWethBalance(), 5 ether, "aWETH down");
    }

    function test_withdraw_usesLiquidBufferFirst() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max); // 1 ether liquid, 9 in AAVE

        (uint256 blockNumber, bytes memory proof) = _makeWithdrawContext();

        // Withdraw 0.5 ether — fully served from liquid buffer, no AAVE call.
        bridge.withdraw(payable(user1), 0.5 ether, 0, blockNumber, proof);
        assertEq(bridge.suppliedPrincipal(), 9 ether, "no AAVE touch");
        assertEq(address(bridge).balance, 0.5 ether, "buffer decremented");
    }

    // -----------------------------------------------------------------
    // withdrawFromAave (owner preemptive top-up)
    // -----------------------------------------------------------------

    function test_withdrawFromAave_preemptivelyTopsUp() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        bridge.withdrawFromAave(3 ether);
        assertEq(bridge.suppliedPrincipal(), 6 ether);
        assertEq(address(bridge).balance, 4 ether);
    }

    function test_withdrawFromAave_onlyOwner() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.withdrawFromAave(1 ether);
    }

    // -----------------------------------------------------------------
    // Yield
    // -----------------------------------------------------------------

    function test_accruedYield_reflectsAaveGrowth() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max); // principal=9 ether

        // Simulate yield: mint extra aWETH directly to the bridge, backed by ETH.
        vm.deal(address(this), 1 ether);
        aWETH.accrueYield{ value: 0.5 ether }(address(bridge), 0.5 ether);

        assertEq(bridge.accruedYield(), 0.5 ether, "accrued yield visible");
        assertEq(bridge.suppliedPrincipal(), 9 ether, "principal unchanged");
    }

    function test_harvestYield_sendsToRecipient() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        vm.deal(address(this), 1 ether);
        aWETH.accrueYield{ value: 0.3 ether }(address(bridge), 0.3 ether);

        bridge.setYieldRecipient(yieldSink);

        uint256 sinkBefore = yieldSink.balance;
        vm.expectEmit(true, false, false, true);
        emit YieldHarvested(yieldSink, 0.3 ether);
        bridge.harvestYield(0.3 ether);

        assertEq(yieldSink.balance, sinkBefore + 0.3 ether, "yield paid");
        assertEq(bridge.suppliedPrincipal(), 9 ether, "principal intact");
        assertEq(bridge.accruedYield(), 0, "yield consumed");
    }

    function test_harvestYield_partialHarvest() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        vm.deal(address(this), 1 ether);
        aWETH.accrueYield{ value: 0.4 ether }(address(bridge), 0.4 ether);

        bridge.setYieldRecipient(yieldSink);
        bridge.harvestYield(0.1 ether);
        assertEq(bridge.accruedYield(), 0.3 ether, "remaining yield");
        assertEq(yieldSink.balance, 0.1 ether, "recipient paid");
    }

    function test_harvestYield_amountExceedsYieldReverts() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        vm.deal(address(this), 1 ether);
        aWETH.accrueYield{ value: 0.1 ether }(address(bridge), 0.1 ether);

        vm.expectRevert(AckiNackiBridge.NoYield.selector);
        bridge.harvestYield(1 ether);
    }

    function test_harvestYield_onlyOwner() public {
        vm.prank(user1);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.harvestYield(1);
    }

    // -----------------------------------------------------------------
    // Emergency
    // -----------------------------------------------------------------

    function test_emergencyWithdrawAll_pullsEverything() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);

        // Throw in some yield too.
        vm.deal(address(this), 1 ether);
        aWETH.accrueYield{ value: 0.7 ether }(address(bridge), 0.7 ether);

        vm.expectEmit(false, false, false, true);
        emit AaveEnabledSet(false);
        bridge.emergencyWithdrawAll();

        assertFalse(bridge.aaveEnabled(), "aave disabled");
        assertEq(bridge.suppliedPrincipal(), 0, "principal zeroed");
        assertEq(bridge.aWethBalance(), 0, "aWETH drained");
        // Liquid reserve 1 ether + principal 9 + yield 0.7 = 10.7
        assertEq(address(bridge).balance, 10.7 ether, "all ETH home");
    }

    function test_emergencyWithdraw_allowsSubsequentUserWithdrawals() public {
        vm.prank(user1);
        bridge.deposit{ value: 10 ether }();
        bridge.supplyToAave(type(uint256).max);
        bridge.emergencyWithdrawAll();

        // Withdrawals still work (all funds are in ETH now).
        (uint256 blockNumber, bytes memory proof) = _makeWithdrawContext();
        uint256 balBefore = user1.balance;
        bridge.withdraw(payable(user1), 10 ether, 0, blockNumber, proof);
        assertEq(user1.balance, balBefore + 10 ether);
    }

    // -----------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------

    function test_setLiquidReserveBps_capped() public {
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooHigh.selector);
        bridge.setLiquidReserveBps(5_001);
        bridge.setLiquidReserveBps(2_500);
        assertEq(bridge.liquidReserveBps(), 2_500);
    }

    function test_transferOwnership_flowsAllAuthorities() public {
        bridge.transferOwnership(user2);
        assertEq(bridge.owner(), user2);

        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setAaveEnabled(false); // old owner can no longer act
        vm.prank(user2);
        bridge.setAaveEnabled(false);
    }

    // -----------------------------------------------------------------
    // Accounting invariants
    // -----------------------------------------------------------------

    function testFuzz_totalAssetsCoversTreasury(uint96 depositAmt, uint16 bps) public {
        vm.assume(depositAmt > 0 && depositAmt <= 100 ether);
        vm.assume(bps <= bridge.MAX_LIQUID_RESERVE_BPS());

        bridge.setLiquidReserveBps(bps);
        vm.deal(user1, depositAmt);
        vm.prank(user1);
        bridge.deposit{ value: depositAmt }();

        // Supply whatever the reserve allows.
        uint256 reserve = (uint256(depositAmt) * bps) / bridge.BPS_DENOMINATOR();
        uint256 supplyable = depositAmt > reserve ? uint256(depositAmt) - reserve : 0;
        if (supplyable > 0) {
            bridge.supplyToAave(supplyable);
        }

        // At all times: ETH + aWETH >= treasuryBalance.
        assertGe(bridge.totalAssets(), bridge.treasuryBalance(), "solvent");
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    function _makeWithdrawContext()
        internal
        view
        returns (uint256 blockNumber, bytes memory proof)
    {
        blockNumber = block.number - 1;
        proof = hex"deadbeef";
    }
}
