// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

import "../../src/AckiNackiBridge.sol";
import { NacklToken } from "../../src/NacklToken.sol";
import { NacklMint } from "../../src/NacklMint.sol";
import "../../src/MockBlockHeaderOracle.sol";
import "../../src/IPrimaryVerifier.sol";
import "../../src/IFallbackVerifier.sol";
import "../../src/ILayerHashesMovementVerifier.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";

import "./VerifyBlockConfigLib.sol";
import "./MockAaveLib.sol";
import "./Bn254FrLib.sol";
import "../mocks/MockPrimaryVerifier.sol";
import "../mocks/MockFallbackVerifier.sol";
import "../mocks/MockLayerHashesMovementVerifier.sol";
import "../mocks/MockBridgeWithdrawalVerifier.sol";
import "../mocks/MockERC20.sol";

abstract contract NacklBridgeFixture is Test {
    AckiNackiBridge internal bridge;
    NacklToken internal nackl;
    MockERC20 internal usdc;
    MockBlockHeaderOracle internal oracle;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    uint256 internal constant NACKL_UNIT = 1e9;
    uint256 internal constant MAX_TRANSFER = 1_000 * NACKL_UNIT;
    uint256 internal constant DAILY_LIMIT = 10_000 * NACKL_UNIT;
    uint256 internal constant DELAY_THRESHOLD = 500 * NACKL_UNIT;

    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    uint256 internal seedAnchor;
    uint256 internal nextNullifier = 1;

    function setUp() public virtual {
        vm.warp(1_800_000_000);
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();
        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

        bridge = _newBridge(address(0));
        nackl = new NacklToken(address(bridge), address(0), 0);
        bridge.listNackl(nackl, MAX_TRANSFER, DAILY_LIMIT, DELAY_THRESHOLD);
        seedAnchor = _seedFirstBlock(bridge);
    }

    function _newBridge(address predecessor) internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(oracle),
            address(usdc),
            MockAaveLib.pool(address(usdc)),
            MockAaveLib.aToken(address(usdc)),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdrawAfter(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR, predecessor
            )
        );
    }

    function _seedFirstBlock(AckiNackiBridge b) internal returns (uint256 l1Anchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("nackl-seed-layer", i))));
        }
        l1Anchor = layers[0];
        b.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("nackl-seed-att")),
            abi.encodePacked(keccak256("nackl-seed-lh")),
            0xC10C40001,
            BK_SET,
            1,
            ACTIVE_LAYERS,
            layers,
            b.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    function _pub(AckiNackiBridge b, uint256 amount, uint256 nullifier, address to)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(to));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 1,
            amount: amount,
            recipientHi: a >> 80,
            recipientLo: a & RECIPIENT_HALF_MASK,
            dstChainId: block.chainid,
            senderAccFr: Bn254FrLib.toFr(uint256(keccak256("senderAcc"))),
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: seedAnchor,
            anchorLayer: 1,
            dstBridge: uint256(uint160(address(b)))
        });
    }

    function _withdraw(uint256 amount) internal returns (uint256 nullifier) {
        nullifier = nextNullifier++;
        bridge.withdrawByProof(hex"00", _pub(bridge, amount, nullifier, RECIPIENT));
    }

    function _fund(address user, uint256 amount) internal {
        vm.prank(address(bridge));
        nackl.mint(user, amount);
    }

    function _state()
        internal
        view
        returns (uint256 accepted, uint256 minted, uint256 pendingCount, uint256 payoutCount)
    {
        (,,, accepted, minted, pendingCount, payoutCount,) = bridge.nacklState();
    }

    function _windowUsed() internal view returns (uint256 used) {
        (,, used,,,,,) = bridge.nacklState();
    }
}
