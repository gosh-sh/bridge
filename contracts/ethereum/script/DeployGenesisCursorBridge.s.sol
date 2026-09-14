// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

/// @title GenesisCursorBridge
/// @notice Thin test/E2E subclass of `AckiNackiBridge` that lets a deployment
///         genesis the AN→ETH layer-bundle cursor (`storedLastSeenBlockSeqNo`)
///         to a NON-zero value.
/// @dev    The base constructor always leaves `storedLastSeenBlockSeqNo == 0`,
///         which forces a fresh bridge to replay a chain segment from its true
///         genesis (the block whose Circuit-1A `last_seen` public input is 0).
///         For a mid-chain replay E2E we only have proofs for a later window, so
///         we seed the cursor to the predecessor of the first replayed block.
///         `storedLastSeenBlockSeqNo` is a `public` (non-private) state var on
///         the base, so this subclass can initialise it directly — no change to
///         the audited base contract, no blast radius on existing constructors.
///         NOT for production: production bridges genesis at 0 and verify the
///         whole chain from its start.
contract GenesisCursorBridge is AckiNackiBridge {
    constructor(
        address _blockHeaderOracle,
        address _usdc,
        address _aavePool,
        address _aUSDC,
        VerifyBlockConfig memory _vb,
        BridgeWithdrawConfig memory _bw,
        uint64 _genesisLastSeenBlockSeqNo
    ) AckiNackiBridge(_blockHeaderOracle, _usdc, _aavePool, _aUSDC, _vb, _bw) {
        storedLastSeenBlockSeqNo = _genesisLastSeenBlockSeqNo;
    }
}

/// @title DeployGenesisCursorBridge
/// @notice Deploy a fresh `GenesisCursorBridge` that REUSES already-deployed
///         Sepolia verifier contracts (Primary/Fallback/LayerHashes/
///         BridgeWithdrawal) and genesis's the layer-bundle cursor to
///         `GENESIS_LAST_SEEN_BLOCK_SEQ_NO`. Used for the AN→ETH mid-chain
///         replay E2E where the local proof window starts above the true chain
///         segment genesis.
contract DeployGenesisCursorBridge is Script {
    address constant USDC_SEPOLIA = 0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8;

    function run() external {
        require(block.chainid != 1, "ETH-5: GenesisCursorBridge is not for mainnet");
        uint256 pk = vm.envUint("PRIVATE_KEY");

        AckiNackiBridge.VerifyBlockConfig memory vb = AckiNackiBridge.VerifyBlockConfig({
            primaryVerifier: IPrimaryVerifier(vm.envAddress("PRIMARY_VERIFIER")),
            fallbackVerifier: IFallbackVerifier(vm.envAddress("FALLBACK_VERIFIER")),
            layerHashesVerifier: ILayerHashesMovementVerifier(
                vm.envAddress("LAYER_HASHES_VERIFIER")
            ),
            genesisBkSetCommitment: vm.envUint("GENESIS_BK_SET_COMMITMENT"),
            genesisPrevMaxLevelLayerHash: vm.envUint("GENESIS_PREV_MAX_LEVEL_LAYER_HASH"),
            genesisLastSeenBlockSeqNo: uint64(vm.envOr("GENESIS_LAST_SEEN_BLOCK_SEQNO", uint256(0)))
        });

        AckiNackiBridge.BridgeWithdrawConfig memory bw = AckiNackiBridge.BridgeWithdrawConfig({
            bridgeWithdrawalVerifier: IBridgeWithdrawalVerifier(
                vm.envAddress("WITHDRAWAL_VERIFIER")
            ),
            dappFr: vm.envUint("WITHDRAW_DAPP_FR"),
            accFr: vm.envUint("WITHDRAW_ACC_FR"),
            altDstChainId: vm.envOr("WITHDRAW_ALT_DST_CHAIN_ID", uint256(1)),
            altDstHostChainId: vm.envOr("WITHDRAW_ALT_DST_HOST_CHAIN_ID", uint256(11_155_111)),
            altTokenId: vm.envOr("WITHDRAW_ALT_TOKEN_ID", uint256(3))
        });

        uint64 genesisLastSeen = uint64(vm.envUint("GENESIS_LAST_SEEN_BLOCK_SEQ_NO"));

        vm.startBroadcast(pk);
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        GenesisCursorBridge bridge = new GenesisCursorBridge(
            address(oracle), USDC_SEPOLIA, address(0), address(0), vb, bw, genesisLastSeen
        );
        vm.stopBroadcast();

        console.log("GenesisCursorBridge:", address(bridge));
        console.log("oracle:", address(oracle));
        console.log("genesisLastSeenBlockSeqNo:", genesisLastSeen);
        console.log("primaryVerifier:", address(vb.primaryVerifier));
        console.log("bridgeWithdrawalVerifier:", address(bw.bridgeWithdrawalVerifier));
    }
}
