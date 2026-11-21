// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../../src/AckiNackiBridge.sol";

/// @notice The bridge with an owner listing entrypoint. `AckiNackiBridge` lists USDC alone and
///         exposes no way to add a token; this keeps its per-token accounting under test for the
///         day Acki Nacki has another currency.
contract MultiTokenBridge is AckiNackiBridge {
    constructor(
        address blockHeaderOracle_,
        address usdc_,
        address aavePool_,
        address aUSDC_,
        VerifyBlockConfig memory vb_,
        BridgeWithdrawConfig memory bw_
    ) AckiNackiBridge(blockHeaderOracle_, usdc_, aavePool_, aUSDC_, vb_, bw_) { }

    function listToken(
        address token,
        uint32 tokenId,
        uint8 decimals,
        uint256 tvlCap,
        uint256 dailyLimit
    ) external onlyOwner {
        _listToken(tokenId, token, decimals, tvlCap, dailyLimit, false);
    }
}
