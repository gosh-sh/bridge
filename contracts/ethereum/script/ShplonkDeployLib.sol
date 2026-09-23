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
/// @dev Production verifyBlock wiring (2026-09-23): Primary, Fallback, and
///      LayerHashes all use SHPLONK aggregators. Every adapter now pins the
///      Poseidon digest of its inner-circuit VK (see
///      `ShplonkAggregatorVerifierBase.vkDigest`); the constants below are
///      extracted from `contracts/ethereum/verifiers/<name>_calldata.bin` at
///      instance slot `12 + NUM_INNER` and must be re-derived from
///      `bridge_evm_aggregator::vk_binding::expected_vk_digest` when the inner
///      snark is rotated.
library ShplonkDeployLib {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    error VerifierBinEmpty(string path);
    error YulDeployFailed();
    /// @notice CREATE'd Yul runtime keccak256 does not match the pin for this
    ///         artefact. Env-path deploys cannot silently swap circuits.
    error YulCodehashMismatch(bytes32 actual, bytes32 expected);

    /// @dev keccak256 of runtime after CREATE of the committed `.bin`.
    ///      Re-measure (`extcodehash`) when n14 regenerates artefacts.
    bytes32 internal constant PRIMARY_YUL_CODEHASH =
        0x87667b88a829e82cd3a840d7840e531cc479b46c383081c906ed6ab77c8f7d7c;
    bytes32 internal constant FALLBACK_YUL_CODEHASH =
        0xea25ba9c1cab6df9616122cb951875a47f41c962ebc699dfb53819215efaf963;
    bytes32 internal constant LAYER_HASHES_YUL_CODEHASH =
        0xe1f47d047e03d59dcacb747fd168efe120da942a84a4e7003d74e8208d99f07b;
    bytes32 internal constant WITHDRAWAL_YUL_CODEHASH =
        0xf3a462e3006568299a439c58c12da7356abf3b73b4ae84c5c168aa0b44ffc18f;

    /// @dev Poseidon digest of the inner-circuit VK witnesses, in the exact
    ///      32-byte layout the aggregator emits at instance slot `12 + NUM_INNER`.
    ///      Adapters pin it as `bytes32 vkDigest` and revert if a proof carries
    ///      any other value at that slot.
    bytes32 internal constant PRIMARY_VK_DIGEST =
        0x2a3a839de41b08a38496074a2b4f04e02daa45bb093ffeb54edf09fef18db89a;
    bytes32 internal constant FALLBACK_VK_DIGEST =
        0x02eabb18cdc35deba417d2a3a9326bb41a722c333d11eccb45f4ae52ccba1484;
    bytes32 internal constant LAYER_HASHES_VK_DIGEST =
        0x022fe6c98b76a4733105a03be905bf3cfcf4cb4373fb856c53ec894a27bb4e17;
    bytes32 internal constant WITHDRAWAL_VK_DIGEST =
        0x1e91c1fe1986129e357231c9b2158797e2feb6fc9caddd9caea846ff5614e0ab;

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
        return IPrimaryVerifier(
            address(new PrimaryAggregatorVerifier(wrapper, PRIMARY_VK_DIGEST))
        );
    }

    /// @notice Circuit 1B fallback attestation — SHPLONK aggregator (K=21 inner).
    function deployFallbackAdapter(string memory binPath) internal returns (IFallbackVerifier) {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, FALLBACK_YUL_CODEHASH));
        return IFallbackVerifier(
            address(new FallbackAggregatorVerifier(wrapper, FALLBACK_VK_DIGEST))
        );
    }

    function deployLayerHashesAdapter(string memory binPath)
        internal
        returns (ILayerHashesMovementVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, LAYER_HASHES_YUL_CODEHASH));
        return ILayerHashesMovementVerifier(
            address(new LayerHashesAggregatorVerifier(wrapper, LAYER_HASHES_VK_DIGEST))
        );
    }

    function deployWithdrawalAdapter(string memory binPath)
        internal
        returns (IBridgeWithdrawalVerifier)
    {
        address wrapper = deployShplonkWrapper(deployYulFromBin(binPath, WITHDRAWAL_YUL_CODEHASH));
        return IBridgeWithdrawalVerifier(
            address(new BridgeWithdrawalAggregatorVerifier(wrapper, WITHDRAWAL_VK_DIGEST))
        );
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
