// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/ShplonkHalo2Verifier.sol";

/// @title ShplonkEmptyCodeTest
/// @notice Phase C / A4 — QC-A4-1: documents unsafe behavior when Yul verifier has no code.
/// @dev INV: A4-INV-6 (target state: verify MUST return false on empty code)
///      Current behavior: staticcall to EOA returns ok==true — this test PINS the bug until fixed.
contract ShplonkEmptyCodeTest is Test {
    /// @dev Any non-zero EOA/empty account — no runtime code.
    address internal constant EMPTY_CODE_ADDR = address(0xBEEF);

    function test_verify_onEmptyCodeAddress_unsafeCurrentBehavior() public {
        assertEq(EMPTY_CODE_ADDR.code.length, 0, "precondition: no code");

        ShplonkHalo2Verifier wrapper = new ShplonkHalo2Verifier(EMPTY_CODE_ADDR);

        // Minimal calldata — content irrelevant when callee has no code.
        bytes memory dummy = hex"00";
        bool ok = wrapper.verify(dummy);

        // QC-A4-1: UNSAFE — documents current behavior; flip to assertFalse after extcodesize fix.
        assertTrue(ok, "QC-A4-1: empty-code staticcall incorrectly returns true");
    }
}
