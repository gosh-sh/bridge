// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/ShplonkHalo2Verifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title ShplonkArtefactPairingTest
/// @notice Each committed SHPLONK `.bin` must accept its committed
///         `_calldata.bin`. All four pairs (1A/1B/C2/C4) are in the default
///         suite after the 2026-09-08 n14 regen.
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
            v.verifyWithdrawal(cd, pub), "Withdrawal .bin must accept its committed calldata"
        );
        uint256 g0 = gasleft();
        bool ok = v.verifyWithdrawal(cd, pub);
        uint256 used = g0 - gasleft();
        assertTrue(ok);
        assertLt(used, VERIFY_GAS_CAP, "Circuit 4 accept must fit VERIFY_GAS_CAP");
    }

    /// @dev A crypto reject must not consume the remaining tx gas.
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
        assertLt(used, VERIFY_GAS_CAP + 200_000, "reject-path gas bound");
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
        assertEq(yul.codehash, pin, "CREATE runtime must match committed extcodehash");
    }

    function test_eth6_primaryCalldata_verifies() public {
        bytes memory bin = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier.bin");
        require(bin.length > 0, "missing PrimaryAggregatorVerifier.bin");
        bytes memory cd = vm.readFileBinary("verifiers/PrimaryAggregatorVerifier_calldata.bin");
        IPrimaryVerifier v =
            ShplonkDeployLib.deployPrimaryAdapter("verifiers/PrimaryAggregatorVerifier.bin");
        assertTrue(
            v.verifyPrimaryAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "Primary .bin must accept its committed calldata"
        );
        uint256 g0 = gasleft();
        bool ok = v.verifyPrimaryAttestation(
            cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
        );
        uint256 used = g0 - gasleft();
        assertTrue(ok);
        assertLt(used, VERIFY_GAS_CAP, "Circuit 1A accept must fit VERIFY_GAS_CAP");
    }

    function test_eth6_fallbackCalldata_verifies() public {
        bytes memory bin = vm.readFileBinary("verifiers/FallbackAggregatorVerifier.bin");
        require(bin.length > 0, "missing FallbackAggregatorVerifier.bin");
        bytes memory cd = vm.readFileBinary("verifiers/FallbackAggregatorVerifier_calldata.bin");
        IFallbackVerifier v =
            ShplonkDeployLib.deployFallbackAdapter("verifiers/FallbackAggregatorVerifier.bin");
        assertTrue(
            v.verifyFallbackAttestation(
                cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
            ),
            "Fallback .bin must accept its committed calldata"
        );
        uint256 g0 = gasleft();
        bool ok = v.verifyFallbackAttestation(
            cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), _word(cd, ACC + 3)
        );
        uint256 used = g0 - gasleft();
        assertTrue(ok);
        assertLt(used, VERIFY_GAS_CAP, "Circuit 1B accept must fit VERIFY_GAS_CAP");
    }

    function test_eth6_layerHashesCalldata_verifies() public {
        bytes memory bin = vm.readFileBinary("verifiers/LayerHashesAggregatorVerifier.bin");
        require(bin.length > 0, "missing LayerHashesAggregatorVerifier.bin");
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
            "LayerHashes .bin must accept its committed calldata"
        );
        uint256 g0 = gasleft();
        bool ok = v.verifyLayerHashesMovement(
            cd, _word(cd, ACC), _word(cd, ACC + 1), _word(cd, ACC + 2), hashes, _word(cd, ACC + 13)
        );
        uint256 used = g0 - gasleft();
        assertTrue(ok);
        assertLt(used, VERIFY_GAS_CAP, "Circuit 2 accept must fit VERIFY_GAS_CAP");
    }

    function test_eth6_primaryYul_extcodehashMatchesPin() public {
        bytes32 pin = 0x01cce5259fa68848b2ef87bec2bfa40c47089a67967b2e8ea5d7492c0c18fbd6;
        address yul =
            ShplonkDeployLib.deployYulFromBin("verifiers/PrimaryAggregatorVerifier.bin", pin);
        assertEq(yul.codehash, pin, "Primary CREATE runtime must match pin");
    }

    function test_eth6_fallbackYul_extcodehashMatchesPin() public {
        bytes32 pin = 0xce215c9aca95eb5c5006615dbfee217d9a3aa0d6847cecef6dcee822ec283c00;
        address yul =
            ShplonkDeployLib.deployYulFromBin("verifiers/FallbackAggregatorVerifier.bin", pin);
        assertEq(yul.codehash, pin, "Fallback CREATE runtime must match pin");
    }

    function test_eth6_layerHashesYul_extcodehashMatchesPin() public {
        bytes32 pin = 0xd6f78f3b014cf94b0fbc8d60e409955adf86c7f2274c84ce19b745f5bb92525e;
        address yul =
            ShplonkDeployLib.deployYulFromBin("verifiers/LayerHashesAggregatorVerifier.bin", pin);
        assertEq(yul.codehash, pin, "LayerHashes CREATE runtime must match pin");
    }
}
