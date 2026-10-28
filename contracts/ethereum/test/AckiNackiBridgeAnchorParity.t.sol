// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeAnchorParityTest
/// @notice Prover ↔ contract parity of the chain anchor (bridge-audit#122).
///
///         `test/fixtures/anchor_parity.json` is written by the prover's own
///         `BootstrapSeed::apply` / `BridgeState` (`bridge-prover-lib/tests/
///         anchor_parity.rs`, which also fails when the prover drifts from the
///         file). Each scenario seeds the prover from a key block — some on a
///         boundary of higher order than the anchor level — and appends a run
///         of bundles. This suite deploys the bridge with the scenario's
///         genesis anchor, replays the bundles through `verifyBlock` and
///         requires `expectedPrevAnchor(n)` to equal the prover's
///         `prev_max_level_layer_hash_for(n)` for every `n` in 1..=10, after
///         the seed and after every bundle.
contract AckiNackiBridgeAnchorParityTest is Test {
    string internal constant FIXTURE = "test/fixtures/anchor_parity.json";
    uint256 internal constant BK_SET = 0xBE5E7;

    string internal json;

    function setUp() public {
        json = vm.readFile(FIXTURE);
    }

    function _deploy(uint256 genesisAnchor) internal returns (AckiNackiBridge bridge) {
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layerHashes.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                genesisAnchor
            ),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function _assertAnchors(
        AckiNackiBridge bridge,
        uint256[] memory expected,
        uint8 fromLayer,
        string memory at
    ) internal view {
        assertEq(expected.length, 10, string.concat(at, ": anchors for n = 1..=10"));
        for (uint8 n = fromLayer; n <= 10; n++) {
            assertEq(
                bridge.expectedPrevAnchor(n),
                expected[n - 1],
                string.concat(at, ": expectedPrevAnchor(", vm.toString(n), ")")
            );
        }
    }

    function _replay(string memory name) internal {
        string memory s = string.concat(".", name);
        AckiNackiBridge bridge =
            _deploy(vm.parseJsonUint(json, string.concat(s, ".genesis_anchor")));

        // Below the anchor level the contract knows no seed root, and no bundle
        // under that level carries fewer layers than the level itself.
        _assertAnchors(
            bridge,
            vm.parseJsonUintArray(json, string.concat(s, ".seed_anchors")),
            uint8(vm.parseJsonUint(json, string.concat(s, ".anchor_level"))),
            string.concat(name, " after the seed")
        );

        uint256 count = vm.parseJsonUint(json, string.concat(s, ".bundle_count"));
        assertGt(count, 0, "scenario has bundles");
        for (uint256 i = 0; i < count; i++) {
            _replayBundle(
                bridge,
                string.concat(s, ".bundles[", vm.toString(i), "]"),
                string.concat(name, " bundle ", vm.toString(i))
            );
        }
    }

    function _replayBundle(AckiNackiBridge bridge, string memory b, string memory at) internal {
        uint256[] memory hashes = vm.parseJsonUintArray(json, string.concat(b, ".layer_hashes"));
        uint256 prevAnchor = vm.parseJsonUint(json, string.concat(b, ".prev_anchor"));
        assertEq(
            bridge.expectedPrevAnchor(uint8(hashes.length)),
            prevAnchor,
            string.concat(at, ": the prover's anchor")
        );
        _submit(
            bridge, uint64(vm.parseJsonUint(json, string.concat(b, ".seq_no"))), hashes, prevAnchor
        );
        _assertAnchors(
            bridge, vm.parseJsonUintArray(json, string.concat(b, ".anchors_after")), 1, at
        );
    }

    function _submit(
        AckiNackiBridge bridge,
        uint64 seqNo,
        uint256[] memory hashes,
        uint256 prevAnchor
    ) internal {
        uint256[10] memory layers;
        for (uint256 j = 0; j < hashes.length; j++) {
            layers[j] = hashes[j];
        }
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            uint256(seqNo),
            BK_SET,
            seqNo,
            uint8(hashes.length),
            layers,
            prevAnchor
        );
    }

    /// @notice The case of bridge-audit#122: an L2 seed on a W³ boundary. The
    ///         contract learns layer 3 only at the next W³ boundary and anchors
    ///         that block at the latest layer-2 root; so must the prover.
    function test_l2SeedOnW3Boundary() public {
        _replay("l2_seed_on_w3_boundary");
    }

    function test_l2SeedOnW4Boundary() public {
        _replay("l2_seed_on_w4_boundary");
    }

    function test_l2SeedOnW2Boundary() public {
        _replay("l2_seed_on_w2_boundary");
    }

    function test_l2FirstBundleOnW3Boundary() public {
        _replay("l2_first_bundle_on_w3_boundary");
    }

    /// @notice The L1 row of bridge-audit#122: an L1 seed on a W² boundary.
    function test_l1SeedOnW2Boundary() public {
        _replay("l1_seed_on_w2_boundary");
    }

    function test_l1SeedOffW2Boundary() public {
        _replay("l1_seed_off_w2_boundary");
    }
}
