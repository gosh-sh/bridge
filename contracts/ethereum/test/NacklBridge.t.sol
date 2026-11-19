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
        (uint256 limit,,,) = bridge.nacklState();
        assertEq(limit, DAILY_LIMIT);
    }

    function test_NB_EVM_LIST_refusesATokenTheBridgeCannotMint() public {
        AckiNackiBridge b = _newBridge(address(0));
        NacklToken foreign = new NacklToken(address(this), address(0), 0);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklNotMinter.selector, address(foreign)));
        b.listNackl(foreign, MAX_TRANSFER, DAILY_LIMIT);

        EighteenDecimalsToken wide = new EighteenDecimalsToken(address(b));
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklBadDecimals.selector, uint8(18)));
        b.listNackl(NacklToken(address(wide)), MAX_TRANSFER, DAILY_LIMIT);
    }

    function test_NB_EVM_LIST_onceAndOwnerOnly() public {
        NacklToken other = new NacklToken(address(bridge), address(0), 0);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenIdAlreadyUsed.selector, 1, address(nackl))
        );
        bridge.listNackl(other, MAX_TRANSFER, DAILY_LIMIT);

        AckiNackiBridge b = _newBridge(address(0));
        NacklToken t = new NacklToken(address(b), address(0), 0);
        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        b.listNackl(t, MAX_TRANSFER, DAILY_LIMIT);
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

    function test_NB_EVM_WITHDRAW_payoutIsMintedAtOnce() public {
        uint256 n = _withdraw(MAX_TRANSFER);
        assertEq(nackl.balanceOf(RECIPIENT), MAX_TRANSFER);
        assertTrue(bridge.isNullifierUsed(n));
        assertEq(_minted(bridge), MAX_TRANSFER);
        assertEq(_windowUsed(), MAX_TRANSFER);
    }

    function test_NB_EVM_WITHDRAW_overTheLimitIsRefusedAndCanBeSentAgain() public {
        for (uint256 i = 0; i < 20; i++) {
            _withdraw(PAYOUT);
        }
        assertEq(_windowUsed(), DAILY_LIMIT);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory late =
            _pub(bridge, 1, 700, RECIPIENT);
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklWindowFull.selector, DAILY_LIMIT, 1, DAILY_LIMIT)
        );
        bridge.withdrawByProof(hex"00", late);
        assertFalse(bridge.isNullifierUsed(700));
        assertEq(nackl.balanceOf(RECIPIENT), DAILY_LIMIT);

        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklWindowFull.selector, DAILY_LIMIT, 1, DAILY_LIMIT)
        );
        bridge.withdrawByProof(hex"00", late);

        vm.warp(block.timestamp + 1 hours);
        bridge.withdrawByProof(hex"00", late);
        assertTrue(bridge.isNullifierUsed(700));
        assertEq(nackl.balanceOf(RECIPIENT), DAILY_LIMIT + 1);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, 700));
        bridge.withdrawByProof(hex"00", late);
    }

    function _tryWithdraw(AckiNackiBridge b, uint256 amount, uint256 nullifier)
        internal
        returns (bool)
    {
        try b.withdrawByProof(hex"00", _pub(b, amount, nullifier, RECIPIENT)) {
            return true;
        } catch {
            return false;
        }
    }

    function test_NB_EVM_WINDOW_any24HoursStayWithinTheLimit(uint8[48] memory spread) public {
        uint256 start = 1_800_000_000;
        uint256[] memory mintedAt = new uint256[](48);
        uint256 n = 10_000;
        for (uint256 h = 0; h < 48; h++) {
            vm.warp(start + h * 1 hours + 1);
            uint256 before = nackl.totalSupply();
            uint256 count = spread[h] % 4;
            for (uint256 i = 0; i < count; i++) {
                bool paid = _tryWithdraw(bridge, PAYOUT, n);
                assertEq(bridge.isNullifierUsed(n), paid);
                n++;
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

    function test_NB_EVM_WITHDRAW_capStillApplies() public {
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, MAX_TRANSFER + 1, 99, RECIPIENT));
    }

    function test_NB_EVM_LIMITS_ownerSetsTheLimitNotBelowTheCap() public {
        bridge.listNackl(nackl, 0, 2 * MAX_TRANSFER);
        (uint256 limit,,,) = bridge.nacklState();
        assertEq(limit, 2 * MAX_TRANSFER);

        vm.expectRevert(
            abi.encodeWithSelector(
                NacklMint.NacklLimitBelowWithdrawalCap.selector, MAX_TRANSFER - 1, MAX_TRANSFER
            )
        );
        bridge.listNackl(nackl, 0, MAX_TRANSFER - 1);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenLimitsInvalid.selector, 2 * MAX_TRANSFER + 1, type(uint256).max
            )
        );
        bridge.setTokenLimits(1, 2 * MAX_TRANSFER + 1, type(uint256).max);

        vm.prank(user);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.listNackl(nackl, 0, DAILY_LIMIT);
    }

    function test_NB_EVM_PAUSE_stopsDepositAndWithdrawal() public {
        _fund(user, NACKL_UNIT);
        bridge.pause();
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
        s.listNackl(nackl, MAX_TRANSFER, DAILY_LIMIT);
    }

    function test_NB_EVM_HANDOVER_refusedUnlessPausedAndReady() public {
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

        bridge.migrateToken(1, address(s));
        assertEq(nackl.owner(), address(s));
    }

    function test_NB_EVM_HANDOVER_movesTheMinterAndStopsTheOldBridge() public {
        _withdraw(PAYOUT);
        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        _seedFirstBlock(s);

        AckiNackiBridge stranger = _newBridge(address(0));
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklNotMinter.selector, address(nackl)));
        stranger.listNackl(nackl, MAX_TRANSFER, DAILY_LIMIT);

        bridge.pause();
        bridge.migrateToken(1, address(s));
        assertEq(nackl.owner(), address(s));
        (,,, bool handedOver) = bridge.nacklState();
        assertTrue(handedOver);
        (,, bool open,,,) = bridge.tokenInfo(1);
        assertFalse(open);

        bridge.unpause();
        vm.expectRevert(NacklMint.NacklMinterGone.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, 1, 500, RECIPIENT));
        assertFalse(bridge.isNullifierUsed(500));
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, 1));
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));
        bridge.setTokenDepositsEnabled(1, true);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, 1));
        bridge.deposit(address(nackl), 1, bytes32(uint256(7)));

        s.withdrawByProof(hex"00", _pub(s, PAYOUT, 501, RECIPIENT));
        assertEq(nackl.balanceOf(RECIPIENT), 2 * PAYOUT);
    }

    function test_NB_HO_3_procedureHandsOverOnlyWhenEverythingBoundIsPaid() public {
        _withdraw(PAYOUT);
        uint256 lateAmount = 7 * NACKL_UNIT;
        uint256 lockedToOld = PAYOUT + lateAmount;

        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        _seedFirstBlock(s);
        assertTrue(_minted(bridge) != lockedToOld);

        _withdraw(lateAmount);
        assertEq(_minted(bridge), lockedToOld);

        bridge.pause();
        bridge.migrateToken(1, address(s));
        s.withdrawByProof(hex"00", _pub(s, NACKL_UNIT, 900, RECIPIENT));
        assertEq(nackl.balanceOf(RECIPIENT), lockedToOld + NACKL_UNIT);
    }

    function test_NB_HO_3_earlyHandoverStrandsAWithdrawalOfTheOldBridge() public {
        _withdraw(PAYOUT);
        uint256 lockedToOld = PAYOUT + 3 * NACKL_UNIT;
        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        assertTrue(_minted(bridge) != lockedToOld);

        bridge.pause();
        bridge.migrateToken(1, address(s));
        bridge.unpause();
        vm.expectRevert(NacklMint.NacklMinterGone.selector);
        bridge.withdrawByProof(hex"00", _pub(bridge, 3 * NACKL_UNIT, 901, RECIPIENT));
    }

    function test_NB_HO_3_aSuccessorRefusesPayoutsUntilItIsTheMinter() public {
        AckiNackiBridge s = _successor();
        _listOnSuccessor(s);
        _seedFirstBlock(s);
        uint256 before = nackl.balanceOf(RECIPIENT);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory p =
            _pub(s, NACKL_UNIT, 902, RECIPIENT);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklNotMinter.selector, address(nackl)));
        s.withdrawByProof(hex"00", p);
        assertFalse(s.isNullifierUsed(902));
        assertEq(_minted(s), 0);

        bridge.pause();
        bridge.migrateToken(1, address(s));
        s.withdrawByProof(hex"00", p);
        assertEq(nackl.balanceOf(RECIPIENT), before + NACKL_UNIT);
        assertEq(_minted(s), NACKL_UNIT);
    }

    function test_NB_EVM_STATE_countsMinted() public {
        _withdraw(PAYOUT);
        _withdraw(PAYOUT + 1);
        assertEq(_minted(bridge), 2 * PAYOUT + 1);
        assertEq(nackl.totalSupply(), 2 * PAYOUT + 1);
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
}
