// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

/// @title ShplonkSpikeOnChainTest
/// @notice M7 acceptance (spike): real Yul SHPLONK verifier + real aggregated calldata.
contract ShplonkSpikeOnChainTest is Test {
    address public verifier;
    bytes public calldata_;

    string internal constant BIN = "test/fixtures/r15_spike/MultiplierSpikeVerifier.bin";
    string internal constant CALLDATA = "test/fixtures/r15_spike/multiplier_spike_calldata.bin";

    function setUp() public {
        try vm.readFileBinary(BIN) returns (bytes memory bytecode) {
            if (bytecode.length == 0) {
                return;
            }
            address deployed;
            assembly {
                deployed := create(0, add(bytecode, 0x20), mload(bytecode))
            }
            require(deployed != address(0), "Failed to deploy spike verifier");
            verifier = deployed;
            calldata_ = vm.readFileBinary(CALLDATA);
        } catch {
            // Fixtures not generated — run `make generate-spike-artifacts` first.
            verifier = address(0);
        }
    }

    function test_spikeVerifier_acceptsExportedCalldata() public {
        if (verifier == address(0)) {
            vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
        }
        require(calldata_.length > 0, "empty calldata fixture");

        (bool ok,) = verifier.call(calldata_);
        assertTrue(ok, "exported spike calldata must verify on-chain");
    }

    function test_spikeVerifier_rejectsTamperedCalldata() public {
        if (verifier == address(0)) {
            vm.skip(true, "spike fixtures missing (run make generate-spike-artifacts)");
        }
        bytes memory bad = calldata_;
        if (bad.length > 32) {
            bad[bad.length - 1] = bytes1(uint8(bad[bad.length - 1]) ^ 0xFF);
        }
        (bool ok,) = verifier.call(bad);
        assertFalse(ok, "tampered calldata must revert");
    }
}
