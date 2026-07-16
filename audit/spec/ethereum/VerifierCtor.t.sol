// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/ShplonkHalo2Verifier.sol";
import "@src/ShplonkAggregatorVerifierBase.sol";
import "@src/PrimaryAggregatorVerifier.sol";
import "@src/FallbackAggregatorVerifier.sol";
import "@src/LayerHashesAggregatorVerifier.sol";
import "@src/BridgeWithdrawalAggregatorVerifier.sol";
import "@src/PrimaryVerifier.sol";
import "@src/LayerHashesMovementVerifier.sol";
import "@src/BridgeWithdrawalVerifier.sol";
import "@src/PrimaryGroth16VerifierGenerated.sol";
import "@src/LayerHashesGroth16VerifierGenerated.sol";

import "./helpers/MockAlwaysTrueGroth16.sol";

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

    function test_primaryGroth16Adapter_zeroAddress_reverts() public {
        vm.expectRevert(PrimaryVerifier.InvalidVerifierAddress.selector);
        new PrimaryVerifier(address(0));
    }

    function test_layerHashesGroth16Adapter_zeroAddress_reverts() public {
        vm.expectRevert(LayerHashesMovementVerifier.InvalidVerifierAddress.selector);
        new LayerHashesMovementVerifier(address(0));
    }

    function test_bridgeWithdrawalGroth16Adapter_zeroAddress_reverts() public {
        vm.expectRevert(BridgeWithdrawalVerifier.InvalidVerifierAddress.selector);
        new BridgeWithdrawalVerifier(address(0));
    }

    function test_verifierCtors_acceptNonZero() public {
        new ShplonkHalo2Verifier(NONZERO);
        new PrimaryAggregatorVerifier(NONZERO);
        new FallbackAggregatorVerifier(NONZERO);
        new LayerHashesAggregatorVerifier(NONZERO);
        new BridgeWithdrawalAggregatorVerifier(NONZERO);
        new PrimaryVerifier(address(new PrimaryGroth16VerifierGenerated()));
        new LayerHashesMovementVerifier(address(new LayerHashesGroth16VerifierGenerated()));
        new BridgeWithdrawalVerifier(address(new MockAlwaysTrueGroth16()));
    }
}
