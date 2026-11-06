// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../../src/AckiNackiBridge.sol";
import "../../src/MockBlockHeaderOracle.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";

import "./VerifyBlockConfigLib.sol";
import "./Bn254FrLib.sol";
import "../mocks/MockAave.sol";
import "../mocks/MockERC20.sol";
import "../mocks/MockPrimaryVerifier.sol";
import "../mocks/MockFallbackVerifier.sol";
import "../mocks/MockLayerHashesMovementVerifier.sol";
import "../mocks/MockBridgeWithdrawalVerifier.sol";

/// @notice A bridge on a mock AAVE market with one anchor recorded, so that
///         `withdrawByProof` pays against the accepting mock verifier.
abstract contract AaveBridgeFixture is Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    MockAUSDC internal aUSDC;
    MockAavePool internal pool;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant UNIT = 10 ** 6;

    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    address internal yieldSink = address(0xBEEF);
    uint256 internal anchor;

    function _deployFixture() internal {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        aUSDC = new MockAUSDC();
        pool = new MockAavePool(address(usdc), address(aUSDC));

        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layers = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layers.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

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
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        uint256[10] memory hashes;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            hashes[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("aave-band-layer", i))));
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
        bridge.setYieldRecipient(yieldSink);
    }

    function _deposit(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.startPrank(who);
        usdc.approve(address(bridge), amount);
        bridge.deposit(address(usdc), amount, bytes32(uint256(uint160(who))));
        vm.stopPrank();
    }

    function _pub(uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
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
            anchorLayer: 1,
            dstBridge: uint256(uint160(address(bridge)))
        });
    }

    function _pay(uint256 amount, uint256 nullifier) internal {
        bridge.withdrawByProof(hex"00", _pub(amount, nullifier));
    }

    /// @dev `L = min(U, B − S)`.
    function _liquid() internal view returns (uint256) {
        uint256 b = bridge.treasuryBalance();
        uint256 s = bridge.suppliedPrincipal();
        uint256 owed = b > s ? b - s : 0;
        uint256 u = usdc.balanceOf(address(bridge));
        return u < owed ? u : owed;
    }

    /// @dev `T = B · r / 1e4`.
    function _target() internal view returns (uint256) {
        return (bridge.treasuryBalance() * bridge.liquidReserveBps()) / bridge.BPS_DENOMINATOR();
    }

    /// @dev `H = T · M / 1e4`.
    function _threshold() internal view returns (uint256) {
        return (_target() * bridge.supplyMultiplierBps()) / bridge.BPS_DENOMINATOR();
    }
}
