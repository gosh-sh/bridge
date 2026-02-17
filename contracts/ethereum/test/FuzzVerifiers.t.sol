// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/Groth16Verifier.sol";
import "../src/Groth16DepositVerifier.sol";
import "../src/IAckiNackiVerifier.sol";
import "../src/DummyVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";

/**
 * @title FuzzVerifiers
 * @notice Fuzz testing base for all ZK verifier circuits and bridge logic
 * @dev Uses Foundry's built-in fuzzer to test:
 *      1. Groth16Verifier: random proofs/inputs are always rejected
 *      2. Halo2Verifier: random calldata is always rejected
 *      3. Groth16DepositVerifier: random proofs/inputs are always rejected
 *      4. AckiNackiBridge: deposit/withdrawal invariants hold under fuzzing
 */

// ============================================================
// Groth16Verifier Fuzz Tests
// ============================================================

contract FuzzGroth16VerifierTest is Test {
    Groth16Verifier public verifier;

    /// @dev BN254 scalar field order
    uint256 constant R = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    function setUp() public {
        verifier = new Groth16Verifier();
    }

    /// @notice Random uncompressed proofs with random inputs must always revert
    function testFuzz_RandomProofReverts(uint256[8] calldata proof, uint256[7] calldata input)
        public
    {
        // The chance of a random 256-byte proof being valid is negligible (~1/2^256)
        vm.expectRevert();
        verifier.verifyProof(proof, input);
    }

    /// @notice Random compressed proofs with random inputs must always revert
    function testFuzz_RandomCompressedProofReverts(
        uint256[4] calldata compressedProof,
        uint256[7] calldata input
    ) public {
        vm.expectRevert();
        verifier.verifyCompressedProof(compressedProof, input);
    }

    /// @notice Valid-looking proof structure (points on curve) with random scalars must revert
    /// @dev Uses the generator point G1 = (1, 2) as proof points — structurally valid but
    ///      cryptographically incorrect
    function testFuzz_GeneratorPointProofReverts(uint256[7] calldata input) public {
        // G1 generator = (1, 2), G2 generator coords
        uint256[8] memory proof;
        proof[0] = 1; // Ax (G1 generator)
        proof[1] = 2; // Ay
        // B is G2 generator
        proof[2] = 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2;
        proof[3] = 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed;
        proof[4] = 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acddb9e557b7367;
        proof[5] = 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa;
        proof[6] = 1; // Cx
        proof[7] = 2; // Cy

        vm.expectRevert();
        verifier.verifyProof(proof, input);
    }

    /// @notice Inputs exceeding the scalar field modulus R must revert
    function testFuzz_InputsAboveFieldModulusRevert(uint256[8] calldata proof, uint8 idx) public {
        uint256 index = uint256(idx) % 7;
        uint256[7] memory input;
        // Set one input to R (which is >= R, so invalid)
        input[index] = R;

        vm.expectRevert();
        verifier.verifyProof(proof, input);
    }
}

// ============================================================
// Halo2Verifier Fuzz Tests
// ============================================================

