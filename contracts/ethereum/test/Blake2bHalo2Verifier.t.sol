// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "forge-std/console.sol";
import { Blake2bChallengeComputer } from "../src/Blake2bChallengeComputer.sol";
import { Blake2bHalo2Verifier } from "../src/Blake2bHalo2Verifier.sol";

/**
 * @title Blake2bHalo2VerifierTest
 * @notice Tests for the Blake2b Halo2 verifier (Acki Nacki → Ethereum path)
 * @dev Verifies a Halo2 proof generated using Blake2b transcript (same as Acki Nacki native).
 *      The circuit proves knowledge of x such that Poseidon(x) = h.
 *
 *      The Blake2bHalo2Verifier contract is deployed from pre-compiled bytecode
 *      because foundry's optimizer settings can strip inline assembly memory
 *      operations when using via_ir + memory-safe annotation.
 *
 *      Calldata layout (2016 bytes):
 *        [0x00..0x20):   1 instance (32 bytes BE)
 *        [0x20..0x3e0):  15 EC points (960 bytes, 64 bytes each)
 *        [0x3e0..0x7e0): 32 scalars (1024 bytes, 32 bytes each)
 */
contract Blake2bHalo2VerifierTest is Test {
    address public verifier;
    address public challengeComputer;

    bytes public validCalldata;

    // Calldata size: 1 instance (32) + 15 points (960) + 32 scalars (1024) = 2016 bytes
    uint256 constant EXPECTED_CALLDATA_SIZE = 2016;

    function setUp() public {
        // Deploy challenge computer (uses Solidity library code, safe with via_ir)
        Blake2bChallengeComputer cc = new Blake2bChallengeComputer();
        challengeComputer = address(cc);
        console.log("Blake2bChallengeComputer deployed at:", challengeComputer);
        console.log("ChallengeComputer code size:", challengeComputer.code.length);

        // Deploy verifier from pre-compiled bytecode to avoid optimizer corruption.
        // The creation bytecode includes a constructor that takes the challenge computer address.
        bytes memory creationCode = vm.readFileBinary("test/blake2b_verifier_bytecode.bin");
        // Append ABI-encoded constructor argument (address padded to 32 bytes)
        bytes memory initCode = abi.encodePacked(creationCode, abi.encode(challengeComputer));
        address deployed;
        assembly {
            deployed := create(0, add(initCode, 0x20), mload(initCode))
        }
        require(deployed != address(0), "Failed to deploy Blake2bHalo2Verifier");
        verifier = deployed;
        console.log("Blake2bHalo2Verifier deployed at:", verifier);
        console.log("Verifier code size:", verifier.code.length);

        // Load proof calldata from file
        validCalldata = vm.readFileBinary("../../poseidon-proof/data/evm_calldata.bin");
        assertEq(validCalldata.length, EXPECTED_CALLDATA_SIZE, "Calldata should be 2016 bytes");
    }

    /// @notice Test that the challenge computer returns correct challenges
    function test_ChallengeComputation() public {
        // Call challenge computer directly
        (bool success, bytes memory result) = challengeComputer.call(validCalldata);
        assertTrue(success, "Challenge computation should succeed");
        assertEq(result.length, 256, "Should return 8 x 32 bytes");

        // Decode challenges
        (
            uint256 theta,
            uint256 beta,
            uint256 gamma,
            uint256 y,
            uint256 x,
            uint256 v,
            uint256 u,
            uint256 finalCh
        ) = abi.decode(
            result, (uint256, uint256, uint256, uint256, uint256, uint256, uint256, uint256)
        );

        // Reference values from poseidon-proof/data/challenges.json
        // NOTE: These values must be regenerated when evm_calldata.bin changes
        //       (run: cd poseidon-proof && cargo run --bin generate-calldata)
        assertEq(
            theta,
            0x21580f1e9d03843f7205a18ed719d1f9f9c656a935df1581ffdf97d6f3cacefc,
            "theta mismatch"
        );
        assertEq(
            beta,
            0x22bca4fc875ef46e388ee1849865876fa903d6d86c8ea6e239d3166604f38a9e,
            "beta mismatch"
        );
        assertEq(
            gamma,
            0x1c9d7edbc9e7d52ea8639cf9bc168b5aac08d34967d246c19784db5739563a86,
            "gamma mismatch"
        );
        assertEq(
            y, 0x2ba5c1421005187dfa13a14bb676ae5f1ccaa5a8a1b72074fd0e2f2259fc61b2, "y mismatch"
        );
        assertEq(
            x, 0x2fca5e15d68d783c0f4c6682e93abd8dd516377441a63fa4eebb01ee8ce2bf7c, "x mismatch"
        );
        assertEq(
            v, 0x12e21359724abad91b3eb8c05910ac1ecb470a31ac73c2d1e355065e3bfefc66, "v mismatch"
        );
        assertEq(
            u, 0x1364d2fab1b9089b9e35f5922add2b44a1c459d2191d13d6686e8747e0d4de74, "u mismatch"
        );
        assertEq(
            finalCh,
            0x048d6fbcfc9a6c1ed2c0bed3faf9e28e84ab401da21e2088cf21dd64e4726989,
            "final mismatch"
        );

        console.log("All 8 Blake2b challenges match reference values!");
    }

    /// @notice Test that a valid Blake2b Halo2 proof verifies successfully
    function test_ValidProofVerifies() public {
        (bool success,) = verifier.call(validCalldata);
        assertTrue(success, "Valid Blake2b proof should verify successfully");
        console.log("Valid Blake2b Halo2 proof verified successfully!");
    }

    /// @notice Test with verifier deployed directly from Solidity (not pre-compiled bytecode)
    function test_ValidProofVerifiesDirectDeploy() public {
        // Deploy verifier directly from Solidity source
        Blake2bHalo2Verifier directVerifier = new Blake2bHalo2Verifier(challengeComputer);
        console.log("Direct verifier deployed at:", address(directVerifier));
        console.log("Direct verifier code size:", address(directVerifier).code.length);

        (bool success,) = address(directVerifier).call(validCalldata);
        assertTrue(success, "Valid Blake2b proof should verify with direct deploy");
        console.log("Valid Blake2b Halo2 proof verified with direct deploy!");
    }

    /// @notice Test that a proof with wrong instance is rejected
    function test_WrongInstanceReverts() public {
        bytes memory wrongCalldata = _copyCalldata();
        // Corrupt the instance (first 32 bytes)
        wrongCalldata[31] = bytes1(uint8(wrongCalldata[31]) ^ 0xFF);

        (bool success,) = verifier.call(wrongCalldata);
        assertFalse(success, "Proof with wrong instance should be rejected");
        console.log("Wrong instance correctly rejected");
    }

    /// @notice Test that a corrupted proof point is rejected
    function test_CorruptedProofPointReverts() public {
        bytes memory corruptedCalldata = _copyCalldata();
        // Corrupt a proof point (after the 32-byte instance section)
        corruptedCalldata[0x30] = bytes1(uint8(corruptedCalldata[0x30]) ^ 0xFF);

        (bool success,) = verifier.call(corruptedCalldata);
        assertFalse(success, "Corrupted proof point should be rejected");
        console.log("Corrupted proof point correctly rejected");
    }

    /// @notice Test that a corrupted proof scalar is rejected
    function test_CorruptedProofScalarReverts() public {
        bytes memory corruptedCalldata = _copyCalldata();
        // Corrupt a proof scalar (in the scalar section at 0x3e0+)
        corruptedCalldata[0x3f0] = bytes1(uint8(corruptedCalldata[0x3f0]) ^ 0xFF);

        (bool success,) = verifier.call(corruptedCalldata);
        assertFalse(success, "Corrupted proof scalar should be rejected");
        console.log("Corrupted proof scalar correctly rejected");
    }

    /// @notice Test that empty calldata is rejected
    function test_EmptyCalldataReverts() public {
        bytes memory emptyCalldata = new bytes(0);
        (bool success,) = verifier.call(emptyCalldata);
        assertFalse(success, "Empty calldata should be rejected");
        console.log("Empty calldata correctly rejected");
    }

    /// @dev Helper to copy validCalldata into a mutable bytes array
    function _copyCalldata() internal view returns (bytes memory) {
        bytes memory copy = new bytes(validCalldata.length);
        for (uint256 i = 0; i < validCalldata.length; i++) {
            copy[i] = validCalldata[i];
        }
        return copy;
    }
}

