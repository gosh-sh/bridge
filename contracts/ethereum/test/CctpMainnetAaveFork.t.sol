// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/IERC20.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/cctp/AnDestinationHook.sol";
import "../src/cctp/CctpDomains.sol";
import "../src/cctp/CctpEndpoint.sol";
import "../src/cctp/CctpHubReceiver.sol";
import "../src/cctp/CctpV2Message.sol";
import "./helpers/Bn254FrLib.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockPrimaryVerifier.sol";

/// @notice The attester controls of Circle's `MessageTransmitterV2`.
interface ICircleAttestable {
    function attesterManager() external view returns (address);
    function enableAttester(address attester) external;
    function setSignatureThreshold(uint256 threshold) external;
}

/// @title CctpMainnetAaveForkTest
/// @notice **Mainnet fork.** An Arbitrum USDC burn reaches the hub through
///         Circle's real `MessageTransmitterV2` and `TokenMessengerV2`, is
///         deposited, goes into the real Aave V3 USDC market, is paid out with
///         a top-up from Aave, and leaves again through a real CCTP burn.
///
///         The only stand-ins are the attestation and the AN→ETH verifiers.
///         The fork enables a test attester and drops the signature threshold
///         to one, so the transmitter verifies a message this test signs;
///         everything after `receiveMessage` is Circle's and Aave's own code.
///
/// ```bash
/// FOUNDRY_PROFILE=fork FORK_URL=<mainnet RPC> \
///   forge test --match-contract CctpMainnetAaveForkTest -vv
/// ```
contract CctpMainnetAaveForkTest is Test {
    address constant USDC = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
    address constant AAVE_V3_POOL = 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2;
    address constant AUSDC = 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c;
    address constant CIRCLE_TRANSMITTER_V2 = 0x81D40F21F12A8F0E3252Bccb954D722d4c464B64;
    address constant CIRCLE_MESSENGER_V2 = 0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d;
    address constant ARB_USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    uint256 constant ARBITRUM_CHAIN = 42_161;

    uint256 constant AMOUNT = 50e6;
    uint256 constant FAST_FEE = 130_000;
    uint256 constant BK_SET = 0xBE5E7;
    uint256 constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint8 constant ACTIVE_LAYERS = 3;

    IERC20 internal usdc = IERC20(USDC);
    AckiNackiBridge internal bridge;
    CctpHubReceiver internal receiver;
    CctpEndpoint internal hubEndpoint;
    address internal arbEndpoint = address(0xA4B1E1);
    address internal depositor = address(0xA11);
    address internal recipient = makeAddr("cctp-fork-recipient");
    bytes32 internal anAccount = keccak256("cctp-mainnet-fork");
    address internal attester;
    uint256 internal attesterKey;
    uint256 internal anchor;

    function setUp() public {
        string memory rpc = vm.envOr("FORK_URL", string(""));
        if (bytes(rpc).length == 0) {
            console.log("FORK_URL not set; skipping fork tests");
            vm.skip(true);
            return;
        }
        uint256 forkBlock = vm.envOr("FORK_BLOCK", uint256(0));
        if (forkBlock > 0) vm.createSelectFork(rpc, forkBlock);
        else vm.createSelectFork(rpc);
        require(block.chainid == 1, "fork must be Ethereum mainnet");

        bridge = _bridge();
        receiver = new CctpHubReceiver(
            address(bridge), CIRCLE_TRANSMITTER_V2, CIRCLE_MESSENGER_V2, CctpDomains.ETHEREUM
        );
        receiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(arbEndpoint),
            _b32(ARB_USDC),
            _b32(CIRCLE_MESSENGER_V2),
            true
        );
        receiver.setForwarder(address(this), true);

        hubEndpoint = new CctpEndpoint(CIRCLE_MESSENGER_V2);
        hubEndpoint.setEgressDomain(CctpDomains.ARBITRUM, true);

        (attester, attesterKey) = makeAddrAndKey("cctp-fork-attester");
        ICircleAttestable transmitter = ICircleAttestable(CIRCLE_TRANSMITTER_V2);
        vm.startPrank(transmitter.attesterManager());
        transmitter.enableAttester(attester);
        transmitter.setSignatureThreshold(1);
        vm.stopPrank();
    }

    function test_arbitrumBurn_depositSupplyPayoutAndBridgeOut() public {
        uint256 supplyBefore = _totalSupply();
        bytes memory message = _message(bytes32(uint256(1)), CctpDomains.FINALIZED, 0);

        (bytes32 key, uint256 minted) = receiver.receiveBurn(message, _attest(message));
        assertEq(minted, AMOUNT);
        assertEq(usdc.balanceOf(address(receiver)), AMOUNT);
        assertEq(_totalSupply(), supplyBefore + AMOUNT, "Circle minted real USDC");

        vm.recordLogs();
        receiver.forward(key);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 2, "forward receipt is Transfer + Deposit");
        assertEq(logs[0].emitter, USDC);
        assertEq(logs[1].emitter, address(bridge));
        assertEq(bridge.treasuryBalance(), AMOUNT);

        bridge.supplyToAave(type(uint256).max);
        uint256 liquid = (AMOUNT * bridge.liquidReserveBps()) / 10_000;
        assertEq(usdc.balanceOf(address(bridge)), liquid);
        assertApproxEqAbs(bridge.aUsdcBalance(), AMOUNT - liquid, 2);

        uint256 payout = liquid + 10e6;
        _pay(payout, 1);
        assertEq(usdc.balanceOf(recipient), payout);
        assertEq(bridge.treasuryBalance(), AMOUNT - payout);
        assertEq(usdc.balanceOf(address(bridge)), 0, "shortfall came from Aave");
        assertApproxEqAbs(bridge.aUsdcBalance(), AMOUNT - payout, 2);

        uint256 beforeOut = _totalSupply();
        vm.startPrank(recipient);
        usdc.approve(address(hubEndpoint), payout);
        hubEndpoint.bridgeOut(
            USDC, payout, CctpDomains.ARBITRUM, _b32(recipient), 0, CctpDomains.FINALIZED
        );
        vm.stopPrank();
        assertEq(usdc.balanceOf(recipient), 0);
        assertEq(_totalSupply(), beforeOut - payout, "Circle burned the egress");
    }

    function test_fastBurn_parksAmountNetOfTheExecutedFee() public {
        bytes memory message = _message(bytes32(uint256(2)), CctpDomains.FAST, FAST_FEE);
        (bytes32 key, uint256 minted) = receiver.receiveBurn(message, _attest(message));
        assertEq(minted, AMOUNT - FAST_FEE);
        (uint256 parked,,,) = receiver.pending(key);
        assertEq(parked, AMOUNT - FAST_FEE);

        receiver.forward(key);
        assertEq(bridge.treasuryBalance(), AMOUNT - FAST_FEE);
    }

    function test_replayedMessage_isRefusedByCircle() public {
        bytes memory message = _message(bytes32(uint256(3)), CctpDomains.FINALIZED, 0);
        bytes memory attestation = _attest(message);
        (bytes32 key,) = receiver.receiveBurn(message, attestation);
        receiver.forward(key);

        vm.expectRevert(bytes("Nonce already used"));
        receiver.receiveBurn(message, attestation);
    }

    function _bridge() internal returns (AckiNackiBridge b) {
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layers = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layers.setShouldAccept(true);
        withdrawal.setShouldAccept(true);
        b = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            USDC,
            AAVE_V3_POOL,
            AUSDC,
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
            hashes[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("cctp-fork-layer", i))));
        }
        anchor = hashes[0];
        b.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            ACTIVE_LAYERS,
            hashes,
            b.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    function _pay(uint256 amount, uint256 nullifier) internal {
        uint256 a = uint256(uint160(recipient));
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

    /// @dev Laid out the way the Arbitrum `TokenMessengerV2` emits a burn:
    ///      the outer sender and recipient are the two token messengers, the
    ///      body's `messageSender` is the endpoint that called it.
    function _message(bytes32 nonce, uint32 finality, uint256 fee)
        internal
        view
        returns (bytes memory)
    {
        return CctpV2Message.format(
            CctpV2Message.Relay({
                sourceDomain: CctpDomains.ARBITRUM,
                destinationDomain: CctpDomains.ETHEREUM,
                nonce: keccak256(abi.encode("cctp-fork-nonce", nonce)),
                sender: _b32(CIRCLE_MESSENGER_V2),
                recipient: _b32(CIRCLE_MESSENGER_V2),
                destinationCaller: _b32(address(receiver)),
                minFinalityThreshold: finality,
                finalityThresholdExecuted: finality,
                burnToken: _b32(ARB_USDC),
                mintRecipient: _b32(address(receiver)),
                amount: AMOUNT,
                messageSender: _b32(arbEndpoint),
                maxFee: fee,
                feeExecuted: fee,
                expirationBlock: 0,
                hookData: AnDestinationHook.encode(0, anAccount, depositor)
            })
        );
    }

    function _attest(bytes memory message) internal view returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attesterKey, keccak256(message));
        return abi.encodePacked(r, s, v);
    }

    function _totalSupply() internal view returns (uint256) {
        (, bytes memory out) = USDC.staticcall(abi.encodeWithSignature("totalSupply()"));
        return abi.decode(out, (uint256));
    }

    function _b32(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}
