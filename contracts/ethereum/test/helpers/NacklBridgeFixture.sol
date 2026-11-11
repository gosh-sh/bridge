// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";

import "../../src/AckiNackiBridge.sol";
import "../../src/MockBlockHeaderOracle.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";
import { NacklToken } from "../../src/NacklToken.sol";

import "./VerifyBlockConfigLib.sol";
import "./Bn254FrLib.sol";
import "../mocks/MockPrimaryVerifier.sol";
import "../mocks/MockFallbackVerifier.sol";
import "../mocks/MockLayerHashesMovementVerifier.sol";
import "../mocks/MockBridgeWithdrawalVerifier.sol";
import "../mocks/MockERC20.sol";
import { MockAaveLib } from "./MockAaveLib.sol";

/// @notice A bridge with one anchored block and ERC20 NACKL listed, owned by it.
abstract contract NacklBridgeFixture is Test {
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    AckiNackiBridge internal bridge;
    NacklToken internal nackl;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint32 internal constant NACKL_ID = 1;
    uint256 internal constant UNIT = 1e9;
    uint256 internal constant MAX_TRANSFER = 1_000 * UNIT;
    uint256 internal constant DAILY = 5_000 * UNIT;
    uint256 internal constant THRESHOLD = 500 * UNIT;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    uint256 internal anchor;

    function setUp() public virtual {
        // An hour boundary plus ten minutes, so window arithmetic is predictable.
        uint256 t = 1_700_000_000;
        vm.warp(t - t % 1 hours + 10 minutes);
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
        anchor = Bn254FrLib.toFr(uint256(keccak256("nackl-anchor")));

        bridge = _liveBridge(address(0));
        nackl = new NacklToken(address(bridge), address(0), 0);
        bridge.listNackl(address(nackl), MAX_TRANSFER, DAILY, THRESHOLD);
    }

    function _liveBridge(address predecessor) internal returns (AckiNackiBridge b) {
        b = new AckiNackiBridge(
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
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)),
                DAPP_FR,
                ACC_FR,
                predecessor
            )
        );
        uint256[10] memory layers;
        layers[0] = anchor;
        b.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            1,
            layers,
            b.expectedPrevAnchor(1)
        );
    }

    function _pub(AckiNackiBridge b, address to, uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(to));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: NACKL_ID,
            amount: amount,
            recipientHi: a >> 80,
            recipientLo: a & ((1 << 80) - 1),
            dstChainId: block.chainid,
            senderAccFr: 7,
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: anchor,
            anchorLayer: 1,
            dstBridge: uint256(uint160(address(b)))
        });
    }

    function _pay(AckiNackiBridge b, address to, uint256 amount, uint256 nullifier) internal {
        b.withdrawByProof(hex"c4", _pub(b, to, amount, nullifier));
    }

    function _pay(uint256 amount, uint256 nullifier) internal {
        _pay(bridge, RECIPIENT, amount, nullifier);
    }

    function _capacity(AckiNackiBridge b) internal view returns (uint256 capacity) {
        (,, capacity,,) = b.nacklState();
    }

    function _queued(AckiNackiBridge b) internal view returns (uint256 queued) {
        (,,, queued,) = b.nacklState();
    }
}
