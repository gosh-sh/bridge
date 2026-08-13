// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockBridgeWithdrawalVerifier.sol";

import "./helpers/MockUSDCRejectZeroRecipient.sol";

/// @title WithdrawRecipientZeroTest
/// @notice Phase C / A3 — WD-Q2: zero recipient fails payout on production-like USDC.
/// @dev INV: WD-5 — with FiatToken-style USDC the whole tx reverts; nullifier not consumed.
contract WithdrawRecipientZeroTest is Test {
    AckiNackiBridge internal bridge;
    MockUSDCRejectZeroRecipient internal usdc;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal seedAnchor;
    address internal funder = address(0xF00D);

    function setUp() public {
        usdc = new MockUSDCRejectZeroRecipient();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();
        withdrawalVerifier.setShouldAccept(true);

        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        usdc.mint(funder, 50 * UsdcTestLib.UNIT);
        vm.startPrank(funder);
        usdc.approve(address(bridge), 50 * UsdcTestLib.UNIT);
        bridge.deposit(50 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(funder))));
        vm.stopPrank();

        seedAnchor = _seedBlock();
    }

    function _seedBlock() internal returns (uint256 l1) {
        uint256[10] memory layers;
        layers[0] = 0xA1;
        layers[1] = 0xA2;
        layers[2] = 0xA3;
        l1 = layers[0];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"aa",
            hex"bb",
            0x6001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    function test_withdrawByProof_zeroRecipient_revertsAndNullifierUnused() public {
        uint256 nullifier = 0xDEADBEEF;
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: UsdcTestLib.UNIT,
            recipientHi: 0,
            recipientLo: 0,
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: seedAnchor
        });

        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.expectRevert(AckiNackiBridge.InvalidRecipient.selector);
        bridge.withdrawByProof(hex"00", pub);

        assertFalse(bridge.isNullifierUsed(nullifier), "WD-Q2: replay still possible");
        assertEq(bridge.treasuryBalance(), treasuryBefore, "treasury unchanged");
    }
}
