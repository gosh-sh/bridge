// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeEventVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeEventVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";

/// @title AckiNackiBridgeWithdrawByProofTest
/// @notice Phase B Circuit 4 v2 (Bridge Withdrawal Prove) tests.
///
/// Drives the `withdrawByProof` entry point — the AN→ETH payout path that
/// retires the legacy refund-style `withdraw` (demolished in Phase 4.3) and
/// supersedes Phase A's attestation-only `verifyEvent`. Uses a mock
/// `IBridgeWithdrawalVerifier` because the real gnark wrap for the partner's
/// Circuit 4 v2 doesn't exist yet — see
/// `docs/an_partner_questions_circuit4_2026-05-17.md` and the 110-Fr layout in
/// `docs/an_partner_circuit4_alina_replies_2026-05-21.md`.
///
/// Coverage:
///
/// 1. **Constructor wiring**: Phase B enabled / disabled toggles; identity
///    inheritance from Phase A (`BridgeEventConfig`); revert if identity slots
///    aren't set when Phase B is enabled.
/// 2. **Happy path**: a verified proof transfers `amount` ETH to the
///    reconstructed `recipient`, marks the nullifier used, emits
///    `WithdrawalByProofExecuted`, decrements `treasuryBalance`.
/// 3. **Replay protection**: re-submitting the same nullifier reverts with
///    `NullifierAlreadyUsed`. The `isNullifierUsed` view reports correctly.
/// 4. **Identity mismatch**: a proof carrying mismatching `(dappFr, accFr)`
///    reverts with `WithdrawIdentityMismatch` *before* the verifier is
///    invoked (cheap path).
/// 5. **dstChainId mismatch**: a proof for a different chain reverts with
///    `DstChainIdMismatch(supplied, expected)`.
/// 6. **Recipient split validation**: `recipientHi` or `recipientLo`
///    exceeding 80 bits reverts with `RecipientHalfOutOfRange`. Valid splits
///    round-trip cleanly through `_reconstructRecipient`.
/// 7. **Treasury shortfall**: amount > treasuryBalance reverts with
///    `WithdrawTreasuryShortfall`.
/// 8. **Unsupported tokenId**: any `tokenId != 0` reverts (Phase B is
///    native-ETH-only).
/// 9. **Mock crypto rejection**: the underlying verifier returning `false`
///    reverts with `WithdrawalProofRejected` and leaves state untouched.
/// 10. **`layerHashes` are forwarded from on-chain `_layerWindow`**, not from
///     the caller — proven via the mock's strict-mode hash matcher.
/// 11. **Public inputs are forwarded byte-for-byte** to the verifier —
///     proven via the mock's strict-pub matcher.
contract AckiNackiBridgeWithdrawByProofTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeEventVerifier internal bridgeEventVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xC10C40001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal constant LAYER_WINDOW_SIZE = 100;
    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    /// @dev Foundry's default `block.chainid` in unit tests is 31337 (Anvil).
    uint256 internal constant DEFAULT_CHAIN_ID = 31337;

    /// @dev Sample recipient — 0x1111...1111
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    /// @dev Sample funder for deposits.
    address internal funder = address(0xF00D);

    // Re-declared from AckiNackiBridge for `vm.expectEmit`.
    event WithdrawalByProofExecuted(
        uint256 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 indexed tokenId,
        address submitter
    );

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        bridgeEventVerifier = new MockBridgeEventVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        bridgeEventVerifier.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withBridgeEvent(
                IBridgeEventVerifier(address(bridgeEventVerifier)), DAPP_FR, ACC_FR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier))
            )
        );

        // Seed the bridge with a healthy treasury so withdrawals don't shortfall.
        vm.deal(funder, 1000 ether);
        vm.prank(funder);
        bridge.deposit{ value: 50 ether }();
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    /// @dev Pack a 20-byte EVM address into split-α (hi=top 10 bytes, lo=bottom 10).
    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & RECIPIENT_HALF_MASK;
    }

    /// @dev Default `WithdrawalPublicInputs` for happy-path tests:
    ///      - bridge identity (DAPP_FR / ACC_FR)
    ///      - dstChainId = current chainid
    ///      - tokenId = 0 (native ETH)
    ///      - recipient split-α from `RECIPIENT`
    ///      - amount = 1 ether
    ///      - nullifier = caller-controlled (so callers can vary it for replay tests)
    function _defaultPub(uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        (uint256 hi, uint256 lo) = _split(RECIPIENT);
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: amount,
            recipientHi: hi,
            recipientLo: lo,
            dstChainId: block.chainid,
            senderDappFr: uint256(keccak256("senderDapp")),
            senderAccFr: uint256(keccak256("senderAcc")),
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier
        });
    }

    function _dummyProof() internal pure returns (bytes memory) {
        bytes memory p = new bytes(256);
        for (uint256 i = 0; i < 256; i++) {
            p[i] = bytes1(uint8(i));
        }
        return p;
    }

    // ─────────────────────────────────────────────────────────────────────
    // Constructor wiring
    // ─────────────────────────────────────────────────────────────────────

    function test_constructor_withdrawDisabledByDefault() public {
        AckiNackiBridge plain = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        assertEq(address(plain.bridgeWithdrawalVerifier()), address(0));
    }

    function test_constructor_withdrawEnabled_storesVerifier() public view {
        assertEq(address(bridge.bridgeWithdrawalVerifier()), address(withdrawalVerifier));
    }

    function test_constructor_withdrawEnabledWithoutIdentity_reverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidBridgeEventIdentity.selector);
        new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent(),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier))
            )
        );
    }

    function test_constructor_withdrawEnabledWithIdentityOnly_isLegal() public {
        // The Phase A verifier is zeroed but identity slots are set →
        // Phase B should construct cleanly and Phase A's `verifyEvent`
        // should revert with VerifyEventDisabled.
        AckiNackiBridge bareB = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.bridgeEventIdentityOnly(DAPP_FR, ACC_FR),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier))
            )
        );
        assertEq(address(bareB.bridgeEventVerifier()), address(0));
        assertEq(bareB.bridgeEventDappFr(), DAPP_FR);
        assertEq(bareB.bridgeEventAccFr(), ACC_FR);
        assertEq(address(bareB.bridgeWithdrawalVerifier()), address(withdrawalVerifier));

        vm.expectRevert(AckiNackiBridge.VerifyEventDisabled.selector);
        bareB.verifyEvent(_dummyProof(), 0);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Happy path
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_happyPath_transfersAndMarksNullifier() public {
        uint256 amount = 2 ether;
        uint256 nullifier = uint256(keccak256("nul-1"));
        uint256 recipientBalBefore = RECIPIENT.balance;
        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.expectEmit(true, true, true, true);
        emit WithdrawalByProofExecuted(nullifier, RECIPIENT, amount, 0, address(this));

        bool ok = bridge.withdrawByProof(_dummyProof(), _defaultPub(amount, nullifier));
        assertTrue(ok);

        assertEq(RECIPIENT.balance, recipientBalBefore + amount, "recipient credited");
        assertEq(bridge.treasuryBalance(), treasuryBefore - amount, "treasury decremented");
        assertTrue(bridge.isNullifierUsed(nullifier), "nullifier marked");
    }

    function test_withdrawByProof_disabled_reverts() public {
        // A fresh bridge without Phase B wiring.
        AckiNackiBridge bareA = new AckiNackiBridge(
            address(oracle),
            address(0),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledBridgeEvent(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        vm.expectRevert(AckiNackiBridge.WithdrawByProofDisabled.selector);
        bareA.withdrawByProof(_dummyProof(), _defaultPub(1 ether, 1));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Replay protection
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_replayRejected() public {
        uint256 nullifier = uint256(keccak256("replay"));

        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 ether, nullifier));

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, nullifier)
        );
        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 ether, nullifier));
    }

    function test_isNullifierUsed_initiallyFalse() public view {
        assertFalse(bridge.isNullifierUsed(0xDEAD_BEEF));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Identity / chain / token validation
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_wrongDappFr_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("idmismatch1")));
        pub.dappFr = uint256(keccak256("evilDapp"));

        vm.expectRevert(AckiNackiBridge.WithdrawIdentityMismatch.selector);
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_wrongAccFr_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("idmismatch2")));
        pub.accFr = uint256(keccak256("evilAcc"));

        vm.expectRevert(AckiNackiBridge.WithdrawIdentityMismatch.selector);
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_wrongDstChainId_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("chainmismatch")));
        uint256 wrong = pub.dstChainId + 1;
        pub.dstChainId = wrong;

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.DstChainIdMismatch.selector, wrong, block.chainid
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_unsupportedTokenId_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("token")));
        pub.tokenId = 1;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, 1));
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Recipient split validation + reconstruction
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_recipientHiOutOfRange_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("hiOOB")));
        pub.recipientHi = uint256(1) << 80; // exactly 1 bit too wide

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.RecipientHalfOutOfRange.selector, pub.recipientHi
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_recipientLoOutOfRange_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("loOOB")));
        pub.recipientLo = uint256(1) << 80;

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.RecipientHalfOutOfRange.selector, pub.recipientLo
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_recipientReconstructsCorrectly() public {
        // Pick a deliberately ugly address with non-trivial bits in both halves.
        address weird = address(0xCafEbabEfeedCAFEDeADBeEfcAfeBaBEfeedCAfE);
        (uint256 hi, uint256 lo) = _split(weird);

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("weirdRecipient")));
        pub.recipientHi = hi;
        pub.recipientLo = lo;

        uint256 before = weird.balance;
        bridge.withdrawByProof(_dummyProof(), pub);
        assertEq(weird.balance, before + 1 ether, "weird recipient credited");
    }

    function test_withdrawByProof_zeroRecipientWorks() public {
        // address(0) is a valid 20-byte address. Whether the bridge *should*
        // pay address(0) is a policy question (today: yes — the circuit said
        // so). This test pins the current behaviour so a future deliberate
        // change shows up as a test break.
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("zeroAddr")));
        pub.recipientHi = 0;
        pub.recipientLo = 0;

        uint256 before = address(0).balance;
        bridge.withdrawByProof(_dummyProof(), pub);
        assertEq(address(0).balance, before + 1 ether);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Treasury + verifier rejection
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_treasuryShortfall_reverts() public {
        uint256 huge = bridge.treasuryBalance() + 1 ether;

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTreasuryShortfall.selector, huge, bridge.treasuryBalance()
            )
        );
        bridge.withdrawByProof(_dummyProof(), _defaultPub(huge, uint256(keccak256("shortfall"))));
    }

    function test_withdrawByProof_verifierRejects_revertsAndDoesNotMutateState() public {
        withdrawalVerifier.setShouldAccept(false);

        uint256 treasuryBefore = bridge.treasuryBalance();
        uint256 nullifier = uint256(keccak256("rejected"));

        vm.expectRevert(AckiNackiBridge.WithdrawalProofRejected.selector);
        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 ether, nullifier));

        // State must be untouched on revert.
        assertEq(bridge.treasuryBalance(), treasuryBefore, "treasury unchanged on revert");
        assertFalse(bridge.isNullifierUsed(nullifier), "nullifier not marked on revert");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Strict-mode plumbing assertions
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_forwardsOnChainLayerWindow() public {
        // Submit one verifyBlock so the on-chain ring buffer has a non-zero anchor.
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("wd-layer", i)));
        }
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("attestation-proof")),
            abi.encodePacked(keccak256("layerHashes-proof")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );

        // Lock the mock to the EXACT window the bridge holds.
        uint256[LAYER_WINDOW_SIZE] memory window = bridge.getLayerWindow();
        withdrawalVerifier.setExpectedLayerHashes(window);

        // Submit a withdrawal — should pass because the bridge forwards
        // the on-chain window byte-for-byte.
        bridge.withdrawByProof(
            _dummyProof(), _defaultPub(1 ether, uint256(keccak256("plumb-window")))
        );

        // Now scramble one slot in the mock's expectation; the next
        // withdrawal must fail because the bridge still forwards the
        // on-chain window, which no longer matches.
        window[0] = uint256(keccak256("garbage"));
        withdrawalVerifier.setExpectedLayerHashes(window);

        vm.expectRevert(AckiNackiBridge.WithdrawalProofRejected.selector);
        bridge.withdrawByProof(
            _dummyProof(), _defaultPub(1 ether, uint256(keccak256("plumb-window-2")))
        );
    }

    function test_withdrawByProof_forwardsPublicInputsByteForByte() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 ether, uint256(keccak256("strictPub")));
        withdrawalVerifier.setExpectedPub(pub);

        // Exact match → passes.
        bridge.withdrawByProof(_dummyProof(), pub);

        // Any field mismatch → fails. We tweak nullifier (any field works;
        // nullifier is cheapest because the bridge-level replay check is
        // mapping-based, not pub-strict, so it surfaces as a verifier reject).
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory wrong = pub;
        wrong.nullifier = uint256(keccak256("strictPub-wrong"));

        vm.expectRevert(AckiNackiBridge.WithdrawalProofRejected.selector);
        bridge.withdrawByProof(_dummyProof(), wrong);
    }

    // The bridge contract has a `receive()` fallback so ETH from AAVE works.
    // No corresponding behaviour to test for the unit-test bridge (no AAVE wiring).
    receive() external payable { }
}
