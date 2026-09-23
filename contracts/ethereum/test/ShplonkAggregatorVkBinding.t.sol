// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/ShplonkAggregatorVerifierBase.sol";
import "../src/BridgeWithdrawalAggregatorVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkAggregatorVkBindingTest
/// @notice Shape-preserving impostor negatives for the inner-VK Poseidon digest
///         binding. For each of the four adapters we take the committed
///         `_calldata.bin`, replace the tail digest slot with a different
///         (still non-zero) 32-byte word, and confirm that the adapter's
///         `verifyX(...)` returns `false`. The calldata length and every
///         re-exposed inner instance are preserved, so this exercises the
///         `vkDigest` check specifically — not the length gate, the inner
///         PI checks, or the underlying Yul verify.
contract ShplonkAggregatorVkBindingTest is Test {
    uint256 internal constant ACC = 12;

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short calldata");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    /// @dev Overwrite the 32-byte word at `wordIndex` in-place with `value`.
    function _setWord(bytes memory b, uint256 wordIndex, bytes32 value) internal pure {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short calldata");
        assembly {
            mstore(add(add(b, 0x20), off), value)
        }
    }

    function test_withdrawal_mutatedVkDigest_rejected() public {
        uint256 numInner = 11;
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
        pub.anchorLayer = _word(cd, ACC + 10);

        // Preserve length + inner PIs; corrupt only the digest slot.
        bytes32 original = bytes32(_word(cd, ACC + numInner));
        bytes32 mutated = bytes32(uint256(original) ^ uint256(1));
        assertTrue(mutated != bytes32(0), "mutated must be non-zero");
        _setWord(cd, ACC + numInner, mutated);

        assertFalse(
            v.verifyWithdrawal(cd, pub), "digest mismatch must be rejected before Yul delegation"
        );
    }

    function test_primary_mutatedVkDigest_rejected() public {
        uint256 numInner = 4;
        bytes memory cd = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier_calldata.bin");
        IPrimaryVerifier v =
            ShplonkDeployLib.deployPrimaryAdapter("verifiers/PrimaryAggregatorVerifier.bin");

        bytes32 original = bytes32(_word(cd, ACC + numInner));
        _setWord(cd, ACC + numInner, bytes32(uint256(original) ^ uint256(1)));

        assertFalse(
            v.verifyPrimaryAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "digest mismatch must be rejected"
        );
    }

    function test_fallback_mutatedVkDigest_rejected() public {
        uint256 numInner = 4;
        bytes memory cd = vm.readFileBinary("verifiers/FallbackAggregatorVerifier_calldata.bin");
        IFallbackVerifier v =
            ShplonkDeployLib.deployFallbackAdapter("verifiers/FallbackAggregatorVerifier.bin");

        bytes32 original = bytes32(_word(cd, ACC + numInner));
        _setWord(cd, ACC + numInner, bytes32(uint256(original) ^ uint256(1)));

        assertFalse(
            v.verifyFallbackAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "digest mismatch must be rejected"
        );
    }

    function test_layerHashes_mutatedVkDigest_rejected() public {
        uint256 numInner = 14;
        bytes memory cd = vm.readFileBinary("verifiers/LayerHashesAggregatorVerifier_calldata.bin");
        ILayerHashesMovementVerifier v = ShplonkDeployLib.deployLayerHashesAdapter(
            "verifiers/LayerHashesAggregatorVerifier.bin"
        );
        uint256[10] memory hashes;
        for (uint256 i = 0; i < 10; i++) {
            hashes[i] = _word(cd, ACC + 3 + i);
        }

        bytes32 original = bytes32(_word(cd, ACC + numInner));
        _setWord(cd, ACC + numInner, bytes32(uint256(original) ^ uint256(1)));

        assertFalse(
            v.verifyLayerHashesMovement(
                cd,
                _word(cd, ACC),
                _word(cd, ACC + 1),
                _word(cd, ACC + 2),
                hashes,
                _word(cd, ACC + 13)
            ),
            "digest mismatch must be rejected"
        );
    }

    /// @dev The base constructor must refuse `bytes32(0)` for the digest.
    function test_base_zeroVkDigest_rejected() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVkDigest.selector);
        // Any non-zero verifier address suffices — the digest check fires first.
        new BridgeWithdrawalAggregatorVerifier(address(uint160(1)), bytes32(0));
    }
}

