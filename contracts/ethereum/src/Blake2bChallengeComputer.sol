// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import { Blake2bTranscript } from "./Blake2bTranscript.sol";

/**
 * @title Blake2bChallengeComputer
 * @notice Computes Halo2 Blake2b transcript challenges from proof calldata.
 * @dev This contract is called by Blake2bHalo2Verifier via staticcall to compute
 *      the 8 Fiat-Shamir challenges (theta, beta, gamma, y, x, v, u, final) using the
 *      Blake2b transcript protocol. Separated from the verifier to avoid
 *      stack-too-deep errors when mixing Solidity library code with large assembly blocks.
 *
 *      Calldata layout (2016 bytes):
 *      - [0x00..0x20): 1 instance (32 bytes BE)
 *      - [0x20..0x3e0): 15 EC points (960 bytes, 64 bytes each: x || y)
 *      - [0x3e0..0x7e0): 32 scalars (1024 bytes, 32 bytes each)
 *
 *      VK transcript repr: 0x0025668373177a1f147e93169d619ca679ee18b911a0013b5cf34ce6e5684bba
 */
contract Blake2bChallengeComputer {
    using Blake2bTranscript for Blake2bTranscript.State;

    /// @notice Computes 8 challenges from proof calldata via fallback.
    /// @dev Returns abi.encode(theta, beta, gamma, y, x, v, u, final) = 256 bytes
    fallback(bytes calldata data) external returns (bytes memory) {
        uint256 f_q = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

        Blake2bTranscript.State memory state = Blake2bTranscript.init();

        // Absorb VK transcript repr
        state.commonScalar(0x0025668373177a1f147e93169d619ca679ee18b911a0013b5cf34ce6e5684bba);

        // Absorb instance
        uint256 instance = uint256(bytes32(data[0:32]));
        state.commonScalar(instance % f_q);

        // Absorb 3 advice commitment points → theta
        for (uint256 i = 0; i < 3; i++) {
            uint256 offset = 0x20 + i * 0x40;
            uint256 px = uint256(bytes32(data[offset:offset + 32]));
            uint256 py = uint256(bytes32(data[offset + 32:offset + 64]));
            state.commonPoint(px, py);
        }
        uint256 theta = state.squeezeChallenge();

        // Absorb 2 lookup commitment points → beta, gamma
        for (uint256 i = 0; i < 2; i++) {
            uint256 offset = 0xe0 + i * 0x40;
            uint256 px = uint256(bytes32(data[offset:offset + 32]));
            uint256 py = uint256(bytes32(data[offset + 32:offset + 64]));
            state.commonPoint(px, py);
        }
        uint256 beta = state.squeezeChallenge();
        uint256 gamma = state.squeezeChallenge();

        // Absorb 5 permutation commitment points → y
        for (uint256 i = 0; i < 5; i++) {
            uint256 offset = 0x160 + i * 0x40;
            uint256 px = uint256(bytes32(data[offset:offset + 32]));
            uint256 py = uint256(bytes32(data[offset + 32:offset + 64]));
            state.commonPoint(px, py);
        }
        uint256 y_ch = state.squeezeChallenge();

        // Absorb 3 vanishing commitment points → x
        for (uint256 i = 0; i < 3; i++) {
            uint256 offset = 0x2a0 + i * 0x40;
            uint256 px = uint256(bytes32(data[offset:offset + 32]));
            uint256 py = uint256(bytes32(data[offset + 32:offset + 64]));
            state.commonPoint(px, py);
        }
        uint256 x_ch = state.squeezeChallenge();

        // Absorb 32 evaluation scalars → v, u
        for (uint256 i = 0; i < 32; i++) {
            uint256 offset = 0x360 + i * 0x20;
            uint256 scalar = uint256(bytes32(data[offset:offset + 32]));
            state.commonScalar(scalar % f_q);
        }
        uint256 v_ch = state.squeezeChallenge();
        uint256 u_ch = state.squeezeChallenge();

        // Absorb W point → final challenge
        {
            uint256 wpx = uint256(bytes32(data[0x760:0x780]));
            uint256 wpy = uint256(bytes32(data[0x780:0x7a0]));
            state.commonPoint(wpx, wpy);
        }
        uint256 final_ch = state.squeezeChallenge();

        return abi.encode(theta, beta, gamma, y_ch, x_ch, v_ch, u_ch, final_ch);
    }
}

