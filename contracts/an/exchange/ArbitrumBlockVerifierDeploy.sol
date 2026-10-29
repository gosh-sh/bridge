pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

/// @title ArbitrumBlockVerifierDeploy
/// @notice The deploy surface of `ArbitrumBlockVerifier` — its constructor and
///         `initialize`, with the same pragmas, the same state variables in
///         the same order (the initial data, and so the address, is built from
///         them) and the same function ids — for the bridge to deploy that contract from
///         `_l2VerifierCode`. Importing the verifier itself would pull
///         `EthTrie`, and with it the KECCAK256 instruction, into the bridge's
///         code. Never deployed with this code.
contract ArbitrumBlockVerifierDeploy {
    uint256 _l2ChainId;
    uint256 _l1ChainId;
    uint160 _rollup;

    constructor() {}
    function initialize(uint256 l2ChainId, uint256 l1ChainId, uint160 rollup) public {}
}
