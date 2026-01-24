// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";
// import "./Halo2Verifier.sol";  // Halo2Verifier is deployed as raw bytecode
import "poseidon-solidity/PoseidonT3.sol";

/**
 * @title DummyVerifier
 * @notice Real ZK-SNARK verifier using Halo2 for withdrawal proofs
 * @dev This wraps the generated Halo2Verifier and implements the IAckiNackiVerifier interface.
 *
 *      Circuit Logic (withdrawal proof):
 *      ==================================
 *      Private inputs (witnesses):
 *        - withdrawal_hash: Secret value known only to depositor
 *        - nullifier_preimage: Secret value for nullifier derivation
 *        - merkle_proof: Merkle path proving commitment is in the tree
 *
 *      Public inputs:
 *        - recipient: Withdrawal recipient address
 *        - amount: Withdrawal amount
 *        - root: Merkle tree root
 *
 *      Public outputs:
 *        - nullifier: Computed as Poseidon(withdrawal_hash, nullifier_preimage)
 *
 *      Circuit constraints:
 *        1. nullifier = Poseidon(withdrawal_hash, nullifier_preimage)
 *        2. commitment = Poseidon(withdrawal_hash, nullifier)
 *        3. Verify commitment exists in Merkle tree with given root
 *
 *      The Halo2 verifier performs BN254 pairing checks to verify the proof.
 */
contract DummyVerifier is IAckiNackiVerifier {
    // The real Halo2 verifier contract (deployed as raw bytecode)
    address public immutable halo2Verifier;

    // Expected number of public inputs for withdrawal proof
    // Public inputs/outputs: [nullifier, recipient, amount, root]
    // Note: nullifier is a public OUTPUT computed inside the circuit from private inputs
    uint256 private constant PUBLIC_INPUTS_COUNT = 4;

    constructor(address _halo2Verifier) {
        halo2Verifier = _halo2Verifier;
    }

    /**
     * @notice Verify a withdrawal proof using Halo2 ZK-SNARK verification
     * @dev This performs REAL cryptographic verification using BN254 pairing checks.
     *
     *      The Halo2 verifier expects calldata in this format:
     *      - Bytes 0-31:   public input 0 (nullifier - public output from circuit)
     *      - Bytes 32-63:  public input 1 (recipient)
     *      - Bytes 64-95:  public input 2 (amount)
     *      - Bytes 96-127: public input 3 (root)
     *      - Bytes 128+:   proof data (elliptic curve points, scalars, etc.)
     *
     *      The verifier performs pairing checks to verify that:
     *      - The prover knows private inputs (withdrawal_hash, nullifier_preimage, merkle_proof)
     *      - These inputs satisfy the circuit constraints
     *      - The nullifier is correctly computed from private inputs
     *      - The commitment exists in the Merkle tree
     *
     * @param proof The Halo2 proof bytes (cryptographic proof data only)
     * @param publicInputs Array of public inputs/outputs [nullifier, recipient, amount, root]
     *                     Note: nullifier is computed inside the circuit from private inputs
     * @return isValid True if the proof passes pairing checks
     * @return nullifier The nullifier from public inputs (public output from circuit)
     */
    function verifyWithdrawalProof(
        bytes calldata proof,
        uint256[] calldata publicInputs
    ) external override returns (bool isValid, bytes32 nullifier) {
        // Validate proof is not empty
        // Halo2 proofs are typically 2272 bytes, but we allow some flexibility
        // Minimum reasonable proof size is at least 100 bytes
        if (proof.length == 0 || proof.length < 100) {
            return (false, bytes32(0));
        }

        // Validate public inputs count
        if (publicInputs.length != PUBLIC_INPUTS_COUNT) {
            return (false, bytes32(0));
        }

        // Extract public inputs/outputs
        // publicInputs[0] = nullifier (public OUTPUT computed in circuit from private inputs)
        // publicInputs[1] = recipient (public INPUT)
        // publicInputs[2] = amount (public INPUT)
        // publicInputs[3] = root (public INPUT)
        bytes32 nullifier_value = bytes32(publicInputs[0]);
        uint256 recipient = publicInputs[1];
        uint256 amount = publicInputs[2];
        uint256 root = publicInputs[3];

        // Validate public inputs are non-zero
        if (nullifier_value == bytes32(0) || recipient == 0 || amount == 0 || root == 0) {
            return (false, bytes32(0));
        }

        // Format calldata for Halo2Verifier:
        // The verifier expects: [public_input_0 || public_input_1 || public_input_2 || public_input_3 || proof_data]
        // Which is: [nullifier || recipient || amount || root || proof_data]
        bytes memory verifierCalldata = abi.encodePacked(
            nullifier_value,      // public output from circuit
            bytes32(recipient),   // public input
            bytes32(amount),      // public input
            bytes32(root),        // public input
            proof                 // cryptographic proof data (elliptic curve points, etc.)
        );

        // Call the Halo2 verifier
        // The verifier will check that the proof is valid for the given public inputs
        (bool success, ) = halo2Verifier.call(verifierCalldata);

        if (!success) {
            return (false, bytes32(0));
        }

        return (true, nullifier_value);
    }

    /**
     * @notice Get the expected number of public inputs
     * @return uint256 The number of public inputs expected by the verifier
     */
    function getPublicInputsCount() external pure override returns (uint256) {
        return PUBLIC_INPUTS_COUNT;
    }
}
