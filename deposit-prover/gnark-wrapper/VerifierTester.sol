// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

interface IGroth16Verifier {
    function verifyProof(
        uint256[8] calldata proof,
        uint256[7] calldata input
    ) external view;
}

contract VerifierTester {
    IGroth16Verifier public verifier;
    
    event VerificationResult(bool success, uint256 gasUsed);
    
    constructor(address _verifier) {
        verifier = IGroth16Verifier(_verifier);
    }
    
    function testVerification(
        uint256[8] calldata proof,
        uint256[7] calldata input
    ) external returns (bool, uint256) {
        uint256 gasBefore = gasleft();
        
        try verifier.verifyProof(proof, input) {
            uint256 gasUsed = gasBefore - gasleft();
            emit VerificationResult(true, gasUsed);
            return (true, gasUsed);
        } catch {
            uint256 gasUsed = gasBefore - gasleft();
            emit VerificationResult(false, gasUsed);
            return (false, gasUsed);
        }
    }
    
    function measureGas(
        uint256[8] calldata proof,
        uint256[7] calldata input
    ) external view returns (uint256) {
        uint256 gasBefore = gasleft();
        verifier.verifyProof(proof, input);
        return gasBefore - gasleft();
    }
}

