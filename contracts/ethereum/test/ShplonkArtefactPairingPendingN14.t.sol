// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkArtefactPairingPendingN14Test
/// @notice ETH-06 quarantine. Primary / Fallback / LayerHashes `.bin` (2026-08-12)
///         do not accept `_calldata.bin` (2026-06-23). Default `forge test`,
///         `make pre-push`, and GitLab `test:solidity` exclude this contract
///         (`--no-match-contract ShplonkArtefactPairingPendingN14`).
///
///         Run explicitly after n14 regen:
///         `forge test --match-contract ShplonkArtefactPairingPendingN14 -vv`
///         All three must PASS before `WIRE_VERIFY_BLOCK=true` on mainnet.
///         Then fold them back into `ShplonkArtefactPairing.t.sol` and drop
///         the exclude flag.
contract ShplonkArtefactPairingPendingN14Test is Test {
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
}
