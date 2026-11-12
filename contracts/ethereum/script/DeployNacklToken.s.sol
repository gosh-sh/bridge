// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/AckiNackiBridge.sol";
import { NacklToken } from "../src/NacklToken.sol";

/// @title DeployNacklToken
/// @notice Deploys ERC20 NACKL with an `AckiNackiBridge` as its owner, the
///         only account that can ever mint it, and optionally lists it on
///         that bridge.
/// @dev Environment:
///      - `NACKL_BRIDGE` (required): the bridge that owns the token.
///      - `NACKL_TEST_TOKEN` (default false): a test token, the only kind that
///        may start with a supply, `NACKL_INITIAL_SUPPLY` base units (9
///        decimals) held by `NACKL_INITIAL_HOLDER`.
///      - `NACKL_LIST` (default false): also call `listNackl` on the bridge,
///        with `NACKL_MAX_TRANSFER`, `NACKL_DAILY_LIMIT` and
///        `NACKL_DELAY_THRESHOLD`; the broadcasting key must be the bridge's
///        owner.
///      - `PRIVATE_KEY`: the deployer.
///      Prints `NACKL_TOKEN=<address>`.
contract DeployNacklToken is Script {
    function run() external returns (NacklToken token) {
        address bridge = vm.envAddress("NACKL_BRIDGE");
        bool testToken = vm.envOr("NACKL_TEST_TOKEN", false);
        address holder = vm.envOr("NACKL_INITIAL_HOLDER", address(0));
        uint256 supply = vm.envOr("NACKL_INITIAL_SUPPLY", uint256(0));
        bool list = vm.envOr("NACKL_LIST", false);

        vm.startBroadcast(vm.envUint("PRIVATE_KEY"));
        token = deploy(bridge, testToken, holder, supply);
        if (list) {
            AckiNackiBridge(bridge)
                .listNackl(
                    address(token),
                    vm.envUint("NACKL_MAX_TRANSFER"),
                    vm.envUint("NACKL_DAILY_LIMIT"),
                    vm.envUint("NACKL_DELAY_THRESHOLD")
                );
        }
        vm.stopBroadcast();
        console.log("NACKL_TOKEN=%s", address(token));
    }

    /// @notice The checks of TOK-4 and TOK-5, then the deployment.
    function deploy(address bridge, bool testToken, address holder, uint256 supply)
        public
        returns (NacklToken)
    {
        require(bridge.code.length != 0, "NACKL_BRIDGE must be a deployed bridge");
        if (!testToken) {
            require(
                supply == 0 && holder == address(0),
                "only a test token (NACKL_TEST_TOKEN=1) starts with a supply"
            );
        } else if (supply != 0) {
            require(holder != address(0), "NACKL_INITIAL_HOLDER required with a supply");
        }
        return new NacklToken(bridge, holder, supply);
    }
}
