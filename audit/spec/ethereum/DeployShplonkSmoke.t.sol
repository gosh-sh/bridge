// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@script/ShplonkDeployLib.sol";

import "@src/IBridgeWithdrawalVerifier.sol";
import "@src/BridgeWithdrawalAggregatorVerifier.sol";
import "@src/ShplonkHalo2Verifier.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/PrimaryAggregatorVerifier.sol";

import "./helpers/MinimalShplonkYul.sol";
import "./helpers/ShplonkDeployHarness.sol";

/// @title DeployShplonkSmokeTest
/// @notice Phase E / E-06 — production deploy lib wires SHPLONK aggregators.
/// @dev WD-Q3 complement: mirrors `DeployRealBridge` / `ShplonkDeployLib` stack.
contract DeployShplonkSmokeTest is Test {
    string internal constant PRIMARY_BIN = "../../../contracts/ethereum/verifiers/PrimaryAggregatorVerifier.bin";
    string internal constant FALLBACK_BIN = "../../../contracts/ethereum/verifiers/FallbackAggregatorVerifier.bin";
    string internal constant LAYER_BIN = "../../../contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.bin";
    string internal constant WITHDRAW_BIN =
        "../../../contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin";

    ShplonkDeployHarness internal harness;

    function setUp() public {
        harness = new ShplonkDeployHarness();
    }

    function _binPresent(string memory path) internal view returns (bool) {
        try vm.readFileBinary(path) returns (bytes memory b) {
            return b.length > 0;
        } catch {
            return false;
        }
    }

    /// @dev E-06: deploy stack with stub Yul yields aggregator adapter types.
    function test_E06_shplonkDeployLib_stubYul_wiresAggregator() public {
        address yul = address(new MinimalShplonkYul());
        assertGt(yul.code.length, 0, "stub Yul has bytecode");

        address wrapper = harness.deployWrapper(yul);
        assertTrue(wrapper != address(0), "ShplonkHalo2Verifier deployed");

        BridgeWithdrawalAggregatorVerifier withdrawal = harness.deployWithdrawalFromWrapper(wrapper);
        assertEq(address(withdrawal.shplonkVerifier()), wrapper, "withdrawal uses SHPLONK wrapper");

        IPrimaryVerifier primary = IPrimaryVerifier(address(new PrimaryAggregatorVerifier(wrapper)));
        assertTrue(address(primary) != address(0), "primary aggregator deployed");
    }

    /// @dev E-06 optional: when `.bin` artefacts exist, full triple deploy succeeds.
    function test_E06_shplonkDeployLib_productionBinsDeploySmoke() public {
        if (
            !_binPresent(PRIMARY_BIN) || !_binPresent(FALLBACK_BIN) || !_binPresent(LAYER_BIN)
                || !_binPresent(WITHDRAW_BIN)
        ) {
            emit log("SKIP: production SHPLONK .bin artefacts absent locally");
            return;
        }

        ShplonkDeployLib.VerifyBlockVerifiers memory v =
            harness.deployVerifyBlockTriple(PRIMARY_BIN, FALLBACK_BIN, LAYER_BIN);

        assertTrue(address(v.primary) != address(0), "primary adapter");
        assertTrue(address(v.fallback_) != address(0), "fallback adapter");
        assertTrue(address(v.layerHashes) != address(0), "layer adapter");

        IBridgeWithdrawalVerifier withdrawal = harness.deployWithdrawal(WITHDRAW_BIN);
        assertTrue(address(withdrawal) != address(0), "withdrawal adapter");

        BridgeWithdrawalAggregatorVerifier agg = BridgeWithdrawalAggregatorVerifier(payable(address(withdrawal)));
        assertGt(address(agg.shplonkVerifier()).code.length, 0, "withdrawal SHPLONK wrapper live");
    }
}
