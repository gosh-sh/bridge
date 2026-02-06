// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";
// import "./Halo2Verifier.sol";  // Halo2Verifier is deployed as raw bytecode
import "poseidon-solidity/PoseidonT3.sol";

/**
 * @title DummyVerifier
 * @notice ⚠️ TEST VERIFIER - NOT SECURE FOR PRODUCTION
 * @dev This is a test-only verifier that accepts any valid proof format.
 *      Replace with real Halo2 verifier before mainnet deployment.
 *
 *      Circuit Logic (deposit event proof):
 *      ====================================
 *      The circuit proves that a Deposit event was emitted by the bridge contract.
 *      It uses axiom-eth to verify Ethereum's Merkle Patricia Trie (MPT) proofs.
 *
 *      Public inputs:
 *        - depositId: Unique deposit identifier from the event
 *        - sender: Original depositor address from the event
 *        - amount: Deposit amount in wei from the event
 *        - contractAddress: Bridge contract address that emitted the event
 *        - blockHashHigh: High 128 bits of the block hash
 *        - blockHashLow: Low 128 bits of the block hash
 *
 *      Circuit constraints:
 *        1. Verify receipt exists in Ethereum's receipt trie (MPT proof)
 *        2. Extract Deposit event from receipt logs
 *        3. Verify event was emitted by the correct contract address
 *        4. Verify event parameters match public inputs
 *
 *      The Halo2 verifier performs BN254 pairing checks to verify the proof.
 */
contract DummyVerifier is IAckiNackiVerifier {
    // The real Halo2 verifier contract (deployed as raw bytecode)
    address public immutable HALO2_VERIFIER;

    // Expected number of public inputs for deposit proof
    // FIX BC-PROVER-003: Updated from 4 to 6 to match circuit's actual public outputs
    // Public inputs: [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
    uint256 private constant PUBLIC_INPUTS_COUNT = 6;

    constructor(address _halo2Verifier) {
        HALO2_VERIFIER = _halo2Verifier;
    }

    /**
     * @notice Verify a withdrawal proof using Halo2 ZK-SNARK verification
     * @dev This performs REAL cryptographic verification using BN254 pairing checks.
     *
     *      The Halo2 verifier expects calldata in this format:
     *      - Bytes 0-31:   public input 0 (depositId)
     *      - Bytes 32-63:  public input 1 (sender)
     *      - Bytes 64-95:  public input 2 (amount)
     *      - Bytes 96-127: public input 3 (contractAddress)
     *      - Bytes 128-159: public input 4 (blockHashHigh)
     *      - Bytes 160-191: public input 5 (blockHashLow)
     *      - Bytes 192+:   proof data (elliptic curve points, scalars, etc.)
     *
     *      The verifier performs pairing checks to verify that:
     *      - The prover knows the Ethereum receipt containing the Deposit event
     *      - The receipt exists in Ethereum's receipt trie (MPT proof)
     *      - The event was emitted by the correct contract address
     *      - The event parameters match the public inputs
     *
     * @param proof The Halo2 proof bytes (cryptographic proof data only)
     * @param publicInputs Array of public inputs [depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow]
     * @return isValid True if the proof passes pairing checks
     * @return depositId The deposit ID from public inputs
     */
    function verifyWithdrawalProof(bytes calldata proof, uint256[] calldata publicInputs)
        external
        override
        returns (bool isValid, bytes32 depositId)
    {
        // Validate proof is not empty
        // Halo2 proofs are typically 2272 bytes, but we allow some flexibility
        // Sanity check: real Halo2 proofs are typically 1-10 KB
        // This catches obvious errors (empty proof, wrong data type, etc.)
        if (proof.length == 0 || proof.length < 100) {
            return (false, bytes32(0));
        }

        // Validate public inputs count
        if (publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }

        // Extract and validate depositId
        bytes32 depositIdValue = bytes32(publicInputs[0]);
        if (depositIdValue == bytes32(0) || publicInputs[1] == 0 || publicInputs[2] == 0 || publicInputs[3] == 0) {
            return (false, bytes32(0));
        }

        // Format calldata for Halo2Verifier:
        // The verifier expects: [public_input_0 || ... || public_input_5 || proof_data]
        // Which is: [depositId || sender || amount || contractAddress || blockHashHigh || blockHashLow || proof_data]
        bytes memory verifierCalldata = abi.encodePacked(
            depositIdValue, // publicInputs[0]: deposit ID
            bytes32(publicInputs[1]), // publicInputs[1]: sender address
            bytes32(publicInputs[2]), // publicInputs[2]: deposit amount
            bytes32(publicInputs[3]), // publicInputs[3]: contract address
            bytes32(publicInputs[4]), // publicInputs[4]: block hash high 128 bits
            bytes32(publicInputs[5]), // publicInputs[5]: block hash low 128 bits
            proof // cryptographic proof data (elliptic curve points, etc.)
        );

        // Call the Halo2 verifier
        // The verifier will check that the proof is valid for the given public inputs
        (bool success,) = HALO2_VERIFIER.call(verifierCalldata);

        if (!success) {
            return (false, bytes32(0));
        }

        return (true, depositIdValue);
    }

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}
