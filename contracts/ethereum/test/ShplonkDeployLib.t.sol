// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../script/ShplonkDeployLib.sol";
import "../src/ShplonkHalo2Verifier.sol";

/// @title ShplonkDeployLibTest
/// @notice Validates Yul `.bin` → wrapper → verify() path used by deploy scripts.
contract ShplonkDeployLibTest is Test {
    string internal constant SPIKE_BIN = "test/fixtures/r15_spike/MultiplierSpikeVerifier.bin";
    string internal constant SPIKE_CALLDATA =
        "test/fixtures/r15_spike/multiplier_spike_calldata.bin";

    function test_shplonkWrapper_acceptsSpikeCalldata() public {
        bytes memory bytecode;
        bytes memory calldata_;
        try vm.readFileBinary(SPIKE_BIN) returns (bytes memory b) {
            bytecode = b;
        } catch {
            vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
        }
        if (bytecode.length == 0) {
            vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
        }

        calldata_ = vm.readFileBinary(SPIKE_CALLDATA);
        require(calldata_.length > 0, "empty spike calldata");

        address yul = ShplonkDeployLib.deployYulFromBin(SPIKE_BIN);
        address wrapper = ShplonkDeployLib.deployShplonkWrapper(yul);

        assertTrue(ShplonkHalo2Verifier(wrapper).verify(calldata_));
    }

    function test_shplonkWrapper_rejectsTamperedCalldata() public {
        try vm.readFileBinary(SPIKE_BIN) returns (bytes memory bytecode) {
            if (bytecode.length == 0) {
                vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
            }
        } catch {
            vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
        }

        bytes memory calldata_ = vm.readFileBinary(SPIKE_CALLDATA);
        if (calldata_.length > 32) {
            calldata_[calldata_.length - 1] = bytes1(uint8(calldata_[calldata_.length - 1]) ^ 0xFF);
        }

        address yul = ShplonkDeployLib.deployYulFromBin(SPIKE_BIN);
        address wrapper = ShplonkDeployLib.deployShplonkWrapper(yul);

        assertFalse(ShplonkHalo2Verifier(wrapper).verify(calldata_));
    }

    function test_eth6_wrongExtcodehash_reverts() public {
        bytes32 pin = 0xf3a462e3006568299a439c58c12da7356abf3b73b4ae84c5c168aa0b44ffc18f;
        bytes32 wrong = bytes32(uint256(1));
        vm.expectRevert(
            abi.encodeWithSelector(ShplonkDeployLib.YulCodehashMismatch.selector, pin, wrong)
        );
        this.deployWithdrawalYulWrongPin();
    }

    function deployWithdrawalYulWrongPin() external {
        ShplonkDeployLib.deployYulFromBin(
            "verifiers/BridgeWithdrawalAggregatorVerifier.bin", bytes32(uint256(1))
        );
    }
}
