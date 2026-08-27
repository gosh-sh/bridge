// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkArtefactPairingTest
/// @notice ETH-6: committed `.bin` + `_calldata.bin` pairs must exist and
///         pairing must be green. No skip if artefacts are missing — CI
///         going green without the production path was the finding.
/// @dev    2026-08-27 committed tree: Circuit 4 PASS; Primary/Fallback/
///         LayerHashes FAIL (Yul 2026-08-12 vs calldata 2026-06-23). That
///         FAIL is the open ETH-6 remainder — regen on n14, then update
///         `verifiers/SHA256SUMS`.
contract ShplonkArtefactPairingTest is Test {
    uint256 internal constant ACC = 12;

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short calldata");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    function _requireBin(string memory path) internal view {
        bytes memory b = vm.readFileBinary(path);
        require(b.length > 0, string.concat("missing or empty ", path));
    }

    function test_eth6_primaryCalldata_verifies() public {
        _requireBin("verifiers/PrimaryAggregatorVerifier.bin");
        bytes memory cd = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier_calldata.bin");
        IPrimaryVerifier v =
            ShplonkDeployLib.deployPrimaryAdapter("verifiers/PrimaryAggregatorVerifier.bin");
        assertTrue(
            v.verifyPrimaryAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "ETH-6: Primary .bin must accept its committed calldata"
        );
    }

    function test_eth6_fallbackCalldata_verifies() public {
        _requireBin("verifiers/FallbackAggregatorVerifier.bin");
        bytes memory cd = vm.readFileBinary("verifiers/FallbackAggregatorVerifier_calldata.bin");
        IFallbackVerifier v =
            ShplonkDeployLib.deployFallbackAdapter("verifiers/FallbackAggregatorVerifier.bin");
        assertTrue(
            v.verifyFallbackAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "ETH-6: Fallback .bin must accept its committed calldata"
        );
    }

    function test_eth6_layerHashesCalldata_verifies() public {
        _requireBin("verifiers/LayerHashesAggregatorVerifier.bin");
        bytes memory cd = vm.readFileBinary("verifiers/LayerHashesAggregatorVerifier_calldata.bin");
        ILayerHashesMovementVerifier v = ShplonkDeployLib.deployLayerHashesAdapter(
            "verifiers/LayerHashesAggregatorVerifier.bin"
        );
        uint256[10] memory hashes;
        for (uint256 i = 0; i < 10; i++) {
            hashes[i] = _word(cd, ACC + 3 + i);
        }
        assertTrue(
            v.verifyLayerHashesMovement(
                cd,
                _word(cd, ACC),
                _word(cd, ACC + 1),
                _word(cd, ACC + 2),
                hashes,
                _word(cd, ACC + 13)
            ),
            "ETH-6: LayerHashes .bin must accept its committed calldata"
        );
    }

    function test_eth6_withdrawalCalldata_verifies() public {
        _requireBin("verifiers/BridgeWithdrawalAggregatorVerifier.bin");
        bytes memory cd =
            vm.readFileBinary("verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin");
        IBridgeWithdrawalVerifier v = ShplonkDeployLib.deployWithdrawalAdapter(
            "verifiers/BridgeWithdrawalAggregatorVerifier.bin"
        );
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub;
        pub.tokenId = _word(cd, ACC + 0);
        pub.amount = _word(cd, ACC + 1);
        pub.recipientHi = _word(cd, ACC + 2);
        pub.recipientLo = _word(cd, ACC + 3);
        pub.dstChainId = _word(cd, ACC + 4);
        pub.senderAccFr = _word(cd, ACC + 5);
        pub.dappFr = _word(cd, ACC + 6);
        pub.accFr = _word(cd, ACC + 7);
        pub.nullifier = _word(cd, ACC + 8);
        pub.finalRoot = _word(cd, ACC + 9);
        assertTrue(
            v.verifyWithdrawal(cd, pub), "ETH-6: Withdrawal .bin must accept its committed calldata"
        );
    }
}
