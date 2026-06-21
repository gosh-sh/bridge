// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import {Vm} from "forge-std/Vm.sol";

import "../src/ShplonkHalo2Verifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/PrimaryAggregatorVerifier.sol";
import "../src/FallbackAggregatorVerifier.sol";
import "../src/LayerHashesAggregatorVerifier.sol";
import "../src/BridgeWithdrawalAggregatorVerifier.sol";

/// @title ShplonkDeployLib
/// @notice Deploy R15 SHPLONK Yul bytecode + aggregator adapters for production scripts.
/// @dev Identity-stub Groth16 wrappers and mock withdrawal verifiers must not be used
///      in deploy scripts — only real `.bin` artefacts from `bridge-evm-aggregator`.
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

    function fallbackBinPath() internal view returns (string memory) {
        return VM.envOr("SHPLONK_BIN_FALLBACK", string("verifiers/FallbackAggregatorVerifier.bin"));
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

    function deployFallbackAdapter(string memory binPath) internal returns (IFallbackVerifier) {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath));
        return IFallbackVerifier(address(new FallbackAggregatorVerifier(wrapper)));
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

    function deployVerifyBlockTriple(
        string memory primaryBin,
        string memory fallbackBin,
        string memory layerBin
    ) internal returns (VerifyBlockVerifiers memory out) {
        out.primary = deployPrimaryAdapter(primaryBin);
        out.fallback_ = deployFallbackAdapter(fallbackBin);
        out.layerHashes = deployLayerHashesAdapter(layerBin);
    }

    function deployVerifyBlockTripleFromEnv() internal returns (VerifyBlockVerifiers memory out) {
        return deployVerifyBlockTriple(primaryBinPath(), fallbackBinPath(), layerHashesBinPath());
    }
}
