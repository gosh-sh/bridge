// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/cctp/CctpEndpoint.sol";
import "../src/cctp/CctpHubReceiver.sol";

/// @title WireCctp
/// @notice One owner transaction that routes CCTP. `CCTP_WIRE` selects it.
///         Addresses are 20-byte `0x` values; this script left-pads them to
///         the bytes32 Circle uses. It does not deploy.
///
///         `set-hub` on the source `CctpEndpoint`:
///         `CCTP_ENDPOINT`, `CCTP_TOKEN`, `CCTP_DESTINATION_DOMAIN`,
///         `CCTP_HUB_RECEIVER`. `CCTP_ENABLED` defaults to true.
///
///         `set-source` on the hub `CctpHubReceiver`:
///         `CCTP_RECEIVER`, `CCTP_SOURCE_DOMAIN`, `CCTP_SOURCE_CHAIN_ID`,
///         `CCTP_ADAPTER` (the source endpoint), `CCTP_REMOTE_TOKEN`.
///         `CCTP_ENABLED` defaults to true.
///
///         `set-egress` on the hub `CctpEndpoint`:
///         `CCTP_ENDPOINT`, `CCTP_EGRESS_DOMAIN`, `CCTP_EGRESS_ALLOWED`.
///
///         `set-forwarder` on the hub `CctpHubReceiver`:
///         `CCTP_RECEIVER`, `CCTP_FORWARDER` (the keeper EOA). `CCTP_ENABLED`
///         defaults to true.
contract WireCctp is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        string memory wire = vm.envString("CCTP_WIRE");

        vm.startBroadcast(deployerPrivateKey);
        if (keccak256(bytes(wire)) == keccak256("set-hub")) {
            CctpEndpoint endpoint = CctpEndpoint(vm.envAddress("CCTP_ENDPOINT"));
            endpoint.setHub(
                vm.envAddress("CCTP_TOKEN"),
                uint32(vm.envUint("CCTP_DESTINATION_DOMAIN")),
                _pad(vm.envAddress("CCTP_HUB_RECEIVER")),
                vm.envOr("CCTP_ENABLED", true)
            );
            console.log("setHub on", address(endpoint));
        } else if (keccak256(bytes(wire)) == keccak256("set-source")) {
            CctpHubReceiver receiver = CctpHubReceiver(vm.envAddress("CCTP_RECEIVER"));
            receiver.setSource(
                uint32(vm.envUint("CCTP_SOURCE_DOMAIN")),
                vm.envUint("CCTP_SOURCE_CHAIN_ID"),
                _pad(vm.envAddress("CCTP_ADAPTER")),
                _pad(vm.envAddress("CCTP_REMOTE_TOKEN")),
                vm.envOr("CCTP_ENABLED", true)
            );
            console.log("setSource on", address(receiver));
        } else if (keccak256(bytes(wire)) == keccak256("set-egress")) {
            CctpEndpoint endpoint = CctpEndpoint(vm.envAddress("CCTP_ENDPOINT"));
            endpoint.setEgressDomain(
                uint32(vm.envUint("CCTP_EGRESS_DOMAIN")), vm.envBool("CCTP_EGRESS_ALLOWED")
            );
            console.log("setEgressDomain on", address(endpoint));
        } else if (keccak256(bytes(wire)) == keccak256("set-forwarder")) {
            CctpHubReceiver receiver = CctpHubReceiver(vm.envAddress("CCTP_RECEIVER"));
            receiver.setForwarder(vm.envAddress("CCTP_FORWARDER"), vm.envOr("CCTP_ENABLED", true));
            console.log("setForwarder on", address(receiver));
        } else {
            revert("CCTP_WIRE must be set-hub, set-source, set-egress, or set-forwarder");
        }
        vm.stopBroadcast();
    }

    function _pad(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}
