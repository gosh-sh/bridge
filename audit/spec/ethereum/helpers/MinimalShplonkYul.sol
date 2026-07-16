// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @dev Non-empty bytecode stub for `ShplonkHalo2Verifier` wiring smoke tests (returns empty on staticcall).
contract MinimalShplonkYul {
    fallback() external payable {
        assembly {
            return(0, 0)
        }
    }
}
