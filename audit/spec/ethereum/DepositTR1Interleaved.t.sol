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
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title DepositTR1InterleavedTest
/// @notice TD-14 — TR-1 solvency under interleaved deposit / AAVE / emergency / skim / donation.
/// @dev INV: TR-1 ghost ledger `liquid + principal >= treasuryBalance`.
contract DepositTR1InterleavedTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;
    TreasuryHandler internal handler;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    uint256 internal seedAnchor;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

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
            address(pool),
            address(aUSDC),
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

        handler = new TreasuryHandler(
            bridge,
            usdc,
            aUSDC,
            pool,
            address(this),
            seedAnchor,
            DAPP_FR,
            ACC_FR,
            RECIPIENT,
            50 * UsdcTestLib.UNIT
        );

        targetContract(address(handler));
    }

    function _seedVerifyBlock() internal returns (uint256 l1) {
        uint256[10] memory layers;
        layers[0] = 0xA1;
        layers[1] = 0xA2;
        layers[2] = 0xA3;
        l1 = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"aa",
            hex"bb",
            0x8001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: TR-1 — custody covers ledger under full handler interleaving.
    function invariant_TR1_solvency_interleaved() public view {
        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 principal = bridge.suppliedPrincipal();
        assertGe(liquid + principal, bridge.treasuryBalance(), "TD-14 TR-1 solvency");
    }

    /// @dev Ghost ledger cross-check (TR-2) while donation/skim paths run.
    function invariant_TR2_ghostTreasuryConservation() public view {
        assertEq(
            bridge.treasuryBalance(),
            handler.ghostDeposited() - handler.ghostWithdrawn(),
            "TD-14 TR-2 ghost ledger"
        );
    }

    /// @dev TR-3 cross-check — direct transfer is donation; ledger unchanged.
    function test_TD14_directDonation_doesNotIncreaseTreasuryBalance() public {
        uint256 beforeTreasury = bridge.treasuryBalance();
        uint256 beforeLiquid = usdc.balanceOf(address(bridge));
        uint256 donation = 25 * UsdcTestLib.UNIT;

        usdc.mint(address(this), donation);
        usdc.transfer(address(bridge), donation);

        assertEq(bridge.treasuryBalance(), beforeTreasury, "donation must not credit treasuryBalance");
        assertEq(usdc.balanceOf(address(bridge)), beforeLiquid + donation, "custody grows");
        assertGe(
            usdc.balanceOf(address(bridge)) + bridge.suppliedPrincipal(),
            bridge.treasuryBalance(),
            "TR-1 holds after donation"
        );
    }

    /// @dev Smoke — one scripted interleaved path exercises new handler selectors.
    function test_TD14_smoke_interleaved_handler_path() public {
        handler.deposit(UsdcTestLib.UNIT);
        handler.supplyToAaveMax();
        handler.accrueAaveYield(100_000);
        handler.harvestAllYield();
        handler.donateDirectUsdc(UsdcTestLib.UNIT);
        handler.emergencyWithdrawAll();
        handler.skimExcessUsdcMax();

        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 principal = bridge.suppliedPrincipal();
        assertGe(liquid + principal, bridge.treasuryBalance(), "TR-1 after interleaved smoke");
    }
}
