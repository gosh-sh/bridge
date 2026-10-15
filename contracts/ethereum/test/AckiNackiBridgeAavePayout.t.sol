// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";
import "./mocks/MockAave.sol";
import "./mocks/MockERC20.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";

/// @title AckiNackiBridgeAavePayoutTest
/// @notice `withdrawByProof` topping up from AAVE when the book and the
///         aToken balance disagree (ETH-28).
contract AckiNackiBridgeAavePayoutTest is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant UNIT = UsdcTestLib.UNIT;

    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    address internal funder = address(0xF00D);
    address internal yieldSink = address(0xBEEF);
    uint256 internal anchor;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layers = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layers.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layers)),
                BK_SET,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawal)), DAPP_FR, ACC_FR
            )
        );

        uint256[10] memory hashes;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            hashes[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("aave-payout-layer", i))));
        }
        anchor = hashes[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            ACTIVE_LAYERS,
            hashes,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 10 * UNIT);
        bridge.supplyToAave(type(uint256).max); // 1 liquid, 9 in AAVE
        bridge.setYieldRecipient(yieldSink);
    }

    function _pay(uint256 amount, uint256 nullifier) internal {
        uint256 a = uint256(uint160(RECIPIENT));
        bridge.withdrawByProof(
            hex"00",
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 0,
                amount: amount,
                recipientHi: a >> 80,
                recipientLo: a & ((1 << 80) - 1),
                dstChainId: block.chainid,
                senderAccFr: Bn254FrLib.toFr(uint256(keccak256("senderAcc"))),
                dappFr: DAPP_FR,
                accFr: ACC_FR,
                nullifier: nullifier,
                finalRoot: anchor,
                anchorLayer: 1
            })
        );
    }

    function _lose(uint256 amount) internal {
        vm.prank(address(bridge));
        aUSDC.transfer(address(0xdead), amount);
    }

    function _accrue(uint256 amount) internal {
        aUSDC.accrueYield(address(bridge), amount);
        usdc.mint(address(pool), amount);
    }

    /// @dev Loss, write-off, interest refills the hole, owner harvests the
    ///      honest surplus. The last payout needs aUSDC above the written-down
    ///      book and must still get it.
    function test_lastPayoutReachesInterestAboveWrittenOffBook() public {
        _lose(2 * UNIT);
        bridge.writeOffUnbackedPrincipal(); // book 7
        _accrue(3 * UNIT); // aUSDC 10
        bridge.harvestYield(1 * UNIT); // aUSDC 9, backing == treasury

        assertEq(bridge.suppliedPrincipal(), 7 * UNIT);
        assertEq(bridge.aUsdcBalance(), 9 * UNIT);

        _pay(10 * UNIT, 1);

        assertEq(usdc.balanceOf(RECIPIENT), 10 * UNIT);
        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.suppliedPrincipal(), 0, "book floors at zero");
        assertEq(bridge.aUsdcBalance(), 0);
    }

    /// @dev Book above an empty pool: the bridge is short and the payout that
    ///      needs AAVE reverts; once the gap is refilled in liquid USDC it
    ///      pays without touching AAVE.
    function test_phantomBookPayoutRevertsUntilLiquidCovers() public {
        pool.setRedeemHaircut(2 * UNIT);
        bridge.emergencyWithdrawAll(); // 7 home: liquid 8, book 2, treasury 10
        assertEq(bridge.aUsdcBalance(), 0);
        assertEq(bridge.suppliedPrincipal(), 2 * UNIT);

        vm.expectRevert(AckiNackiBridge.InsufficientTreasury.selector);
        _pay(10 * UNIT, 1);

        usdc.mint(address(bridge), 2 * UNIT);
        _pay(10 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 10 * UNIT);
    }

    /// @dev Under-booked position, no write-off: the payout pulls the exact
    ///      shortfall and the book drops by the same amount.
    function test_payoutPullsExactShortfall() public {
        _pay(4 * UNIT, 1);
        assertEq(usdc.balanceOf(RECIPIENT), 4 * UNIT);
        assertEq(bridge.suppliedPrincipal(), 6 * UNIT);
        assertEq(bridge.aUsdcBalance(), 6 * UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 0);
    }
}
