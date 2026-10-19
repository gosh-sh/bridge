// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Script.sol";
import "../src/cctp/CctpEndpoint.sol";
import "../src/cctp/CctpHubReceiver.sol";
import "../src/testnet/SepoliaAaveUsdcShim.sol";

/// @title DeployCctp
/// @notice Deploys one CCTP contract. `CCTP_KIND=endpoint` on a source chain
///         (Arbitrum) or on the hub (for `bridgeOut`). `CCTP_KIND=receiver` on
///         the hub, next to an already deployed `AckiNackiBridge`.
///         `CCTP_KIND=aave-shim` on Sepolia only: a `SepoliaAaveUsdcShim` in
///         front of a bridge that custodies the Aave faucet USDC. Deploy the
///         receiver with `DEPOSIT_BRIDGE` set to the shim.
///
///         Routing is a follow-up from the owner, not part of this broadcast:
///         `setHub` / `setEgressDomain` on the endpoint, `setSource` /
///         `setForwarder` on the receiver (`forward` reverts `NotForwarder`
///         until the keeper EOA is set). Circle domain ids are `CctpDomains`
///         (Arbitrum is 3, Ethereum is 0). Confirm messenger and transmitter
///         addresses against Circle's registry — this script does not default
///         them. A receiver deploy needs `TOKEN_MESSENGER` as well as
///         `MESSAGE_TRANSMITTER`.
contract DeployCctp is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        string memory kind = vm.envString("CCTP_KIND");

        vm.startBroadcast(deployerPrivateKey);
        if (keccak256(bytes(kind)) == keccak256("endpoint")) {
            address messenger = vm.envAddress("TOKEN_MESSENGER");
            CctpEndpoint endpoint = new CctpEndpoint(messenger);
            console.log("CctpEndpoint", address(endpoint));
        } else if (keccak256(bytes(kind)) == keccak256("receiver")) {
            address bridge = vm.envAddress("DEPOSIT_BRIDGE");
            address transmitter = vm.envAddress("MESSAGE_TRANSMITTER");
            address messenger = vm.envAddress("TOKEN_MESSENGER");
            uint32 domain = uint32(vm.envUint("CCTP_LOCAL_DOMAIN"));
            CctpHubReceiver receiver = new CctpHubReceiver(bridge, transmitter, messenger, domain);
            console.log("CctpHubReceiver", address(receiver));
        } else if (keccak256(bytes(kind)) == keccak256("aave-shim")) {
            address bridge = vm.envAddress("DEPOSIT_BRIDGE");
            address circleUsdc = vm.envAddress("CIRCLE_USDC");
            address faucet = vm.envAddress("AAVE_FAUCET");
            SepoliaAaveUsdcShim shim = new SepoliaAaveUsdcShim(bridge, circleUsdc, faucet);
            console.log("SepoliaAaveUsdcShim", address(shim));
        } else {
            revert("CCTP_KIND must be endpoint, receiver or aave-shim");
        }
        vm.stopBroadcast();
    }
}
