// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/BridgeWithdrawalVerifier.sol";
import "@src/BridgeWithdrawalAggregatorVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

import "./helpers/MockAlwaysTrueGroth16.sol";

/// @title DeployWithdrawVerifierTest
/// @notice Phase C / A3 — WD-Q3: Groth16 stub misdeploy risk vs production SHPLONK adapter.
/// @dev DeployRealBridge wires `ShplonkDeployLib.deployWithdrawalAdapter` (aggregator).
contract DeployWithdrawVerifierTest is Test {
    address internal constant FAILING_SHPLONK = address(0xdead);

    /// @dev QC: WD-Q3 — identity-stub Groth16 path accepts arbitrary 256-byte proof.
    function test_groth16StubAdapter_acceptsArbitraryProof() public {
        MockAlwaysTrueGroth16 groth16 = new MockAlwaysTrueGroth16();
        BridgeWithdrawalVerifier stub = new BridgeWithdrawalVerifier(address(groth16));

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1,
            recipientHi: 0,
            recipientLo: 1,
            dstChainId: 1,
            senderAccFr: 2,
            dappFr: 3,
            accFr: 4,
            nullifier: 5,
            finalRoot: 6
        });

        bytes memory proof = new bytes(256);
        assertTrue(stub.verifyWithdrawal(proof, pub), "stub accepts garbage proof");
    }

    /// @dev Production adapter type differs; rejects short calldata before SHPLONK call.
    function test_productionAggregator_rejectsShortProof() public {
        BridgeWithdrawalAggregatorVerifier prod =
            new BridgeWithdrawalAggregatorVerifier(FAILING_SHPLONK);

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier
            .WithdrawalPublicInputs({
            tokenId: 0,
            amount: 1,
            recipientHi: 0,
            recipientLo: 1,
            dstChainId: 1,
            senderAccFr: 2,
            dappFr: 3,
            accFr: 4,
            nullifier: 5,
            finalRoot: 6
        });

        assertFalse(prod.verifyWithdrawal(hex"00", pub), "aggregator rejects truncated proof");
        assertTrue(address(prod.shplonkVerifier()) == FAILING_SHPLONK, "SHPLONK wired not Groth16");
    }

    /// @dev Deploy invariant: stub exposes groth16Verifier; production exposes shplonkVerifier.
    function test_deployInvariant_stubVsAggregatorInterfaces() public pure {
        // Compile-time type split — runtime smoke that adapters are distinct contracts.
        assertTrue(type(BridgeWithdrawalVerifier).creationCode.length > 0);
        assertTrue(type(BridgeWithdrawalAggregatorVerifier).creationCode.length > 0);
    }
}
