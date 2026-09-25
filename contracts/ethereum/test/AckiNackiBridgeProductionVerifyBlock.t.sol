// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import { stdJson } from "forge-std/StdJson.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../script/ShplonkDeployLib.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeProductionVerifyBlockTest
/// @notice Production-path E2E: SHPLONK aggregator adapters for 1A, 1B, and 2 + real
///         aggregator calldata. Circuit 1B is keygen'd at K=21 so its aggregated Yul
///         (21,493 B) fits EIP-170 — the gnark Groth16 fallback hybrid is retired.
/// @dev Public inputs and layer hashes come from `bound_scenario.json` (same witness as
///      `export-bound-poseidon-snarks` / n14 Phase C). Calldata from `verifiers/*_calldata.bin`.
contract AckiNackiBridgeProductionVerifyBlockTest is Test {
    using stdJson for string;

    string internal constant PRIMARY_BIN = "verifiers/PrimaryAggregatorVerifier.bin";
    string internal constant FALLBACK_BIN = "verifiers/FallbackAggregatorVerifier.bin";
    string internal constant LAYER_BIN = "verifiers/LayerHashesAggregatorVerifier.bin";
    string internal constant PRIMARY_CALLDATA = "verifiers/PrimaryAggregatorVerifier_calldata.bin";
    string internal constant FALLBACK_CALLDATA =
        "verifiers/FallbackAggregatorVerifier_calldata.bin";
    string internal constant LAYER_CALLDATA =
        "verifiers/LayerHashesAggregatorVerifier_calldata.bin";
    string internal constant BOUND_SCENARIO =
        "../../crates/bridge-snark-utils/proofs/bound/bound_scenario.json";

    struct BoundScenario {
        uint256 blockId;
        uint256 bkSetPoseidon;
        uint64 blockSeqNo;
        uint8 numLayers;
        uint256 prevMaxLevelLayerHash;
        uint256[10] layerHashes;
    }

    AckiNackiBridge internal bridge;
    BoundScenario internal scenario;

    function _binPresent(string memory path) internal view returns (bool) {
        try vm.readFileBinary(path) returns (bytes memory b) {
            return b.length > 0;
        } catch {
            return false;
        }
    }

    function _artefactsPresent() internal view returns (bool) {
        try vm.readFile(BOUND_SCENARIO) returns (string memory) { }
        catch {
            return false;
        }
        return _binPresent(PRIMARY_BIN) && _binPresent(FALLBACK_BIN) && _binPresent(LAYER_BIN)
            && _binPresent(PRIMARY_CALLDATA) && _binPresent(FALLBACK_CALLDATA)
            && _binPresent(LAYER_CALLDATA);
    }

    function _loadScenario() internal view returns (BoundScenario memory s) {
        string memory json = vm.readFile(BOUND_SCENARIO);
        s.blockId = vm.parseUint(json.readString(".block_id_decimal"));
        s.bkSetPoseidon = vm.parseUint(json.readString(".bk_set_poseidon_decimal"));
        s.blockSeqNo = uint64(json.readUint(".block_seq_no"));
        s.numLayers = uint8(json.readUint(".num_layers"));
        s.prevMaxLevelLayerHash =
            vm.parseUint(json.readString(".prev_max_level_layer_hash_decimal"));
        for (uint256 i = 0; i < 10; i++) {
            string memory key = string.concat(".layer_hash_decimals[", vm.toString(i), "]");
            s.layerHashes[i] = vm.parseUint(json.readString(key));
        }
    }

    function _deployTriple() internal returns (ShplonkDeployLib.VerifyBlockVerifiers memory) {
        return ShplonkDeployLib.deployVerifyBlockProduction(PRIMARY_BIN, FALLBACK_BIN, LAYER_BIN);
    }

    /// Skip this test (not setUp) so a missing fixture shows as four
    /// skipped cases with a reason, not one anonymous `setUp` skip.
    function _skipIfNoArtefacts() internal {
        if (!_artefactsPresent()) {
            vm.skip(true, "bound_scenario.json + verifier calldata not in repo");
        }
    }

    function setUp() public {
        if (!_artefactsPresent()) {
            return;
        }
        scenario = _loadScenario();

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);

        ShplonkDeployLib.VerifyBlockVerifiers memory v = _deployTriple();

        AckiNackiBridge.VerifyBlockConfig memory vb = VerifyBlockConfigLib.with(
            v.primary,
            v.fallback_,
            v.layerHashes,
            scenario.bkSetPoseidon,
            scenario.prevMaxLevelLayerHash
        );

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            vb,
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    function test_productionPrimaryAttestation_isolated() public {
        _skipIfNoArtefacts();
        ShplonkDeployLib.VerifyBlockVerifiers memory v = _deployTriple();
        bytes memory proofPrimary = vm.readFileBinary(PRIMARY_CALLDATA);
        assertTrue(
            v.primary
                .verifyPrimaryAttestation(
                    proofPrimary, scenario.blockId, scenario.bkSetPoseidon, scenario.blockSeqNo, 0
                ),
            "primary SHPLONK calldata must verify"
        );
    }

    function test_productionFallbackAttestation_isolated() public {
        _skipIfNoArtefacts();
        ShplonkDeployLib.VerifyBlockVerifiers memory v = _deployTriple();
        bytes memory proofFallback = vm.readFileBinary(FALLBACK_CALLDATA);
        assertTrue(
            v.fallback_
                .verifyFallbackAttestation(
                    proofFallback, scenario.blockId, scenario.bkSetPoseidon, scenario.blockSeqNo, 0
                ),
            "fallback SHPLONK (K=21) calldata must verify"
        );
    }

    /// @dev Helper: does the layer-hashes SHPLONK proof verify in isolation?
    ///      The Circuit 2 aggregator KZG pairing is green as of the bound-witness
    ///      fix (Circuit 2 chain-step off-by-one + canonical block_id endianness),
    ///      so this must return true for a well-formed scenario.
    function _layerProofVerifies(ShplonkDeployLib.VerifyBlockVerifiers memory v)
        internal
        view
        returns (bool)
    {
        return v.layerHashes
            .verifyLayerHashesMovement(
                vm.readFileBinary(LAYER_CALLDATA),
                scenario.blockId,
                scenario.bkSetPoseidon,
                scenario.numLayers,
                scenario.layerHashes,
                scenario.prevMaxLevelLayerHash
            );
    }

    /// Full `verifyBlock` E2E with both real SHPLONK aggregator proofs (Primary
    /// 1A + Circuit 2). Exercises the cross-circuit `block_id` / `bkSetCommitment`
    /// binding on-chain — green since the bound-witness fix.
    function test_productionVerifyBlock_boundCalldata_advancesState() public {
        _skipIfNoArtefacts();

        ShplonkDeployLib.VerifyBlockVerifiers memory v = _deployTriple();
        assertTrue(_layerProofVerifies(v), "layer-hashes SHPLONK pairing must be green");

        bytes memory proofPrimary = vm.readFileBinary(PRIMARY_CALLDATA);
        bytes memory proofLayer = vm.readFileBinary(LAYER_CALLDATA);

        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            proofPrimary,
            proofLayer,
            scenario.blockId,
            scenario.bkSetPoseidon,
            scenario.blockSeqNo,
            scenario.numLayers,
            scenario.layerHashes,
            scenario.prevMaxLevelLayerHash
        );

        assertEq(bridge.storedLastSeenBlockSeqNo(), scenario.blockSeqNo);
        // Storage v2.0: per-layer state now lives in `_layerWindows` and is
        // observed via `getLatestPerLayer()`; the flat `storedNumLayers` /
        // `storedLayerHashes` cache is gone. Assert the per-layer head hashes
        // instead — entry [L-1] is the most recent append for layer L.
        uint256[10] memory latest = bridge.getLatestPerLayer();
        for (uint8 L = 1; L <= scenario.numLayers; L++) {
            assertEq(latest[L - 1], scenario.layerHashes[L - 1], "layer head");
        }
        for (uint8 L = scenario.numLayers + 1; L <= 10; L++) {
            assertEq(latest[L - 1], 0, "unused layer stays zero");
        }
        // storedPrevMaxLevelLayerHash is now the immutable genesis seed.
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            scenario.prevMaxLevelLayerHash,
            "immutable genesis seed unchanged"
        );
        assertEq(bridge.storedBkSetCommitment(), scenario.bkSetPoseidon);
    }

    function test_productionVerifyBlock_tamperedCalldata_reverts() public {
        _skipIfNoArtefacts();

        bytes memory proofPrimary = vm.readFileBinary(PRIMARY_CALLDATA);
        bytes memory proofLayer = vm.readFileBinary(LAYER_CALLDATA);
        if (proofPrimary.length > 0) {
            proofPrimary[proofPrimary.length - 1] =
                bytes1(uint8(proofPrimary[proofPrimary.length - 1]) ^ 0xFF);
        }

        vm.expectRevert();
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            proofPrimary,
            proofLayer,
            scenario.blockId,
            scenario.bkSetPoseidon,
            scenario.blockSeqNo,
            scenario.numLayers,
            scenario.layerHashes,
            scenario.prevMaxLevelLayerHash
        );
    }
}