/**
 * @title KeccakHalo2VerifierTest
 * @notice Sanity test: deploy the Keccak verifier and verify a Keccak proof.
 *         This confirms the base verifier + Foundry test infrastructure works.
 */
contract KeccakHalo2VerifierTest is Test {
    address public verifier;
    bytes public validCalldata;

    function setUp() public {
        // Deploy Keccak verifier from deployment bytecode generated by snark-verifier-sdk
        bytes memory deploymentCode =
            vm.readFileBinary("../../poseidon-proof/data/keccak_verifier_deployment.bin");
        address deployed;
        assembly {
            deployed := create(0, add(deploymentCode, 0x20), mload(deploymentCode))
        }
        require(deployed != address(0), "Failed to deploy Keccak verifier");
        verifier = deployed;
        console.log("Keccak verifier deployed at:", verifier);
        console.log("Keccak verifier code size:", verifier.code.length);

        // Load Keccak proof calldata
        validCalldata = vm.readFileBinary("../../poseidon-proof/data/keccak_evm_calldata.bin");
        console.log("Keccak calldata size:", validCalldata.length);
    }

    /// @notice Test that a valid Keccak Halo2 proof verifies in Foundry
    function test_KeccakProofVerifies() public {
        (bool success,) = verifier.call(validCalldata);
        assertTrue(success, "Valid Keccak proof should verify successfully");
        console.log("Valid Keccak Halo2 proof verified successfully in Foundry!");
    }
}
