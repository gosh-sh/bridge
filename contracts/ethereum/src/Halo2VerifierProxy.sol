// SPDX-License-Identifier: MIT
pragma solidity 0.8.19;

/**
 * @title Halo2VerifierProxy
 * @notice Proxy contract that coordinates verification across multiple split verifier contracts
 * @dev This contract uses DELEGATECALL to execute verification logic split across multiple contracts.
 *      Each part shares the same memory space, allowing the verification to be split while maintaining
 *      the exact same behavior as a monolithic verifier.
 * 
 * The verification process works as follows:
 * 1. Part1 is called with the proof data - it performs initial setup and first third of computation
 * 2. Part2 is called - it continues computation from where Part1 left off
 * 3. Part3 is called - it completes the computation and performs the final pairing check
 * 
 * All parts share memory via DELEGATECALL, so state is preserved across calls.
 */
contract Halo2VerifierProxy {
    address public immutable part1;
    address public immutable part2;
    address public immutable part3;
    
    error VerificationFailed(uint8 partNumber);
    
    /**
     * @notice Constructor
     * @param _part1 Address of Halo2VerifierPart1
     * @param _part2 Address of Halo2VerifierPart2
     * @param _part3 Address of Halo2VerifierPart3
     */
    constructor(address _part1, address _part2, address _part3) {
        require(_part1 != address(0), "Part1 address cannot be zero");
        require(_part2 != address(0), "Part2 address cannot be zero");
        require(_part3 != address(0), "Part3 address cannot be zero");
        
        part1 = _part1;
        part2 = _part2;
        part3 = _part3;
    }
    
    /**
     * @notice Verify a proof by delegating to the three parts in sequence
     * @dev This function receives the proof data and forwards it to each part via DELEGATECALL.
     *      The memory state is preserved across all three calls, allowing the verification
     *      to work exactly as if it were a single contract.
     */
    fallback(bytes calldata) external returns (bytes memory) {
        // Call Part 1 - Initial setup and first third of computation
        (bool success1, bytes memory result1) = part1.delegatecall(msg.data);
        if (!success1) {
            revert VerificationFailed(1);
        }
        
        // Call Part 2 - Continue computation
        (bool success2, bytes memory result2) = part2.delegatecall(msg.data);
        if (!success2) {
            revert VerificationFailed(2);
        }
        
        // Call Part 3 - Final computation and pairing check
        (bool success3, bytes memory result3) = part3.delegatecall(msg.data);
        if (!success3) {
            revert VerificationFailed(3);
        }
        
        // Return empty bytes on success (matching original verifier behavior)
        return "";
    }
}