contract FuzzHalo2VerifierTest is Test {
    address public verifier;
    bytes public validCalldata;

    uint256 constant NUM_INSTANCES = 3;
    uint256 constant INSTANCE_SIZE = NUM_INSTANCES * 32; // 96 bytes

    function setUp() public {
        // Deploy Halo2 verifier from pre-compiled bytecode
        bytes memory bytecode = vm.readFileBinary("test/halo2_verifier_bytecode.bin");
        address deployed;
        assembly {
            deployed := create(0, add(bytecode, 0x20), mload(bytecode))
        }
        require(deployed != address(0), "Failed to deploy Halo2Verifier");
        verifier = deployed;

        // Load valid calldata for mutation-based fuzzing
        validCalldata = vm.readFileBinary("test/halo2_proof_calldata.bin");
    }

    /// @notice Completely random calldata must always be rejected
    function testFuzz_RandomCalldataReverts(bytes calldata randomData) public {
        (bool success,) = verifier.call(randomData);
        assertFalse(success, "Random calldata should never verify");
    }

    /// @notice Random calldata of the exact expected length must still be rejected
    function testFuzz_CorrectLengthRandomCalldataReverts(uint256 seed) public {
        bytes memory data = new bytes(validCalldata.length);
        // Fill with pseudo-random bytes
        for (uint256 i = 0; i < data.length; i += 32) {
            bytes32 chunk = keccak256(abi.encodePacked(seed, i));
            uint256 remaining = data.length - i;
            uint256 toCopy = remaining < 32 ? remaining : 32;
            for (uint256 j = 0; j < toCopy; j++) {
                data[i + j] = chunk[j];
            }
        }
        (bool success,) = verifier.call(data);
        assertFalse(success, "Random data of correct length should never verify");
    }

    /// @notice Mutating a single byte in valid calldata must cause rejection
    function testFuzz_SingleByteMutationReverts(uint256 byteIndex, uint8 xorMask) public {
        vm.assume(xorMask != 0); // Must actually change something
        uint256 idx = byteIndex % validCalldata.length;

        bytes memory mutated = _copyCalldata();
        mutated[idx] = bytes1(uint8(mutated[idx]) ^ xorMask);

        (bool success,) = verifier.call(mutated);
        assertFalse(success, "Single byte mutation should invalidate proof");
    }

    /// @notice Mutating a single instance value must cause rejection
    function testFuzz_MutatedInstanceReverts(uint8 instanceIdx, uint256 newValue) public {
        uint256 idx = uint256(instanceIdx) % NUM_INSTANCES;
        // Read the original value and make sure we're changing it
        uint256 originalValue;
        uint256 offset = idx * 32;
        bytes memory data = _copyCalldata();
        assembly {
            originalValue := mload(add(add(data, 0x20), offset))
        }
        vm.assume(newValue != originalValue);

        // Write new value
        assembly {
            mstore(add(add(data, 0x20), offset), newValue)
        }

        (bool success,) = verifier.call(data);
        assertFalse(success, "Mutated instance should invalidate proof");
    }

    /// @notice Truncated calldata must always cause rejection
    /// @dev Note: The Halo2 verifier reads fixed positions from calldata and ignores
    ///      trailing bytes, so appending extra bytes does NOT invalidate verification.
    ///      Only truncation (less data than needed) causes failure.
    function testFuzz_TruncatedCalldataReverts(uint16 truncAmount) public {
        uint256 delta = (uint256(truncAmount) % (validCalldata.length - 1)) + 1; // 1 to len-1
        uint256 newLen = validCalldata.length - delta;
        bytes memory data = new bytes(newLen);
        for (uint256 i = 0; i < newLen; i++) {
            data[i] = validCalldata[i];
        }

        (bool success,) = verifier.call(data);
        assertFalse(success, "Truncated calldata should never verify");
    }

    function _copyCalldata() internal view returns (bytes memory) {
        bytes memory copy = new bytes(validCalldata.length);
        for (uint256 i = 0; i < validCalldata.length; i++) {
            copy[i] = validCalldata[i];
        }
        return copy;
    }
}

// ============================================================
// Groth16DepositVerifier Fuzz Tests
// ============================================================

contract FuzzGroth16DepositVerifierTest is Test {
    Groth16DepositVerifier public depositVerifier;

    function setUp() public {
        Groth16Verifier groth16 = new Groth16Verifier();
        depositVerifier = new Groth16DepositVerifier(address(groth16));
    }

    /// @notice Random proof bytes and random public inputs must never verify
    function testFuzz_RandomProofAndInputsReject(
        bytes calldata proof,
        uint256 depositId,
        uint256 sender,
        uint256 amount,
        uint256 contractAddr,
        uint256 blockHashHigh,
        uint256 blockHashLow
    ) public {
        uint256[] memory inputs = new uint256[](6);
        inputs[0] = depositId;
        inputs[1] = sender;
        inputs[2] = amount;
        inputs[3] = contractAddr;
        inputs[4] = blockHashHigh;
        inputs[5] = blockHashLow;

        (bool isValid,) = depositVerifier.verifyWithdrawalProof(proof, inputs);
        assertFalse(isValid, "Random proof should never verify through Groth16DepositVerifier");
    }

    /// @notice Wrong public input count must always return false
    function testFuzz_WrongInputCountRejects(bytes calldata proof, uint8 count) public {
        uint256 inputCount = uint256(count) % 20; // 0-19, but never 6
        vm.assume(inputCount != 6);

        uint256[] memory inputs = new uint256[](inputCount);
        for (uint256 i = 0; i < inputCount; i++) {
            inputs[i] = i + 1;
        }

        (bool isValid,) = depositVerifier.verifyWithdrawalProof(proof, inputs);
        assertFalse(isValid, "Wrong input count should always be rejected");
    }

    /// @notice Proof of wrong length must always return false
    function testFuzz_WrongProofLengthRejects(uint16 proofLen) public {
        vm.assume(proofLen != 288); // 256 proof + 32 promise_commit
        bytes memory proof = new bytes(proofLen);

        uint256[] memory inputs = new uint256[](6);
        inputs[0] = 1; // depositId
        inputs[1] = uint256(uint160(address(this))); // sender
        inputs[2] = 1 ether; // amount
        inputs[3] = uint256(uint160(address(this))); // contractAddr
        inputs[4] = 123; // blockHashHigh
        inputs[5] = 456; // blockHashLow

        (bool isValid,) = depositVerifier.verifyWithdrawalProof(proof, inputs);
        assertFalse(isValid, "Wrong proof length should always be rejected");
    }
}

