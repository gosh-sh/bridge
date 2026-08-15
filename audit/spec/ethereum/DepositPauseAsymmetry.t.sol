// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IBridgeWithdrawalVerifier.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/BlacklistableERC20.sol";

/// @title DepositPauseAsymmetryTest
/// @notice TD-58 — bridge has no `pause()` / `whenNotPaused` (#20); token pause is external (TD-24).
contract DepositPauseAsymmetryTest is Test {
    address internal user = address(0xA11CE);
    bytes32 internal anAccount = bytes32(uint256(uint160(user)));

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

    /// @dev TD-58 / #20 — deposit is not bridge-pause gated (see also `DepositEdgeCases`).
    function test_td58_deposit_no_bridge_pause_gate() public {
        MockERC20 usdc = new MockERC20("USDC", "USDC", 6);
        AckiNackiBridge bridge = _bridge(address(usdc));
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, UsdcTestLib.UNIT);
        assertEq(bridge.depositCounter(), 1);
    }

    function test_td58_verifyBlock_disabled_not_bridge_paused() public {
        AckiNackiBridge bridge = _bridge(address(new MockERC20("USDC", "USDC", 6)));
        uint256[10] memory layers;
        layers[0] = 1;
        vm.expectRevert(AckiNackiBridge.VerifyBlockDisabled.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            1,
            1,
            1,
            1,
            layers,
            0
        );
    }

    function test_td58_withdrawByProof_disabled_not_bridge_paused() public {
        AckiNackiBridge bridge = _bridge(address(new MockERC20("USDC", "USDC", 6)));
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1,
            recipientHi: 0,
            recipientLo: 1,
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: 0,
            accFr: 1,
            nullifier: 42,
            finalRoot: 1
        });
        vm.expectRevert(AckiNackiBridge.WithdrawByProofDisabled.selector);
        bridge.withdrawByProof(hex"00", pub);
    }

    /// @dev USDC token pause → `transferFrom` revert; ledger frozen (TD-24 / TD-55).
    function test_td58_token_pause_deposit_reverts_ledger_frozen() public {
        BlacklistableERC20 token = new BlacklistableERC20();
        AckiNackiBridge bridge = _bridge(address(token));
        token.mint(user, UsdcTestLib.UNIT);
        token.setPaused(true);

        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(BlacklistableERC20.Paused.selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);
        assertEq(token.balanceOf(address(bridge)), 0);
    }
}
