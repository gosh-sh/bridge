// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/cctp/AnDestinationHook.sol";
import "../src/cctp/CctpDomains.sol";
import "../src/cctp/CctpEndpoint.sol";
import "../src/cctp/CctpHubReceiver.sol";
import "../src/cctp/CctpV2Message.sol";
import "../src/cctp/TwoStepOwnable.sol";
import "../src/cctp/IMessageTransmitterV2.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockCctp.sol";
import "./mocks/MockERC20.sol";

/// @title CctpArbitrumUsdcTest
/// @notice Arbitrum USDC rides Circle CCTP onto a hub `AckiNackiBridge`.
///         The same contracts carry a second domain (Base) and a second token
///         (EURC) without a code change — the polymorphism is the registries.
contract CctpArbitrumUsdcTest is Test {
    uint256 internal constant ARBITRUM_CHAIN = 42_161;
    uint256 internal constant BASE_CHAIN = 8_453;
    uint256 internal constant AMOUNT = 1_000_000;
    uint256 internal constant FEE = 7_000;

    MockBlockHeaderOracle internal oracle;
    MockERC20 internal hubUsdc;
    MockERC20 internal arbUsdc;
    MockERC20 internal hubEurc;
    MockERC20 internal arbEurc;
    AckiNackiBridge internal usdcBridge;
    AckiNackiBridge internal eurcBridge;
    MockTokenMessenger internal messenger;
    MockMessageTransmitter internal transmitter;
    CctpEndpoint internal endpoint;
    CctpHubReceiver internal usdcReceiver;
    CctpHubReceiver internal eurcReceiver;

    address internal user = address(0xA11);
    bytes32 internal anAccount = bytes32(uint256(0xabc));
    int8 internal anWorkchain = -1;

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        hubUsdc = new MockERC20("Hub USDC", "USDC", 6);
        arbUsdc = new MockERC20("Arb USDC", "USDC", 6);
        hubEurc = new MockERC20("Hub EURC", "EURC", 6);
        arbEurc = new MockERC20("Arb EURC", "EURC", 6);

        usdcBridge = _bridge(hubUsdc);
        eurcBridge = _bridge(hubEurc);

        messenger = new MockTokenMessenger();
        transmitter = new MockMessageTransmitter();
        endpoint = new CctpEndpoint(address(messenger));
        usdcReceiver = new CctpHubReceiver(
            address(usdcBridge), address(transmitter), address(messenger), CctpDomains.ETHEREUM
        );
        eurcReceiver = new CctpHubReceiver(
            address(eurcBridge), address(transmitter), address(messenger), CctpDomains.ETHEREUM
        );

        endpoint.setHub(address(arbUsdc), CctpDomains.ETHEREUM, _b32(address(usdcReceiver)), true);
        endpoint.setHub(address(arbEurc), CctpDomains.ETHEREUM, _b32(address(eurcReceiver)), true);
        endpoint.setEgressDomain(CctpDomains.ARBITRUM, true);

        usdcReceiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(address(endpoint)),
            _b32(address(arbUsdc)),
            _b32(address(messenger)),
            true
        );
        usdcReceiver.setSource(
            CctpDomains.BASE,
            BASE_CHAIN,
            _b32(address(0xBACE)),
            _b32(address(arbUsdc)),
            _b32(address(messenger)),
            true
        );
        eurcReceiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(address(endpoint)),
            _b32(address(arbEurc)),
            _b32(address(messenger)),
            true
        );

        usdcReceiver.setForwarder(address(this), true);
        eurcReceiver.setForwarder(address(this), true);

        transmitter.setLocalMessenger(address(messenger));
        transmitter.setLocalToken(_b32(address(arbUsdc)), address(hubUsdc));
        transmitter.setLocalToken(_b32(address(arbEurc)), address(hubEurc));

        arbUsdc.mint(user, 10 * AMOUNT);
        arbEurc.mint(user, 10 * AMOUNT);
        hubUsdc.mint(user, 10 * AMOUNT);
        vm.prank(user);
        arbUsdc.approve(address(endpoint), type(uint256).max);
        vm.prank(user);
        arbEurc.approve(address(endpoint), type(uint256).max);
        vm.prank(user);
        hubUsdc.approve(address(endpoint), type(uint256).max);
    }

    function test_message_roundTrip_keepsHookAndFee() public view {
        bytes memory hook = AnDestinationHook.encode(-1, bytes32(uint256(0xabc)), address(0xA11));
        bytes memory message =
            _message(CctpDomains.ARBITRUM, _b32(address(0xE1)), hook, bytes32(uint256(7)), FEE);
        CctpV2Message.Parsed memory parsed = CctpV2Message.parse(message);
        assertEq(parsed.sourceDomain, CctpDomains.ARBITRUM);
        assertEq(parsed.destinationDomain, CctpDomains.ETHEREUM);
        assertEq(parsed.feeExecuted, FEE);
        assertEq(parsed.amount, AMOUNT);
        (int8 workchain, bytes32 account, address depositor) =
            AnDestinationHook.decode(parsed.hookData);
        assertEq(workchain, -1);
        assertEq(account, bytes32(uint256(0xabc)));
        assertEq(depositor, address(0xA11));
    }

    function test_parse_rejectsShortMessageAndForeignVersion() public {
        vm.expectRevert(CctpV2Message.MessageTooShort.selector);
        this.parseForTest(hex"01");

        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(0xE1)), _hook(address(0xA11)), bytes32(uint256(1)), 0
        );
        message[3] = bytes1(uint8(2));
        vm.expectRevert(
            abi.encodeWithSelector(CctpV2Message.UnsupportedVersion.selector, uint32(2))
        );
        this.parseForTest(message);
    }

    /// @dev External so `vm.expectRevert` sees a call depth. `parse` itself is internal.
    function parseForTest(bytes memory message)
        external
        pure
        returns (CctpV2Message.Parsed memory)
    {
        return CctpV2Message.parse(message);
    }

    function test_arbitrumUsdc_depositsNetOfFeeOnTheHub() public {
        vm.prank(user);
        endpoint.depositToHub(
            address(arbUsdc), AMOUNT, anWorkchain, anAccount, 0, CctpDomains.FINALIZED
        );

        assertEq(messenger.lastDestinationDomain(), CctpDomains.ETHEREUM);
        assertEq(messenger.lastMintRecipient(), _b32(address(usdcReceiver)));
        assertEq(messenger.lastDestinationCaller(), _b32(address(usdcReceiver)));
        assertEq(messenger.lastAmount(), AMOUNT);
        assertTrue(messenger.lastWithHook());
        assertEq(arbUsdc.balanceOf(address(endpoint)), 0);
        assertEq(arbUsdc.allowance(address(endpoint), address(messenger)), 0);
        (int8 workchain, bytes32 account, address depositor) =
            AnDestinationHook.decode(messenger.lastHook());
        assertEq(workchain, anWorkchain);
        assertEq(account, anAccount);
        assertEq(depositor, user);

        bytes memory message = _message(
            CctpDomains.ARBITRUM,
            _b32(address(endpoint)),
            messenger.lastHook(),
            bytes32(uint256(1)),
            FEE
        );

        vm.recordLogs();
        (bytes32 key, uint256 minted) = usdcReceiver.receiveBurn(message, "");
        Vm.Log[] memory received = vm.getRecordedLogs();

        assertEq(key, usdcReceiver.messageKey(CctpDomains.ARBITRUM, bytes32(uint256(1))));
        assertEq(minted, AMOUNT - FEE);
        assertEq(usdcBridge.depositCounter(), 0);
        assertEq(usdcReceiver.totalPending(), AMOUNT - FEE);
        assertEq(hubUsdc.balanceOf(address(usdcReceiver)), AMOUNT - FEE);
        _assertReceived(
            received, key, CctpDomains.ARBITRUM, ARBITRUM_CHAIN, user, AMOUNT - FEE, FEE
        );

        vm.recordLogs();
        usdcReceiver.forward(key);
        Vm.Log[] memory forwarded = vm.getRecordedLogs();

        // MockERC20 emits nothing; a real token adds only its `Transfer`.
        assertEq(forwarded.length, 1, "forward emits Deposit and no Approval");
        _assertDeposit(forwarded, address(usdcReceiver), AMOUNT - FEE);
        assertEq(usdcBridge.treasuryBalance(), AMOUNT - FEE);
        assertEq(usdcBridge.depositCounter(), 1);
        assertEq(hubUsdc.balanceOf(address(usdcBridge)), AMOUNT - FEE);
        assertEq(hubUsdc.balanceOf(address(usdcReceiver)), 0);
        assertEq(usdcReceiver.totalPending(), 0);
        assertEq(eurcBridge.treasuryBalance(), 0);

        vm.expectRevert(abi.encodeWithSelector(CctpHubReceiver.NotPending.selector, key));
        usdcReceiver.forward(key);
    }

    function test_forward_isForwarderOnly() public {
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(11)), 0
        );
        (bytes32 key,) = usdcReceiver.receiveBurn(message, "");

        vm.prank(address(0xBAD));
        vm.expectRevert(CctpHubReceiver.NotForwarder.selector);
        usdcReceiver.forward(key);

        usdcReceiver.setForwarder(address(this), false);
        vm.prank(user);
        vm.expectRevert(CctpHubReceiver.NotForwarder.selector);
        usdcReceiver.forward(key);

        usdcReceiver.setForwarder(address(this), true);
        usdcReceiver.forward(key);
        assertEq(usdcBridge.treasuryBalance(), AMOUNT);
    }

    function test_receiveBurn_requiresTokenMessengerRecipientAndSender() public {
        bytes memory generic = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(21)), 0
        );
        // Outer recipient sits at byte 76. Point it at this receiver instead
        // of TokenMessengerV2: a permissionless sendMessage body.
        bytes memory who = abi.encodePacked(_b32(address(usdcReceiver)));
        for (uint256 i = 0; i < 32; i++) {
            generic[76 + i] = who[i];
        }
        hubUsdc.mint(address(usdcReceiver), AMOUNT);
        vm.expectRevert(CctpHubReceiver.UnexpectedTokenMessenger.selector);
        usdcReceiver.receiveBurn(generic, "");
        assertEq(transmitter.calls(), 0);
        assertEq(usdcReceiver.totalPending(), 0);

        bytes memory wrongSender = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(22)), 0
        );
        bytes memory other = abi.encodePacked(_b32(address(0xBAD5)));
        for (uint256 i = 0; i < 32; i++) {
            wrongSender[44 + i] = other[i];
        }
        vm.expectRevert(CctpHubReceiver.UnexpectedSender.selector);
        usdcReceiver.receiveBurn(wrongSender, "");
        assertEq(transmitter.calls(), 0);
    }

    function test_rescue_cannotTakeParkedDeposits() public {
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(12)), 0
        );
        (bytes32 key,) = usdcReceiver.receiveBurn(message, "");
        hubUsdc.mint(address(usdcReceiver), 50);

        vm.expectRevert(abi.encodeWithSelector(CctpHubReceiver.PendingFunds.selector, 50));
        usdcReceiver.rescue(address(hubUsdc), user, 51);

        usdcReceiver.rescue(address(hubUsdc), user, 50);
        usdcReceiver.forward(key);
        assertEq(usdcBridge.treasuryBalance(), AMOUNT);
    }

    function test_baseSource_usesTheSameReceiver() public {
        bytes memory hook = _hook(address(0xB0B));
        bytes memory message =
            _messageFrom(CctpDomains.BASE, _b32(address(0xBACE)), hook, bytes32(uint256(2)), 0);
        uint256 minted = _receiveAndForward(usdcReceiver, message);
        assertEq(minted, AMOUNT);
        assertEq(usdcBridge.treasuryBalance(), AMOUNT);
    }

    function test_swappedAdapter_doesNotMint() public {
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(0xBACE)), _hook(user), bytes32(uint256(3)), 0
        );
        vm.expectRevert(CctpHubReceiver.UnexpectedAdapter.selector);
        usdcReceiver.receiveBurn(message, "");
        assertEq(transmitter.calls(), 0);
        assertEq(hubUsdc.balanceOf(address(usdcBridge)), 0);
    }

    function test_eurc_doesNotMoveUsdc() public {
        vm.prank(user);
        endpoint.depositToHub(address(arbEurc), AMOUNT, anWorkchain, anAccount, 0, CctpDomains.FAST);

        bytes memory message = CctpV2Message.format(
            CctpV2Message.Relay({
                sourceDomain: CctpDomains.ARBITRUM,
                destinationDomain: CctpDomains.ETHEREUM,
                nonce: bytes32(uint256(4)),
                sender: _b32(address(messenger)),
                recipient: _b32(address(messenger)),
                destinationCaller: _b32(address(eurcReceiver)),
                minFinalityThreshold: CctpDomains.FAST,
                finalityThresholdExecuted: CctpDomains.FAST,
                burnToken: _b32(address(arbEurc)),
                mintRecipient: _b32(address(eurcReceiver)),
                amount: AMOUNT,
                messageSender: _b32(address(endpoint)),
                maxFee: 0,
                feeExecuted: 0,
                expirationBlock: 0,
                hookData: messenger.lastHook()
            })
        );
        _receiveAndForward(eurcReceiver, message);
        assertEq(eurcBridge.treasuryBalance(), AMOUNT);
        assertEq(hubEurc.balanceOf(address(eurcBridge)), AMOUNT);
        assertEq(usdcBridge.treasuryBalance(), 0);
        assertEq(hubUsdc.balanceOf(address(usdcBridge)), 0);
    }

    function test_bridgeOut_toArbitrum_hasNoHook() public {
        bytes32 arbRecipient = _b32(address(0xA9B));
        vm.prank(user);
        endpoint.bridgeOut(
            address(hubUsdc), AMOUNT, CctpDomains.ARBITRUM, arbRecipient, 0, CctpDomains.FINALIZED
        );
        assertFalse(messenger.lastWithHook());
        assertEq(messenger.lastHook().length, 0);
        assertEq(messenger.lastDestinationDomain(), CctpDomains.ARBITRUM);
        assertEq(messenger.lastMintRecipient(), arbRecipient);
        assertEq(messenger.lastDestinationCaller(), bytes32(0));
        assertEq(hubUsdc.balanceOf(address(endpoint)), 0);
    }

    function test_bridgeOut_rejectsUnknownDomain() public {
        vm.prank(user);
        vm.expectRevert(
            abi.encodeWithSelector(CctpEndpoint.EgressDisabled.selector, CctpDomains.BASE)
        );
        endpoint.bridgeOut(
            address(hubUsdc), AMOUNT, CctpDomains.BASE, _b32(address(0x1)), 0, CctpDomains.FINALIZED
        );
    }

    function test_receive_rejectsForeignCallerAndWrongFee() public {
        bytes memory unlocked = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(5)), 0
        );
        // destinationCaller sits at byte 108. Zero it.
        for (uint256 i = 108; i < 140; i++) {
            unlocked[i] = 0;
        }
        vm.expectRevert(CctpHubReceiver.UnexpectedCaller.selector);
        usdcReceiver.receiveBurn(unlocked, "");
        assertEq(transmitter.calls(), 0);

        transmitter.setMintDelta(1);
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(6)), FEE
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                CctpHubReceiver.MintMismatch.selector, AMOUNT - FEE, AMOUNT - FEE + 1
            )
        );
        usdcReceiver.receiveBurn(message, "");
        assertEq(hubUsdc.balanceOf(address(usdcReceiver)), 0);
        assertEq(usdcReceiver.totalPending(), 0);

        transmitter.setMintDelta(-1);
        bytes memory short = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(23)), 0
        );
        vm.expectRevert(
            abi.encodeWithSelector(CctpHubReceiver.MintMismatch.selector, AMOUNT, AMOUNT - 1)
        );
        usdcReceiver.receiveBurn(short, "");
        transmitter.setMintDelta(0);
    }

    function test_ownerGatesAndDisabledSource() public {
        vm.prank(user);
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        usdcReceiver.setForwarder(user, true);

        vm.prank(user);
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        usdcReceiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(address(endpoint)),
            _b32(address(arbUsdc)),
            _b32(address(messenger)),
            true
        );

        usdcReceiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(address(endpoint)),
            _b32(address(arbUsdc)),
            _b32(address(messenger)),
            false
        );
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(24)), 0
        );
        vm.expectRevert(
            abi.encodeWithSelector(CctpHubReceiver.SourceDisabled.selector, CctpDomains.ARBITRUM)
        );
        usdcReceiver.receiveBurn(message, "");
        assertEq(transmitter.calls(), 0);
    }

    function test_receive_replayReverts() public {
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(8)), 0
        );
        (bytes32 key,) = usdcReceiver.receiveBurn(message, "");
        vm.expectRevert(abi.encodeWithSelector(CctpHubReceiver.AlreadyPending.selector, key));
        usdcReceiver.receiveBurn(message, "");

        usdcReceiver.forward(key);
        vm.expectRevert(MockMessageTransmitter.NonceUsed.selector);
        usdcReceiver.receiveBurn(message, "");
        assertEq(usdcBridge.depositCounter(), 1);
    }

    function test_receive_reentrancyReverts() public {
        ReenteringTransmitter evil = new ReenteringTransmitter();
        CctpHubReceiver trapped = new CctpHubReceiver(
            address(usdcBridge), address(evil), address(messenger), CctpDomains.ETHEREUM
        );
        evil.arm(trapped);
        trapped.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_CHAIN,
            _b32(address(endpoint)),
            _b32(address(arbUsdc)),
            _b32(address(messenger)),
            true
        );
        bytes memory message = _message(
            CctpDomains.ARBITRUM, _b32(address(endpoint)), _hook(user), bytes32(uint256(9)), 0
        );
        // The crafted message names `usdcReceiver` as destinationCaller. Point it at `trapped`.
        bytes memory aimed = _retarget(message, address(trapped));
        vm.expectRevert(ReentrancyGuard.Reentrancy.selector);
        trapped.receiveBurn(aimed, "");
    }

    function test_hook_rejectsUnknownVersion() public {
        bytes memory hook = abi.encode(uint8(2), anWorkchain, anAccount, user);
        bytes memory message =
            _message(CctpDomains.ARBITRUM, _b32(address(endpoint)), hook, bytes32(uint256(10)), 0);
        vm.expectRevert(abi.encodeWithSelector(AnDestinationHook.BadHookVersion.selector, uint8(2)));
        usdcReceiver.receiveBurn(message, "");
        assertEq(transmitter.calls(), 0);
    }

    function test_depositToHub_rejectsUnroutedTokenAndFatFee() public {
        vm.prank(user);
        vm.expectRevert(
            abi.encodeWithSelector(CctpEndpoint.HubNotConfigured.selector, address(hubUsdc))
        );
        endpoint.depositToHub(
            address(hubUsdc), AMOUNT, anWorkchain, anAccount, 0, CctpDomains.FINALIZED
        );

        uint256 beforeBal = arbUsdc.balanceOf(user);
        vm.prank(user);
        vm.expectRevert(CctpEndpoint.FeeNotBelowAmount.selector);
        endpoint.depositToHub(
            address(arbUsdc), AMOUNT, anWorkchain, anAccount, AMOUNT, CctpDomains.FAST
        );
        assertEq(arbUsdc.balanceOf(user), beforeBal);
    }

    function test_rescue_isOwnerOnly() public {
        hubUsdc.mint(address(usdcReceiver), 50);
        vm.prank(user);
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        usdcReceiver.rescue(address(hubUsdc), user, 50);

        usdcReceiver.rescue(address(hubUsdc), user, 50);
        assertEq(hubUsdc.balanceOf(user), 10 * AMOUNT + 50);
        assertEq(hubUsdc.balanceOf(address(usdcReceiver)), 0);
    }

    function test_ownership_isTwoStep() public {
        endpoint.transferOwnership(user);
        assertEq(endpoint.owner(), address(this));
        vm.prank(user);
        endpoint.acceptOwnership();
        assertEq(endpoint.owner(), user);
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        endpoint.setEgressDomain(CctpDomains.BASE, true);
    }

    function _receiveAndForward(CctpHubReceiver receiver, bytes memory message)
        internal
        returns (uint256 minted)
    {
        bytes32 key;
        (key, minted) = receiver.receiveBurn(message, "");
        receiver.forward(key);
    }

    function _bridge(MockERC20 token) internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(oracle),
            address(token),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function _hook(address depositor) internal view returns (bytes memory) {
        return AnDestinationHook.encode(anWorkchain, anAccount, depositor);
    }

    function _message(
        uint32 sourceDomain,
        bytes32 adapter,
        bytes memory hook,
        bytes32 nonce,
        uint256 fee
    ) internal view returns (bytes memory) {
        return _messageFrom(sourceDomain, adapter, hook, nonce, fee);
    }

    function _messageFrom(
        uint32 sourceDomain,
        bytes32 adapter,
        bytes memory hook,
        bytes32 nonce,
        uint256 fee
    ) internal view returns (bytes memory) {
        bytes32 remote = _b32(address(arbUsdc));
        bytes32 receiver = _b32(address(usdcReceiver));
        return CctpV2Message.format(
            CctpV2Message.Relay({
                sourceDomain: sourceDomain,
                destinationDomain: CctpDomains.ETHEREUM,
                nonce: nonce,
                sender: _b32(address(messenger)),
                recipient: _b32(address(messenger)),
                destinationCaller: receiver,
                minFinalityThreshold: CctpDomains.FINALIZED,
                finalityThresholdExecuted: CctpDomains.FINALIZED,
                burnToken: remote,
                mintRecipient: receiver,
                amount: AMOUNT,
                messageSender: adapter,
                maxFee: fee,
                feeExecuted: fee,
                expirationBlock: 0,
                hookData: hook
            })
        );
    }

    function _retarget(bytes memory message, address receiver)
        internal
        pure
        returns (bytes memory)
    {
        bytes memory who = abi.encodePacked(_b32(receiver));
        // destinationCaller at 108, mintRecipient at BODY_OFFSET + 36 = 184.
        for (uint256 i = 0; i < 32; i++) {
            message[108 + i] = who[i];
            message[184 + i] = who[i];
        }
        return message;
    }

    function _assertDeposit(Vm.Log[] memory logs, address sender, uint256 amount) internal pure {
        bytes32 topic = keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)");
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != topic) continue;
            assertEq(logs[i].topics[2], bytes32(uint256(uint160(sender))));
            (uint256 loggedAmount, int8 workchain, bytes32 account,) =
                abi.decode(logs[i].data, (uint256, int8, bytes32, uint256));
            assertEq(loggedAmount, amount);
            assertEq(workchain, int8(-1));
            assertEq(account, bytes32(uint256(0xabc)));
            return;
        }
        revert("Deposit log missing");
    }

    function _assertReceived(
        Vm.Log[] memory logs,
        bytes32 key,
        uint32 domain,
        uint256 chainId,
        address depositor,
        uint256 amount,
        uint256 fee
    ) internal pure {
        bytes32 topic = keccak256(
            "RemoteDepositReceived(bytes32,uint32,uint256,address,bytes32,uint256,uint256)"
        );
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != topic) continue;
            assertEq(logs[i].topics[1], key);
            assertEq(uint32(uint256(logs[i].topics[2])), domain);
            assertEq(uint256(logs[i].topics[3]), chainId);
            (address loggedDepositor, bytes32 account, uint256 loggedAmount, uint256 loggedFee) =
                abi.decode(logs[i].data, (address, bytes32, uint256, uint256));
            assertEq(loggedDepositor, depositor);
            assertEq(account, bytes32(uint256(0xabc)));
            assertEq(loggedAmount, amount);
            assertEq(loggedFee, fee);
            return;
        }
        revert("RemoteDepositReceived log missing");
    }

    function _b32(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}

/// @notice Re-enters `receiveBurn` from inside `receiveMessage`.
contract ReenteringTransmitter is IMessageTransmitterV2 {
    CctpHubReceiver public receiver;
    bool public entered;

    function arm(CctpHubReceiver receiver_) external {
        receiver = receiver_;
    }

    function receiveMessage(bytes calldata message, bytes calldata attestation)
        external
        returns (bool)
    {
        if (!entered) {
            entered = true;
            receiver.receiveBurn(message, attestation);
        }
        return true;
    }
}
