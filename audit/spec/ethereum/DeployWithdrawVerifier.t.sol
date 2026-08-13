// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/BridgeWithdrawalAggregatorVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

/// @title DeployWithdrawVerifierTest
/// @notice Phase C / A3 — WD-Q3: production SHPLONK aggregator wiring vs misdeploy risk.
/// @dev Groth16 adapter contracts removed on main; overlay documents aggregator-only path.
contract DeployWithdrawVerifierTest is Test {
    address internal constant FAILING_SHPLONK = address(0xdead);

    /// @dev Production adapter rejects short calldata before SHPLONK call.
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
        assertTrue(address(prod.shplonkVerifier()) == FAILING_SHPLONK, "SHPLONK wired");
    }

    function test_deployInvariant_aggregatorCreationCodePresent() public pure {
        assertTrue(type(BridgeWithdrawalAggregatorVerifier).creationCode.length > 0);
    }
}
