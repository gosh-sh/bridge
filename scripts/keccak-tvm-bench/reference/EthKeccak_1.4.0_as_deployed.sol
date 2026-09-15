pragma gosh-solidity >=0.80;

/// @notice Keccak-256 over a byte string (Ethereum `keccak256`) and the
///         parentHash extractor for an execution block-header RLP list.
///         TVM has no keccak builtin (`tvm.hash` is SHA-256 of a cell).
///         Vectors: keccak256("") = c5d24601…d85a470, keccak256("abc") =
///         4e03657a…2d6c45 (same as the relayer's tiny-keccak tests).
library EthKeccak {
    function hash(bytes data) internal pure returns (uint256) {
        uint64[] st;
        uint i;
        for (i = 0; i < 25; i++) {
            st.push(0);
        }
        TvmSlice s = data.toSlice();
        uint8[] buf;
        while (true) {
            if (s.bits() < 8) {
                if (s.refs() == 0) {
                    break;
                }
                s = s.loadRef().toSlice();
                continue;
            }
            buf.push(uint8(s.loadUint(8)));
            if (buf.length == 136) {
                st = _absorb(st, buf);
                st = _keccakf(st);
                buf = emptyBuf();
            }
        }
        buf.push(0x01);
        while (buf.length < 136) {
            buf.push(0);
        }
        buf[buf.length - 1] = uint8(buf[buf.length - 1] | 0x80);
        st = _absorb(st, buf);
        st = _keccakf(st);
        return _squeeze32(st);
    }

    /// @dev First RLP item of an Ethereum block header is `parentHash` (32 bytes).
    function rlpParentHash(bytes header) internal pure returns (uint256) {
        TvmSlice s = header.toSlice();
        require(s.bits() >= 8, 250);
        uint8 p = uint8(s.loadUint(8));
        uint skip;
        if (p <= 0xf7) {
            skip = 0;
        } else {
            skip = uint(p - 0xf7);
        }
        uint k;
        for (k = 0; k < skip; k++) {
            require(s.bits() >= 8, 250);
            s.loadUint(8);
        }
        require(s.bits() >= 8, 250);
        require(uint8(s.loadUint(8)) == 0xa0, 250);
        uint256 out = 0;
        for (k = 0; k < 32; k++) {
            require(s.bits() >= 8, 250);
            out = (out << 8) | uint256(uint8(s.loadUint(8)));
        }
        return out;
    }

    function emptyBuf() private pure returns (uint8[]) {
        uint8[] b;
        return b;
    }

    // TVM copies dynamic arrays on call; return the mutated state.
    function _absorb(uint64[] st, uint8[] buf) private pure returns (uint64[]) {
        uint i;
        uint j;
        for (i = 0; i < 17; i++) {
            uint64 lane = 0;
            for (j = 0; j < 8; j++) {
                lane |= uint64(buf[i * 8 + j]) << uint64(8 * j);
            }
            st[i] = st[i] ^ lane;
        }
        return st;
    }

    function _squeeze32(uint64[] st) private pure returns (uint256 out) {
        uint i;
        uint j;
        for (i = 0; i < 4; i++) {
            uint64 lane = st[i];
            for (j = 0; j < 8; j++) {
                // Lane is little-endian; hash byte 0 is the uint256 MSB.
                out = (out << 8) | uint256(uint8(lane >> uint64(8 * j)));
            }
        }
    }

    function _rotl(uint64 x, uint8 n) private pure returns (uint64) {
        if (n == 0) {
            return x;
        }
        return (x << n) | (x >> (64 - n));
    }

    function _rc(uint r) private pure returns (uint64) {
        if (r == 0) { return 0x0000000000000001; }
        if (r == 1) { return 0x0000000000008082; }
        if (r == 2) { return 0x800000000000808a; }
        if (r == 3) { return 0x8000000080008000; }
        if (r == 4) { return 0x000000000000808b; }
        if (r == 5) { return 0x0000000080000001; }
        if (r == 6) { return 0x8000000080008081; }
        if (r == 7) { return 0x8000000000008009; }
        if (r == 8) { return 0x000000000000008a; }
        if (r == 9) { return 0x0000000000000088; }
        if (r == 10) { return 0x0000000080008009; }
        if (r == 11) { return 0x000000008000000a; }
        if (r == 12) { return 0x000000008000808b; }
        if (r == 13) { return 0x800000000000008b; }
        if (r == 14) { return 0x8000000000008089; }
        if (r == 15) { return 0x8000000000008003; }
        if (r == 16) { return 0x8000000000008002; }
        if (r == 17) { return 0x8000000000000080; }
        if (r == 18) { return 0x000000000000800a; }
        if (r == 19) { return 0x800000008000000a; }
        if (r == 20) { return 0x8000000080008081; }
        if (r == 21) { return 0x8000000000008080; }
        if (r == 22) { return 0x0000000080000001; }
        return 0x8000000080008008;
    }

    function _piln(uint i) private pure returns (uint) {
        if (i == 0) { return 10; }
        if (i == 1) { return 7; }
        if (i == 2) { return 11; }
        if (i == 3) { return 17; }
        if (i == 4) { return 18; }
        if (i == 5) { return 3; }
        if (i == 6) { return 5; }
        if (i == 7) { return 16; }
        if (i == 8) { return 8; }
        if (i == 9) { return 21; }
        if (i == 10) { return 24; }
        if (i == 11) { return 4; }
        if (i == 12) { return 15; }
        if (i == 13) { return 23; }
        if (i == 14) { return 19; }
        if (i == 15) { return 13; }
        if (i == 16) { return 12; }
        if (i == 17) { return 2; }
        if (i == 18) { return 20; }
        if (i == 19) { return 14; }
        if (i == 20) { return 22; }
        if (i == 21) { return 9; }
        if (i == 22) { return 6; }
        return 1;
    }

    function _rotc(uint i) private pure returns (uint8) {
        if (i == 0) { return 1; }
        if (i == 1) { return 3; }
        if (i == 2) { return 6; }
        if (i == 3) { return 10; }
        if (i == 4) { return 15; }
        if (i == 5) { return 21; }
        if (i == 6) { return 28; }
        if (i == 7) { return 36; }
        if (i == 8) { return 45; }
        if (i == 9) { return 55; }
        if (i == 10) { return 2; }
        if (i == 11) { return 14; }
        if (i == 12) { return 27; }
        if (i == 13) { return 41; }
        if (i == 14) { return 56; }
        if (i == 15) { return 8; }
        if (i == 16) { return 25; }
        if (i == 17) { return 43; }
        if (i == 18) { return 62; }
        if (i == 19) { return 18; }
        if (i == 20) { return 39; }
        if (i == 21) { return 61; }
        if (i == 22) { return 20; }
        return 44;
    }

    function _keccakf(uint64[] st) private pure returns (uint64[]) {
        uint round;
        uint i;
        uint j;
        for (round = 0; round < 24; round++) {
            uint64[5] bc;
            for (i = 0; i < 5; i++) {
                bc[i] = st[i] ^ st[i + 5] ^ st[i + 10] ^ st[i + 15] ^ st[i + 20];
            }
            for (i = 0; i < 5; i++) {
                uint64 tt = bc[(i + 4) % 5] ^ _rotl(bc[(i + 1) % 5], 1);
                for (j = 0; j < 25; j += 5) {
                    st[j + i] = st[j + i] ^ tt;
                }
            }
            uint64 t = st[1];
            for (i = 0; i < 24; i++) {
                j = _piln(i);
                uint64 tmp = st[j];
                st[j] = _rotl(t, _rotc(i));
                t = tmp;
            }
            for (j = 0; j < 25; j += 5) {
                for (i = 0; i < 5; i++) {
                    bc[i] = st[j + i];
                }
                for (i = 0; i < 5; i++) {
                    st[j + i] = st[j + i] ^ ((~bc[(i + 1) % 5]) & bc[(i + 2) % 5]);
                }
            }
            st[0] = st[0] ^ _rc(round);
        }
        return st;
    }
}
