pragma gosh-solidity >=0.80;
pragma AbiHeader expire;

/// @title BridgeZerostateData
/// @notice Builds the data cell a premined eccUSDCBridge is upgraded with.
///         The tuple below is the one `eccUSDCBridge.onCodeUpgrade` decodes; the
///         two must be changed together, and `scripts/check_zerostate_data_encoder.py`
///         fails if they drift apart.
/// @dev Never deployed: it is executed in tvm-debugger at an arbitrary address,
///      which is why it carries no constructor and its code hash does not matter.
contract BridgeZerostateData {
    function getBridgeData(
        uint256 pubkey, address usdcWallet,
        uint128 totalMinted, uint64 mintNonce, uint64 mintAccumulatorNonce,
        uint32[] mintedKeys, uint128[] mintedValues,
        uint32[] burnedKeys, uint128[] burnedValues,
        TvmCell depositVoucherCode
    ) public pure returns (TvmCell) {
        mapping(uint32 => uint128) totalMintedBridgeByToken;
        mapping(uint32 => uint128) totalBurnedBridgeByToken;
        for (uint i = 0; i < mintedKeys.length; i++) {
            totalMintedBridgeByToken[mintedKeys[i]] = mintedValues[i];
        }
        for (uint i = 0; i < burnedKeys.length; i++) {
            totalBurnedBridgeByToken[burnedKeys[i]] = burnedValues[i];
        }
        // The ninth field mirrors eccUSDCBridge.updateCode's `userCell`
        // passthrough, so onCodeUpgrade decodes one shape on both paths. Empty
        // here: the zerostate carries the voucher code in `depositVoucherCode`.
        TvmCell userCell;
        return abi.encode(pubkey, usdcWallet, totalMinted, mintNonce, mintAccumulatorNonce,
                          totalMintedBridgeByToken, totalBurnedBridgeByToken, depositVoucherCode, userCell);
    }
}
