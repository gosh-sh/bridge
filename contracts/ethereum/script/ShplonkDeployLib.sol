// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import { Vm } from "forge-std/Vm.sol";

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
///      identical to Primary.
library ShplonkDeployLib {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    error VerifierBinEmpty(string path);
    error YulDeployFailed();
    /// @notice ETH-06: CREATE'd Yul runtime keccak256 does not match the pin
    ///         for this artefact. Env-path deploys cannot silently swap circuits.
    error YulCodehashMismatch(bytes32 actual, bytes32 expected);

    /// @dev keccak256 of runtime after CREATE of the committed `.bin`.
    ///      Re-measure (`extcodehash`) when n14 regenerates artefacts.
    bytes32 internal constant PRIMARY_YUL_CODEHASH =
        0x01cce5259fa68848b2ef87bec2bfa40c47089a67967b2e8ea5d7492c0c18fbd6;
    bytes32 internal constant FALLBACK_YUL_CODEHASH =
        0xce215c9aca95eb5c5006615dbfee217d9a3aa0d6847cecef6dcee822ec283c00;
    bytes32 internal constant LAYER_HASHES_YUL_CODEHASH =
        0xd6f78f3b014cf94b0fbc8d60e409955adf86c7f2274c84ce19b745f5bb92525e;
    bytes32 internal constant WITHDRAWAL_YUL_CODEHASH =
        0x8c7a66973776b835349c8053d1b302149162cbece4a7b70e8a5c25c593fee1b5;

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
        return deployYulFromBin(binPath, bytes32(0));
    }

    /// @param expectedCodehash Runtime `extcodehash` pin. Zero skips the check
    ///        (spike fixtures). Production adapters always pass a non-zero pin.
    function deployYulFromBin(string memory binPath, bytes32 expectedCodehash)
        internal
        returns (address yul)
    {
        bytes memory bytecode = VM.readFileBinary(binPath);
        if (bytecode.length == 0) revert VerifierBinEmpty(binPath);
        assembly {
            yul := create(0, add(bytecode, 0x20), mload(bytecode))
        }
        if (yul == address(0)) revert YulDeployFailed();
        if (expectedCodehash != bytes32(0) && yul.codehash != expectedCodehash) {
            revert YulCodehashMismatch(yul.codehash, expectedCodehash);
        }
    }

    function deployShplonkWrapper(address yul) internal returns (address wrapper) {
        return address(new ShplonkHalo2Verifier(yul));
    }

    function deployPrimaryAdapter(string memory binPath) internal returns (IPrimaryVerifier) {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, PRIMARY_YUL_CODEHASH));
        return IPrimaryVerifier(address(new PrimaryAggregatorVerifier(wrapper)));
    }

    /// @notice Circuit 1B fallback attestation — SHPLONK aggregator (K=21 inner).
    function deployFallbackAdapter(string memory binPath) internal returns (IFallbackVerifier) {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, FALLBACK_YUL_CODEHASH));
        return IFallbackVerifier(address(new FallbackAggregatorVerifier(wrapper)));
    }

    function deployLayerHashesAdapter(string memory binPath)
        internal
        returns (ILayerHashesMovementVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, LAYER_HASHES_YUL_CODEHASH));
        return ILayerHashesMovementVerifier(address(new LayerHashesAggregatorVerifier(wrapper)));
    }

    function deployWithdrawalAdapter(string memory binPath)
        internal
        returns (IBridgeWithdrawalVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, WITHDRAWAL_YUL_CODEHASH));
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
        return deployVerifyBlockProduction(
            primaryBinPath(), fallbackBinPath(), layerHashesBinPath()
        );
    }
}
