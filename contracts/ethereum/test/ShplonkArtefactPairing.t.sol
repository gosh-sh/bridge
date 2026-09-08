// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/ShplonkHalo2Verifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkArtefactPairingTest
/// @notice ETH-6 default-suite half: Circuit 4 `.bin` + `_calldata.bin` must
///         pair. 1A/1B/C2 live in `ShplonkArtefactPairingPendingN14` and are
///         excluded from `forge test` / CI until n14 regen (explicit quarantine,
///         re-review ETH-06).
contract ShplonkArtefactPairingTest is Test {
    uint256 internal constant ACC = 12;
    /// @dev Must match `ShplonkHalo2Verifier.VERIFY_GAS_CAP`.
    uint256 internal constant VERIFY_GAS_CAP = 1_500_000;

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
        uint256 g0 = gasleft();
        bool ok = v.verifyWithdrawal(cd, pub);
        uint256 used = g0 - gasleft();
        assertTrue(ok);
        assertLt(used, VERIFY_GAS_CAP, "ETH-19: Circuit 4 accept must fit VERIFY_GAS_CAP");
    }

    /// @dev ETH-19: a crypto reject must not consume the remaining tx gas.
    function test_eth19_withdrawalReject_staysUnderGasCap() public {
        IBridgeWithdrawalVerifier v = ShplonkDeployLib.deployWithdrawalAdapter(
            "verifiers/BridgeWithdrawalAggregatorVerifier.bin"
        );
        bytes memory cd =
            vm.readFileBinary("verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin");
        require(cd.length > 32, "short calldata");
        cd[cd.length - 1] ^= 0x01;
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
        uint256 g0 = gasleft();
        bool ok = v.verifyWithdrawal(cd, pub);
        uint256 used = g0 - gasleft();
        assertFalse(ok, "mutated proof must fail");
        assertLt(used, VERIFY_GAS_CAP + 200_000, "ETH-19 reject bound");
    }

    function test_eth19_verifyGasCapConstant() public {
        address yul =
            ShplonkDeployLib.deployYulFromBin("verifiers/BridgeWithdrawalAggregatorVerifier.bin");
        ShplonkHalo2Verifier w = ShplonkHalo2Verifier(ShplonkDeployLib.deployShplonkWrapper(yul));
        assertEq(w.VERIFY_GAS_CAP(), VERIFY_GAS_CAP);
    }

    function test_eth6_withdrawalYul_extcodehashMatchesPin() public {
        bytes32 pin = 0x8c7a66973776b835349c8053d1b302149162cbece4a7b70e8a5c25c593fee1b5;
        address yul = ShplonkDeployLib.deployYulFromBin(
            "verifiers/BridgeWithdrawalAggregatorVerifier.bin", pin
        );
        assertEq(yul.codehash, pin, "ETH-06: CREATE runtime must match committed extcodehash");
    }
}
