// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/**
 * @title Blake2bTranscript
 * @notice Library implementing the Halo2 Blake2b transcript protocol using EIP-152 precompile.
 * @dev Implements the exact same transcript protocol as halo2_proofs::transcript::Blake2bRead/Write:
 *      - Initialization: hash_length=64, personal=b"Halo2-Transcript"
 *      - Point absorption: prefix 0x01 + LE x-coordinate (32 bytes) + LE y-coordinate (32 bytes)
 *      - Scalar absorption: prefix 0x02 + LE scalar (32 bytes)
 *      - Challenge squeeze: prefix 0x00 → clone state → finalize → 64 bytes → from_uniform_bytes → scalar
 *
 *      Blake2b state is stored as:
 *      - h[0..7]: 8 x uint64 = 64 bytes (chained state)
 *      - t[0..1]: 2 x uint64 = 16 bytes (byte counter)
 *      - buf[0..127]: 128 bytes (message buffer)
 *      - buflen: uint64 (current buffer position)
 *
 *      Total state: 64 + 16 + 128 + 8 = 216 bytes
 */
library Blake2bTranscript {
    // Blake2b IV constants (same as SHA-512 IV)
    uint64 constant IV0 = 0x6A09E667F3BCC908;
    uint64 constant IV1 = 0xBB67AE8584CAA73B;
    uint64 constant IV2 = 0x3C6EF372FE94F82B;
    uint64 constant IV3 = 0xA54FF53A5F1D36F1;
    uint64 constant IV4 = 0x510E527FADE682D1;
    uint64 constant IV5 = 0x9B05688C2B3E6C1F;
    uint64 constant IV6 = 0x1F83D9ABFB41BD6B;
    uint64 constant IV7 = 0x5BE0CD19137E2179;

    // BN254 scalar field order
    uint256 constant F_Q = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    // EIP-152 Blake2f precompile address
    address constant BLAKE2F_PRECOMPILE = address(0x09);

    // Transcript prefix bytes (matching halo2_proofs)
    uint8 constant PREFIX_CHALLENGE = 0x00;
    uint8 constant PREFIX_POINT = 0x01;
    uint8 constant PREFIX_SCALAR = 0x02;

    /// @dev Blake2b state struct
    struct State {
        uint64[8] h; // chained state
        uint64[2] t; // byte counter
        bytes buf; // message buffer (up to 128 bytes)
    }

    /// @notice Initialize Blake2b state with Halo2 transcript personalization
    /// @return state The initialized Blake2b state
    function init() internal pure returns (State memory state) {
        // Parameter block: p[0] = 0x01010040 (digest_length=64, key_length=0, fanout=1, depth=1)
        // Personalization "Halo2-Transcript" goes in p[6] and p[7]
        // h[i] = IV[i] ^ p[i]
        state.h[0] = IV0 ^ 0x01010040; // XOR with parameter block word 0
        state.h[1] = IV1; // p[1] = 0
        state.h[2] = IV2; // p[2] = 0
        state.h[3] = IV3; // p[3] = 0
        state.h[4] = IV4; // p[4] = 0
        state.h[5] = IV5; // p[5] = 0
        state.h[6] = IV6 ^ 0x72542d326f6c6148; // "Halo2-Tr" as LE uint64
        state.h[7] = IV7 ^ 0x7470697263736e61; // "anscript" as LE uint64
        state.t[0] = 0;
        state.t[1] = 0;
        state.buf = new bytes(0);
    }

    /// @notice Update the Blake2b state with arbitrary data
    /// @param state The Blake2b state to update
    /// @param data The data to absorb
    function update(State memory state, bytes memory data) internal view {
        uint256 dataLen = data.length;
        uint256 bufLen = state.buf.length;

        for (uint256 i = 0; i < dataLen; i++) {
            if (bufLen == 128) {
                // Buffer is full, compress
                state.t[0] += 128;
                if (state.t[0] < 128) state.t[1]++; // carry
                _compress(state, false);
                // Reset buffer
                state.buf = new bytes(0);
                bufLen = 0;
            }
            // Append byte to buffer
            bytes memory newBuf = new bytes(bufLen + 1);
            for (uint256 j = 0; j < bufLen; j++) {
                newBuf[j] = state.buf[j];
            }
            newBuf[bufLen] = data[i];
            state.buf = newBuf;
            bufLen++;
        }
    }

    /// @notice Absorb a single byte
    function updateByte(State memory state, uint8 b) internal view {
        bytes memory data = new bytes(1);
        data[0] = bytes1(b);
        update(state, data);
    }

    /// @notice Absorb a scalar (LE 32 bytes) with prefix 0x02
    /// @param state The Blake2b state
    /// @param scalarBE The scalar in big-endian format (as stored in EVM)
    function commonScalar(State memory state, uint256 scalarBE) internal view {
        updateByte(state, PREFIX_SCALAR);
        // Convert BE scalar to LE bytes and absorb
        bytes memory leBytes = _toLE32(scalarBE);
        update(state, leBytes);
    }

    /// @notice Absorb a point (LE coordinates) with prefix 0x01
    /// @param state The Blake2b state
    /// @param xBE The x-coordinate in big-endian format
    /// @param yBE The y-coordinate in big-endian format
    function commonPoint(State memory state, uint256 xBE, uint256 yBE) internal view {
        updateByte(state, PREFIX_POINT);
        bytes memory xLE = _toLE32(xBE);
        bytes memory yLE = _toLE32(yBE);
        update(state, xLE);
        update(state, yLE);
    }

    /// @notice Squeeze a challenge from the transcript
    /// @dev Adds prefix 0x00, clones state, finalizes clone, converts 64 bytes to scalar
    /// @param state The Blake2b state (modified: prefix byte is absorbed)
    /// @return challenge The challenge scalar (mod F_Q)
    function squeezeChallenge(State memory state) internal view returns (uint256 challenge) {
        // Add challenge prefix to the ongoing state
        updateByte(state, PREFIX_CHALLENGE);

        // Clone the state for finalization
        State memory clone;
        clone.h = [
            state.h[0],
            state.h[1],
            state.h[2],
            state.h[3],
            state.h[4],
            state.h[5],
            state.h[6],
            state.h[7]
        ];
        clone.t = [state.t[0], state.t[1]];
        clone.buf = new bytes(state.buf.length);
        for (uint256 i = 0; i < state.buf.length; i++) {
            clone.buf[i] = state.buf[i];
        }

        // Finalize the clone
        bytes memory digest = _finalize(clone);

        // Convert 64-byte digest to scalar via from_uniform_bytes
        // Interpret as LE 512-bit integer, reduce mod F_Q
        challenge = _fromUniformBytes(digest);
    }

    /// @dev Finalize Blake2b state and return 64-byte digest
    function _finalize(State memory state) internal view returns (bytes memory) {
        uint256 bufLen = state.buf.length;

        // Update counter with remaining bytes
        state.t[0] += uint64(bufLen);
        if (state.t[0] < uint64(bufLen)) state.t[1]++;

        // Pad buffer to 128 bytes with zeros
        bytes memory padded = new bytes(128);
        for (uint256 i = 0; i < bufLen; i++) {
            padded[i] = state.buf[i];
        }
        state.buf = padded;

        // Compress with last block flag
        _compress(state, true);

        // Extract digest (64 bytes, LE from h[0..7])
        bytes memory digest = new bytes(64);
        for (uint256 i = 0; i < 8; i++) {
            uint64 word = state.h[i];
            for (uint256 j = 0; j < 8; j++) {
                digest[i * 8 + j] = bytes1(uint8(word & 0xFF));
                word >>= 8;
            }
        }
        return digest;
    }

    /// @dev Call EIP-152 Blake2f precompile for compression
    /// @param state The Blake2b state (h and buf must be set)
    /// @param lastBlock Whether this is the last block
    function _compress(State memory state, bool lastBlock) internal view {
        // Build the 213-byte input for EIP-152:
        // [4 bytes rounds=12][64 bytes h][128 bytes m][16 bytes t][1 byte f]
        bytes memory input = new bytes(213);

        // Rounds = 12 (big-endian uint32)
        input[0] = 0x00;
        input[1] = 0x00;
        input[2] = 0x00;
        input[3] = 0x0c;

        // h[0..7] as little-endian uint64s
        for (uint256 i = 0; i < 8; i++) {
            uint64 word = state.h[i];
            for (uint256 j = 0; j < 8; j++) {
                input[4 + i * 8 + j] = bytes1(uint8(word & 0xFF));
                word >>= 8;
            }
        }

        // m[0..127] (message block, already in buffer)
        for (uint256 i = 0; i < 128; i++) {
            input[68 + i] = state.buf[i];
        }

        // t[0..1] as little-endian uint64s
        for (uint256 k = 0; k < 2; k++) {
            uint64 tWord = state.t[k];
            for (uint256 j = 0; j < 8; j++) {
                input[196 + k * 8 + j] = bytes1(uint8(tWord & 0xFF));
                tWord >>= 8;
            }
        }

        // f (last block flag)
        input[212] = lastBlock ? bytes1(0x01) : bytes1(0x00);

        // Call EIP-152 precompile
        bytes memory output = new bytes(64);
        bool success;
        assembly {
            success := staticcall(gas(), 0x09, add(input, 32), 213, add(output, 32), 64)
        }
        require(success, "Blake2f precompile call failed");

        // Parse output back into h[0..7] (LE uint64s)
        for (uint256 i = 0; i < 8; i++) {
            uint64 word = 0;
            for (uint256 j = 0; j < 8; j++) {
                word |= uint64(uint8(output[i * 8 + j])) << uint64(j * 8);
            }
            state.h[i] = word;
        }
    }

    /// @dev Convert a big-endian uint256 to 32 little-endian bytes
    function _toLE32(uint256 valueBE) internal pure returns (bytes memory) {
        bytes memory result = new bytes(32);
        for (uint256 i = 0; i < 32; i++) {
            result[i] = bytes1(uint8(valueBE & 0xFF));
            valueBE >>= 8;
        }
        return result;
    }

    /// @dev Convert 64 LE bytes to a scalar via from_uniform_bytes (mod F_Q)
    /// @param digest 64-byte Blake2b digest
    /// @return scalar The resulting scalar field element
    function _fromUniformBytes(bytes memory digest) internal pure returns (uint256) {
        // Interpret 64 bytes as a little-endian 512-bit integer
        uint256 lo = 0; // lower 256 bits
        uint256 hi = 0; // upper 256 bits

        for (uint256 i = 0; i < 32; i++) {
            lo |= uint256(uint8(digest[i])) << (i * 8);
        }
        for (uint256 i = 0; i < 32; i++) {
            hi |= uint256(uint8(digest[32 + i])) << (i * 8);
        }

        // Compute (hi * 2^256 + lo) mod F_Q
        // First compute hi_mod = hi mod F_Q
        uint256 hiMod = hi % F_Q;
        // Then compute (hiMod * 2^256) mod F_Q
        // 2^256 mod F_Q is a known constant
        // 2^256 mod F_Q = 2^256 - F_Q (since 2^256 > F_Q and 2^256 < 2*F_Q)
        // Actually: 2^256 mod F_Q needs to be computed properly
        // F_Q = 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001
        // 2^256 = 0x10000...0 (257 bits)
        // 2^256 mod F_Q = 2^256 - F_Q * floor(2^256 / F_Q)
        // Since F_Q < 2^254, floor(2^256/F_Q) ≈ 5
        // Let's use: R = 2^256 mod F_Q
        uint256 R = _R_MOD_Q();
        uint256 hiShifted = mulmod(hiMod, R, F_Q);
        uint256 loMod = lo % F_Q;
        return addmod(hiShifted, loMod, F_Q);
    }

    /// @dev Returns 2^256 mod F_Q (precomputed constant)
    function _R_MOD_Q() internal pure returns (uint256) {
        // 2^256 mod 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001
        // = 0x0e0a77c19a07df2f666ea36f7879462e36fc76959f60cd29ac96341c4ffffffb
        return 0x0e0a77c19a07df2f666ea36f7879462e36fc76959f60cd29ac96341c4ffffffb;
    }
}