// ============================================================
// AckiNackiBridge Fuzz Tests
// ============================================================

contract FuzzAckiNackiBridgeTest is Test {
    AckiNackiBridge public bridge;
    DummyVerifier public verifier;
    MockBlockHeaderOracle public oracle;

    // Events (must match contract)
    event Deposit(
        uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp
    );
    event Withdrawal(
        uint256 indexed depositId, address indexed recipient, uint256 amount, uint256 timestamp
    );

    function setUp() public {
        verifier = new DummyVerifier();
        oracle = new MockBlockHeaderOracle();
        bridge = new AckiNackiBridge(address(verifier), address(oracle));
    }

    /// @notice Any valid deposit amount should succeed and update state correctly
    function testFuzz_DepositAmountInvariants(uint256 amount) public {
        amount = bound(amount, 1, 100 ether); // Within valid range

        address user = address(uint160(uint256(keccak256(abi.encodePacked(amount)))));
        vm.deal(user, amount);

        uint256 counterBefore = bridge.depositCounter();
        uint256 treasuryBefore = bridge.treasuryBalance();
        uint256 bridgeBalBefore = address(bridge).balance;

        vm.prank(user);
        bridge.deposit{ value: amount }();

        assertEq(bridge.depositCounter(), counterBefore + 1, "Counter must increment by 1");
        assertEq(
            bridge.treasuryBalance(), treasuryBefore + amount, "Treasury must increase by amount"
        );
        assertEq(address(bridge).balance, bridgeBalBefore + amount, "Bridge balance must increase");
    }

    /// @notice Amounts outside valid range must revert
    function testFuzz_DepositInvalidAmountReverts(uint256 amount) public {
        // Either 0 or > 100 ether
        vm.assume(amount == 0 || amount > 100 ether);
        address user = address(0xBEEF);
        vm.deal(user, type(uint256).max);
        vm.prank(user);

        if (amount == 0) {
            vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        } else {
            vm.expectRevert(AckiNackiBridge.DepositTooLarge.selector);
        }
        bridge.deposit{ value: amount }();
    }

    /// @notice Multiple deposits from different users should all track correctly
    function testFuzz_MultipleDepositsInvariant(uint8 numDeposits, uint256 seed) public {
        uint256 count = bound(uint256(numDeposits), 1, 20);
        uint256 totalDeposited = 0;

        for (uint256 i = 0; i < count; i++) {
            address user = address(uint160(uint256(keccak256(abi.encodePacked(seed, i)))));
            uint256 amount =
                bound(uint256(keccak256(abi.encodePacked(seed, i, "amount"))), 1, 10 ether);
            vm.deal(user, amount);
            vm.prank(user);
            bridge.deposit{ value: amount }();
            totalDeposited += amount;
        }

        assertEq(bridge.depositCounter(), count, "Counter must match deposit count");
        assertEq(bridge.treasuryBalance(), totalDeposited, "Treasury must equal total deposited");
        assertEq(address(bridge).balance, totalDeposited, "Bridge ETH must equal total deposited");
    }

    /// @notice Double-spend must always revert regardless of parameters
    function testFuzz_DoubleSpendReverts(uint256 amount, uint256 depositId) public {
        amount = bound(amount, 1, 100 ether);

        address user = address(0xCAFE);
        vm.deal(user, amount);
        vm.prank(user);
        bridge.deposit{ value: amount }();

        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;

        // First withdrawal
        bridge.withdraw(payable(user), amount, 0, blockNumber, proof);

        // Second withdrawal with same depositId must revert
        vm.deal(address(bridge), amount); // re-fund for the attempt
        vm.expectRevert(AckiNackiBridge.DepositAlreadyProcessed.selector);
        bridge.withdraw(payable(user), amount, 0, blockNumber, proof);
    }

    /// @notice Withdrawal to zero address must always revert
    function testFuzz_WithdrawToZeroAddressReverts(uint256 amount, uint256 depositId) public {
        amount = bound(amount, 1, 100 ether);
        vm.deal(address(0xBEEF), amount);
        vm.prank(address(0xBEEF));
        bridge.deposit{ value: amount }();

        bytes memory proof = hex"0123456789abcdef";
        uint256 blockNumber = block.number - 1;

        vm.expectRevert(AckiNackiBridge.InvalidRecipient.selector);
        bridge.withdraw(payable(address(0)), amount, depositId, blockNumber, proof);
    }
}

