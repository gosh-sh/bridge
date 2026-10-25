pragma gosh-solidity >=0.81;
pragma AbiHeader expire;

import "exchange/EthTrie.sol";

/// The two proofs inside ArbitrumBlockVerifier.proveBlock, one call each, so
/// live data from different chains can run (fixtures/live_steps.json); and
/// the code salt the bridge puts on a verifier, so run.sh can install the
/// real verifier code under it.
contract ProofBench {
    constructor() { tvm.accept(); }

    /// The verifier code as `deployL2Verifier` salts it.
    function salted(TvmCell code, uint256 l2ChainId) external pure returns (TvmCell code_) {
        tvm.accept();
        return abi.setCodeSalt(code, abi.encode(l2ChainId));
    }

    /// The hash of the Arbitrum block M an `AssertionCreated` receipt names.
    function assertionBlock(
        bytes l1Header, uint256 l1Hash, uint32 txIndex, bytes[] receiptProof,
        uint160 rollup, uint256 topic0
    ) external pure returns (uint256 blockHashM) {
        tvm.accept();
        (, uint256 receiptsRoot) = EthTrie.headerRoots(l1Header, l1Hash);
        (uint256 key, uint nibbles) = EthTrie.receiptKey(txIndex);
        (TvmSlice r, ) = EthTrie.prove(receiptsRoot, key, nibbles, receiptProof);
        return EthTrie.logWord(r, rollup, topic0, 13);
    }

    /// The hash of block N from the history contract in block M's state.
    function ancestorHash(
        bytes l2Header, uint256 blockHashM, uint160 history,
        bytes[] accountProof, uint256 slot, bytes[] storageProof
    ) external pure returns (uint256 blockHashN) {
        tvm.accept();
        (uint256 stateRoot, ) = EthTrie.headerRoots(l2Header, blockHashM);
        return EthTrie.storageAt(stateRoot, history, accountProof, slot, storageProof);
    }
}
