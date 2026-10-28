pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./EthTrie.sol";
import "./eccUSDCBridge.sol";

/// @title ArbitrumBlockVerifier
/// @notice Admits blocks of one Arbitrum chain into the bridge's
///         `_acceptedBlockHash`, so deposits made on that chain can be
///         finalized without the owner anchoring them.
///
///         A block is proven through the state assertion the chain posts on
///         its parent chain (Ethereum for Arbitrum One, Sepolia for Arbitrum
///         Sepolia). The assertion is taken as soon as it is posted, without
///         waiting for its confirmation: for v1 a false assertion is treated
///         as impossible (none has been challenged in Arbitrum One's history,
///         and posting one locks a 3 600 ETH bond against a bridge capped at
///         10 000 USDC per chain).
///
///         `proveBlock` takes, in one call:
///           1. the parent-chain block header and the receipt-trie proof of
///              the transaction that emitted `AssertionCreated` from the
///              rollup contract. The header's hash must already be accepted
///              for the parent chain — the bridge checks that when it receives
///              the result — and the event gives the hash of the Arbitrum
///              block M the assertion ends at
///              (`assertion.afterState.globalState.bytes32Vals[0]`);
///           2. block M's header and an `eth_getProof` answer, at M, for the
///              EIP-2935 history contract's slot of block N. The slot holds
///              the hash of N, an ancestor of M.
///         The hash of N goes to the bridge, which admits it for this chain.
///
///         Permissionless: the keccak checks are the authorization. Like the
///         light client, it pays its own gas and mints the shell for it.
///
///         Deployed by the bridge only (`deployL2Verifier`), at an address
///         derived from its code salted with the Arbitrum chain id, so the
///         bridge recognizes it without an address being set by hand.
contract ArbitrumBlockVerifier {
    string constant version = "1.0.0";

    uint16 constant ERR_INVALID_SENDER = 310;
    uint16 constant ERR_ALREADY_INITIALIZED = 311;
    uint16 constant ERR_WRONG_CODE_SALT = 312;
    uint16 constant ERR_ZERO_CONFIG = 313;
    uint16 constant ERR_NOT_INITIALIZED = 314;
    uint16 constant ERR_EMPTY_SLOT = 315;

    /// keccak256 of the `AssertionCreated` event signature (nitro-contracts
    /// `IRollupCore`, BoLD).
    uint256 constant ASSERTION_CREATED =
        0x901c3aee23cf4478825462caaab375c606ab83516060388344f0650340753630;
    /// Data word of `AssertionCreated` holding the end block hash: the
    /// non-indexed `AssertionInputs` is 19 static words (before-state data 7,
    /// before state 6, after state 6), and the after state's
    /// `globalState.bytes32Vals[0]` is its first word.
    uint constant ASSERTION_BLOCK_HASH_WORD = 13;
    /// EIP-2935 history contract as ArbOS 40 deploys it, and its window.
    uint160 constant HISTORY = 0x0000F90827F1C53a10cb7A02335B175320002935;
    uint256 constant HISTORY_WINDOW = 393168;

    uint64 constant SINK_VALUE = 1 vmshell;
    uint64 constant MIN_BALANCE = 100 vmshell;

    address constant USDC_BRIDGE_ADDRESS =
        address.makeAddrStd(0, 0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a);

    uint256 _l2ChainId;
    uint256 _l1ChainId;
    uint160 _rollup;

    modifier accept() {
        tvm.accept();
        _;
    }

    constructor() accept {
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
    }

    /// @param l2ChainId — EIP-155 id of the Arbitrum chain; must equal the code salt.
    /// @param l1ChainId — EIP-155 id of the chain its rollup contract lives on.
    /// @param rollup    — the rollup contract that emits `AssertionCreated`.
    function initialize(uint256 l2ChainId, uint256 l1ChainId, uint160 rollup) public accept {
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
        require(_l2ChainId == 0, ERR_ALREADY_INITIALIZED);
        require(l2ChainId != 0 && l1ChainId != 0 && rollup != 0, ERR_ZERO_CONFIG);
        optional(TvmCell) salt = abi.codeSalt(tvm.code());
        require(salt.hasValue() && abi.decode(salt.get(), uint256) == l2ChainId,
                ERR_WRONG_CODE_SALT);
        _l2ChainId = l2ChainId;
        _l1ChainId = l1ChainId;
        _rollup = rollup;
    }

    function ensureBalance() private pure {
        uint64 needed = MIN_BALANCE + SINK_VALUE;
        if (address(this).balance >= needed) { return; }
        gosh.mintshellq(needed);
    }

    /// @notice Proves block `blockNumber` of the Arbitrum chain through an
    ///         assertion and hands its hash to the bridge.
    /// @param l1Header     — RLP of the parent-chain block with the assertion.
    /// @param txIndex      — index of the `AssertionCreated` transaction in it.
    /// @param receiptProof — receipt-trie nodes from the root to that receipt.
    /// @param l2Header     — RLP of block M, the block the assertion ends at.
    /// @param accountProof — `eth_getProof(HISTORY, [slot], M).accountProof`.
    /// @param blockNumber  — N, the block to admit; its slot is N % 393168.
    /// @param storageProof — `eth_getProof(...).storageProof[0].proof`.
    function proveBlock(
        bytes l1Header,
        uint32 txIndex,
        bytes[] receiptProof,
        bytes l2Header,
        bytes[] accountProof,
        uint64 blockNumber,
        bytes[] storageProof
    ) public {
        require(_l2ChainId != 0, ERR_NOT_INITIALIZED);
        tvm.accept();
        ensureBalance();
        uint256 l1Hash = gosh.keccak256(l1Header);
        (, uint256 receiptsRoot) = EthTrie.headerRoots(l1Header, l1Hash);
        (uint256 key, uint nibbles) = EthTrie.receiptKey(txIndex);
        (TvmSlice receipt, ) = EthTrie.prove(receiptsRoot, key, nibbles, receiptProof);
        uint256 m = EthTrie.logWord(receipt, _rollup, ASSERTION_CREATED, ASSERTION_BLOCK_HASH_WORD);
        (uint256 stateRoot, ) = EthTrie.headerRoots(l2Header, m);
        uint256 n = EthTrie.storageAt(
            stateRoot, HISTORY, accountProof, uint256(blockNumber) % HISTORY_WINDOW, storageProof);
        require(n != 0, ERR_EMPTY_SLOT);
        // bounce: true so a refusal (the parent-chain block is not accepted
        // yet) returns the value; nothing is stored here, so a retry is the
        // same call once the light client has caught up.
        eccUSDCBridge(USDC_BRIDGE_ADDRESS).acceptBlockHashFromL2Verifier{
            value: SINK_VALUE,
            bounce: true,
            flag: 1
        }(_l2ChainId, l1Hash, n);
    }

    function getConfig() external view returns (uint256 l2ChainId, uint256 l1ChainId, uint160 rollup) {
        return (_l2ChainId, _l1ChainId, _rollup);
    }

    function getVersion() external pure returns (string, string) {
        return (version, "ArbitrumBlockVerifier");
    }
}
