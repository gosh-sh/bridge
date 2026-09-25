// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../script/ShplonkDeployLib.sol";

/// @title ShplonkVkDigestPinDerivationTest
/// @notice The four `*_VK_DIGEST` constants pinned in `ShplonkDeployLib` must
///         equal the value the aggregator actually emits at instance slot
///         `12 + NUM_INNER` for the real inner circuit — otherwise the
///         adapter constructor accepts a pin no real proof of this build
///         carries, and every honest proof would make the adapter return
///         false (`WithdrawalProofRejected` on the bridge).
///
///         The `<name>_calldata.bin` files under `contracts/ethereum/verifiers/`
///         are the committed aggregator calldata, so word `12 + N` is the
///         digest that calldata carries. Comparing that word against the pasted
///         constant catches a typo, a swap between circuits, or a stale pin
///         left behind after a partial regeneration — the class of mistake
///         `WITHDRAWAL_YUL_CODEHASH` / `YulCodehashMismatch` guards against
///         for the Yul side. No inner snark file is committed here.
///
///         Runs in `.woodpecker/solidity.yaml`'s `forge test` step. Cheap
///         (no keygen, no proof — just a file read and one `assertEq`).
contract ShplonkVkDigestPinDerivationTest is Test {
    /// @dev KZG pairing accumulator limb count (see
    ///      `ShplonkAggregatorVerifierBase.NUM_ACCUMULATOR_INSTANCES`).
    uint256 internal constant ACC = 12;

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (bytes32 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short calldata");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    /// @dev Circuit 1A: 4 re-exposed inner PIs
    ///      (`blockId`, `bkSetCommitment`, `blockSeqNo`, `lastSeenBlockSeqNo`).
    function test_primary_pinnedDigestMatchesCalldata() public view {
        bytes memory cd = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier_calldata.bin");
        require(cd.length > 0, "missing PrimaryAggregatorVerifier_calldata.bin");
        assertEq(
            _word(cd, ACC + 4),
            ShplonkDeployLib.PRIMARY_VK_DIGEST,
            "PRIMARY_VK_DIGEST must equal word 16 of PrimaryAggregatorVerifier_calldata.bin"
        );
    }

    /// @dev Circuit 1B fallback: same 4 inner PIs as Primary.
    function test_fallback_pinnedDigestMatchesCalldata() public view {
        bytes memory cd = vm.readFileBinary("verifiers/FallbackAggregatorVerifier_calldata.bin");
        require(cd.length > 0, "missing FallbackAggregatorVerifier_calldata.bin");
        assertEq(
            _word(cd, ACC + 4),
            ShplonkDeployLib.FALLBACK_VK_DIGEST,
            "FALLBACK_VK_DIGEST must equal word 16 of FallbackAggregatorVerifier_calldata.bin"
        );
    }

    /// @dev Circuit 2 layer-hashes movement: 14 inner PIs. Word 25
    ///      (`ACC + 13`) is `prevMaxLevelLayerHash`. Word 26 is the digest.
    function test_layerHashes_pinnedDigestMatchesCalldata() public view {
        bytes memory cd = vm.readFileBinary("verifiers/LayerHashesAggregatorVerifier_calldata.bin");
        require(cd.length > 0, "missing LayerHashesAggregatorVerifier_calldata.bin");
        assertEq(
            _word(cd, ACC + 14),
            ShplonkDeployLib.LAYER_HASHES_VK_DIGEST,
            "LAYER_HASHES_VK_DIGEST must equal word 26 of LayerHashesAggregatorVerifier_calldata.bin"
        );
    }

    /// @dev Circuit 4 withdrawal: 11 inner PIs (tokenId, amount, recipientHi/Lo,
    ///      dstChainId, senderAccFr, dappFr, accFr, nullifier, finalRoot, anchorLayer).
    function test_withdrawal_pinnedDigestMatchesCalldata() public view {
        bytes memory cd =
            vm.readFileBinary("verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin");
        require(cd.length > 0, "missing BridgeWithdrawalAggregatorVerifier_calldata.bin");
        assertEq(
            _word(cd, ACC + 11),
            ShplonkDeployLib.WITHDRAWAL_VK_DIGEST,
            "WITHDRAWAL_VK_DIGEST must equal word 23 of BridgeWithdrawalAggregatorVerifier_calldata.bin"
        );
    }

    /// @dev Every pin sits strictly below the BN254 scalar-field modulus `r`.
    ///      The base contract's constructor rejects a pin at or above `r`
    ///      (`VkDigestExceedsFieldModulus`); any legitimate Poseidon-over-Fr
    ///      output is by construction `< r`, so a constant that ever fails
    ///      this check is either a typo or a hex/BE mix-up.
    function test_allPins_belowFieldModulus() public pure {
        uint256 r = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;
        assertLt(uint256(ShplonkDeployLib.PRIMARY_VK_DIGEST), r, "PRIMARY_VK_DIGEST >= r");
        assertLt(uint256(ShplonkDeployLib.FALLBACK_VK_DIGEST), r, "FALLBACK_VK_DIGEST >= r");
        assertLt(uint256(ShplonkDeployLib.LAYER_HASHES_VK_DIGEST), r, "LAYER_HASHES_VK_DIGEST >= r");
        assertLt(uint256(ShplonkDeployLib.WITHDRAWAL_VK_DIGEST), r, "WITHDRAWAL_VK_DIGEST >= r");
    }
}
