// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import "../test/mocks/MockERC20.sol";

/// @title ListE2EToken
/// @notice The local-network e2e's second token: a fresh 18-decimal ERC-20 with
///         10 tokens on the broadcaster, listed on `BRIDGE_ADDRESS` as ECC id 4
///         with deposits open. Run by the bridge's owner, on a throwaway bridge.
contract ListE2EToken is Script {
    uint32 internal constant TOKEN_ID = 4;
    uint8 internal constant DECIMALS = 18;
    uint256 internal constant SUPPLY = 10e18;
    uint256 internal constant MAX_TRANSFER = 10e18;
    uint256 internal constant TVL_CAP = 100e18;

    function run() external {
        uint256 key = vm.envUint("PRIVATE_KEY");
        AckiNackiBridge bridge = AckiNackiBridge(vm.envAddress("BRIDGE_ADDRESS"));
        vm.startBroadcast(key);
        MockERC20 token = new MockERC20("E2E Second Token", "E2E2", DECIMALS);
        token.mint(vm.addr(key), SUPPLY);
        bridge.listToken(address(token), TOKEN_ID, DECIMALS, MAX_TRANSFER, TVL_CAP);
        bridge.setTokenDepositsEnabled(TOKEN_ID, true);
        vm.stopBroadcast();
        console.log("Token: %s", address(token));
    }
}
