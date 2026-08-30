// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkArtefactPairingTest
/// @notice ETH-6 default-suite half: Circuit 4 `.bin` + `_calldata.bin` must
///         pair. 1A/1B/C2 live in `ShplonkArtefactPairingPendingN14` and are
///         excluded from `forge test` / CI until n14 regen (explicit quarantine,
///         re-review ETH-06).
contract ShplonkArtefactPairingTest is Test {
    uint256 internal constant ACC = 12;

    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "short calldata");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    function test_eth6_withdrawalCalldata_verifies() public {
        bytes memory bin = vm.readFileBinary("verifiers/BridgeWithdrawalAggregatorVerifier.bin");
        require(bin.length > 0, "missing or empty BridgeWithdrawalAggregatorVerifier.bin");
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
