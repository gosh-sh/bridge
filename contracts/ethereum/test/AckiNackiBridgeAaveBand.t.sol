// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./helpers/AaveBridgeFixture.sol";

/// @title AckiNackiBridgeAaveBandTest
/// @notice The liquid band of `docs/aave-integration-spec.md`: the supply in
///         `deposit`, the refill in `withdrawByProof`, the two setters and
///         the owner paths that read the band. Test names carry the TC id.
contract AckiNackiBridgeAaveBandTest is AaveBridgeFixture {
    address internal user = address(0xA11CE);

    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        bytes32 anAccount,
        uint256 timestamp
    );
    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event AaveSupplyRefused(uint256 amount, bytes reason);
    event AaveRefillFailed(uint256 amount, bytes reason);

    function setUp() public {
        _deployFixture();
    }

    /// @dev U = 1, S = 9, B = 10.
    function _seed() internal {
        _deposit(user, 10 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 1 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
    }

    // -----------------------------------------------------------------
    // Supply in deposit
    // -----------------------------------------------------------------

    /// @dev TC-01: `L` one unit below `H` — no call to the pool.
    function test_tc01_oneUnitBelowThresholdMakesNoPoolCall() public {
        _seed();
        // B = 11_249_997, T = 1_124_999, H = 2_249_998, L = 2_249_997.
        uint256 d = 1_249_997;
        usdc.mint(user, d);
        vm.prank(user);
        usdc.approve(address(bridge), d);
        vm.expectCall(address(pool), abi.encodeWithSelector(IAavePool.supply.selector), 0);
        vm.prank(user);
        bridge.deposit(d, bytes32(uint256(1)));
        assertEq(_liquid() + 1, _threshold(), "one unit below");
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
    }

    /// @dev TC-02: `L` exactly at `H` supplies `L − T`.
    function test_tc02_atThresholdSuppliesAboveTarget() public {
        _seed();
        // B = 11_250_000, T = 1_125_000, H = L = 2_250_000.
        _deposit(user, 1_250_000);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT + 1_125_000, "S grew by L - T");
        assertEq(_liquid(), _target(), "L = T");
    }

    /// @dev TC-03: the first deposit into an empty bridge supplies all above
    ///      the target.
    function test_tc03_firstDepositSuppliesAboveTarget() public {
        assertEq(bridge.treasuryBalance(), 0);
        _deposit(user, 37 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 37 * UNIT / 10);
        assertEq(bridge.suppliedPrincipal(), 37 * UNIT - 37 * UNIT / 10);
    }

    /// @dev TC-04: a reverting `supply`.
    function test_tc04_revertingSupply() public {
        pool.setSupplyRefused(true);
        _assertRefused(abi.encodeWithSelector(MockAavePool.SupplyRefused.selector));
    }

    /// @dev TC-04: a paused reserve (`Errors.RESERVE_PAUSED`).
    function test_tc04_pausedReserve() public {
        _assertRefusedWithPoolError("29");
    }

    /// @dev TC-04: a frozen reserve (`Errors.RESERVE_FROZEN`).
    function test_tc04_frozenReserve() public {
        _assertRefusedWithPoolError("28");
    }

    /// @dev TC-04: a supply cap (`Errors.SUPPLY_CAP_EXCEEDED`).
    function test_tc04_supplyCap() public {
        _assertRefusedWithPoolError("51");
    }

    /// @dev TC-04: zero aUSDC credited.
    function test_tc04_zeroCredited() public {
        pool.setSupplyHaircut(9 * UNIT);
        _assertRefused(
            abi.encodeWithSelector(AckiNackiBridge.AaveSupplyFailed.selector, 9 * UNIT, 0)
        );
    }

    /// @dev TC-04: `approve` returns false.
    function test_tc04_approveFalse() public {
        usdc.mint(user, 10 * UNIT);
        vm.prank(user);
        usdc.approve(address(bridge), 10 * UNIT);
        usdc.setApproveReturnsFalse(true);
        _expectRefusedDeposit(abi.encodeWithSelector(AckiNackiBridge.ApproveFailed.selector));
    }

    function _assertRefusedWithPoolError(string memory code) internal {
        bytes memory reason = abi.encodeWithSignature("Error(string)", code);
        vm.mockCallRevert(address(pool), abi.encodeWithSelector(IAavePool.supply.selector), reason);
        _assertRefused(reason);
    }

    function _assertRefused(bytes memory reason) internal {
        usdc.mint(user, 10 * UNIT);
        vm.prank(user);
        usdc.approve(address(bridge), 10 * UNIT);
        _expectRefusedDeposit(reason);
    }

    /// @dev The user is funded and has approved 10 USDC.
    function _expectRefusedDeposit(bytes memory reason) internal {
        vm.expectEmit(true, true, false, true, address(bridge));
        emit Deposit(0, user, 10 * UNIT, bytes32(uint256(1)), block.timestamp);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveSupplyRefused(9 * UNIT, reason);
        vm.prank(user);
        bridge.deposit(10 * UNIT, bytes32(uint256(1)));

        assertEq(usdc.balanceOf(address(bridge)), 10 * UNIT, "U grew by the deposit");
        assertEq(bridge.suppliedPrincipal(), 0, "S unchanged");
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
        assertEq(usdc.allowance(address(bridge), address(pool)), 0, "no allowance left");
    }

    /// @dev TC-05: USDC sent directly stays on the bridge; `S ≤ B`.
    function test_tc05_directUsdcIsNotSupplied() public {
        usdc.mint(address(bridge), 100 * UNIT);
        _deposit(user, 10 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 101 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
        assertLe(bridge.suppliedPrincipal(), bridge.treasuryBalance());
    }

    // -----------------------------------------------------------------
    // Setters
    // -----------------------------------------------------------------

    /// @dev TC-06: owner only, default 2×, 1× and below revert.
    function test_tc06_multiplierSetter() public {
        assertEq(bridge.supplyMultiplierBps(), 20_000);
        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setSupplyMultiplierBps(30_000);

        vm.expectRevert(AckiNackiBridge.MultiplierTooLow.selector);
        bridge.setSupplyMultiplierBps(10_000);
        vm.expectRevert(AckiNackiBridge.MultiplierTooLow.selector);
        bridge.setSupplyMultiplierBps(0);

        bridge.setSupplyMultiplierBps(10_001);
        assertEq(bridge.supplyMultiplierBps(), 10_001);
    }

    /// @dev TC-07: either setter reverts when `r × M` would exceed 100%.
    function test_tc07_bandAboveTheWholeBookReverts() public {
        bridge.setLiquidReserveBps(3_000);
        bridge.setSupplyMultiplierBps(33_333);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.ReserveBandTooWide.selector, 3_000, 33_334)
        );
        bridge.setSupplyMultiplierBps(33_334);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.ReserveBandTooWide.selector, 3_001, 33_333)
        );
        bridge.setLiquidReserveBps(3_001);
    }

    /// @dev TC-08: the next deposit uses the new `M`.
    function test_tc08_newMultiplierAppliesToNextDeposit() public {
        _seed();
        bridge.setSupplyMultiplierBps(50_000);
        // L = 2.5, B = 11.5, T = 1.15: under 2× (2.3) it would supply; 5× is 5.75.
        vm.expectCall(address(pool), abi.encodeWithSelector(IAavePool.supply.selector), 0);
        _deposit(user, 15 * UNIT / 10);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
    }

    /// @dev TC-08: the next deposit uses the new `r`.
    function test_tc08_newReserveAppliesToNextDeposit() public {
        _seed();
        bridge.setLiquidReserveBps(2_000);
        // L = 4, B = 13, T = 2.6, H = 5.2: under 10% (H = 2.6) it would supply.
        _deposit(user, 3 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT, "below the new threshold");
        // L = 9, B = 18, T = 3.6, H = 7.2: supplies 5.4.
        _deposit(user, 5 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT + 54 * UNIT / 10);
        assertEq(_liquid(), 36 * UNIT / 10);
    }

    /// @dev TC-23: the reserve floor is 10%.
    function test_tc23_reserveFloor() public {
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooLow.selector);
        bridge.setLiquidReserveBps(0);
        vm.expectRevert(AckiNackiBridge.ReserveBpsTooLow.selector);
        bridge.setLiquidReserveBps(999);
        bridge.setLiquidReserveBps(1_000);
        assertEq(bridge.liquidReserveBps(), 1_000);
    }

    /// @dev TC-13: no external function supplies the pool; the deposit's
    ///      own step answers to the bridge alone.
    function test_tc13_supplyToAaveIsGone() public {
        _seed();
        (bool ok,) = address(bridge).call(abi.encodeWithSelector(0x70ded44d, uint256(1)));
        assertFalse(ok);
        vm.expectRevert(AckiNackiBridge.NotSelf.selector);
        bridge.supplyForSelf(1);
        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotSelf.selector);
        bridge.supplyForSelf(1);
    }

    // -----------------------------------------------------------------
    // Refill in withdrawByProof
    // -----------------------------------------------------------------

    /// @dev TC-09: a payout `L` covers makes no call to the pool.
    function test_tc09_payoutCoveredByLiquidMakesNoPoolCall() public {
        _seed();
        vm.expectCall(address(pool), abi.encodeWithSelector(IAavePool.withdraw.selector), 0);
        _pay(1 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 1 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
    }

    /// @dev TC-10: a shortfall leaves `L` at the target of the new book.
    function test_tc10_shortfallRefillsTarget() public {
        _seed();
        _pay(4 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 4 * UNIT);
        assertEq(bridge.treasuryBalance(), 6 * UNIT);
        assertEq(_liquid(), _target(), "L = T on the new book");
        assertEq(_liquid(), 6 * UNIT / 10);
    }

    /// @dev TC-11: the pool returns the shortfall but not the extra.
    function test_tc11_refillRefusedStillPays() public {
        _seed();
        pool.setWithdrawLimit(3 * UNIT);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveRefillFailed(
            6 * UNIT / 10, abi.encodeWithSelector(MockAavePool.NotEnoughAvailableLiquidity.selector)
        );
        _pay(4 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 4 * UNIT);
        assertLt(_liquid(), _target(), "below the target");
        assertEq(bridge.suppliedPrincipal(), 6 * UNIT);
    }

    /// @dev TC-12: the pool cannot return the shortfall — revert, nullifier unused.
    function test_tc12_shortfallRefusedReverts() public {
        _seed();
        pool.setWithdrawLimit(2 * UNIT);
        vm.expectRevert(MockAavePool.NotEnoughAvailableLiquidity.selector);
        _pay(4 * UNIT, 1);
        _assertUnpaid(1);
    }

    /// @dev TC-12: the pool holds less than the shortfall.
    function test_tc12_poolHoldsLessThanTheShortfall() public {
        _seed();
        aUSDC.burnFrom(address(bridge), 7 * UNIT);
        vm.expectRevert(AckiNackiBridge.InsufficientTreasury.selector);
        _pay(4 * UNIT, 1);
        _assertUnpaid(1);
    }

    /// @dev TC-12: the pool returns less than it was asked for, on the pull
    ///      with a refill (want 3.6 > required 3) and on the bare pull
    ///      (the position is exactly the shortfall).
    function test_tc12_poolReturnsLessThanAsked() public {
        _seed();
        pool.setRedeemHaircut(4 * UNIT);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.AaveWithdrawFailed.selector, 36 * UNIT / 10, 0)
        );
        _pay(4 * UNIT, 1);
        _assertUnpaid(1);

        aUSDC.burnFrom(address(bridge), 6 * UNIT);
        pool.setRedeemHaircut(1);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.AaveWithdrawFailed.selector, 3 * UNIT, 3 * UNIT - 1
            )
        );
        _pay(4 * UNIT, 1);
        _assertUnpaid(1);
    }

    function _assertUnpaid(uint256 nullifier) internal view {
        assertFalse(bridge.isNullifierUsed(nullifier));
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
        assertEq(usdc.balanceOf(RECIPIENT), 0);
    }

    /// @dev WD-2 with USDC above the principal: `U` pays without a required
    ///      pull, and the pull that keeps `S ≤ B` is best effort.
    function test_directUsdcPaysWhenThePoolRefuses() public {
        _seed();
        usdc.mint(address(bridge), 5 * UNIT);
        pool.setWithdrawLimit(0);
        _pay(5 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 5 * UNIT);

        pool.setWithdrawLimit(type(uint256).max);
        _pay(1 * UNIT, 2);
        assertLe(bridge.suppliedPrincipal(), bridge.treasuryBalance(), "S <= B once it can");
        assertEq(_liquid(), _target());
    }

    // -----------------------------------------------------------------
    // Owner paths
    // -----------------------------------------------------------------

    /// @dev TC-24: direct USDC is skimmable while principal is in the pool.
    function test_tc24_directUsdcIsSkimmable() public {
        _seed();
        usdc.mint(address(bridge), 5 * UNIT);
        assertEq(bridge.excessUsdc(), 5 * UNIT);
        bridge.skimExcessUsdc(type(uint256).max);
        assertEq(usdc.balanceOf(yieldSink), 5 * UNIT);
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
    }

    /// @dev TC-25: a pool loss covered by direct USDC is not skimmable.
    function test_tc25_lossIsNotSkimmable() public {
        _seed();
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), 2 * UNIT); // A = 7
        usdc.mint(address(bridge), 3 * UNIT); // U = 4

        uint256 holdings = usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance();
        assertEq(bridge.excessUsdc(), holdings - bridge.treasuryBalance(), "X = U + A - B");
        assertEq(bridge.excessUsdc(), 1 * UNIT);
        bridge.skimExcessUsdc(type(uint256).max);
        assertGe(usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance(), bridge.treasuryBalance());
        vm.expectRevert(AckiNackiBridge.NoExcessUsdc.selector);
        bridge.skimExcessUsdc(1);
    }

    /// @dev TC-26: `migrateTreasury` needs the book liquid.
    function test_tc26_migrateNeedsLiquidBook() public {
        _seed();
        AckiNackiBridge successor = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.with(
                bridge.primaryVerifier(),
                bridge.fallbackVerifier(),
                bridge.layerHashesVerifier(),
                BK_SET,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdrawAfter(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)),
                DAPP_FR,
                ACC_FR,
                address(bridge)
            )
        );
        bridge.pause();
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TreasuryNotLiquid.selector, 1 * UNIT, 10 * UNIT)
        );
        bridge.migrateTreasury(address(successor));

        bridge.withdrawFromAave(type(uint256).max);
        bridge.migrateTreasury(address(successor));
        assertEq(bridge.treasuryBalance(), 0);
        assertEq(successor.treasuryBalance(), 10 * UNIT);
        assertEq(usdc.balanceOf(address(successor)), 10 * UNIT);
    }

    // -----------------------------------------------------------------
    // Branches no other test reaches (OWN-1, OWN-4, OWN-9..11)
    // -----------------------------------------------------------------

    /// @dev TC-27: harvest while the pool returns less than asked.
    function test_tc27_harvestRevertsWhenThePoolPaysShort() public {
        _seed();
        aUSDC.accrueYield(address(bridge), 1 * UNIT);
        pool.setRedeemHaircut(1);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.AaveWithdrawFailed.selector, 1 * UNIT, 1 * UNIT - 1
            )
        );
        bridge.harvestYield(1 * UNIT);
        assertEq(bridge.accruedYield(), 1 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
    }

    /// @dev TC-28: `approve` returns false.
    function test_tc28_migrateRevertsWhenApproveFails() public {
        ShortSuccessor successor = _liquidCutover(0);
        usdc.setApproveReturnsFalse(true);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTransferFailed.selector, address(successor), 10 * UNIT
            )
        );
        bridge.migrateTreasury(address(successor));
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 10 * UNIT);
    }

    /// @dev TC-28: the successor takes less than the book.
    function test_tc28_migrateRevertsWhenTheSuccessorTakesLess() public {
        ShortSuccessor successor = _liquidCutover(1);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTransferFailed.selector, address(successor), 10 * UNIT
            )
        );
        bridge.migrateTreasury(address(successor));
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 10 * UNIT);
    }

    /// @dev TC-29: the transfer to `yieldRecipient` returns false.
    function test_tc29_collectorsRevertWhenTheTransferFails() public {
        _seed();
        aUSDC.accrueYield(address(bridge), 1 * UNIT);
        usdc.mint(address(bridge), 2 * UNIT);
        usdc.setTransferFailsTo(yieldSink);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTransferFailed.selector, yieldSink, 1 * UNIT
            )
        );
        bridge.harvestYield(1 * UNIT);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTransferFailed.selector, yieldSink, 2 * UNIT
            )
        );
        bridge.skimExcessUsdc(2 * UNIT);

        assertEq(bridge.accruedYield(), 1 * UNIT);
        assertEq(bridge.excessUsdc(), 2 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 9 * UNIT);
        assertEq(bridge.treasuryBalance(), 10 * UNIT);
    }

    /// @dev TC-30: `fundTreasury(0)`, from the owner and from the predecessor.
    function test_tc30_fundTreasuryZeroReverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        bridge.fundTreasury(0);

        AckiNackiBridge successor = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.with(
                bridge.primaryVerifier(),
                bridge.fallbackVerifier(),
                bridge.layerHashesVerifier(),
                BK_SET,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdrawAfter(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)),
                DAPP_FR,
                ACC_FR,
                address(bridge)
            )
        );
        vm.prank(address(bridge));
        vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        successor.fundTreasury(0);
    }

    /// @dev TC-31: `withdrawFromAave(max)` pulls `S` over interest and `A`
    ///      under a loss (A = 1 + 36 − 30 = 7).
    function test_tc31_withdrawMaxPullsTheBackedPrincipal() public {
        _seed();
        aUSDC.accrueYield(address(bridge), 1 * UNIT);
        bridge.withdrawFromAave(type(uint256).max);
        assertEq(usdc.balanceOf(address(bridge)), 10 * UNIT, "pulled S");
        assertEq(bridge.aUsdcBalance(), 1 * UNIT, "interest stays");
        assertEq(bridge.suppliedPrincipal(), 0);

        _deposit(user, 30 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 36 * UNIT);
        aUSDC.burnFrom(address(bridge), 30 * UNIT);
        bridge.withdrawFromAave(type(uint256).max);
        assertEq(bridge.aUsdcBalance(), 0, "pulled A");
        assertEq(bridge.suppliedPrincipal(), 29 * UNIT, "the loss stays booked for the write-off");
    }

    /// @dev `yieldRecipient` is never zero, so the collectors need no check.
    function test_yieldRecipientCannotBeZero() public {
        vm.expectRevert(AckiNackiBridge.InvalidRecipient.selector);
        bridge.setYieldRecipient(address(0));
    }

    /// @dev Paused bridge with its book liquid and a successor that takes
    ///      `shortBy` less than it is offered.
    function _liquidCutover(uint256 shortBy) internal returns (ShortSuccessor successor) {
        _seed();
        bridge.pause();
        bridge.withdrawFromAave(type(uint256).max);
        successor = new ShortSuccessor(address(bridge), usdc, shortBy);
    }
}

/// @notice Names the bridge as its predecessor and takes `shortBy` less than
///         `fundTreasury` is called with.
contract ShortSuccessor {
    address public immutable predecessor;
    MockERC20 internal immutable usdc;
    uint256 internal immutable shortBy;

    constructor(address _predecessor, MockERC20 _usdc, uint256 _shortBy) {
        predecessor = _predecessor;
        usdc = _usdc;
        shortBy = _shortBy;
    }

    function fundTreasury(uint256 amount) external {
        usdc.transferFrom(msg.sender, address(this), amount - shortBy);
    }
}
