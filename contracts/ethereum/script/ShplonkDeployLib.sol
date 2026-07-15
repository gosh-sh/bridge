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
import "../src/FallbackAggregatorVerifier.sol";

/// @title ShplonkDeployLib
/// @notice Deploy R15 SHPLONK Yul bytecode + aggregator adapters for production scripts.
/// @dev Production verifyBlock wiring (2026-06-22): Primary, Fallback, and
///      LayerHashes all use SHPLONK aggregators. Circuit 1B is keygen'd at K=21
///      (vs the K=20 primary path), which halves its auto-configured advice
///      columns (44 → 22) so the aggregated Yul fits EIP-170 at 21,493 bytes —
///      identical to Primary. The earlier gnark Groth16 hybrid for 1B is retired.
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

    /// @notice Circuit 1B fallback attestation — SHPLONK aggregator (K=21 inner).
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

    /// @notice Production verifyBlock triple — SHPLONK aggregators for 1A, 1B, and 2.
    function deployVerifyBlockProduction(
        string memory primaryBin,
        string memory fallbackBin,
        string memory layerBin
    ) internal returns (VerifyBlockVerifiers memory out) {
        out.primary = deployPrimaryAdapter(primaryBin);
        out.fallback_ = deployFallbackAdapter(fallbackBin);
        out.layerHashes = deployLayerHashesAdapter(layerBin);
    }

    function deployVerifyBlockProductionFromEnv()
        internal
        returns (VerifyBlockVerifiers memory out)
    {
        return deployVerifyBlockProduction(primaryBinPath(), fallbackBinPath(), layerHashesBinPath());
    }
}
