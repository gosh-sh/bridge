// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/ShplonkAggregatorVerifierBase.sol";
import "../src/PrimaryAggregatorVerifier.sol";
import "../src/FallbackAggregatorVerifier.sol";
import "../src/LayerHashesAggregatorVerifier.sol";
import "../src/BridgeWithdrawalAggregatorVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkAggregatorVkBindingTest
/// @notice Inner-VK Poseidon digest binding. Flipping a bit in the
///         calldata digest slot also changes a public instance, so the
///         Yul verifier rejects that mutant on its own. The tests that
///         actually pin the adapter check feed the *unmodified*
///         committed `_calldata.bin` to an adapter constructed with a
///         wrong `vkDigest`.
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

        assertFalse(v.verifyWithdrawal(cd, pub), "digest mismatch must be rejected");
    }

    function test_withdrawal_wrongPin_unmodifiedCalldata_rejected() public {
        bytes memory cd =
            vm.readFileBinary("verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin");
        address w = ShplonkDeployLib.deployShplonkWrapper(
            ShplonkDeployLib.deployYulFromBin(
                "verifiers/BridgeWithdrawalAggregatorVerifier.bin",
                ShplonkDeployLib.WITHDRAWAL_YUL_CODEHASH
            )
        );
        BridgeWithdrawalAggregatorVerifier bad =
            new BridgeWithdrawalAggregatorVerifier(w, bytes32(uint256(_word(cd, ACC + 11)) ^ 1));
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
        assertFalse(bad.verifyWithdrawal(cd, pub), "wrong pin must reject unmodified calldata");
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

    function test_primary_wrongPin_unmodifiedCalldata_rejected() public {
        bytes memory cd = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier_calldata.bin");
        address w = ShplonkDeployLib.deployShplonkWrapper(
            ShplonkDeployLib.deployYulFromBin(
                "verifiers/PrimaryAggregatorVerifier.bin", ShplonkDeployLib.PRIMARY_YUL_CODEHASH
            )
        );
        PrimaryAggregatorVerifier bad =
            new PrimaryAggregatorVerifier(w, bytes32(uint256(_word(cd, ACC + 4)) ^ 1));
        assertFalse(
            bad.verifyPrimaryAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "wrong pin must reject unmodified calldata"
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

    function test_fallback_wrongPin_unmodifiedCalldata_rejected() public {
        bytes memory cd = vm.readFileBinary("verifiers/FallbackAggregatorVerifier_calldata.bin");
        address w = ShplonkDeployLib.deployShplonkWrapper(
            ShplonkDeployLib.deployYulFromBin(
                "verifiers/FallbackAggregatorVerifier.bin", ShplonkDeployLib.FALLBACK_YUL_CODEHASH
            )
        );
        FallbackAggregatorVerifier bad =
            new FallbackAggregatorVerifier(w, bytes32(uint256(_word(cd, ACC + 4)) ^ 1));
        assertFalse(
            bad.verifyFallbackAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "wrong pin must reject unmodified calldata"
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

    function test_layerHashes_wrongPin_unmodifiedCalldata_rejected() public {
        bytes memory cd = vm.readFileBinary("verifiers/LayerHashesAggregatorVerifier_calldata.bin");
        address w = ShplonkDeployLib.deployShplonkWrapper(
            ShplonkDeployLib.deployYulFromBin(
                "verifiers/LayerHashesAggregatorVerifier.bin",
                ShplonkDeployLib.LAYER_HASHES_YUL_CODEHASH
            )
        );
        LayerHashesAggregatorVerifier bad =
            new LayerHashesAggregatorVerifier(w, bytes32(uint256(_word(cd, ACC + 14)) ^ 1));
        uint256[10] memory hashes;
        for (uint256 i = 0; i < 10; i++) {
            hashes[i] = _word(cd, ACC + 3 + i);
        }
        assertFalse(
            bad.verifyLayerHashesMovement(
                cd,
                _word(cd, ACC),
                _word(cd, ACC + 1),
                _word(cd, ACC + 2),
                hashes,
                _word(cd, ACC + 13)
            ),
            "wrong pin must reject unmodified calldata"
        );
    }

    /// @dev The base constructor must refuse `bytes32(0)` for the digest.
    function test_base_zeroVkDigest_rejected() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVkDigest.selector);
        // The address check runs first; any non-zero verifier address
        // is enough to reach the zero-digest check.
        new BridgeWithdrawalAggregatorVerifier(address(uint160(1)), bytes32(0));
    }

    /// @dev The base constructor must refuse a `_vkDigest` that is not a
    ///      valid Fr (`>= r`). Catches an operator who passes a raw 32-byte
    ///      hash instead of a real Fr digest. A chain id is far below `r`
    ///      and is not what this guard catches.
    function test_base_vkDigestAtModulus_rejected() public {
        bytes32 atModulus =
            bytes32(uint256(0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001));
        vm.expectRevert(ShplonkAggregatorVerifierBase.VkDigestExceedsFieldModulus.selector);
        new BridgeWithdrawalAggregatorVerifier(address(uint160(1)), atModulus);
    }

    function test_base_vkDigestMaxUint256_rejected() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.VkDigestExceedsFieldModulus.selector);
        new BridgeWithdrawalAggregatorVerifier(address(uint160(1)), bytes32(type(uint256).max));
    }

    /// @dev Calldata length is exactly `(12 + NUM_INNER) * 32` — one word
    ///      short of the required trailing digest slot. The adapter's
    ///      `proof.length < (NUM_ACCUMULATOR_INSTANCES + NUM_INNER + 1) * 32`
    ///      guard must return false without reverting inside `_readInstance`.
    function test_withdrawal_calldataShortOfDigestSlot_rejected() public {
        IBridgeWithdrawalVerifier v = ShplonkDeployLib.deployWithdrawalAdapter(
            "verifiers/BridgeWithdrawalAggregatorVerifier.bin"
        );
        // (12 accumulator + 11 inner) * 32 = 736 B. No digest word.
        bytes memory tooShort = new bytes((12 + 11) * 32);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub;
        assertFalse(
            v.verifyWithdrawal(tooShort, pub),
            "calldata one word short of the digest slot must reject, not revert"
        );
    }

    function test_primary_calldataShortOfDigestSlot_rejected() public {
        IPrimaryVerifier v =
            ShplonkDeployLib.deployPrimaryAdapter("verifiers/PrimaryAggregatorVerifier.bin");
        // (12 accumulator + 4 inner) * 32 = 512 B. No digest word.
        bytes memory tooShort = new bytes((12 + 4) * 32);
        assertFalse(
            v.verifyPrimaryAttestation(tooShort, 0, 0, 0, 0),
            "calldata one word short of the digest slot must reject, not revert"
        );
    }

    function test_fallback_calldataShortOfDigestSlot_rejected() public {
        IFallbackVerifier v =
            ShplonkDeployLib.deployFallbackAdapter("verifiers/FallbackAggregatorVerifier.bin");
        bytes memory tooShort = new bytes((12 + 4) * 32);
        assertFalse(
            v.verifyFallbackAttestation(tooShort, 0, 0, 0, 0),
            "calldata one word short of the digest slot must reject, not revert"
        );
    }

    function test_layerHashes_calldataShortOfDigestSlot_rejected() public {
        ILayerHashesMovementVerifier v = ShplonkDeployLib.deployLayerHashesAdapter(
            "verifiers/LayerHashesAggregatorVerifier.bin"
        );
        bytes memory tooShort = new bytes((12 + 14) * 32);
        uint256[10] memory hashes;
        assertFalse(
            v.verifyLayerHashesMovement(tooShort, 0, 0, 0, hashes, 0),
            "calldata one word short of the digest slot must reject, not revert"
        );
    }

    function test_base_vkDigestJustBelowModulus_accepted() public {
        bytes32 justBelow =
            bytes32(uint256(0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000000));
        BridgeWithdrawalAggregatorVerifier v =
            new BridgeWithdrawalAggregatorVerifier(address(uint160(1)), justBelow);
        assertEq(v.vkDigest(), justBelow);
    }
}

