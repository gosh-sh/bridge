// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/ShplonkHalo2Verifier.sol";
import "@src/ShplonkAggregatorVerifierBase.sol";
import "@src/PrimaryAggregatorVerifier.sol";
import "@src/FallbackAggregatorVerifier.sol";
import "@src/LayerHashesAggregatorVerifier.sol";
import "@src/BridgeWithdrawalAggregatorVerifier.sol";

import "./helpers/MinimalShplonkYul.sol";

/// @title VerifierCtorTest
/// @notice Phase C / A4 — ZK-4: zero-address verifier wiring reverts at construction.
contract VerifierCtorTest is Test {
    address internal constant NONZERO = address(0xBEEF);

    function test_shplonkHalo2Verifier_zeroAddress_reverts() public {
        vm.expectRevert(ShplonkHalo2Verifier.InvalidYulVerifierAddress.selector);
        new ShplonkHalo2Verifier(address(0));
    }

    function test_primaryAggregatorVerifier_zeroAddress_reverts() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVerifierAddress.selector);
        new PrimaryAggregatorVerifier(address(0));
    }

    function test_fallbackAggregatorVerifier_zeroAddress_reverts() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVerifierAddress.selector);
        new FallbackAggregatorVerifier(address(0));
    }

    function test_layerHashesAggregatorVerifier_zeroAddress_reverts() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVerifierAddress.selector);
        new LayerHashesAggregatorVerifier(address(0));
    }

    function test_bridgeWithdrawalAggregatorVerifier_zeroAddress_reverts() public {
        vm.expectRevert(ShplonkAggregatorVerifierBase.InvalidVerifierAddress.selector);
        new BridgeWithdrawalAggregatorVerifier(address(0));
    }

    function test_verifierCtors_acceptNonZero() public {
        address yul = address(new MinimalShplonkYul());
        new ShplonkHalo2Verifier(yul);
        new PrimaryAggregatorVerifier(yul);
        new FallbackAggregatorVerifier(yul);
        new LayerHashesAggregatorVerifier(yul);
        new BridgeWithdrawalAggregatorVerifier(yul);
    }
}
