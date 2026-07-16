// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

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
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsWithdrawTest
/// @notice Phase D — F-WD-1 / WD-7 nullifier uniqueness.
contract InvariantsWithdrawTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    WithdrawReplayHandler internal handler;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    uint256 internal seedAnchor;

    function setUp() public {
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

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
                IBridgeWithdrawalVerifier(address(withdrawal)), DAPP_FR, ACC_FR
            )
        );

        seedAnchor = _seedVerifyBlock();
        UsdcTestLib.depositUsdc(vm, usdc, bridge, address(0xF00D), 50 * UsdcTestLib.UNIT);

        handler = new WithdrawReplayHandler(bridge, seedAnchor, DAPP_FR, ACC_FR, RECIPIENT);
        targetContract(address(handler));
    }

    function _seedVerifyBlock() internal returns (uint256 l1) {
        uint256[10] memory layers;
        layers[0] = 0xB1;
        layers[1] = 0xB2;
        layers[2] = 0xB3;
        l1 = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"cc",
            hex"dd",
            0x9001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: WD-7 — every recorded nullifier is marked used on-chain.
    function invariant_WD7_spentNullifiersMarkedUsed() public view {
        uint256 n = handler.spentNullifiersLength();
        for (uint256 i = 0; i < n; i++) {
            assertTrue(bridge.isNullifierUsed(handler.spentNullifiers(i)), "WD-7 used");
        }
    }

    /// @dev INV: WD-7 — replay of a spent nullifier always reverts.
    function test_WD7_replaySpentNullifier_reverts() public {
        handler.withdrawOnce(1);
        require(handler.spentNullifiersLength() > 0, "need one withdrawal");

        uint256 spent = handler.spentNullifiers(0);
        uint256 tb = bridge.treasuryBalance();
        if (tb == 0) return;

        (uint256 hi, uint256 lo) = _split(RECIPIENT);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1,
            recipientHi: hi,
            recipientLo: lo,
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: spent,
            finalRoot: seedAnchor
        });

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, spent)
        );
        bridge.withdrawByProof(hex"00", pub);
    }

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & ((1 << 80) - 1);
    }
}
