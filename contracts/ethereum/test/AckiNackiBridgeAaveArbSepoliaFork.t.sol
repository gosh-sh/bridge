// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/IAavePool.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/VerifyBlockConfigLib.sol";

interface IAaveAToken {
    function UNDERLYING_ASSET_ADDRESS() external view returns (address);
    function POOL() external view returns (address);
}

/// @title AckiNackiBridgeAaveArbSepoliaForkTest
/// @notice The Arbitrum Sepolia AAVE market whose underlying is Circle's USDC.
/// Opt-in via `FORK_URL`. Without it the test skips, the same way the other
/// fork suites do.
///
/// ```bash
/// FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
///   forge test --match-contract AckiNackiBridgeAaveArbSepoliaForkTest -vv
/// ```
///
/// The three addresses are the ones `DeployRealBridge` writes on chain id
/// 421614. The test fails when the pool's reserve no longer lists this aToken.
contract AckiNackiBridgeAaveArbSepoliaForkTest is Test {
    address constant AAVE_V3_POOL = 0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff;
    address constant USDC = 0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d;
    address constant AUSDC = 0x460b97BD498E1157530AEb3086301d5225b91216;

    function setUp() public {
        string memory rpc = vm.envOr("FORK_URL", string(""));
        if (bytes(rpc).length == 0) {
            console.log("FORK_URL not set; skipping fork tests");
            vm.skip(true);
            return;
        }
        uint256 forkBlock = vm.envOr("FORK_BLOCK", uint256(0));
        if (forkBlock > 0) {
            vm.createSelectFork(rpc, forkBlock);
        } else {
            vm.createSelectFork(rpc);
        }
        require(block.chainid == 421614, "fork must be Arbitrum Sepolia (chainid=421614)");
    }

    function test_fork_aTokenIsThisPoolsCircleUsdc() public view {
        assertEq(IAaveAToken(AUSDC).UNDERLYING_ASSET_ADDRESS(), USDC, "underlying");
        assertEq(IAaveAToken(AUSDC).POOL(), AAVE_V3_POOL, "pool");
        assertEq(
            IAavePool(AAVE_V3_POOL).getReserveData(USDC).aTokenAddress, AUSDC, "reserve aToken"
        );
    }

    function test_fork_constructorWiresThatMarket() public {
        MockBlockHeaderOracle oracle = new MockBlockHeaderOracle();
        AckiNackiBridge bridge = new AckiNackiBridge(
            address(oracle),
            USDC,
            AAVE_V3_POOL,
            AUSDC,
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        assertEq(address(bridge.usdc()), USDC, "usdc");
        assertEq(address(bridge.aavePool()), AAVE_V3_POOL, "pool");
        assertEq(address(bridge.aUSDC()), AUSDC, "aUSDC");
        assertTrue(bridge.aaveEnabled(), "wired pool starts enabled");
    }
}
