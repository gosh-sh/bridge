// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import { AckiNackiBridge } from "../src/AckiNackiBridge.sol";
import { NacklToken } from "../src/NacklToken.sol";

contract DeployNacklToken is Script {
    function run() external returns (NacklToken token) {
        uint256 key = vm.envUint("PRIVATE_KEY");
        address bridge = vm.envAddress("NACKL_BRIDGE");
        uint256 supply = vm.envOr("NACKL_INITIAL_SUPPLY", uint256(0));
        address holder = vm.envOr("NACKL_INITIAL_HOLDER", address(0));
        require(supply == 0 || holder != address(0), "NACKL_INITIAL_HOLDER required with a supply");

        vm.startBroadcast(key);
        token = new NacklToken(bridge, holder, supply);
        if (vm.envOr("NACKL_LIST", false)) {
            AckiNackiBridge(bridge).listNackl(token, vm.envUint("NACKL_DAILY_LIMIT"));
        }
        vm.stopBroadcast();
        console.log("NacklToken: %s", address(token));
    }
}
