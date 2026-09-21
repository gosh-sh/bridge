// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/IBridgeWithdrawalVerifier.sol";
import "../script/ShplonkDeployLib.sol";

/// @title AckiNackiBridgeProductionWithdrawByProofTest
/// @notice M7 on-chain harness for Circuit 4 (withdrawal): deploys the committed
///         `BridgeWithdrawalAggregatorVerifier.bin` (real SHPLONK aggregator Yul)
///         and feeds it the real aggregator calldata produced by
///         `aggregate-proof` / `export-inner-aggregator`. Proves the *deployed*
///         verifier accepts real Poseidon-inner aggregated calldata — i.e. the
///         withdraw path is cryptographically real, not a mock.
/// @dev Mirrors `AckiNackiBridgeProductionVerifyBlockTest` (which covers 1A/1B/2).
///      The 11 Circuit-4 public inputs are re-exposed inside the calldata at
///      instance slots [12..22], so we extract `WithdrawalPublicInputs` directly
///      from the calldata rather than a sidecar file.
contract AckiNackiBridgeProductionWithdrawByProofTest is Test {
    string internal constant WITHDRAWAL_BIN = "verifiers/BridgeWithdrawalAggregatorVerifier.bin";
    string internal constant WITHDRAWAL_CALLDATA_DEFAULT =
        "verifiers/BridgeWithdrawalAggregatorVerifier_calldata.bin";

    /// Calldata file to feed the deployed verifier. Override with `C4_CALLDATA`
    /// (path relative to `contracts/ethereum`) to exercise a *different* inner
    /// snark's aggregated calldata against the same committed verifier — the M7
    /// universality check.
    function _calldataPath() internal view returns (string memory) {
        return vm.envOr("C4_CALLDATA", WITHDRAWAL_CALLDATA_DEFAULT);
    }

    /// KZG accumulator limb count (snark-verifier-sdk layout) preceding the
    /// re-exposed inner public inputs.
    uint256 internal constant ACC = 12;

    function _binPresent(string memory path) internal view returns (bool) {
        try vm.readFileBinary(path) returns (bytes memory b) {
            return b.length > 0;
        } catch {
            return false;
        }
    }

    function _artefactsPresent() internal view returns (bool) {
        return _binPresent(WITHDRAWAL_BIN) && _binPresent(_calldataPath());
    }

    /// Read the 32-byte word at `wordIndex` (big-endian, matching the adapter's
    /// `_readInstance` = `uint256(bytes32(...))`).
    function _word(bytes memory b, uint256 wordIndex) internal pure returns (uint256 v) {
        uint256 off = wordIndex * 32;
        require(b.length >= off + 32, "calldata too short for word");
        assembly {
            v := mload(add(add(b, 0x20), off))
        }
    }

    function _pubFromCalldata(bytes memory cd)
        internal
        pure
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub)
    {
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
    }

    function test_productionWithdrawal_isolated_verifies() public {
        require(
            _artefactsPresent(),
            "verifiers/BridgeWithdrawalAggregatorVerifier{,_calldata}.bin required"
        );
        IBridgeWithdrawalVerifier verifier =
            ShplonkDeployLib.deployWithdrawalAdapter(WITHDRAWAL_BIN);
        bytes memory cd = vm.readFileBinary(_calldataPath());
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromCalldata(cd);

        assertTrue(
            verifier.verifyWithdrawal(cd, pub),
            "Circuit 4 SHPLONK aggregator calldata must verify on the deployed Yul verifier"
        );
    }

    /// Tampering a byte in the proof region (past the 23 instance words) makes the
    /// SHPLONK pairing fail -> the Yul verifier reverts.
    function test_productionWithdrawal_tamperedProof_reverts() public {
        require(_artefactsPresent(), "C4 verifier artefacts required");
        IBridgeWithdrawalVerifier verifier =
            ShplonkDeployLib.deployWithdrawalAdapter(WITHDRAWAL_BIN);
        bytes memory cd = vm.readFileBinary(_calldataPath());
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromCalldata(cd);

        // Flip the last byte (in the proof region, not an instance word).
        cd[cd.length - 1] = bytes1(uint8(cd[cd.length - 1]) ^ 0xFF);

        // A tampered proof must be rejected — either by a revert inside the Yul
        // pairing or by returning false. Both are acceptable rejections.
        try verifier.verifyWithdrawal(cd, pub) returns (bool ok) {
            assertFalse(ok, "tampered proof must not verify");
        } catch {
            // revert is an acceptable rejection
        }
    }

    /// Tampering a re-exposed public input makes the adapter's instance check fail
    /// (returns false, no revert) before the pairing is even reached.
    function test_productionWithdrawal_mismatchedPub_returnsFalse() public {
        require(_artefactsPresent(), "C4 verifier artefacts required");
        IBridgeWithdrawalVerifier verifier =
            ShplonkDeployLib.deployWithdrawalAdapter(WITHDRAWAL_BIN);
        bytes memory cd = vm.readFileBinary(_calldataPath());
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = _pubFromCalldata(cd);

        pub.amount = pub.amount ^ 1; // lie about the amount

        assertFalse(
            verifier.verifyWithdrawal(cd, pub),
            "amount mismatch vs re-exposed instance must be rejected"
        );
    }
}
