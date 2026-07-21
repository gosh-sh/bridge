// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/ShplonkHalo2Verifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockERC20.sol";
import "./mocks/MockAave.sol";

/// @title EthAuditQcHardeningTest
/// @notice Regression gates for ETH QC hardenings (WD-Q2 covered in
///         AckiNackiBridgeWithdrawByProof; this file covers A2-3, A4-1, A1-3).
contract EthAuditQcHardeningTest is Test {
    AckiNackiBridge internal bridge;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockERC20 internal usdc;
    MockAavePool internal pool;
    MockAUSDC internal aUSDC;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;

    function setUp() public {
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);

        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(pool),
            address(aUSDC),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    /// @notice QC-A2-3: zero in an active layer slot must revert.
    function test_verifyBlock_activeZeroLayerHash_reverts() public {
        uint256[10] memory layers;
        layers[0] = 0;
        layers[1] = 0xA2;
        layers[2] = 0xA3;

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.LayerHashActiveZero.selector, uint256(0))
        );
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"aa",
            hex"bb",
            0x5001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @notice QC-A4-1: ShplonkHalo2Verifier rejects empty-code yul target.
    function test_shplonkHalo2Verifier_emptyCode_reverts() public {
        address eoa = makeAddr("noCode");
        vm.expectRevert(ShplonkHalo2Verifier.EmptyYulVerifierCode.selector);
        new ShplonkHalo2Verifier(eoa);
    }

    /// @notice QC-A1-3: after emergency, yield-as-liquid-USDC is skimable.
    function test_skimExcessUsdc_afterEmergency() public {
        address user = makeAddr("user");
        UsdcTestLib.depositUsdc(vm, usdc, bridge, user, 10 * UsdcTestLib.UNIT);

        bridge.supplyToAave(type(uint256).max);
        aUSDC.accrueYield(address(bridge), 500_000);
        usdc.mint(address(pool), 500_000);

        bridge.emergencyWithdrawAll();
        assertEq(bridge.suppliedPrincipal(), 0);
        assertGt(bridge.excessUsdc(), 0, "yield liquid after emergency");

        uint256 excess = bridge.excessUsdc();
        uint256 recipientBefore = usdc.balanceOf(bridge.yieldRecipient());
        bridge.skimExcessUsdc(type(uint256).max);
        assertEq(bridge.excessUsdc(), 0);
        assertEq(usdc.balanceOf(bridge.yieldRecipient()), recipientBefore + excess);
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT);
    }

    function test_skimExcessUsdc_noExcess_reverts() public {
        vm.expectRevert(AckiNackiBridge.NoExcessUsdc.selector);
        bridge.skimExcessUsdc(1);
    }
}
