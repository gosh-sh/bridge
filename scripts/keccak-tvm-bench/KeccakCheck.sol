pragma gosh-solidity >=0.76.1;

import "EthKeccak.sol";

/// @notice Exit-code oracle for `EthKeccak`, so the library can be executed in
///         tvm-cli's local VM (`debug run --tvc`) without any network or keys.
///         Exit 0 = the digest matches the expected value, 201 = it does not,
///         anything else = the library itself threw (50 / 4 are the sold
///         defects fixed in gosh-sh/bridge#36).
contract KeccakCheck {
    function checkEmpty() public pure {
        require(EthKeccak.hash("") == 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470, 201);
    }

    function checkAbc() public pure {
        require(EthKeccak.hash("abc") == 0x4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45, 201);
    }

    function checkHash(bytes data, uint256 expected) public pure {
        require(EthKeccak.hash(data) == expected, 201);
    }

    function checkParent(bytes header, uint256 expected) public pure {
        require(EthKeccak.rlpParentHash(header) == expected, 201);
    }
}
