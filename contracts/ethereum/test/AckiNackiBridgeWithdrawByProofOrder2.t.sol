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
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockERC20.sol";
import "./helpers/UsdcTestLib.sol";

/// @title AckiNackiBridgeWithdrawByProofOrder2Test
/// @notice Q3 regression: order>=2 key blocks record L1 in the layer-1 window,
///         so withdrawals anchored to `layerHashes[0]` succeed.
contract AckiNackiBridgeWithdrawByProofOrder2Test is Test {
    AckiNackiBridge internal bridge;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;
    MockERC20 internal usdc;

    uint256 internal constant BK_SET = 0xBEEF;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    address internal constant RECIPIENT = address(0x2222222222222222222222222222222222222222);
    address internal funder = address(0xF00D);

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layer = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();

        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layer.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layer)),
                BK_SET,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 10 * UsdcTestLib.UNIT);
    }

    function test_withdrawOnL1Anchor_succeedsWhenNumLayersIsTwo() public {
        uint256[10] memory layers;
        layers[0] = uint256(keccak256("order2-L1-root"));
        layers[1] = uint256(keccak256("order2-L2-top"));

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked("att"),
            abi.encodePacked("lh"),
            0xC10C40001,
            BK_SET,
            1,
            2,
            layers,
            0xA10C
        );

        assertTrue(bridge.isKnownLayerAnchor(1, layers[0]), "L1 must be in layer-1 window");
        assertFalse(
            bridge.isKnownLayerAnchor(1, layers[1]),
            "L2 top must not satisfy L1-only withdraw check"
        );

        (uint256 hi, uint256 lo) = _split(RECIPIENT);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 0,
                amount: 1 * UsdcTestLib.UNIT,
                recipientHi: hi,
                recipientLo: lo,
                dstChainId: block.chainid,
                senderAccFr: 1,
                dappFr: DAPP_FR,
                accFr: ACC_FR,
                nullifier: 42,
                finalRoot: layers[0]
            });

        assertTrue(bridge.withdrawByProof(_dummyProof(), pub));
    }

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & RECIPIENT_HALF_MASK;
    }

    function _dummyProof() internal pure returns (bytes memory) {
        return hex"00";
    }
}
