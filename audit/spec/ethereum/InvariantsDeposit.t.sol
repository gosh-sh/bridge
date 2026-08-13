// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/StdInvariant.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";

import "./handlers/AuditHandlers.sol";

/// @title InvariantsDepositTest
/// @notice Phase D / F9 — DEP-5 counter monotonicity; DEP-6 deposit does not touch verifyBlock state.
contract InvariantsDepositTest is StdInvariant, Test {
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    DepositHandler internal handler;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;

    uint64 internal baselineSeqNo;
    uint256 internal baselineBkSet;
    uint256 internal baselineAnchor;

    function setUp() public {
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);

        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _seedVerifyBlock();
        baselineSeqNo = bridge.storedLastSeenBlockSeqNo();
        baselineBkSet = bridge.storedBkSetCommitment();
        baselineAnchor = bridge.storedPrevMaxLevelLayerHash();

        handler = new DepositHandler(bridge, usdc);
        targetContract(address(handler));
    }

    function _seedVerifyBlock() internal {
        uint256[10] memory layers;
        layers[0] = 0xD1;
        layers[1] = 0xD2;
        layers[2] = 0xD3;
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"de",
            hex"ad",
            0xA001,
            BK_SET,
            1,
            3,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    /// @dev INV: DEP-5 — each successful deposit increments counter exactly once.
    function invariant_DEP5_depositCounterMatchesOps() public view {
        assertEq(bridge.depositCounter(), handler.depositOps(), "DEP-5 counter");
    }

    /// @dev INV: DEP-3 / DEP-5 — treasury ledger tracks sum of deposits.
    function invariant_DEP3_treasuryMatchesGhostDeposits() public view {
        assertEq(bridge.treasuryBalance(), handler.ghostDeposited(), "DEP-3 treasury");
    }

    /// @dev INV: DEP-6 — deposit must not mutate verifyBlock stored state.
    function invariant_DEP6_verifyBlockStateUntouched() public view {
        assertEq(bridge.storedLastSeenBlockSeqNo(), baselineSeqNo, "DEP-6 seqNo");
        assertEq(bridge.storedBkSetCommitment(), baselineBkSet, "DEP-6 bkSet");
        assertEq(bridge.storedPrevMaxLevelLayerHash(), baselineAnchor, "DEP-6 anchor");
    }
}
