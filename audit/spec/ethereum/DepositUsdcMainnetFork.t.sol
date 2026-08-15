// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IERC20.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";

/// @notice Minimal Circle FiatToken V2 surface (mainnet USDC proxy).
interface IFiatTokenUSDC is IERC20 {
    function blacklist(address account) external;
    function unBlacklist(address account) external;
    function isBlacklisted(address account) external view returns (bool);
    function pause() external;
    function unpause() external;
    function paused() external view returns (bool);
    function blacklister() external view returns (address);
    function pauser() external view returns (address);
}

/// @title DepositUsdcMainnetForkTest
/// @notice TD-56 — mainnet-fork PoC: real USDC proxy pause/blacklist (mirrors TD-24).
///
/// Opt-in: `FORK_URL` (mainnet). Optional `FORK_BLOCK` pin.
///
/// ```bash
/// FOUNDRY_PROFILE=fork FORK_URL=https://eth.llamarpc.com \
///   forge test --match-contract DepositUsdcMainnetForkTest -vv
/// ```
contract DepositUsdcMainnetForkTest is Test {
    address internal constant USDC = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
    /// Large USDC holder on mainnet (Binance hot wallet).
    address internal constant USDC_WHALE = 0x28C6c06298d514Db089934071355E5743bf21d60;

    AckiNackiBridge internal bridge;
    IFiatTokenUSDC internal usdc;
    address internal blacklister;
    address internal pauser;

    address internal user = address(0xA11CE);
    bytes32 internal anAccount = bytes32(uint256(uint160(user)));

    function setUp() public {
        string memory rpc = vm.envOr("FORK_URL", string(""));
        if (bytes(rpc).length == 0) {
            console.log("FORK_URL not set; skipping TD-56 fork tests");
            vm.skip(true);
            return;
        }

        uint256 forkBlock = vm.envOr("FORK_BLOCK", uint256(0));
        try this._td56_create_fork(rpc, forkBlock) {
            // ok
        } catch {
            console.log("TD-56: fork RPC failed; skipping");
            vm.skip(true);
            return;
        }
        require(block.chainid == 1, "TD-56: fork must be Ethereum mainnet (chainid=1)");

        usdc = IFiatTokenUSDC(USDC);
        blacklister = usdc.blacklister();
        pauser = usdc.pauser();
        require(blacklister != address(0), "TD-56: USDC blacklister");
        require(pauser != address(0), "TD-56: USDC pauser");

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            USDC,
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        _fundUsdc(user, 10 * UsdcTestLib.UNIT);
    }

    function _fundUsdc(address to, uint256 amount) internal {
        uint256 whaleBal = usdc.balanceOf(USDC_WHALE);
        require(whaleBal >= amount, "TD-56: whale USDC balance");
        vm.prank(USDC_WHALE);
        require(usdc.transfer(to, amount), "TD-56: whale transfer");
    }

    function _depositExpectRevert(uint256 amount) internal {
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        vm.expectRevert();
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();
    }

    /// TD-24 (a) — blacklisted sender before deposit.
    function test_td56_a_blacklisted_user_deposit_reverts() public {
        vm.prank(blacklister);
        usdc.blacklist(user);

        _depositExpectRevert(UsdcTestLib.UNIT);

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);
        assertEq(usdc.balanceOf(address(bridge)), 0);

        vm.prank(blacklister);
        usdc.unBlacklist(user);
    }

    /// TD-24 (b) — global token pause.
    function test_td56_b_paused_token_deposit_reverts() public {
        bool wasPaused = usdc.paused();
        if (!wasPaused) {
            vm.prank(pauser);
            usdc.pause();
        }

        _depositExpectRevert(UsdcTestLib.UNIT);

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);

        if (!wasPaused) {
            vm.prank(pauser);
            usdc.unpause();
        }
    }

    /// TD-24 (c) — post-deposit blacklist does not claw back ledger/custody.
    function test_td56_c_post_deposit_blacklist_treasury_unchanged() public {
        uint256 amount = UsdcTestLib.UNIT;
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        uint256 treasury = bridge.treasuryBalance();
        uint256 custody = usdc.balanceOf(address(bridge));

        vm.prank(blacklister);
        usdc.blacklist(user);

        assertEq(bridge.treasuryBalance(), treasury, "TD-56: ledger unchanged");
        assertEq(usdc.balanceOf(address(bridge)), custody, "TD-56: custody unchanged");
        assertEq(bridge.depositCounter(), 1);

        _depositExpectRevert(amount);

        vm.prank(blacklister);
        usdc.unBlacklist(user);
    }

    /// TD-24 (d) — control deposit on real USDC.
    function test_td56_d_deposit_ok_on_real_usdc() public {
        uint256 amount = UsdcTestLib.UNIT;
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), anAccount);
        vm.stopPrank();

        assertEq(bridge.treasuryBalance(), amount);
        assertEq(usdc.balanceOf(address(bridge)), amount);
        assertEq(bridge.depositCounter(), 1);
    }

    /// TD-24 — bridge recipient blacklisted blocks inbound `transferFrom`.
    function test_td56_blacklisted_bridge_recipient_deposit_reverts() public {
        vm.prank(blacklister);
        usdc.blacklist(address(bridge));

        _depositExpectRevert(UsdcTestLib.UNIT);

        assertEq(bridge.treasuryBalance(), 0);
        assertEq(bridge.depositCounter(), 0);

        vm.prank(blacklister);
        usdc.unBlacklist(address(bridge));
    }

    /// @dev External helper so `setUp` can `try/catch` fork RPC failures.
    function _td56_create_fork(string memory rpc, uint256 forkBlock) external {
        if (forkBlock > 0) {
            vm.createSelectFork(rpc, forkBlock);
        } else {
            vm.createSelectFork(rpc);
        }
    }
}
