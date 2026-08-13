// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/ShplonkHalo2Verifier.sol";

/// @title ShplonkEmptyCodeTest
/// @notice Phase C / A4 — QC-A4-1: empty Yul bytecode rejected at wrapper deploy (main #16).
contract ShplonkEmptyCodeTest is Test {
    address internal constant EMPTY_CODE_ADDR = address(0xBEEF);

    function test_shplonkWrapper_rejectsEmptyYulCode() public {
        assertEq(EMPTY_CODE_ADDR.code.length, 0, "precondition: no code");

        vm.expectRevert(ShplonkHalo2Verifier.EmptyYulVerifierCode.selector);
        new ShplonkHalo2Verifier(EMPTY_CODE_ADDR);
    }
}
