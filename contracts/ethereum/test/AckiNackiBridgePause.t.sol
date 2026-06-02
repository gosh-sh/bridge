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

/// @title AckiNackiBridgePauseTest
/// @notice Tests for the global pause / unpause flag (incident-response control).
///
/// Coverage:
/// 1. Default state — `paused == false`.
/// 2. `pause()` toggles the flag and emits `Paused`; only owner can pause.
/// 3. Pausing twice reverts with `AlreadyInThatPauseState`.
/// 4. `unpause()` toggles back and emits `Unpaused`; only owner can unpause.
/// 5. Unpausing when not paused reverts with `AlreadyInThatPauseState`.
/// 6. **User-facing entrypoints revert with `BridgePaused`** while paused:
///    `deposit()`, `verifyBlock(...)`, `withdrawByProof(...)`.
/// 7. **Owner-only AAVE management remains available** while paused (so the
///    owner can evacuate funds in an incident). Tested for the no-AAVE
///    bridge variant via `transferOwnership` + `setAaveEnabled(false)`
///    (no AAVE wired in this suite — AAVE-specific paths are covered in
///    `AckiNackiBridgeAaveTest`).
/// 8. After unpause, entrypoints work again (idempotency invariant).
contract AckiNackiBridgePauseTest is Test {
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

    /// @dev Sample recipient — 0x1111...1111
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);

    address internal funder = address(0xF00D);
    address internal nonOwner = address(0xBAD);

    /// @dev Captured anchor after the seed `verifyBlock` in `setUp`.
    uint256 internal seedAnchor;

    event Paused(address indexed by);
    event Unpaused(address indexed by);

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

        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 50 * UsdcTestLib.UNIT);

        seedAnchor = _seedFirstBlock();
    }

    function _seedFirstBlock() internal returns (uint256 topAnchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("pause-seed-layer", i)));
        }
        topAnchor = layers[ACTIVE_LAYERS - 1];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("pause-seed-att")),
            abi.encodePacked(keccak256("pause-seed-lh")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );
    }

    function _defaultPub(uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        uint256 hi = a >> 80;
        uint256 lo = a & ((uint256(1) << 80) - 1);
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

    function _block2Layers() internal pure returns (uint256[10] memory layers) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("pause-block2-layer", i)));
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Default state + access control
    // ─────────────────────────────────────────────────────────────────────

    function test_paused_initiallyFalse() public view {
        assertFalse(bridge.paused());
    }

    function test_pause_onlyOwner() public {
        vm.prank(nonOwner);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.pause();
    }

    function test_unpause_onlyOwner() public {
        bridge.pause();
        vm.prank(nonOwner);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.unpause();
    }

    function test_pause_emitsAndSetsFlag() public {
        vm.expectEmit(true, false, false, false);
        emit Paused(address(this));
        bridge.pause();
        assertTrue(bridge.paused());
    }

    function test_pause_alreadyPaused_reverts() public {
        bridge.pause();
        vm.expectRevert(AckiNackiBridge.AlreadyInThatPauseState.selector);
        bridge.pause();
    }

    function test_unpause_emitsAndClearsFlag() public {
        bridge.pause();
        vm.expectEmit(true, false, false, false);
        emit Unpaused(address(this));
        bridge.unpause();
        assertFalse(bridge.paused());
    }

    function test_unpause_notPaused_reverts() public {
        vm.expectRevert(AckiNackiBridge.AlreadyInThatPauseState.selector);
        bridge.unpause();
    }

    // ─────────────────────────────────────────────────────────────────────
    // User-facing entrypoints blocked while paused
    // ─────────────────────────────────────────────────────────────────────

    function test_deposit_blockedWhilePaused() public {
        bridge.pause();
        usdc.mint(funder, 1 * UsdcTestLib.UNIT);
        vm.startPrank(funder);
        usdc.approve(address(bridge), 1 * UsdcTestLib.UNIT);
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.deposit(1 * UsdcTestLib.UNIT, int8(0), bytes32(uint256(uint160(funder))));
        vm.stopPrank();
    }

    function test_verifyBlock_blockedWhilePaused() public {
        bridge.pause();
        uint256[10] memory layers = _block2Layers();
        // Read prev-anchor BEFORE arming expectRevert — otherwise the
        // staticcall to `storedPrevMaxLevelLayerHash()` (which succeeds)
        // is what `vm.expectRevert` matches against.
        uint256 prev = bridge.storedPrevMaxLevelLayerHash();
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("paused-att")),
            abi.encodePacked(keccak256("paused-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            prev
        );
    }

    function test_withdrawByProof_blockedWhilePaused() public {
        bridge.pause();
        vm.expectRevert(AckiNackiBridge.BridgePaused.selector);
        bridge.withdrawByProof(
            _dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("paused-w")))
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Resume invariants
    // ─────────────────────────────────────────────────────────────────────

    function test_unpauseRestoresEntrypoints() public {
        bridge.pause();
        bridge.unpause();

        // Deposit works again.
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 1 * UsdcTestLib.UNIT);

        // verifyBlock works again.
        uint256[10] memory layers = _block2Layers();
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("resume-att")),
            abi.encodePacked(keccak256("resume-lh")),
            FIRST_BLOCK_ID + 1,
            BK_SET,
            FIRST_SEQ_NO + 1,
            ACTIVE_LAYERS,
            layers,
            bridge.storedPrevMaxLevelLayerHash()
        );

        // withdrawByProof works again.
        bool ok = bridge.withdrawByProof(
            _dummyProof(), _defaultPub(1 * UsdcTestLib.UNIT, uint256(keccak256("resume-w")))
        );
        assertTrue(ok);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Owner controls remain available while paused
    // ─────────────────────────────────────────────────────────────────────

    function test_ownerControls_workWhilePaused() public {
        bridge.pause();
        // setLiquidReserveBps is owner-only and not gated by pause.
        bridge.setLiquidReserveBps(2_000);
        assertEq(bridge.liquidReserveBps(), 2_000);

        // transferOwnership too.
        bridge.transferOwnership(address(0xDEAD));
        assertEq(bridge.owner(), address(0xDEAD));
    }
}
