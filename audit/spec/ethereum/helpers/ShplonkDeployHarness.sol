// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "@script/ShplonkDeployLib.sol";
import "@src/IBridgeWithdrawalVerifier.sol";
import "@src/BridgeWithdrawalAggregatorVerifier.sol";

/// @dev Exposes internal ShplonkDeployLib helpers to audit E2E deploy smoke tests.
contract ShplonkDeployHarness {
    function deployWrapper(address yul) external returns (address wrapper) {
        return ShplonkDeployLib.deployShplonkWrapper(yul);
    }

    function deployVerifyBlockTriple(string memory primaryBin, string memory fallbackBin, string memory layerBin)
        external
        returns (ShplonkDeployLib.VerifyBlockVerifiers memory)
    {
        return ShplonkDeployLib.deployVerifyBlockProduction(primaryBin, fallbackBin, layerBin);
    }

    function deployWithdrawal(string memory binPath) external returns (IBridgeWithdrawalVerifier) {
        return ShplonkDeployLib.deployWithdrawalAdapter(binPath);
    }

    function deployWithdrawalFromWrapper(address wrapper)
        external
        returns (BridgeWithdrawalAggregatorVerifier)
    {
        return new BridgeWithdrawalAggregatorVerifier(wrapper);
    }
}
