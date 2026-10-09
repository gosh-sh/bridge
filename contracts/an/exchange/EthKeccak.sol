pragma gosh-solidity >=0.80;

/// @notice The parentHash extractor for an execution block-header RLP list.
library EthKeccak {
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
}
