// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/FeeOnTransferERC20.sol";
import "@bridge-test/mocks/ReturnlessERC20.sol";
import "@bridge-test/mocks/ReentrantERC20.sol";

/// @title DepositEdgeCasesTest
/// @notice Phase G — deposit edge cases: token quirks, boundaries, donations, reentrancy.
/// @dev INV: DEP-1..4, TR-3, TR-4
contract DepositEdgeCasesTest is Test {
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    function _bridge(address token) internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            token,
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    /// @dev DEP-2 — exact cap succeeds (#20 / QC-A1-1 closed in code).
    function test_deposit_exactMaxAmount_succeeds() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0xCAFE);
        uint256 max = bridge.MAX_DEPOSIT_AMOUNT();

        usdc.mint(user, max);
        vm.startPrank(user);
        usdc.approve(address(bridge), max);
        bridge.deposit(max, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), max);
        assertEq(usdc.balanceOf(address(bridge)), max);
    }

    /// @dev DEP-4 / reentrancy guard.
    function test_deposit_reentrantToken_reverts() public {
        ReentrantERC20 token = new ReentrantERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xBEEF);
        bytes32 anAccount = bytes32(uint256(0x1234));

        token.mint(user, UsdcTestLib.UNIT);
        token.wireReenter(bridge, anAccount);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.Reentrancy.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();
    }

    /// @dev TR-3 — direct USDC transfer is a donation; ledger unchanged (A1-F4).
    function test_directUsdcTransfer_doesNotCreditTreasury() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));

        usdc.mint(address(bridge), 50 * UsdcTestLib.UNIT);
        assertEq(bridge.treasuryBalance(), 0, "donation does not credit ledger");
        assertEq(usdc.balanceOf(address(bridge)), 50 * UsdcTestLib.UNIT, "custody still grows");
    }

    /// @dev TR-4 / ETH-11 — fee-on-transfer fails closed (custody delta must equal amount).
    function test_feeOnTransferToken_reverts() public {
        FeeOnTransferERC20 token = new FeeOnTransferERC20("FUSDC", "FUSDC", 6, 1_000);
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xA11CE);
        uint256 amount = 100 * UsdcTestLib.UNIT;

        token.mint(user, amount);
        vm.startPrank(user);
        token.approve(address(bridge), amount);
        vm.expectRevert(AckiNackiBridge.TransferAmountMismatch.selector);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 0, "ledger unchanged");
        assertEq(token.balanceOf(address(bridge)), 0, "custody unchanged on revert");
    }

    /// @dev A1-F8 — non-standard token without bool return fails closed.
    function test_returnlessToken_revertsOnDeposit() public {
        ReturnlessERC20 token = new ReturnlessERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        address user = address(0xD00D);

        token.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_deposit_insufficientAllowance_reverts() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0x1111);

        usdc.mint(user, 10 * UsdcTestLib.UNIT);
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert();
        bridge.deposit(2 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_deposit_insufficientBalance_reverts() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0x2222);

        usdc.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        usdc.approve(address(bridge), 10 * UsdcTestLib.UNIT);
        vm.expectRevert("MockERC20: balance");
        bridge.deposit(10 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    /// @dev #20 — pause removed; deposit always callable.
    function test_deposit_noPauseGate_alwaysCallable() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0x3333);

        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, UsdcTestLib.UNIT);
        assertEq(bridge.depositCounter(), 1);
    }

    /// @dev DEP-1 — event timestamp matches block time.
    function test_deposit_eventTimestamp_matchesBlock() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address user = address(0x4444);

        vm.warp(1_700_000_000);
        usdc.mint(user, UsdcTestLib.UNIT);

        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, user, UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))), block.timestamp);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    /// @dev QC-ETH-DEP-01 — `sender` in event is `msg.sender`; USDC pulled from caller only.
    function test_deposit_senderIsMsgSender_cannotPullFromThirdParty() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address relayer = address(0x5555);
        address user = address(0x6666);

        usdc.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.stopPrank();

        vm.startPrank(relayer);
        vm.expectRevert("MockERC20: allowance");
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function test_deposit_senderInEvent_isMsgSender() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        address relayer = address(0x5555);

        usdc.mint(relayer, UsdcTestLib.UNIT);
        vm.startPrank(relayer);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, relayer, UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(relayer))), block.timestamp);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(relayer))));
        vm.stopPrank();
    }
}
