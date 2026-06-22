// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import {Vm} from "forge-std/Vm.sol";

import "../src/ShplonkHalo2Verifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/PrimaryAggregatorVerifier.sol";
import "../src/LayerHashesAggregatorVerifier.sol";
import "../src/BridgeWithdrawalAggregatorVerifier.sol";
import "../src/FallbackVerifier.sol";
import "../src/FallbackGroth16VerifierGenerated.sol";

/// @title ShplonkDeployLib
/// @notice Deploy R15 SHPLONK Yul bytecode + aggregator adapters for production scripts.
/// @dev Hybrid verifyBlock wiring (2026-06-22): Primary + LayerHashes use SHPLONK
///      aggregators; Fallback 1B uses the gnark Groth16 wrapper (~7 KB runtime)
///      because the 1B inner VK exceeds EIP-170 when aggregated to Yul (~28 KB).
library ShplonkDeployLib {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    error VerifierBinEmpty(string path);
    error YulDeployFailed();

    struct VerifyBlockVerifiers {
        IPrimaryVerifier primary;
        IFallbackVerifier fallback_;
        ILayerHashesMovementVerifier layerHashes;
    }

    /// @dev Default paths relative to `contracts/ethereum/` when running `forge script`.
    function primaryBinPath() internal view returns (string memory) {
        return VM.envOr("SHPLONK_BIN_PRIMARY", string("verifiers/PrimaryAggregatorVerifier.bin"));
    }

    function layerHashesBinPath() internal view returns (string memory) {
        return VM.envOr(
            "SHPLONK_BIN_LAYER_HASHES", string("verifiers/LayerHashesAggregatorVerifier.bin")
        );
    }

    function withdrawalBinPath() internal view returns (string memory) {
        return VM.envOr(
            "SHPLONK_BIN_WITHDRAWAL", string("verifiers/BridgeWithdrawalAggregatorVerifier.bin")
        );
    }

    function deployYulFromBin(string memory binPath) internal returns (address yul) {
        bytes memory bytecode = VM.readFileBinary(binPath);
        if (bytecode.length == 0) revert VerifierBinEmpty(binPath);
        assembly {
            yul := create(0, add(bytecode, 0x20), mload(bytecode))
        }
        if (yul == address(0)) revert YulDeployFailed();
    }

    function deployShplonkWrapper(address yul) internal returns (address wrapper) {
        return address(new ShplonkHalo2Verifier(yul));
    }

    function deployPrimaryAdapter(string memory binPath) internal returns (IPrimaryVerifier) {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath));
        return IPrimaryVerifier(address(new PrimaryAggregatorVerifier(wrapper)));
    }

    /// @notice Circuit 1B fallback attestation — gnark Groth16 (EIP-170-safe).
    function deployFallbackGroth16Adapter() internal returns (IFallbackVerifier) {
        address groth16 = address(new FallbackGroth16VerifierGenerated());
        return IFallbackVerifier(address(new FallbackVerifier(groth16)));
    }

    function deployLayerHashesAdapter(string memory binPath)
        internal
        returns (ILayerHashesMovementVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath));
        return ILayerHashesMovementVerifier(address(new LayerHashesAggregatorVerifier(wrapper)));
    }

    function deployWithdrawalAdapter(string memory binPath)
        internal
        returns (IBridgeWithdrawalVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath));
        return IBridgeWithdrawalVerifier(address(new BridgeWithdrawalAggregatorVerifier(wrapper)));
    }

    /// @notice SHPLONK for 1A + 2; Groth16 for 1B fallback.
    function deployVerifyBlockHybrid(
        string memory primaryBin,
        string memory layerBin
    ) internal returns (VerifyBlockVerifiers memory out) {
        out.primary = deployPrimaryAdapter(primaryBin);
        out.fallback_ = deployFallbackGroth16Adapter();
        out.layerHashes = deployLayerHashesAdapter(layerBin);
    }

    function deployVerifyBlockHybridFromEnv() internal returns (VerifyBlockVerifiers memory out) {
        return deployVerifyBlockHybrid(primaryBinPath(), layerHashesBinPath());
    }
}
