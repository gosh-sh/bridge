// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import {stdJson} from "forge-std/StdJson.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../script/ShplonkDeployLib.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockERC20.sol";

/// @title AckiNackiBridgeHybridVerifyBlockTest
/// @notice Production-path E2E: hybrid SHPLONK adapters (1A + 2) + real aggregator calldata.
/// @dev Public inputs and layer hashes come from `bound_scenario.json` (same witness as
///      `export-bound-poseidon-snarks` / n14 Phase C). Calldata from `verifiers/*_calldata.bin`.
contract AckiNackiBridgeHybridVerifyBlockTest is Test {
    using stdJson for string;

    string internal constant PRIMARY_BIN = "verifiers/PrimaryAggregatorVerifier.bin";
    string internal constant LAYER_BIN = "verifiers/LayerHashesAggregatorVerifier.bin";
    string internal constant PRIMARY_CALLDATA = "verifiers/PrimaryAggregatorVerifier_calldata.bin";
    string internal constant LAYER_CALLDATA = "verifiers/LayerHashesAggregatorVerifier_calldata.bin";
    string internal constant BOUND_SCENARIO =
        "../../crates/bridge-prover-orchestrator/proofs/bound/bound_scenario.json";

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

    function _artefactsPresent() internal view returns (bool) {
        try vm.readFile(BOUND_SCENARIO) returns (string memory) {}
        catch {
            return false;
        }
        try vm.readFileBinary(PRIMARY_BIN) returns (bytes memory p) {
            if (p.length == 0) return false;
        } catch {
            return false;
        }
        try vm.readFileBinary(LAYER_BIN) returns (bytes memory l) {
            if (l.length == 0) return false;
        } catch {
            return false;
        }
        try vm.readFileBinary(PRIMARY_CALLDATA) returns (bytes memory pc) {
            if (pc.length == 0) return false;
        } catch {
            return false;
        }
        try vm.readFileBinary(LAYER_CALLDATA) returns (bytes memory lc) {
            return lc.length > 0;
        } catch {
            return false;
        }
    }

    function _loadScenario() internal view returns (BoundScenario memory s) {
        string memory json = vm.readFile(BOUND_SCENARIO);
        s.blockId = vm.parseUint(json.readString(".block_id_decimal"));
        s.bkSetPoseidon = vm.parseUint(json.readString(".bk_set_poseidon_decimal"));
        s.blockSeqNo = uint64(json.readUint(".block_seq_no"));
        s.numLayers = uint8(json.readUint(".num_layers"));
        s.prevMaxLevelLayerHash = vm.parseUint(json.readString(".prev_max_level_layer_hash_decimal"));
        for (uint256 i = 0; i < 10; i++) {
            string memory key = string.concat(".layer_hash_decimals[", vm.toString(i), "]");
            s.layerHashes[i] = vm.parseUint(json.readString(key));
        }
    }

    function setUp() public {
        if (!_artefactsPresent()) {
            return;
        }
        scenario = _loadScenario();

        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        MockERC20 usdc = new MockERC20("Mock USDC", "mUSDC", 6);

        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockHybrid(PRIMARY_BIN, LAYER_BIN);

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

    function test_hybridPrimaryAttestation_isolated() public {
        if (!_artefactsPresent()) return;
        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            ShplonkDeployLib.deployVerifyBlockHybrid(PRIMARY_BIN, LAYER_BIN);
        bytes memory proofPrimary = vm.readFileBinary(PRIMARY_CALLDATA);
        assertTrue(
            v.primary.verifyPrimaryAttestation(
                proofPrimary,
                scenario.blockId,
                scenario.bkSetPoseidon,
                scenario.blockSeqNo,
                0
            ),
            "primary SHPLONK calldata must verify"
        );
    }

    /// Full `verifyBlock` needs both proofs; layer K=22 aggregation is tracked in production plan.
    function test_hybridVerifyBlock_boundCalldata_advancesState() public {
        if (!_artefactsPresent()) {
            emit log("SKIP: bound_scenario.json + verifiers/*_calldata.bin required");
            return;
        }

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
        assertEq(bridge.storedNumLayers(), scenario.numLayers);
        assertEq(
            bridge.storedPrevMaxLevelLayerHash(),
            scenario.layerHashes[scenario.numLayers - 1],
            "chain anchor = top active layer hash"
        );
        assertEq(bridge.storedBkSetCommitment(), scenario.bkSetPoseidon);
    }

    function test_hybridVerifyBlock_tamperedCalldata_reverts() public {
        if (!_artefactsPresent()) {
            return;
        }

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
