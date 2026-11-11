// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { IERC20Errors } from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";

import "./helpers/NacklBridgeFixture.sol";
import { NacklMint } from "../src/NacklMint.sol";

/// @notice A token that reports 9 decimals but keeps its owner on
///         `transferOwnership`, so a handover to it cannot be completed.
contract StickyOwnerToken {
    address public owner;

    constructor(address owner_) {
        owner = owner_;
    }

    function decimals() external pure returns (uint8) {
        return 9;
    }

    function transferOwnership(address) external { }
}

/// @title NacklBridgeTest
/// @notice ERC20 NACKL on `AckiNackiBridge`: listing, burn on deposit, mint on
///         payout, the daily limit, the delayed queue and the minter handover.
///         Each test names the requirements of docs/NACKL-bridge-extension.md
///         it covers.
contract NacklBridgeTest is NacklBridgeFixture {
    address internal holder = address(0xA11CE);

    // Local copies of the bridge's events, for `expectEmit`.
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        bytes32 anAccount,
        uint256 timestamp,
        address token
    );
    event WithdrawalByProofExecuted(
        uint256 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 indexed tokenId,
        address submitter
    );
    event NacklMintLimitsSet(uint256 dailyLimit, uint256 delayThreshold);
    event NacklPayoutQueued(
        uint256 indexed nullifier, address indexed recipient, uint256 amount, uint256 readyAt
    );
    event NacklPayoutAbandoned(uint256 indexed nullifier, address indexed recipient, uint256 amount);
    event NacklMinterMigrated(address indexed successor);

    // ─────────────────────────────────────────────────────────────────────
    // Listing
    // ─────────────────────────────────────────────────────────────────────

    /// @notice EVM-1: NACKL is listed under id 1, open for deposits, with its limits.
    function test_EVM1_listNacklListsIdOneWithLimits() public view {
        (address token, uint8 decimals, bool open, uint256 maxTransfer, uint256 tvlCap, uint256 owed) =
            bridge.tokenInfo(NACKL_ID);
        assertEq(token, address(nackl));
        assertEq(decimals, 9);
        assertTrue(open);
        assertEq(maxTransfer, MAX_TRANSFER);
        assertEq(tvlCap, MAX_TRANSFER);
        assertEq(owed, 0);
        assertEq(bridge.maxWithdrawalAmount(NACKL_ID), MAX_TRANSFER);
        (bool listed, uint32 id) = bridge.tokenIdOf(address(nackl));
        assertTrue(listed);
        assertEq(id, NACKL_ID);
        (uint256 daily, uint256 threshold, uint256 capacity, uint256 queued, address migratedTo) =
            bridge.nacklState();
        assertEq(daily, DAILY);
        assertEq(threshold, THRESHOLD);
        assertEq(capacity, DAILY);
        assertEq(queued, 0);
        assertEq(migratedTo, address(0));
    }

    /// @notice EVM-1: the listing emits the limits.
    function test_EVM1_listNacklEmitsLimits() public {
        AckiNackiBridge b = _liveBridge(address(0));
        NacklToken t = new NacklToken(address(b), address(0), 0);
        vm.expectEmit(address(b));
        emit NacklMintLimitsSet(DAILY, THRESHOLD);
        b.listNackl(address(t), MAX_TRANSFER, DAILY, THRESHOLD);
    }

    /// @notice EVM-2: once, owner only, 9 decimals, sane limits.
    function test_EVM2_listNacklRefusals() public {
        vm.expectRevert(NacklMint.NacklAlreadyListed.selector);
        bridge.listNackl(address(nackl), MAX_TRANSFER, DAILY, THRESHOLD);

        AckiNackiBridge b = _liveBridge(address(0));
        NacklToken t = new NacklToken(address(b), address(0), 0);
        vm.prank(address(0xBAD));
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        b.listNackl(address(t), MAX_TRANSFER, DAILY, THRESHOLD);

        MockERC20 eighteen = new MockERC20("x", "x", 18);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklDecimalsMismatch.selector, 18));
        b.listNackl(address(eighteen), MAX_TRANSFER, DAILY, THRESHOLD);

        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklLimitsInvalid.selector, 0, MAX_TRANSFER));
        b.listNackl(address(t), MAX_TRANSFER, 0, THRESHOLD);
        vm.expectRevert(
            abi.encodeWithSelector(
                NacklMint.NacklLimitsInvalid.selector, MAX_TRANSFER - 1, MAX_TRANSFER
            )
        );
        b.listNackl(address(t), MAX_TRANSFER, MAX_TRANSFER - 1, THRESHOLD);

        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklDecimalsMismatch.selector, 6));
        b.listNackl(address(usdc), MAX_TRANSFER, DAILY, THRESHOLD);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Deposit
    // ─────────────────────────────────────────────────────────────────────

    uint256 internal fundNullifier = 1 << 100;

    /// @dev Mint `amount` to `who` through proofs, in payouts that need no delay.
    function _fund(address who, uint256 amount) internal {
        while (amount > 0) {
            uint256 part = amount < THRESHOLD ? amount : THRESHOLD;
            _pay(bridge, who, part, fundNullifier++);
            amount -= part;
        }
    }

    /// @notice EVM-3, P-5: a deposit burns the caller's NACKL and books nothing.
    function test_EVM3_depositBurnsAndEmits() public {
        _fund(holder, 300 * UNIT);
        uint256 supply = nackl.totalSupply();
        vm.startPrank(holder);
        nackl.approve(address(bridge), 120 * UNIT);
        vm.expectEmit(address(bridge));
        emit Deposit(0, holder, 120 * UNIT, bytes32(uint256(43)), block.timestamp, address(nackl));
        bridge.deposit(address(nackl), 120 * UNIT, bytes32(uint256(43)));
        vm.stopPrank();
        assertEq(nackl.totalSupply(), supply - 120 * UNIT);
        assertEq(nackl.balanceOf(holder), 180 * UNIT);
        assertEq(nackl.balanceOf(address(bridge)), 0);
        assertEq(bridge.liabilitiesOf(NACKL_ID), 0);
        assertEq(nackl.allowance(holder, address(bridge)), 0);
    }

    /// @notice EVM-3: the checks every deposit makes still apply.
    function test_EVM3_depositKeepsTheTokenChecks() public {
        _fund(holder, MAX_TRANSFER);
        _fund(holder, MAX_TRANSFER);
        vm.startPrank(holder);
        vm.expectRevert(
            abi.encodeWithSelector(
                IERC20Errors.ERC20InsufficientAllowance.selector, address(bridge), 0, UNIT
            )
        );
        bridge.deposit(address(nackl), UNIT, bytes32(uint256(43)));
        nackl.approve(address(bridge), type(uint256).max);
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.deposit(address(nackl), MAX_TRANSFER + 1, bytes32(uint256(43)));
        vm.expectRevert(AckiNackiBridge.InvalidAnAccount.selector);
        bridge.deposit(address(nackl), UNIT, bytes32(0));
        vm.stopPrank();

        bridge.setTokenDepositsEnabled(NACKL_ID, false);
        vm.prank(holder);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, 1));
        bridge.deposit(address(nackl), UNIT, bytes32(uint256(43)));
        bridge.setTokenDepositsEnabled(NACKL_ID, true);

        bridge.pause();
        vm.prank(holder);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.deposit(address(nackl), UNIT, bytes32(uint256(43)));
        bridge.unpause();

        // Two full deposits in a row: no TVL ceiling, since nothing is booked.
        vm.startPrank(holder);
        bridge.deposit(address(nackl), MAX_TRANSFER, bytes32(uint256(43)));
        bridge.deposit(address(nackl), MAX_TRANSFER, bytes32(uint256(43)));
        vm.stopPrank();
        assertEq(nackl.totalSupply(), 0);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Payout
    // ─────────────────────────────────────────────────────────────────────

    /// @notice EVM-4, LIM-2, P-5: a payout within the threshold and the capacity mints at once.
    function test_EVM4_LIM2_payoutMintsAtOnce() public {
        vm.expectEmit(address(bridge));
        emit WithdrawalByProofExecuted(1, RECIPIENT, THRESHOLD, NACKL_ID, address(this));
        _pay(THRESHOLD, 1);
        assertEq(nackl.balanceOf(RECIPIENT), THRESHOLD);
        assertEq(nackl.totalSupply(), THRESHOLD);
        assertEq(nackl.balanceOf(address(bridge)), 0);
        assertTrue(bridge.isNullifierUsed(1));
        assertEq(_capacity(bridge), DAILY - THRESHOLD);
        assertEq(bridge.liabilitiesOf(NACKL_ID), 0);
    }

    /// @notice EVM-4: the checks before the mint still apply and a refused proof mints nothing.
    function test_EVM4_payoutKeepsEveryCheck() public {
        _pay(UNIT, 1);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, 1));
        _pay(UNIT, 1);

        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        _pay(MAX_TRANSFER + 1, 2);

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pub(bridge, RECIPIENT, UNIT, 3);
        pub.dstBridge = 0xBEEF;
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.WrongDstBridge.selector, 0xBEEF));
        bridge.withdrawByProof(hex"c4", pub);

        withdrawalVerifier.setShouldAccept(false);
        vm.expectRevert(AckiNackiBridge.WithdrawalProofRejected.selector);
        _pay(UNIT, 4);
        withdrawalVerifier.setShouldAccept(true);

        bridge.pause();
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        _pay(UNIT, 5);
        bridge.unpause();

        assertEq(nackl.totalSupply(), UNIT);
        assertFalse(bridge.isNullifierUsed(4));
    }

    /// @notice EVM-4: a payout made while the bridge does not own the token reverts whole.
    function test_EVM4_payoutBeforeOwnershipRevertsAndKeepsTheNullifier() public {
        AckiNackiBridge b = _liveBridge(address(0));
        NacklToken t = new NacklToken(address(this), address(0), 0);
        b.listNackl(address(t), MAX_TRANSFER, DAILY, THRESHOLD);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, address(b)));
        _pay(b, RECIPIENT, UNIT, 9);
        assertFalse(b.isNullifierUsed(9));
        t.transferOwnership(address(b));
        _pay(b, RECIPIENT, UNIT, 9);
        assertEq(t.balanceOf(RECIPIENT), UNIT);
    }

    /// @notice EVM-5: there is no NACKL custody to fund or migrate.
    function test_EVM5_fundAndMigrateTokenRefuseNackl() public {
        vm.expectRevert(NacklMint.NacklNotCustodied.selector);
        bridge.fundToken(NACKL_ID, address(nackl), UNIT);
        AckiNackiBridge successor = _liveBridge(address(bridge));
        bridge.pause();
        vm.expectRevert(AckiNackiBridge.NothingToMigrate.selector);
        bridge.migrateToken(NACKL_ID, address(successor));
    }

    /// @notice EVM-6: the deposit cap, and with it the withdrawal cap, stays within the daily limit.
    function test_EVM6_setTokenLimitsStaysWithinTheDailyLimit() public {
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklLimitsInvalid.selector, DAILY, DAILY + 1));
        bridge.setTokenLimits(NACKL_ID, DAILY + 1, DAILY + 1);
        bridge.setTokenLimits(NACKL_ID, DAILY, DAILY);
        assertEq(bridge.maxWithdrawalAmount(NACKL_ID), DAILY);
    }

    /// @notice EVM-8: the bridge stays within EIP-170 with the library linked.
    function test_EVM8_bridgeFitsEip170() public view {
        assertLe(address(bridge).code.length, 24_576);
    }

    /// @notice The events and errors the bridge declares for its ABI are the library's.
    function test_EVM8_bridgeAbiCopiesMatchTheLibrary() public pure {
        assertEq(AckiNackiBridge.NacklMintLimitsSet.selector, NacklMint.NacklMintLimitsSet.selector);
        assertEq(AckiNackiBridge.NacklPayoutQueued.selector, NacklMint.NacklPayoutQueued.selector);
        assertEq(AckiNackiBridge.NacklPayoutAbandoned.selector, NacklMint.NacklPayoutAbandoned.selector);
        assertEq(AckiNackiBridge.NacklMinterMigrated.selector, NacklMint.NacklMinterMigrated.selector);
        bytes4[9] memory bridgeErrors = [
            AckiNackiBridge.NacklAlreadyListed.selector,
            AckiNackiBridge.NacklDecimalsMismatch.selector,
            AckiNackiBridge.NacklPayoutNotQueued.selector,
            AckiNackiBridge.NacklPayoutNotDue.selector,
            AckiNackiBridge.NacklMintLimitReached.selector,
            AckiNackiBridge.NacklQueueNotEmpty.selector,
            AckiNackiBridge.NacklMinterHandedOver.selector,
            AckiNackiBridge.NacklSuccessorMismatch.selector,
            AckiNackiBridge.NacklNotListed.selector
        ];
        bytes4[9] memory libraryErrors = [
            NacklMint.NacklAlreadyListed.selector,
            NacklMint.NacklDecimalsMismatch.selector,
            NacklMint.NacklPayoutNotQueued.selector,
            NacklMint.NacklPayoutNotDue.selector,
            NacklMint.NacklMintLimitReached.selector,
            NacklMint.NacklQueueNotEmpty.selector,
            NacklMint.NacklMinterHandedOver.selector,
            NacklMint.NacklSuccessorMismatch.selector,
            NacklMint.NacklNotListed.selector
        ];
        for (uint256 i = 0; i < 9; ++i) {
            assertEq(bridgeErrors[i], libraryErrors[i]);
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Daily limit
    // ─────────────────────────────────────────────────────────────────────

    function _fillWindow() internal {
        for (uint256 i = 0; i < DAILY / THRESHOLD; ++i) {
            _pay(THRESHOLD, 100 + i);
        }
        assertEq(_capacity(bridge), 0);
    }

    /// @notice LIM-3, LIM-6, DLY-2: a payout that does not fit is queued, due at once, and
    ///         executes once the window frees room — more than 24 hours after the mints.
    function test_LIM3_payoutOverCapacityWaitsForTheWindow() public {
        _fillWindow();
        vm.expectEmit(address(bridge));
        emit NacklPayoutQueued(7, RECIPIENT, UNIT, block.timestamp);
        _pay(UNIT, 7);
        assertEq(nackl.totalSupply(), DAILY);
        assertEq(_queued(bridge), 1);

        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklMintLimitReached.selector, UNIT, 0));
        bridge.executeNacklPayout(7);

        // The mints were 10 minutes into hour h; they still count at h + 24 + 49 minutes.
        vm.warp(block.timestamp + 24 hours + 49 minutes);
        assertEq(_capacity(bridge), 0);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklMintLimitReached.selector, UNIT, 0));
        bridge.executeNacklPayout(7);

        vm.warp(block.timestamp + 1 minutes);
        assertEq(_capacity(bridge), DAILY);
        address relayer = address(0xCAFE);
        vm.expectEmit(address(bridge));
        emit WithdrawalByProofExecuted(7, RECIPIENT, UNIT, NACKL_ID, relayer);
        vm.prank(relayer);
        bridge.executeNacklPayout(7);
        assertEq(nackl.totalSupply(), DAILY + UNIT);
        assertEq(_queued(bridge), 0);
        assertEq(_capacity(bridge), DAILY - UNIT);
    }

    /// @notice LIM-1: mints within one window never exceed the limit, wherever they fall in it.
    function test_LIM1_mintsOverAnyDayStayWithinTheLimit() public {
        _pay(THRESHOLD, 1);
        vm.warp(block.timestamp + 20 hours);
        for (uint256 i = 0; i < 9; ++i) {
            _pay(THRESHOLD, 10 + i);
        }
        assertEq(_capacity(bridge), 0);
        _pay(THRESHOLD, 20);
        assertEq(_queued(bridge), 1);
        // 24 h 50 min after the first mint it leaves the window; the other nine do not.
        vm.warp(block.timestamp + 4 hours + 50 minutes);
        assertEq(_capacity(bridge), THRESHOLD);
        bridge.executeNacklPayout(20);
        assertEq(_capacity(bridge), 0);
        assertEq(nackl.totalSupply(), DAILY + THRESHOLD);
    }

    /// @notice LIM-4: the owner changes the limits; what the window minted keeps counting.
    function test_LIM4_ownerSetsTheLimits() public {
        _pay(THRESHOLD, 1);
        vm.expectEmit(address(bridge));
        emit NacklMintLimitsSet(2 * MAX_TRANSFER, 10 * UNIT);
        bridge.setNacklMintLimits(2 * MAX_TRANSFER, 10 * UNIT);
        assertEq(_capacity(bridge), 2 * MAX_TRANSFER - THRESHOLD);
        _pay(10 * UNIT + 1, 2);
        assertEq(_queued(bridge), 1);
        bridge.setNacklMintLimits(MAX_TRANSFER, 10 * UNIT);
        assertEq(_capacity(bridge), MAX_TRANSFER - THRESHOLD);

        vm.prank(address(0xBAD));
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setNacklMintLimits(DAILY, THRESHOLD);
    }

    /// @notice LIM-5: every payout the bridge accepts must be able to fit some day.
    function test_LIM5_theDailyLimitCoversTheWithdrawalCap() public {
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklLimitsInvalid.selector, 0, MAX_TRANSFER));
        bridge.setNacklMintLimits(0, THRESHOLD);
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklLimitsInvalid.selector, MAX_TRANSFER - 1, MAX_TRANSFER)
        );
        bridge.setNacklMintLimits(MAX_TRANSFER - 1, THRESHOLD);
        uint256 tooBig = uint256(type(uint128).max) + 1;
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklLimitsInvalid.selector, tooBig, MAX_TRANSFER));
        bridge.setNacklMintLimits(tooBig, THRESHOLD);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Delay
    // ─────────────────────────────────────────────────────────────────────

    /// @notice DLY-1, DLY-2, DLY-6, P-4: a payout above the threshold waits 24 hours.
    function test_DLY1_largePayoutWaitsADay() public {
        uint256 amount = THRESHOLD + 1;
        uint256 due = block.timestamp + 24 hours;
        vm.expectEmit(address(bridge));
        emit NacklPayoutQueued(5, RECIPIENT, amount, due);
        _pay(amount, 5);
        assertEq(nackl.totalSupply(), 0);
        assertTrue(bridge.isNullifierUsed(5));
        (address to, uint256 queuedAmount, uint256 readyAt) = bridge.nacklPayout(5);
        assertEq(to, RECIPIENT);
        assertEq(queuedAmount, amount);
        assertEq(readyAt, due);
        assertEq(_queued(bridge), 1);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, 5));
        _pay(amount, 5);

        vm.warp(due - 1);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotDue.selector, 5, due));
        bridge.executeNacklPayout(5);

        vm.warp(due);
        bridge.executeNacklPayout(5);
        assertEq(nackl.balanceOf(RECIPIENT), amount);
        (to,,) = bridge.nacklPayout(5);
        assertEq(to, address(0));
        assertEq(_queued(bridge), 0);
    }

    /// @notice DLY-3: a payout that is not queued cannot be executed, and a payout executes once.
    function test_DLY3_executeRefusals() public {
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotQueued.selector, 77));
        bridge.executeNacklPayout(77);
        _pay(THRESHOLD + 1, 5);
        vm.warp(block.timestamp + 24 hours);
        bridge.executeNacklPayout(5);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotQueued.selector, 5));
        bridge.executeNacklPayout(5);
        assertEq(nackl.totalSupply(), THRESHOLD + 1);
    }

    /// @notice DLY-4: pausing stops a queued payout during its delay and after it.
    function test_DLY4_pauseStopsExecution() public {
        _pay(THRESHOLD + 1, 5);
        bridge.pause();
        vm.warp(block.timestamp + 48 hours);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.executeNacklPayout(5);
        assertEq(nackl.totalSupply(), 0);
    }

    /// @notice DLY-5: changing the limits neither cancels nor speeds up a queued payout.
    function test_DLY5_aQueuedPayoutIsFixed() public {
        _pay(THRESHOLD + 1, 5);
        (address to, uint256 amount, uint256 readyAt) = bridge.nacklPayout(5);
        bridge.setNacklMintLimits(DAILY * 10, DAILY * 10);
        (address to2, uint256 amount2, uint256 readyAt2) = bridge.nacklPayout(5);
        assertEq(to2, to);
        assertEq(amount2, amount);
        assertEq(readyAt2, readyAt);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotDue.selector, 5, readyAt));
        bridge.executeNacklPayout(5);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Minter handover
    // ─────────────────────────────────────────────────────────────────────

    function _successor() internal returns (AckiNackiBridge s) {
        s = _liveBridge(address(bridge));
        s.listNackl(address(nackl), MAX_TRANSFER, DAILY, THRESHOLD);
    }

    /// @notice HO-1, HO-4: the successor owns and mints the token; the old bridge mints no more.
    function test_HO1_HO4_handoverMovesTheMinter() public {
        _pay(UNIT, 1);
        AckiNackiBridge s = _successor();
        bridge.pause();
        vm.expectEmit(address(bridge));
        emit NacklMinterMigrated(address(s));
        bridge.migrateNacklMinter(address(s), new uint256[](0));
        assertEq(nackl.owner(), address(s));
        (,,,, address migratedTo) = bridge.nacklState();
        assertEq(migratedTo, address(s));

        _pay(s, RECIPIENT, UNIT, 2);
        assertEq(nackl.balanceOf(RECIPIENT), 2 * UNIT);

        bridge.unpause();
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklMinterHandedOver.selector, address(s)));
        _pay(UNIT, 3);
        assertFalse(bridge.isNullifierUsed(3));
        vm.prank(address(bridge));
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, address(bridge)));
        nackl.mint(RECIPIENT, 1);
        bridge.pause();
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklMinterHandedOver.selector, address(s)));
        bridge.migrateNacklMinter(address(s), new uint256[](0));
    }

    /// @notice HO-2: a payout still queued blocks the handover; an unknown abandoned entry too.
    function test_HO2_aQueuedPayoutBlocksTheHandover() public {
        _pay(THRESHOLD + 1, 5);
        AckiNackiBridge s = _successor();
        bridge.pause();
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklQueueNotEmpty.selector, 1));
        bridge.migrateNacklMinter(address(s), new uint256[](0));
        uint256[] memory unknown = new uint256[](1);
        unknown[0] = 6;
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotQueued.selector, 6));
        bridge.migrateNacklMinter(address(s), unknown);

        // Executed before the handover, as HO-6 orders.
        bridge.unpause();
        vm.warp(block.timestamp + 24 hours);
        bridge.executeNacklPayout(5);
        bridge.pause();
        bridge.migrateNacklMinter(address(s), new uint256[](0));
        assertEq(nackl.owner(), address(s));
    }

    /// @notice HO-3: an abandoned payout is dropped with an event and never minted.
    function test_HO3_abandonedPayoutIsNeverMinted() public {
        _pay(THRESHOLD + 1, 5);
        _pay(THRESHOLD + 2, 6);
        AckiNackiBridge s = _successor();
        bridge.pause();
        uint256[] memory abandoned = new uint256[](2);
        abandoned[0] = 5;
        abandoned[1] = 6;
        vm.expectEmit(address(bridge));
        emit NacklPayoutAbandoned(5, RECIPIENT, THRESHOLD + 1);
        vm.expectEmit(address(bridge));
        emit NacklPayoutAbandoned(6, RECIPIENT, THRESHOLD + 2);
        bridge.migrateNacklMinter(address(s), abandoned);
        assertEq(_queued(bridge), 0);
        bridge.unpause();
        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklMinterHandedOver.selector, address(s)));
        bridge.executeNacklPayout(5);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklPayoutNotQueued.selector, 5));
        s.executeNacklPayout(5);
        assertEq(nackl.totalSupply(), 0);
    }

    /// @notice HO-1, HO-5: the handover checks the bridge, the successor and the result.
    function test_HO5_handoverRefusals() public {
        AckiNackiBridge s = _successor();
        vm.expectRevert(AckiNackiBridge.NotPaused.selector);
        bridge.migrateNacklMinter(address(s), new uint256[](0));
        bridge.pause();

        vm.prank(address(0xBAD));
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.migrateNacklMinter(address(s), new uint256[](0));

        AckiNackiBridge stranger = _liveBridge(address(0));
        stranger.listNackl(address(nackl), MAX_TRANSFER, DAILY, THRESHOLD);
        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.NotSuccessor.selector, address(stranger)));
        bridge.migrateNacklMinter(address(stranger), new uint256[](0));

        AckiNackiBridge otherToken = _liveBridge(address(bridge));
        otherToken.listNackl(
            address(new NacklToken(address(otherToken), address(0), 0)), MAX_TRANSFER, DAILY, THRESHOLD
        );
        vm.expectRevert(
            abi.encodeWithSelector(NacklMint.NacklSuccessorMismatch.selector, address(otherToken))
        );
        bridge.migrateNacklMinter(address(otherToken), new uint256[](0));

        AckiNackiBridge lowCap = _liveBridge(address(bridge));
        lowCap.listNackl(address(nackl), MAX_TRANSFER - 1, DAILY, THRESHOLD);
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklSuccessorMismatch.selector, address(lowCap)));
        bridge.migrateNacklMinter(address(lowCap), new uint256[](0));

        AckiNackiBridge unlisted = _liveBridge(address(bridge));
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklSuccessorMismatch.selector, address(unlisted)));
        bridge.migrateNacklMinter(address(unlisted), new uint256[](0));
        assertEq(nackl.owner(), address(bridge));
    }

    /// @notice HO-5: a token that keeps its owner fails the handover.
    function test_HO5_ownershipMustReachTheSuccessor() public {
        AckiNackiBridge old = _liveBridge(address(0));
        StickyOwnerToken sticky = new StickyOwnerToken(address(old));
        old.listNackl(address(sticky), MAX_TRANSFER, DAILY, THRESHOLD);
        AckiNackiBridge s = _liveBridge(address(old));
        s.listNackl(address(sticky), MAX_TRANSFER, DAILY, THRESHOLD);
        old.pause();
        vm.expectRevert(abi.encodeWithSelector(NacklMint.NacklSuccessorMismatch.selector, address(s)));
        old.migrateNacklMinter(address(s), new uint256[](0));
    }

    /// @notice HO-1: a bridge without NACKL listed has nothing to hand over.
    function test_HO1_unlistedBridgeHasNothingToHandOver() public {
        AckiNackiBridge old = _liveBridge(address(0));
        AckiNackiBridge s = _liveBridge(address(old));
        old.pause();
        vm.expectRevert(NacklMint.NacklNotListed.selector);
        old.migrateNacklMinter(address(s), new uint256[](0));
    }
}
