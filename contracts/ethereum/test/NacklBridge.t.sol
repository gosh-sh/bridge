// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./helpers/NacklBridgeFixture.sol";

contract EighteenDecimalsToken {
    address public owner;

    constructor(address o) {
        owner = o;
    }

    function decimals() external pure returns (uint8) {
        return 18;
    }
}

contract NacklBridgeTest is NacklBridgeFixture {
    address internal user = address(0xA11CE);

    event NacklPayoutQueued(
        uint256 indexed payoutId, address indexed recipient, uint256 amount, uint64 readyAt
    );
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        bytes32 anAccount,
        uint256 timestamp,
        address token
    );

    function test_NB_EVM_LIST_listsNacklUnderIdOne() public view {
        (address token, uint8 decimals, bool open, uint256 maxTransfer,,) = bridge.tokenInfo(1);
        assertEq(token, address(nackl));
        assertEq(decimals, 9);
        assertTrue(open);
        assertEq(maxTransfer, MAX_TRANSFER);
        (uint256 limit, uint256 threshold,,,,,,,) = bridge.nacklState();
        assertEq(limit, DAILY_LIMIT);
        assertEq(threshold, DELAY_THRESHOLD);
    }

    function test_NB_EVM_LIST_refusesATokenTheBridgeCannotMint() public {
        AckiNackiBridge b = _newBridge(address(0));
        NacklToken foreign = new NacklToken(address(this), address(0), 0);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklNotMinter.selector, address(foreign)));
        b.listNackl(foreign, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);

        EighteenDecimalsToken wide = new EighteenDecimalsToken(address(b));
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklBadDecimals.selector, uint8(18)));
        b.listNackl(NacklToken(address(wide)), MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);
    }

    function test_NB_EVM_LIST_onceAndOwnerOnly() public {
        NacklToken other = new NacklToken(address(bridge), address(0), 0);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenIdAlreadyUsed.selector, 1, address(nackl))
        );
        bridge.listNackl(other, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);

        AckiNackiBridge b = _newBridge(address(0));
        NacklToken t = new NacklToken(address(b), address(0), 0);
        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        b.listNackl(t, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);
    }

    function test_NB_EVM_DEPOSIT_burnsAndEmitsTheSameDeposit() public {
        _fund(user, 300 * NACKL_UNIT);
        vm.startPrank(user);
        nackl.approve(address(bridge), 200 * NACKL_UNIT);
        vm.expectEmit(true, true, false, true, address(bridge));
        emit Deposit(
            0, user, 200 * NACKL_UNIT, bytes32(uint256(7)), block.timestamp, address(nackl)
        );
        bridge.deposit(address(nackl), 200 * NACKL_UNIT, bytes32(uint256(7)));
        vm.stopPrank();
        assertEq(nackl.balanceOf(user), 100 * NACKL_UNIT);
        assertEq(nackl.totalSupply(), 100 * NACKL_UNIT);
        assertEq(nackl.balanceOf(address(bridge)), 0);
        assertEq(bridge.liabilitiesOf(1), 0);
    }

    function test_NB_EVM_DEPOSIT_capAndAllowance() public {
        _fund(user, 2 * MAX_TRANSFER);
        vm.startPrank(user);
        nackl.approve(address(bridge), 2 * MAX_TRANSFER);
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.deposit(address(nackl), MAX_TRANSFER + 1, bytes32(uint256(7)));
        nackl.approve(address(bridge), 0);
        vm.expectRevert();
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));
        vm.stopPrank();
    }

    function test_NB_EVM_DEPOSIT_noTvlCap() public {
        _fund(user, 20 * MAX_TRANSFER);
        vm.startPrank(user);
        nackl.approve(address(bridge), type(uint256).max);
        for (uint256 i = 0; i < 20; i++) {
            bridge.deposit(address(nackl), MAX_TRANSFER, bytes32(uint256(7)));
        }
        vm.stopPrank();
        assertEq(nackl.totalSupply(), 0);
    }

    function test_NB_EVM_WITHDRAW_smallPayoutIsMintedAtOnce() public {
        uint256 n = _withdraw(DELAY_THRESHOLD);
        assertEq(nackl.balanceOf(RECIPIENT), DELAY_THRESHOLD);
        assertTrue(bridge.isNullifierUsed(n));
        (uint256 accepted, uint256 minted, uint256 pending,) = _state();
        assertEq(accepted, DELAY_THRESHOLD);
        assertEq(minted, DELAY_THRESHOLD);
        assertEq(pending, 0);
    }

    function test_NB_EVM_WITHDRAW_largePayoutWaits24Hours() public {
        uint64 due = uint64(block.timestamp + 24 hours);
        vm.expectEmit(true, true, false, true, address(bridge));
        emit NacklPayoutQueued(1, RECIPIENT, DELAY_THRESHOLD + 1, due);
        _withdraw(DELAY_THRESHOLD + 1);
        assertEq(nackl.balanceOf(RECIPIENT), 0);

        vm.warp(due - 1);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotDue.selector, 1, due));
        bridge.executeNacklPayout(1);

        vm.warp(due);
        vm.prank(address(0xBEEF));
        bridge.executeNacklPayout(1);
        assertEq(nackl.balanceOf(RECIPIENT), DELAY_THRESHOLD + 1);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutAlreadyExecuted.selector, 1));
        bridge.executeNacklPayout(1);
    }

    function test_NB_EVM_WITHDRAW_overTheLimitWaitsForTheWindow() public {
        for (uint256 i = 0; i < 20; i++) {
            _withdraw(DELAY_THRESHOLD);
        }
        assertEq(_windowUsed(), DAILY_LIMIT);
        _withdraw(1);
        (,, uint256 pending, uint256 payouts) = _state();
        assertEq(pending, 1);
        (address to, uint256 amount, uint64 readyAt, bool done) = _payout(payouts);
        assertEq(to, RECIPIENT);
        assertEq(amount, 1);
        assertEq(readyAt, block.timestamp);
        assertFalse(done);

        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklWindowFull.selector, DAILY_LIMIT, 1, DAILY_LIMIT)
        );
        bridge.executeNacklPayout(payouts);

        vm.warp(block.timestamp + 1 hours);
        bridge.executeNacklPayout(payouts);
        assertEq(nackl.balanceOf(RECIPIENT), DAILY_LIMIT + 1);
    }

    function test_NB_EVM_WINDOW_any24HoursStayWithinTheLimit(uint8[48] memory spread) public {
        uint256 start = 1_800_000_000;
        uint256[] memory mintedAt = new uint256[](48);
        for (uint256 h = 0; h < 48; h++) {
            vm.warp(start + h * 1 hours + 1);
            uint256 before = nackl.totalSupply();
            uint256 count = spread[h] % 4;
            for (uint256 i = 0; i < count; i++) {
                _withdraw(DELAY_THRESHOLD);
            }
            mintedAt[h] = nackl.totalSupply() - before;
        }
        for (uint256 h = 0; h + 24 <= 48; h++) {
            uint256 sum;
            for (uint256 k = h; k < h + 24; k++) {
                sum += mintedAt[k];
            }
            assertLe(sum, DAILY_LIMIT);
        }
    }

    function _next() internal view returns (uint256 id) {
        (,,,,,,,, id) = bridge.nacklState();
    }

    function test_NB_EVM_ORDER_aQueuedPayoutIsNotStarvedBySmallerOnes() public {
        bridge.listNackl(nackl, MAX_TRANSFER, MAX_TRANSFER, MAX_TRANSFER);
        _withdraw(1);
        _withdraw(MAX_TRANSFER);
        assertEq(_next(), 1);
        uint256 t = block.timestamp;
        for (uint256 h = 0; h < 30; h++) {
            t += 1 hours;
            vm.warp(t);
            _withdraw(1);
        }
        assertEq(nackl.balanceOf(RECIPIENT), 1);
        (,, uint256 pending, uint256 payouts) = _state();
        assertEq(pending, 31);
        assertEq(payouts, 31);

        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotNext.selector, 2, 1));
        bridge.executeNacklPayout(2);

        bridge.executeNacklPayout(1);
        assertEq(nackl.balanceOf(RECIPIENT), 1 + MAX_TRANSFER);
        assertEq(_next(), 2);

        vm.warp(t + 25 hours);
        for (uint256 id = 2; id <= 31; id++) {
            assertEq(_next(), id);
            bridge.executeNacklPayout(id);
        }
        (,, pending,) = _state();
        assertEq(pending, 0);
        assertEq(_next(), 0);
        _withdraw(1);
        assertEq(nackl.balanceOf(RECIPIENT), 1 + MAX_TRANSFER + 31);

        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        bridge.pause();
        bridge.migrateToken(1, address(s));
        assertEq(nackl.owner(), address(s));
    }

    function test_NB_EVM_ORDER_aDelayedPayoutNotYetDueHoldsNothingBack() public {
        _withdraw(DELAY_THRESHOLD + 1);
        assertEq(_next(), 0);
        _withdraw(DELAY_THRESHOLD);
        assertEq(nackl.balanceOf(RECIPIENT), DELAY_THRESHOLD);
        (,, uint256 pending,) = _state();
        assertEq(pending, 1);
    }

    function test_NB_EVM_ORDER_aDueDelayedPayoutGoesBeforeLaterOnes() public {
        _withdraw(DELAY_THRESHOLD + 1);
        vm.warp(block.timestamp + 24 hours);
        assertEq(_next(), 1);
        _withdraw(1);
        assertEq(nackl.balanceOf(RECIPIENT), 0);
        assertEq(_next(), 1);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotNext.selector, 2, 1));
        bridge.executeNacklPayout(2);
        bridge.executeNacklPayout(1);
        assertEq(_next(), 2);
        bridge.executeNacklPayout(2);
        assertEq(nackl.balanceOf(RECIPIENT), DELAY_THRESHOLD + 2);
        assertEq(_next(), 0);
    }

    function test_NB_EVM_ORDER_windowQueuedPaysBeforeALaterDueDelayed() public {
        for (uint256 i = 0; i < 20; i++) {
            _withdraw(DELAY_THRESHOLD);
        }
        _withdraw(1);
        _withdraw(DELAY_THRESHOLD + 1);
        assertEq(_next(), 1);
        vm.warp(block.timestamp + 25 hours);
        assertEq(_next(), 1);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotNext.selector, 2, 1));
        bridge.executeNacklPayout(2);
        bridge.executeNacklPayout(1);
        bridge.executeNacklPayout(2);
        assertEq(_next(), 0);
    }

    function test_NB_EVM_EXECUTE_unknownPayout() public {
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutUnknown.selector, 7));
        bridge.executeNacklPayout(7);
    }

    function test_NB_P_SUPPLY_queuedNullifierIsSpent() public {
        uint256 n = _withdraw(DELAY_THRESHOLD + 1);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, n));
        bridge.withdrawByProof(hex"00", _pub(bridge, DELAY_THRESHOLD + 1, n, RECIPIENT));
    }

    function test_NB_EVM_WITHDRAW_capStillApplies() public {
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, MAX_TRANSFER + 1, 99, RECIPIENT));
    }

    function test_NB_EVM_LIMITS_ownerSetsLimitsNotBelowTheCap() public {
        bridge.listNackl(nackl, 0, 2 * MAX_TRANSFER, 10);
        (uint256 limit, uint256 threshold,,,,,,,) = bridge.nacklState();
        assertEq(limit, 2 * MAX_TRANSFER);
        assertEq(threshold, 10);

        vm.expectRevert(
            abi.encodeWithSelector(
                NacklMint.NacklLimitBelowWithdrawalCap.selector, MAX_TRANSFER - 1, MAX_TRANSFER
            )
        );
        bridge.listNackl(nackl, 0, MAX_TRANSFER - 1, 10);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenLimitsInvalid.selector, 2 * MAX_TRANSFER + 1, type(uint256).max
            )
        );
        bridge.setTokenLimits(1, 2 * MAX_TRANSFER + 1, type(uint256).max);

        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.listNackl(nackl, 0, DAILY_LIMIT, DELAY_THRESHOLD);
    }

    function test_NB_EVM_NOCANCEL_ownerCallsLeaveAQueuedPayout() public {
        _withdraw(DELAY_THRESHOLD + 1);
        bridge.listNackl(nackl, 0, MAX_TRANSFER, 0);
        bridge.pause();
        bridge.unpause();
        bridge.setTokenDepositsEnabled(1, false);
        (address to, uint256 amount, uint64 readyAt, bool done) = _payout(1);
        assertEq(to, RECIPIENT);
        assertEq(amount, DELAY_THRESHOLD + 1);
        assertEq(readyAt, block.timestamp + 24 hours);
        assertFalse(done);
        vm.warp(readyAt);
        bridge.executeNacklPayout(1);
        assertEq(nackl.balanceOf(RECIPIENT), DELAY_THRESHOLD + 1);
    }

    function test_NB_EVM_PAUSE_stopsDepositWithdrawalAndExecution() public {
        _withdraw(DELAY_THRESHOLD + 1);
        _fund(user, NACKL_UNIT);
        bridge.pause();
        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.executeNacklPayout(1);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, 1, 77, RECIPIENT));
        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));
    }

    function _successor() internal returns (AckiNackiBridge s) {
        s = _newBridge(address(bridge));
    }

    function _listOnSuccessor(AckiNackiBridge s) internal {
        s.listNackl(nackl, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);
    }

    function test_NB_EVM_HANDOVER_refusedUnlessPausedReadyAndDrained() public {
        AckiNackiBridge s = _successor();
        vm.expectRevert(AckiNackiBridge.NotPaused.selector);
        bridge.migrateToken(1, address(s));

        bridge.pause();
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklSuccessorNotReady.selector, address(s))
        );
        bridge.migrateToken(1, address(s));
        _listOnSuccessor(s);

        AckiNackiBridge stranger = _newBridge(address(0));
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(stranger))
        );
        bridge.migrateToken(1, address(stranger));

        bridge.unpause();
        _withdraw(DELAY_THRESHOLD + 1);
        bridge.pause();
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutsPending.selector, 1));
        bridge.migrateToken(1, address(s));
    }

    function test_NB_EVM_HANDOVER_movesTheMinterAndStopsTheOldBridge() public {
        _withdraw(DELAY_THRESHOLD);
        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        _seedFirstBlock(s);
        vm.expectRevert();
        s.withdrawByProof(hex"00", _pub(s, 1, 400, RECIPIENT));

        AckiNackiBridge stranger = _newBridge(address(0));
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklNotMinter.selector, address(nackl)));
        stranger.listNackl(nackl, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);

        bridge.pause();
        bridge.migrateToken(1, address(s));
        assertEq(nackl.owner(), address(s));
        (,,,,,,, bool handedOver,) = bridge.nacklState();
        assertTrue(handedOver);
        (,, bool open,,,) = bridge.tokenInfo(1);
        assertFalse(open);

        bridge.unpause();
        vm.expectRevert(NacklMint.NacklMinterGone.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, 1, 500, RECIPIENT));
        vm.expectRevert(NacklMint.NacklMinterGone.selector);
        bridge.executeNacklPayout(1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, 1));
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));
        bridge.setTokenDepositsEnabled(1, true);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, 1));
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));

        s.withdrawByProof(hex"00", _pub(s, DELAY_THRESHOLD, 501, RECIPIENT));
        assertEq(nackl.balanceOf(RECIPIENT), 2 * DELAY_THRESHOLD);
    }

    function _handoverReady(AckiNackiBridge b, uint256 lockedTo) internal view returns (bool) {
        (,,, uint256 accepted,, uint256 pendingCount,,,) = b.nacklState();
        return accepted == lockedTo && pendingCount == 0;
    }

    function test_NB_HO_3_procedureHandsOverOnlyWhenEverythingBoundIsPaid() public {
        _withdraw(DELAY_THRESHOLD);
        _withdraw(DELAY_THRESHOLD + 1);
        uint256 lateAmount = 7 * NACKL_UNIT;
        uint256 lockedToOld = 2 * DELAY_THRESHOLD + 1 + lateAmount;

        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        _seedFirstBlock(s);
        assertFalse(_handoverReady(bridge, lockedToOld));

        _withdraw(lateAmount);
        assertFalse(_handoverReady(bridge, lockedToOld));
        vm.warp(block.timestamp + 24 hours);
        bridge.executeNacklPayout(1);
        assertTrue(_handoverReady(bridge, lockedToOld));

        bridge.pause();
        bridge.migrateToken(1, address(s));
        s.withdrawByProof(hex"00", _pub(s, NACKL_UNIT, 900, RECIPIENT));
        assertEq(nackl.balanceOf(RECIPIENT), lockedToOld + NACKL_UNIT);
    }

    function test_NB_HO_3_earlyHandoverStrandsAWithdrawalOfTheOldBridge() public {
        _withdraw(DELAY_THRESHOLD);
        uint256 lockedToOld = DELAY_THRESHOLD + 3 * NACKL_UNIT;
        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        assertFalse(_handoverReady(bridge, lockedToOld));

        bridge.pause();
        bridge.migrateToken(1, address(s));
        bridge.unpause();
        vm.expectRevert(NacklMint.NacklMinterGone.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, 3 * NACKL_UNIT, 901, RECIPIENT));
    }

    function test_NB_EVM_STATE_countsAcceptedMintedAndQueued() public {
        _withdraw(DELAY_THRESHOLD);
        _withdraw(DELAY_THRESHOLD + 1);
        (uint256 accepted, uint256 minted, uint256 pending, uint256 payouts) = _state();
        assertEq(accepted, 2 * DELAY_THRESHOLD + 1);
        assertEq(minted, DELAY_THRESHOLD);
        assertEq(pending, 1);
        assertEq(payouts, 1);
    }

    function test_NB_EVM_12_usdcStillPaysFromCustody() public {
        usdc.mint(user, 5e6);
        vm.startPrank(user);
        usdc.approve(address(bridge), 5e6);
        bridge.deposit(address(usdc), 5e6, bytes32(uint256(7)));
        vm.stopPrank();
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory p =
            _pub(bridge, 2e6, 600, RECIPIENT);
        p.tokenId = 0;
        bridge.withdrawByProof(hex"00", p);
        assertEq(usdc.balanceOf(RECIPIENT), 2e6);
        assertEq(bridge.treasuryBalance(), 3e6);
        assertEq(nackl.totalSupply(), 0);
    }

    function _payout(uint256 id)
        internal
        view
        returns (address to, uint256 amount, uint64 readyAt, bool executed)
    {
        bytes32 base = keccak256(abi.encode(id, _payoutsSlot()));
        uint256 w0 = uint256(vm.load(address(bridge), base));
        to = address(uint160(w0));
        readyAt = uint64(w0 >> 160);
        executed = (w0 >> 224) & 1 == 1;
        amount = uint256(vm.load(address(bridge), bytes32(uint256(base) + 1)));
    }

    uint256 internal constant NACKL_SLOT = 19;

    function _payoutsSlot() internal pure returns (uint256) {
        return NACKL_SLOT + 4;
    }
}
