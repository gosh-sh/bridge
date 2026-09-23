// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/BridgeWithdrawalAggregatorVerifier.sol";
import "../src/PrimaryAggregatorVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";

/// @title ShplonkAggregatorForgeryTest
/// @notice Forgery negatives for R15 aggregator adapters (mock SHPLONK = always false).
contract ShplonkAggregatorForgeryTest is Test {
    /// @dev Codeless address. A high-level call with a return value reverts via
    ///         Solidity's `extcodesize` check — not because staticcall always
    ///         fails. The production adapter uses a low-level staticcall in
    ///         `ShplonkHalo2Verifier`, where a codeless target would return
    ///         success; the real guard is the constructor `extcodesize` check
    ///         (QC-A4-1).
    address internal constant FAILING_SHPLONK = address(0xdead);
    /// @dev Non-zero placeholder — the base contract's constructor rejects
    ///      `bytes32(0)`. These forgery tests trip the length or shape check
    ///      long before the digest check, so any non-zero value works.
    bytes32 internal constant DUMMY_VK_DIGEST = bytes32(uint256(1));

    function test_withdrawalAggregator_rejectsGroth16StubProof() public {
        BridgeWithdrawalAggregatorVerifier v =
            new BridgeWithdrawalAggregatorVerifier(FAILING_SHPLONK, DUMMY_VK_DIGEST);

        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 0,
                amount: 1,
                recipientHi: 1,
                recipientLo: 2,
                dstChainId: 1,
                senderAccFr: 3,
                dappFr: 4,
                accFr: 5,
                nullifier: 6,
                finalRoot: 7,
                anchorLayer: 1
            });

        // 256-byte legacy Groth16 stub — wrong shape for SHPLONK aggregator calldata.
        bytes memory groth16Stub = new bytes(256);
        assertFalse(v.verifyWithdrawal(groth16Stub, pub));
    }

    function test_primaryAggregator_rejectsShortCalldata() public {
        PrimaryAggregatorVerifier v = new PrimaryAggregatorVerifier(FAILING_SHPLONK, DUMMY_VK_DIGEST);
        assertFalse(v.verifyPrimaryAttestation(hex"00", 1, 2, 3, 4));
    }

    function test_mockGroth16Verifier_acceptsStubButShplonkPathDoesNot() public {
        MockBridgeWithdrawalVerifier mock = new MockBridgeWithdrawalVerifier();
        mock.setShouldAccept(true);

        bytes memory groth16Stub = new bytes(256);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 0,
                amount: 1,
                recipientHi: 0,
                recipientLo: 0,
                dstChainId: 1,
                senderAccFr: 0,
                dappFr: 0,
                accFr: 1,
                nullifier: 123,
                finalRoot: 456,
                anchorLayer: 1
            });

        assertTrue(mock.verifyWithdrawal(groth16Stub, pub));

        BridgeWithdrawalAggregatorVerifier shplonk =
            new BridgeWithdrawalAggregatorVerifier(FAILING_SHPLONK, DUMMY_VK_DIGEST);
        assertFalse(shplonk.verifyWithdrawal(groth16Stub, pub));
    }
}
