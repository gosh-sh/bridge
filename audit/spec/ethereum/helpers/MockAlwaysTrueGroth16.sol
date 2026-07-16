// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "@src/IBridgeWithdrawalGroth16Verifier.sol";

/// @dev Test double: always accepts — simulates the R15 identity-stub gnark wrapper.
contract MockAlwaysTrueGroth16 is IBridgeWithdrawalGroth16Verifier {
    function verifyProof(uint256[8] calldata, uint256[10] calldata) external pure {
        // empty body = success
    }
}
