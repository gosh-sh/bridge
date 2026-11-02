// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../script/PredecessorEnv.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockERC20.sol";
import "./mocks/MockPredecessorBridge.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "../src/MockBlockHeaderOracle.sol";

contract PredecessorEnvReader {
    function legacy() external view returns (IPredecessorBridge[] memory) {
        return PredecessorEnv.readLegacy();
    }
}

/// @notice `LEGACY_PREDECESSOR_BRIDGES` as the deploy scripts read it. One test
///         function: the environment is shared by every test in the process.
contract PredecessorEnvTest is Test {
    uint256 internal constant DAPP_FR = 0xD499;
    uint256 internal constant ACC_FR = 0xAC0F;

    function _bridge() internal returns (AckiNackiBridge) {
        return new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(new MockERC20("Mock USDC", "mUSDC", 6)),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(new MockPrimaryVerifier())),
                IFallbackVerifier(address(new MockFallbackVerifier())),
                ILayerHashesMovementVerifier(address(new MockLayerHashesMovementVerifier())),
                0xBE5E7,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(new MockBridgeWithdrawalVerifier())),
                DAPP_FR,
                ACC_FR
            )
        );
    }

    function test_readLegacy() public {
        PredecessorEnvReader reader = new PredecessorEnvReader();
        AckiNackiBridge current = _bridge();
        AckiNackiBridge paused = _bridge();
        paused.pause();
        MockPredecessorBridge older = new MockPredecessorBridge(DAPP_FR, ACC_FR);

        // A first bridge, or a predecessor that answers for its own line: optional.
        vm.setEnv("PREDECESSOR_BRIDGE", vm.toString(address(0)));
        assertEq(reader.legacy().length, 0);
        vm.setEnv("PREDECESSOR_BRIDGE", vm.toString(address(current)));
        assertEq(reader.legacy().length, 0);

        // A predecessor from before the field: the list has to be given.
        vm.setEnv("PREDECESSOR_BRIDGE", vm.toString(address(older)));
        vm.expectRevert();
        reader.legacy();

        vm.setEnv("LEGACY_PREDECESSOR_BRIDGES", "");
        assertEq(reader.legacy().length, 0);

        vm.setEnv(
            "LEGACY_PREDECESSOR_BRIDGES",
            string.concat(vm.toString(address(paused)), ",", vm.toString(address(paused)))
        );
        IPredecessorBridge[] memory got = reader.legacy();
        assertEq(got.length, 2);
        assertEq(address(got[0]), address(paused));

        vm.setEnv("LEGACY_PREDECESSOR_BRIDGES", vm.toString(address(current)));
        vm.expectRevert(bytes("pause every LEGACY_PREDECESSOR_BRIDGES entry before the deploy"));
        reader.legacy();
    }
}
