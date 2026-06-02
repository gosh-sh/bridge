// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdtTestLib.sol";
import "./mocks/MockERC20.sol";

/**
 * @title FuzzVerifiers
 * @notice Fuzz testing for surviving verifier surfaces and bridge deposit logic.
 * @dev Phase 4.3 (Decision Log 2026-05-17) retired the legacy
 *      `Groth16Verifier` / `Groth16DepositVerifier` chain. Their dedicated
 *      fuzz suites — `FuzzGroth16VerifierTest`, `FuzzGroth16DepositVerifierTest`,
 *      and the withdraw-flow fuzz tests in `FuzzAckiNackiBridgeTest` — went with
 *      them. What remains here:
 *
 *      1. `FuzzHalo2VerifierTest`: random calldata against the bare Halo2 Yul
 *         verifier (kept as a sanity check on the deposit-prover Halo2 output;
 *         independent of the deleted Groth16 chain).
 *      2. `FuzzAckiNackiBridgeDepositTest`: deposit-side invariants under
 *         fuzzed amounts / multi-user sequences.
 */

// ============================================================
// Halo2Verifier Fuzz Tests
// ============================================================

contract FuzzHalo2VerifierTest is Test {
    address public verifier;
    bytes public validCalldata;

    /// @dev BN254 scalar field order
    uint256 constant R = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;
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
        vm.assume(xorMask != 0);
        uint256 idx = byteIndex % validCalldata.length;

        bytes memory mutated = _copyCalldata();
        mutated[idx] = bytes1(uint8(mutated[idx]) ^ xorMask);

        (bool success,) = verifier.call(mutated);
        assertFalse(success, "Single byte mutation should invalidate proof");
    }

    /// @notice Mutating a single instance value must cause rejection
    /// @dev We bound newValue to [0, R) to avoid values that are congruent to the original
    ///      modulo the BN254 scalar field (the verifier reduces inputs mod R internally).
    function testFuzz_MutatedInstanceReverts(uint8 instanceIdx, uint256 newValue) public {
        uint256 idx = uint256(instanceIdx) % NUM_INSTANCES;
        newValue = bound(newValue, 0, R - 1);
        uint256 originalValue;
        uint256 offset = idx * 32;
        bytes memory data = _copyCalldata();
        assembly {
            originalValue := mload(add(add(data, 0x20), offset))
        }
        vm.assume(newValue != originalValue);

        assembly {
            mstore(add(add(data, 0x20), offset), newValue)
        }

        (bool success,) = verifier.call(data);
        assertFalse(success, "Mutated instance should invalidate proof");
    }

    /// @notice Regression for fuzzer counterexample: instanceIdx=49, newValue=R+1.
    ///         R+1 ≡ 1 (mod R), so the verifier reduces it to 1 — the same as the original
    ///         instance value. The proof still verifies despite different raw bytes.
    ///         The fuzz test must use bound(newValue, 0, R-1) to exclude such values.
    function test_Regression_FieldOverflowInstance() public {
        uint256 idx = 1; // 49 % 3
        uint256 overflowValue = R + 1; // reduces to 1 mod R
        uint256 offset = idx * 32;
        bytes memory data = _copyCalldata();

        uint256 originalValue;
        assembly {
            originalValue := mload(add(add(data, 0x20), offset))
        }
        assertEq(originalValue, 1, "Original instance[1] should be 1");

        assembly {
            mstore(add(add(data, 0x20), offset), overflowValue)
        }

        (bool success,) = verifier.call(data);
        assertTrue(success, "R+1 mod R == 1 == original, so proof must still verify");

        uint256 bounded = bound(overflowValue, 0, R - 1);
        assertTrue(bounded < R, "bound() must constrain to valid field elements");
        assertTrue(bounded != overflowValue, "bound() must change the overflow value");
    }

    /// @notice Truncated calldata must always cause rejection
    /// @dev Note: The Halo2 verifier reads fixed positions from calldata and ignores
    ///      trailing bytes, so appending extra bytes does NOT invalidate verification.
    ///      Only truncation (less data than needed) causes failure.
    function testFuzz_TruncatedCalldataReverts(uint16 truncAmount) public {
        uint256 delta = (uint256(truncAmount) % (validCalldata.length - 1)) + 1;
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
// AckiNackiBridge Deposit Fuzz Tests
// ============================================================

contract FuzzAckiNackiBridgeDepositTest is Test {
    AckiNackiBridge public bridge;
    MockBlockHeaderOracle public oracle;
    MockERC20 public usdt;

    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdt = new MockERC20("Mock USDT", "mUSDT", 6);
        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdt),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
    }

    /// @notice Any valid deposit amount should succeed and update state correctly
    function testFuzz_DepositAmountInvariants(uint256 amount) public {
        amount = bound(amount, 1, 100 * UsdtTestLib.UNIT);

        address user = address(uint160(uint256(keccak256(abi.encodePacked(amount)))));
        usdt.mint(user, amount);

        uint256 counterBefore = bridge.depositCounter();
        uint256 treasuryBefore = bridge.treasuryBalance();
        uint256 bridgeBalBefore = usdt.balanceOf(address(bridge));

        vm.startPrank(user);
        usdt.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        assertEq(bridge.depositCounter(), counterBefore + 1, "Counter must increment by 1");
        assertEq(
            bridge.treasuryBalance(), treasuryBefore + amount, "Treasury must increase by amount"
        );
        assertEq(
            usdt.balanceOf(address(bridge)), bridgeBalBefore + amount, "Bridge USDT must increase"
        );
    }

    /// @notice Amounts outside valid range must revert
    function testFuzz_DepositInvalidAmountReverts(uint256 amount) public {
        vm.assume(amount == 0 || amount > 100 * UsdtTestLib.UNIT);
        address user = address(0xBEEF);
        usdt.mint(user, type(uint256).max);
        vm.startPrank(user);
        usdt.approve(address(bridge), amount);

        if (amount == 0) {
            vm.expectRevert(AckiNackiBridge.InvalidAmount.selector);
        } else {
            vm.expectRevert(AckiNackiBridge.DepositTooLarge.selector);
        }
        bridge.deposit(amount, int8(0), bytes32(uint256(1)));
        vm.stopPrank();
    }

    /// @notice Multiple deposits from different users should all track correctly
    function testFuzz_MultipleDepositsInvariant(uint8 numDeposits, uint256 seed) public {
        uint256 count = bound(uint256(numDeposits), 1, 20);
        uint256 totalDeposited = 0;

        for (uint256 i = 0; i < count; i++) {
            address user = address(uint160(uint256(keccak256(abi.encodePacked(seed, i)))));
            uint256 amount = bound(
                uint256(keccak256(abi.encodePacked(seed, i, "amount"))), 1, 10 * UsdtTestLib.UNIT
            );
            UsdtTestLib.depositUsdt(vm, usdt, bridge, user, amount);
            totalDeposited += amount;
        }

        assertEq(bridge.depositCounter(), count, "Counter must match deposit count");
        assertEq(bridge.treasuryBalance(), totalDeposited, "Treasury must equal total deposited");
        assertEq(
            usdt.balanceOf(address(bridge)),
            totalDeposited,
            "Bridge USDT must equal total deposited"
        );
    }
}
