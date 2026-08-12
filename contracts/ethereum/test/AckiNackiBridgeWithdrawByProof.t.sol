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
/// 2. **Anchor recording**: every `verifyBlock` records each layer anchor in
///    its per-layer window; the `LayerAnchorAppended(layer, hash, height)`
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
    uint256 internal constant SEPOLIA_CHAIN_ID = 11_155_111;
    uint256 internal constant ARBITRUM_ONE_CHAIN_ID = 42_161;
    uint256 internal constant MAINNET_CHAIN_ID = 1;
    uint256 internal constant SHELLNET_LOGICAL_DST_CHAIN_ID = 1;
    uint256 internal constant SHELLNET_USDC_TOKEN_ID = 3;

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

    event LayerAnchorAppended(uint8 indexed layer, uint256 hashValue, uint64 blockHeight);

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

    /// @dev Run one `verifyBlock` with fully-mocked verifiers; return the L1
    ///      anchor (`layerHashes[0]`) used by the partner witness builder.
    function _seedFirstBlock() internal returns (uint256 l1Anchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("wd-seed-layer", i)));
        }
        l1Anchor = layers[0];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("seed-attestation-proof")),
            abi.encodePacked(keccak256("seed-layerHashes-proof")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
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

    function test_constructor_withdrawEnabledWithZeroDappFr_succeeds() public {
        // Shellnet uses dapp_id=0; accFr must still be non-zero.
        AckiNackiBridge shellnet = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), 0, ACC_FR
            )
        );
        assertEq(shellnet.bridgeWithdrawalDappFr(), 0);
        assertEq(shellnet.bridgeWithdrawalAccFr(), ACC_FR);
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
        assertTrue(bridge.isKnownLayerAnchor(1, seedAnchor), "L1 seed anchor recorded");
    }

    function test_verifyBlock_recordsAnchor_andEmitsEvent() public {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("wd-block2-layer", i)));
        }
        uint256 expectedL1 = layers[0];

        // First layer appended is L1; assert its `LayerAnchorAppended` fires.
        vm.expectEmit(true, false, false, true);
        emit LayerAnchorAppended(1, expectedL1, FIRST_SEQ_NO + 1);

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("block2-att")),
            abi.encodePacked(keccak256("block2-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );

        assertTrue(bridge.isKnownLayerAnchor(1, expectedL1));
        assertTrue(bridge.isKnownLayerAnchor(1, seedAnchor), "seed L1 remains valid");
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

    /// @dev Mainnet-destined proof must not replay on Arbitrum (production wiring:
    ///      no `altDstChainId` alias).
    function test_withdrawByProof_mainnetDstChainId_reverts_on_arbitrum() public {
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("arbitrumReplay")));
        pub.dstChainId = MAINNET_CHAIN_ID;

        vm.chainId(ARBITRUM_ONE_CHAIN_ID);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.DstChainIdMismatch.selector, MAINNET_CHAIN_ID, ARBITRUM_ONE_CHAIN_ID
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    /// @dev Sepolia USDC payout cannot be replayed on Arbitrum: `dstChainId` is
    ///      bound to the chain where the proof was destined.
    function test_withdrawByProof_sepoliaPayoutCannotReplayOnArbitrum() public {
        uint256 nullifier = uint256(keccak256("sepoliaToArbitrum"));
        uint256 amount = 1 * UsdcTestLib.UNIT;

        vm.chainId(SEPOLIA_CHAIN_ID);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _defaultPub(amount, nullifier);
        assertEq(pub.dstChainId, SEPOLIA_CHAIN_ID);

        bridge.withdrawByProof(_dummyProof(), pub);

        vm.chainId(ARBITRUM_ONE_CHAIN_ID);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.DstChainIdMismatch.selector, SEPOLIA_CHAIN_ID, ARBITRUM_ONE_CHAIN_ID
            )
        );
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    /// @dev Shellnet logical `dstChainId = 1` is accepted on Sepolia only when
    ///      the alias is scoped to `altDstHostChainId = 11155111`.
    function test_withdrawByProof_shellnetAliasAcceptedOnSepoliaOnly() public {
        MockBridgeWithdrawalVerifier shellnetVerifier = new MockBridgeWithdrawalVerifier();
        shellnetVerifier.setShouldAccept(true);

        AckiNackiBridge shellnetBridge = new AckiNackiBridge(
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
            VerifyBlockConfigLib.withWithdrawShellnet(
                IBridgeWithdrawalVerifier(address(shellnetVerifier)),
                0,
                ACC_FR,
                SHELLNET_LOGICAL_DST_CHAIN_ID,
                SEPOLIA_CHAIN_ID,
                SHELLNET_USDC_TOKEN_ID
            )
        );

        UsdcTestLib.depositUsdc(vm, usdc, shellnetBridge, funder, 10 * UsdcTestLib.UNIT);
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("shellnet-alias-layer", i)));
        }
        shellnetBridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("shellnet-att")),
            abi.encodePacked(keccak256("shellnet-lh")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            shellnetBridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("shellnetAlias")));
        pub.dappFr = 0;
        pub.dstChainId = SHELLNET_LOGICAL_DST_CHAIN_ID;
        pub.tokenId = SHELLNET_USDC_TOKEN_ID;
        pub.finalRoot = layers[0];

        vm.chainId(SEPOLIA_CHAIN_ID);
        shellnetBridge.withdrawByProof(_dummyProof(), pub);

        vm.chainId(ARBITRUM_ONE_CHAIN_ID);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.DstChainIdMismatch.selector,
                SHELLNET_LOGICAL_DST_CHAIN_ID,
                ARBITRUM_ONE_CHAIN_ID
            )
        );
        shellnetBridge.withdrawByProof(_dummyProof(), pub);
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

    /// @notice NB-Q1 regression: an anchor recorded in a non-L1 layer window
    ///         (L2, L3, or any L in `1..MAX_LAYER_HASHES`) must be accepted
    ///         by `withdrawByProof`. Before NB-Q1 (2026-08-04) the scan was
    ///         pinned to L1 via `WITHDRAW_ANCHOR_LAYER = 1` — every partner
    ///         L≥2 witness would revert `UnknownAnchor` here.
    function test_withdrawByProof_anchorRecordedInL2Window_isAccepted() public {
        // Seed block records `layers[0..ACTIVE_LAYERS]` into L1..L3 windows.
        // `_seedFirstBlock` uses `keccak256(abi.encode("wd-seed-layer", i))`;
        // reconstruct the L2 anchor (i=1) and prove against it.
        uint256 l2Anchor = uint256(keccak256(abi.encode("wd-seed-layer", uint256(1))));

        // Sanity: the seed block did populate the L2 window with this value.
        assertTrue(
            bridge.isKnownLayerAnchor(2, l2Anchor), "seed block should have written L2 window"
        );
        assertFalse(
            bridge.isKnownLayerAnchor(1, l2Anchor),
            "L2 anchor must not appear in L1 window (rules out false positive)"
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("l2-anchor-withdraw")));
        pub.finalRoot = l2Anchor;

        bool ok = bridge.withdrawByProof(_dummyProof(), pub);
        assertTrue(ok, "L2 anchor withdrawal must succeed under flat _isKnownAnchor");
    }

    /// @notice NB-Q1 regression (L3 variant): coverage at the highest active
    ///         layer of the seed block, exercising the loop's upper end.
    function test_withdrawByProof_anchorRecordedInL3Window_isAccepted() public {
        uint256 l3Anchor = uint256(keccak256(abi.encode("wd-seed-layer", uint256(2))));

        assertTrue(bridge.isKnownLayerAnchor(3, l3Anchor), "L3 seeded");
        assertFalse(bridge.isKnownLayerAnchor(1, l3Anchor), "not in L1");
        assertFalse(bridge.isKnownLayerAnchor(2, l3Anchor), "not in L2");

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("l3-anchor-withdraw")));
        pub.finalRoot = l3Anchor;

        bool ok = bridge.withdrawByProof(_dummyProof(), pub);
        assertTrue(ok, "L3 anchor withdrawal must succeed under flat _isKnownAnchor");
    }

    /// @notice NB-Q1 regression: a `finalRoot` that matches no layer window
    ///         still reverts `UnknownAnchor` (flat scan didn't accidentally
    ///         become permissive).
    function test_withdrawByProof_anchorInNoLayerWindow_reverts() public {
        uint256 stranger = uint256(keccak256("this-anchor-was-never-written"));
        // Explicit belt-and-suspenders: every layer window must reject it.
        for (uint8 L = 1; L <= 10; L++) {
            assertFalse(
                bridge.isKnownLayerAnchor(L, stranger),
                "stranger anchor must be absent from every window"
            );
        }

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("stranger-withdraw")));
        pub.finalRoot = stranger;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnknownAnchor.selector, stranger));
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    function test_withdrawByProof_anchorRecordedByLaterVerifyBlock_isAccepted() public {
        // Submit a second block and prove withdrawal against its L1 root.
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("later-anchor", i)));
        }
        uint256 laterL1 = layers[0];

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("later-att")),
            abi.encodePacked(keccak256("later-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("later-withdraw")));
        pub.finalRoot = laterL1;

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

    function test_withdrawByProof_zeroRecipient_reverts() public {
        // WD-Q2: address(0) is rejected so a Circuit-4 event binding
        // recipient=0 cannot strand forever against real USDC.
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("zeroAddr")));
        pub.recipientHi = 0;
        pub.recipientLo = 0;

        vm.expectRevert(AckiNackiBridge.InvalidRecipient.selector);
        bridge.withdrawByProof(_dummyProof(), pub);
        assertFalse(bridge.isNullifierUsed(pub.nullifier));
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
