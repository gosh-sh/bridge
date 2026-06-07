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

/// @title AckiNackiBridgeWithdrawByProofTest
/// @notice Circuit 4 (single-final-root) tests — the unified AN→ETH payout
///         path landed in v3 of `bridge-event-prove-circuit` (partner branch
///         `circuit4-single-final-root`). Replaces the previous Phase A
///         (`verifyEvent`) + Phase B (110-input withdrawal) split.
///
/// Uses a mock `IBridgeWithdrawalVerifier` because the R15 gnark wrapper is
/// still an identity stub — tracked as Phase 8 of
/// `docs/an_partner_integration_plan.md`. Once the real Halo2-in-gnark
/// verifier lands, this suite stays unchanged and a sibling E2E suite picks
/// up real bound proofs.
///
/// Coverage:
///
/// 1. **Constructor wiring**: enabled / disabled toggles; revert if identity
///    slots aren't set when the verifier is non-zero
///    (`InvalidBridgeWithdrawalIdentity`).
/// 2. **Anchor recording**: every `verifyBlock` records the new top-of-chain
///    anchor in `_knownAnchors` and bumps `anchorsRecorded`; `AnchorRecorded`
///    event reflects the same state change.
/// 3. **Happy path**: a verified proof transfers `amount` ETH to the
///    reconstructed `recipient`, marks the nullifier used, emits
///    `WithdrawalByProofExecuted`, decrements `treasuryBalance`.
/// 4. **Replay protection**: re-submitting the same nullifier reverts with
///    `NullifierAlreadyUsed`. The `isNullifierUsed` view reports correctly.
/// 5. **Identity mismatch**: a proof carrying mismatching `(dappFr, accFr)`
///    reverts with `WithdrawIdentityMismatch` *before* the verifier is
///    invoked.
/// 6. **dstChainId mismatch**: a proof for a different chain reverts with
///    `DstChainIdMismatch(supplied, expected)`.
/// 7. **Recipient split validation**: `recipientHi` or `recipientLo`
///    exceeding 80 bits reverts with `RecipientHalfOutOfRange`. Valid splits
///    round-trip cleanly through `_reconstructRecipient`.
/// 8. **Anchor unknown**: a proof referencing a `finalRoot` not in
///    `_knownAnchors` reverts with `UnknownAnchor`.
/// 9. **Treasury shortfall**: amount > treasuryBalance reverts with
///    `WithdrawTreasuryShortfall`.
/// 10. **Unsupported tokenId**: any `tokenId != 0` reverts (Phase B is
///     native-ETH-only).
/// 11. **Mock crypto rejection**: the underlying verifier returning `false`
///     reverts with `WithdrawalProofRejected` and leaves state untouched.
/// 12. **Public inputs are forwarded byte-for-byte** to the verifier —
///     proven via the mock's strict-pub matcher.
contract AckiNackiBridgeWithdrawByProofTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xC10C40001;
    uint64 internal constant FIRST_SEQ_NO = 1;

    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;

    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    /// @dev Foundry's default `block.chainid` in unit tests is 31337 (Anvil).
    uint256 internal constant DEFAULT_CHAIN_ID = 31337;

    /// @dev Sample recipient — 0x1111...1111
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    /// @dev Sample funder for deposits.
    address internal funder = address(0xF00D);

    /// @dev Anchor recorded into `_knownAnchors` via `setUp`'s seed
    ///      `verifyBlock`. Used as `pub.finalRoot` for happy-path tests.
    uint256 internal seedAnchor;

    // Re-declared from AckiNackiBridge for `vm.expectEmit`.
    event WithdrawalByProofExecuted(
        uint256 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 indexed tokenId,
        address submitter
    );

    event AnchorRecorded(uint256 indexed anchor, uint256 totalAnchors);

    function setUp() public {
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

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        // Seed the bridge with a healthy treasury so withdrawals don't shortfall.
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 50 * UsdcTestLib.UNIT);

        // Drive one `verifyBlock` so the suite has at least one anchor to
        // bind withdrawal proofs to (mirrors the production timeline:
        // every `withdrawByProof` requires a prior `verifyBlock` whose
        // top-of-chain anchor matches the proof's `finalRoot`).
        seedAnchor = _seedFirstBlock();
    }

    // ─────────────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────────────

    /// @dev Run one `verifyBlock` with fully-mocked verifiers; return the
    ///      anchor (top-of-chain layer hash) the bridge then records.
    function _seedFirstBlock() internal returns (uint256 topAnchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("wd-seed-layer", i)));
        }
        topAnchor = layers[ACTIVE_LAYERS - 1];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("seed-attestation-proof")),
            abi.encodePacked(keccak256("seed-layerHashes-proof")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );
    }

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
    ///      - amount = caller-controlled
    ///      - nullifier = caller-controlled (so callers can vary it for replay tests)
    ///      - finalRoot = `seedAnchor` (recorded by the setUp `verifyBlock`)
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
            senderAccFr: uint256(keccak256("senderAcc")),
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: seedAnchor
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
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        assertEq(address(plain.bridgeWithdrawalVerifier()), address(0));
        assertEq(plain.bridgeWithdrawalDappFr(), 0);
        assertEq(plain.bridgeWithdrawalAccFr(), 0);
    }

    function test_constructor_withdrawEnabled_storesVerifierAndIdentity() public view {
        assertEq(address(bridge.bridgeWithdrawalVerifier()), address(withdrawalVerifier));
        assertEq(bridge.bridgeWithdrawalDappFr(), DAPP_FR);
        assertEq(bridge.bridgeWithdrawalAccFr(), ACC_FR);
    }

    function test_constructor_withdrawEnabledWithoutDappFr_reverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidBridgeWithdrawalIdentity.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), 0, ACC_FR
            )
        );
    }

    function test_constructor_withdrawEnabledWithoutAccFr_reverts() public {
        vm.expectRevert(AckiNackiBridge.InvalidBridgeWithdrawalIdentity.selector);
        new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, 0
            )
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Anchor recording
    // ─────────────────────────────────────────────────────────────────────

    function test_setUp_recordsSeedAnchor() public view {
        assertTrue(bridge.isKnownAnchor(seedAnchor), "seed anchor recorded");
        assertEq(bridge.anchorsRecorded(), 1, "one anchor recorded");
    }

    function test_verifyBlock_recordsAnchor_andEmitsEvent() public {
        // Build a second block on top of the seed anchor.
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("wd-block2-layer", i)));
        }
        uint256 expectedAnchor = layers[ACTIVE_LAYERS - 1];

        vm.expectEmit(true, false, false, true);
        emit AnchorRecorded(expectedAnchor, 2);

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("block2-att")),
            abi.encodePacked(keccak256("block2-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );

        assertTrue(bridge.isKnownAnchor(expectedAnchor));
        assertTrue(bridge.isKnownAnchor(seedAnchor), "seed anchor remains valid");
        assertEq(bridge.anchorsRecorded(), 2);
    }

    function test_isKnownAnchor_initiallyFalseForRandomValue() public view {
        assertFalse(bridge.isKnownAnchor(0xDEAD_BEEF));
        assertFalse(bridge.isKnownAnchor(0));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Happy path
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_happyPath_transfersAndMarksNullifier() public {
        uint256 amount = 2 * UsdcTestLib.UNIT;
        uint256 nullifier = uint256(keccak256("nul-1"));
        uint256 recipientBalBefore = usdc.balanceOf(RECIPIENT);
        uint256 treasuryBefore = bridge.treasuryBalance();

        vm.expectEmit(true, true, true, true);
        emit WithdrawalByProofExecuted(nullifier, RECIPIENT, amount, 0, address(this));

        bool ok = bridge.withdrawByProof(_dummyProof(), _defaultPub(amount, nullifier));
        assertTrue(ok);

        assertEq(usdc.balanceOf(RECIPIENT), recipientBalBefore + amount, "recipient credited");
        assertEq(bridge.treasuryBalance(), treasuryBefore - amount, "treasury decremented");
        assertTrue(bridge.isNullifierUsed(nullifier), "nullifier marked");
    }

    function test_withdrawByProof_disabled_reverts() public {
        AckiNackiBridge bareA = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        vm.expectRevert(AckiNackiBridge.WithdrawByProofDisabled.selector);
        bareA.withdrawByProof(_dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, 1));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Replay protection
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_replayRejected() public {
        uint256 nullifier = uint256(keccak256("replay"));

        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, nullifier));

        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.NullifierAlreadyUsed.selector, nullifier)
        );
        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, nullifier));
    }

    function test_isNullifierUsed_initiallyFalse() public view {
        assertFalse(bridge.isNullifierUsed(0xDEAD_BEEF));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Identity / chain / token / anchor validation
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_wrongDappFr_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("idmismatch1")));
        pub.dappFr = uint256(keccak256("evilDapp"));

        vm.expectRevert(AckiNackiBridge.WithdrawIdentityMismatch.selector);
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_wrongAccFr_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("idmismatch2")));
        pub.accFr = uint256(keccak256("evilAcc"));

        vm.expectRevert(AckiNackiBridge.WithdrawIdentityMismatch.selector);
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_wrongDstChainId_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("chainmismatch")));
        uint256 wrong = pub.dstChainId + 1;
        pub.dstChainId = wrong;

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.DstChainIdMismatch.selector, wrong, block.chainid
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    /// @dev Mainnet-destined proof must not replay on Arbitrum (or any other chain).
    function test_withdrawByProof_mainnetDstChainId_reverts_on_arbitrum() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("arbitrumReplay")));
        pub.dstChainId = 1; // Ethereum mainnet

        vm.chainId(42_161);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.DstChainIdMismatch.selector, 1, 42_161)
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_unsupportedTokenId_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("token")));
        pub.tokenId = 1;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, 1));
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_unknownAnchor_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("unknownAnchor")));
        uint256 unknownRoot = uint256(keccak256("not-recorded"));
        pub.finalRoot = unknownRoot;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnknownAnchor.selector, unknownRoot));
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_anchorRecordedByLaterVerifyBlock_isAccepted() public {
        // Submit a second block, capture its anchor, and prove a withdrawal
        // against it. Exercises that *any* anchor in the set works, not just
        // the most recent.
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("later-anchor", i)));
        }
        uint256 laterAnchor = layers[ACTIVE_LAYERS - 1];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("later-att")),
            abi.encodePacked(keccak256("later-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("later-withdraw")));
        pub.finalRoot = laterAnchor;

        bool ok = bridge.withdrawByProof(_dummyProof(), pub);
        assertTrue(ok);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Recipient split validation + reconstruction
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_recipientHiOutOfRange_reverts() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("hiOOB")));
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
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("loOOB")));
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
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("weirdRecipient")));
        pub.recipientHi = hi;
        pub.recipientLo = lo;

        uint256 before = usdc.balanceOf(weird);
        bridge.withdrawByProof(_dummyProof(), pub);
        assertEq(usdc.balanceOf(weird), before + 1 * UsdcTestLib.UNIT, "weird recipient credited");
    }

    function test_withdrawByProof_zeroRecipientWorks() public {
        // address(0) is a valid 20-byte address. Whether the bridge *should*
        // pay address(0) is a policy question (today: yes — the circuit said
        // so). This test pins the current behaviour so a future deliberate
        // change shows up as a test break.
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("zeroAddr")));
        pub.recipientHi = 0;
        pub.recipientLo = 0;

        uint256 before = usdc.balanceOf(address(0));
        bridge.withdrawByProof(_dummyProof(), pub);
        assertEq(usdc.balanceOf(address(0)), before + 1 * UsdcTestLib.UNIT);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Treasury + verifier rejection
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_treasuryShortfall_reverts() public {
        uint256 huge = bridge.treasuryBalance() + 1 * UsdcTestLib.UNIT;

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
        bridge.withdrawByProof(_dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, nullifier));

        // State must be untouched on revert.
        assertEq(bridge.treasuryBalance(), treasuryBefore, "treasury unchanged on revert");
        assertFalse(bridge.isNullifierUsed(nullifier), "nullifier not marked on revert");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Strict-mode plumbing assertions
    // ─────────────────────────────────────────────────────────────────────

    function test_withdrawByProof_forwardsPublicInputsByteForByte() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("strictPub")));
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
}
