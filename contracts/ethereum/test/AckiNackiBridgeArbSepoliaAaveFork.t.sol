// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/IAavePool.sol";
import "../src/IBridgeWithdrawalVerifier.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IERC20.sol";
import "../src/IPredecessorBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/Bn254FrLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "../script/DeployRealBridge.s.sol";
import "../script/DeployShellnetE2EBridge.s.sol";

interface IAaveAToken {
    function UNDERLYING_ASSET_ADDRESS() external view returns (address);
    function POOL() external view returns (address);
}

interface IDebtToken {
    function totalSupply() external view returns (uint256);
}

interface IPoolAddressesProvider {
    function getPoolConfigurator() external view returns (address);
    function getACLManager() external view returns (address);
    function getACLAdmin() external view returns (address);
}

interface IPoolWithProvider {
    function ADDRESSES_PROVIDER() external view returns (address);
}

interface IACLManager {
    function addPoolAdmin(address admin) external;
}

interface IPoolConfigurator {
    function setReservePause(address asset, bool paused) external;
    function setReserveFreeze(address asset, bool freeze) external;
    function setSupplyCap(address asset, uint256 newSupplyCap) external;
}

/// @title AckiNackiBridgeArbSepoliaAaveForkTest
/// @notice Live Arbitrum Sepolia AAVE market (Circle USDC). Opt-in via
/// `FORK_URL`. Without it, or when the fork is another chain, the test skips.
///
/// ```bash
/// FOUNDRY_PROFILE=fork FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
///   forge test --match-contract AckiNackiBridgeArbSepoliaAaveForkTest -vv
/// ```
///
/// The three addresses are the ones `DeployRealBridge` writes on chain id
/// 421614. The supply in `deposit`, the refill in `withdrawByProof`, harvest,
/// withdraw and emergency unwind all run against that market; pause, freeze
/// and the supply cap are set through its own `PoolConfigurator`. The
/// withdrawal proof is the test mock: the pool calls are the real ones.
contract AckiNackiBridgeArbSepoliaAaveForkTest is Test {
    address constant AAVE_V3_POOL = 0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff;
    address constant USDC = 0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d;
    address constant AUSDC = 0x460b97BD498E1157530AEb3086301d5225b91216;
    /// @dev The 421614 bridge without a pool.
    address constant LIVE_BRIDGE = 0x3FB082a905a895151224F6d93bD074E816338638;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant DRIFT_TOLERANCE = 10;

    AckiNackiBridge internal bridge;
    IERC20 internal usdc;
    address internal user = address(0xA11CE);
    address internal yieldSink = address(0xBEEF);
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    uint256 internal anchor;

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
        if (block.chainid != 421614) {
            console.log("FORK_URL is not Arbitrum Sepolia; skipping");
            vm.skip(true);
            return;
        }

        usdc = IERC20(USDC);
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallback_ = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layers = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallback_.setShouldAccept(true);
        layers.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            USDC,
            AAVE_V3_POOL,
            AUSDC,
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallback_)),
                ILayerHashesMovementVerifier(address(layers)),
                BK_SET,
                0xA10C
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawal)), DAPP_FR, ACC_FR
            )
        );
        deal(USDC, user, 200 * UsdcTestLib.UNIT);
        bridge.setYieldRecipient(yieldSink);
    }

    function test_fork_aTokenIsThisPoolsCircleUsdc() public view {
        assertEq(IAaveAToken(AUSDC).UNDERLYING_ASSET_ADDRESS(), USDC, "underlying");
        assertEq(IAaveAToken(AUSDC).POOL(), AAVE_V3_POOL, "pool");
        assertEq(
            IAavePool(AAVE_V3_POOL).getReserveData(USDC).aTokenAddress, AUSDC, "reserve aToken"
        );
    }

    event AaveSupplyRefused(uint256 amount, bytes reason);
    event AaveRefillFailed(uint256 amount, bytes reason);

    /// @dev Supply in `deposit`: one 10 USDC deposit into an empty bridge
    ///      leaves the 10% target liquid and supplies the rest.
    function test_fork_depositSuppliesAboveTarget() public {
        _deposit(10 * UsdcTestLib.UNIT);

        assertApproxEqAbs(
            bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "90% supplied"
        );
        assertEq(usdc.balanceOf(address(bridge)), 1 * UsdcTestLib.UNIT, "target liquid");
        assertApproxEqAbs(
            bridge.aUsdcBalance(), bridge.suppliedPrincipal(), DRIFT_TOLERANCE, "aUSDC"
        );
    }

    /// @dev TC-15: a deposit below the threshold makes no call to the market.
    function test_fork_tc15_belowThresholdMakesNoPoolCall() public {
        _deposit(10 * UsdcTestLib.UNIT);
        vm.expectCall(AAVE_V3_POOL, abi.encodeWithSelector(IAavePool.supply.selector), 0);
        _deposit(1 * UsdcTestLib.UNIT);
        assertEq(usdc.balanceOf(address(bridge)), 2 * UsdcTestLib.UNIT, "deposit stayed liquid");
    }

    /// @dev TC-16: a paused reserve refuses the supply; the deposit goes through.
    function test_fork_tc16_pausedReserveDoesNotBlockDeposit() public {
        _configurator().setReservePause(USDC, true);
        _assertDepositRefused();
    }

    /// @dev TC-16: a frozen reserve refuses the supply; the deposit goes through.
    function test_fork_tc16_frozenReserveDoesNotBlockDeposit() public {
        _configurator().setReserveFreeze(USDC, true);
        _assertDepositRefused();
    }

    /// @dev TC-17: a supply cap below the reserve's supply refuses the supply.
    function test_fork_tc17_supplyCapDoesNotBlockDeposit() public {
        _configurator().setSupplyCap(USDC, 1);
        _assertDepositRefused();
    }

    /// @dev TC-18: the reserve's free USDC is borrowed out down to zero — a
    ///      payout with a shortfall reverts and its nullifier stays unused.
    function test_fork_tc18_noLiquidityForTheShortfallReverts() public {
        _recordAnchor();
        _fillBridge();
        deal(USDC, AUSDC, 0);

        vm.expectRevert();
        _pay(20 * UsdcTestLib.UNIT, 1);
        assertFalse(bridge.isNullifierUsed(1), "nullifier unused");
        assertEq(bridge.treasuryBalance(), 100 * UsdcTestLib.UNIT, "book unchanged");
    }

    /// @dev TC-18: the reserve can return the shortfall but not the refill —
    ///      the payout goes through and the event names the amount not pulled.
    function test_fork_tc18_shortfallOnlyPaysAndReports() public {
        _recordAnchor();
        _fillBridge();
        // U = 10, S = 90. A 20 USDC payout is short 10; the new book is 80,
        // so the refill would pull 8 more.
        uint256 shortfall = 10 * UsdcTestLib.UNIT;
        uint256 extra = 8 * UsdcTestLib.UNIT;
        deal(USDC, AUSDC, shortfall);
        bytes memory reason = _poolWithdrawRevertData(shortfall + extra);

        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveRefillFailed(extra, reason);
        uint256 paidBefore = usdc.balanceOf(RECIPIENT);
        _pay(20 * UsdcTestLib.UNIT, 1);

        assertEq(usdc.balanceOf(RECIPIENT) - paidBefore, 20 * UsdcTestLib.UNIT, "paid");
        assertEq(usdc.balanceOf(address(bridge)), 0, "below the target");
        assertApproxEqAbs(
            bridge.suppliedPrincipal(), 80 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "S = new book"
        );
    }

    /// @dev TC-19 / WD-5: interest accrued, a payout that refills the target,
    ///      then a harvest — the bridge stays solvent throughout.
    function test_fork_tc19_refillAndHarvestAfterInterestStaySolvent() public {
        address debt = IAavePool(AAVE_V3_POOL).getReserveData(USDC).variableDebtTokenAddress;
        assertGt(IDebtToken(debt).totalSupply(), 0, "a borrower is already in the USDC reserve");

        _recordAnchor();
        _fillBridge();
        vm.warp(block.timestamp + 365 days);
        vm.roll(block.number + 1);
        assertGt(bridge.accruedYield(), 0, "the borrowed reserve paid interest");

        _pay(20 * UsdcTestLib.UNIT, 1);
        _assertSolvent();
        assertEq(usdc.balanceOf(address(bridge)), 8 * UsdcTestLib.UNIT, "target of the new book");

        uint256 yield = bridge.accruedYield();
        uint256 take = yield / 2;
        if (take == 0) take = yield;
        uint256 sinkBefore = usdc.balanceOf(yieldSink);
        uint256 principalBefore = bridge.suppliedPrincipal();
        bridge.harvestYield(take);

        assertApproxEqAbs(usdc.balanceOf(yieldSink) - sinkBefore, take, DRIFT_TOLERANCE, "harvest");
        assertEq(bridge.suppliedPrincipal(), principalBefore, "harvest leaves principal");
        _assertSolvent();
    }

    function test_fork_withdrawFromAave() public {
        _deposit(10 * UsdcTestLib.UNIT);
        uint256 principalBefore = bridge.suppliedPrincipal();
        uint256 liquidBefore = usdc.balanceOf(address(bridge));

        bridge.withdrawFromAave(3 * UsdcTestLib.UNIT);

        assertEq(bridge.suppliedPrincipal(), principalBefore - 3 * UsdcTestLib.UNIT, "book");
        assertEq(usdc.balanceOf(address(bridge)) - liquidBefore, 3 * UsdcTestLib.UNIT, "USDC back");
    }

    function test_fork_emergencyWithdrawAll() public {
        _deposit(10 * UsdcTestLib.UNIT);
        uint256 supplied = bridge.suppliedPrincipal();
        uint256 liquidBefore = usdc.balanceOf(address(bridge));

        bridge.emergencyWithdrawAll();

        assertFalse(bridge.aaveEnabled(), "supplies disabled");
        assertEq(bridge.aUsdcBalance(), 0, "position closed");
        assertEq(bridge.suppliedPrincipal(), 0, "book cleared");
        assertGe(
            usdc.balanceOf(address(bridge)) - liquidBefore, supplied - DRIFT_TOLERANCE, "USDC home"
        );
    }

    /// @dev WD-3, WD-5: a payout larger than the liquid USDC pulls the
    ///      shortfall plus the target of the new book.
    function test_fork_withdrawByProof_refillsFromAave() public {
        _recordAnchor();
        _deposit(10 * UsdcTestLib.UNIT);
        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 payout = 4 * UsdcTestLib.UNIT;
        assertLt(liquid, payout, "the payout must reach into AAVE");

        uint256 aBefore = bridge.aUsdcBalance();
        uint256 paidBefore = usdc.balanceOf(RECIPIENT);
        _pay(payout, 1);

        uint256 target = (10 * UsdcTestLib.UNIT - payout) / 10;
        assertEq(usdc.balanceOf(RECIPIENT) - paidBefore, payout, "recipient paid");
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT - payout, "book of users");
        assertApproxEqAbs(
            aBefore - bridge.aUsdcBalance(), payout - liquid + target, DRIFT_TOLERANCE, "pool pull"
        );
        assertEq(usdc.balanceOf(address(bridge)), target, "target refilled");
    }

    /// @dev TC-20: both deploy scripts wire the market of 421614.
    function test_fork_tc20_deployScriptsWireTheMarket() public {
        _setDeployEnv();
        AckiNackiBridge real = new DeployRealBridge().deploy();
        assertEq(address(real.aavePool()), AAVE_V3_POOL, "real: pool");
        assertEq(address(real.aUSDC()), AUSDC, "real: aUSDC");
        assertEq(address(real.usdc()), USDC, "real: USDC");
        assertTrue(real.aaveEnabled(), "real: enabled");

        AckiNackiBridge shellnet = new DeployShellnetE2EBridge().deploy();
        assertEq(address(shellnet.aavePool()), AAVE_V3_POOL, "shellnet: pool");
        assertEq(address(shellnet.aUSDC()), AUSDC, "shellnet: aUSDC");
        assertEq(address(shellnet.usdc()), USDC, "shellnet: USDC");
    }

    /// @dev TC-20 / ENV-1: `DeployRealBridge` refuses every other chain id.
    function test_fork_tc20_deployRealBridgeRefusesOtherChains() public {
        _setDeployEnv();
        DeployRealBridge script = new DeployRealBridge();
        uint256[3] memory others = [uint256(1), 11_155_111, 42_161];
        for (uint256 i; i < others.length; ++i) {
            vm.chainId(others[i]);
            vm.expectRevert(bytes("DeployRealBridge: Arbitrum Sepolia (421614) only"));
            script.deploy();
        }
    }

    function _setDeployEnv() internal {
        vm.setEnv("PRIVATE_KEY", vm.toString(uint256(0xA11CE5)));
        vm.setEnv("WIRE_VERIFY_BLOCK", "true");
        vm.setEnv("GENESIS_BK_SET_COMMITMENT", vm.toString(BK_SET));
        vm.setEnv("GENESIS_PREV_MAX_LEVEL_LAYER_HASH", "0");
        vm.setEnv("BUNDLE_STRIDE", "16384");
        vm.setEnv("WITHDRAW_ACC_FR", vm.toString(ACC_FR));
        vm.setEnv("WITHDRAW_DAPP_FR", vm.toString(DAPP_FR));
    }

    function _fillBridge() internal {
        _deposit(50 * UsdcTestLib.UNIT);
        _deposit(50 * UsdcTestLib.UNIT);
        assertApproxEqAbs(
            bridge.suppliedPrincipal(), 90 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "principal"
        );
        assertEq(usdc.balanceOf(address(bridge)), 10 * UsdcTestLib.UNIT, "target");
    }

    function _assertSolvent() internal view {
        assertGe(
            usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance(),
            bridge.treasuryBalance(),
            "U + A >= B"
        );
    }

    /// @dev A crossing 10 USDC deposit into the empty bridge, with the
    ///      market refusing supplies: everything stays liquid and the
    ///      event carries what the pool itself reverts with.
    function _assertDepositRefused() internal {
        uint256 toSupply = 9 * UsdcTestLib.UNIT;
        bytes memory reason = _poolSupplyRevertData(toSupply);
        vm.prank(user);
        usdc.approve(address(bridge), 10 * UsdcTestLib.UNIT);
        vm.expectEmit(false, false, false, true, address(bridge));
        emit AaveSupplyRefused(toSupply, reason);
        vm.prank(user);
        bridge.deposit(10 * UsdcTestLib.UNIT, bytes32(uint256(uint160(user))));

        assertEq(bridge.depositCounter(), 1, "deposit recorded");
        assertEq(bridge.suppliedPrincipal(), 0, "nothing booked");
        assertEq(usdc.balanceOf(address(bridge)), 10 * UsdcTestLib.UNIT, "USDC stayed");
        assertEq(usdc.allowance(address(bridge), AAVE_V3_POOL), 0, "no allowance left");
    }

    function _poolSupplyRevertData(uint256 amount) internal returns (bytes memory data) {
        address probe = makeAddr("supply-probe");
        deal(USDC, probe, amount);
        vm.startPrank(probe);
        usdc.approve(AAVE_V3_POOL, amount);
        bool ok;
        (ok, data) = AAVE_V3_POOL.call(
            abi.encodeWithSelector(IAavePool.supply.selector, USDC, amount, probe, uint16(0))
        );
        vm.stopPrank();
        assertFalse(ok, "the market must refuse the probe");
    }

    function _poolWithdrawRevertData(uint256 amount) internal returns (bytes memory data) {
        vm.prank(address(bridge));
        bool ok;
        (ok, data) = AAVE_V3_POOL.call(
            abi.encodeWithSelector(IAavePool.withdraw.selector, USDC, amount, address(bridge))
        );
        assertFalse(ok, "the market must refuse the probe");
    }

    function _configurator() internal returns (IPoolConfigurator) {
        IPoolAddressesProvider provider =
            IPoolAddressesProvider(IPoolWithProvider(AAVE_V3_POOL).ADDRESSES_PROVIDER());
        IACLManager acl = IACLManager(provider.getACLManager());
        vm.prank(provider.getACLAdmin());
        acl.addPoolAdmin(address(this));
        return IPoolConfigurator(provider.getPoolConfigurator());
    }

    function _deposit(uint256 amount) internal {
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function _recordAnchor() internal {
        uint256[10] memory hashes;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            hashes[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("arb-sepolia-aave", i))));
        }
        anchor = hashes[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            hex"01",
            hex"02",
            0xC10C40001,
            BK_SET,
            1,
            ACTIVE_LAYERS,
            hashes,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    /// @dev The 421614 cutover `docs/aave-yield.md` describes, against the
    ///      live bridge this one replaces: the constructor accepts it as
    ///      `predecessor`, and since it predates `migrateTreasury` the owner
    ///      covers its stranded treasury with `fundTreasury`.
    function test_fork_replacesLiveBridgeAsPredecessor() public {
        IPredecessorBridge old = IPredecessorBridge(LIVE_BRIDGE);
        uint256 oldTreasury = AckiNackiBridge(LIVE_BRIDGE).treasuryBalance();

        AckiNackiBridge.BridgeWithdrawConfig memory bw = VerifyBlockConfigLib.withWithdrawAfter(
            IBridgeWithdrawalVerifier(address(new MockBridgeWithdrawalVerifier())),
            old.bridgeWithdrawalDappFr(),
            old.bridgeWithdrawalAccFr(),
            LIVE_BRIDGE
        );
        AckiNackiBridge successor = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            USDC,
            AAVE_V3_POOL,
            AUSDC,
            VerifyBlockConfigLib.with(
                bridge.primaryVerifier(),
                bridge.fallbackVerifier(),
                bridge.layerHashesVerifier(),
                BK_SET,
                0xA10C
            ),
            bw
        );
        assertEq(address(successor.predecessor()), LIVE_BRIDGE, "predecessor recorded");

        deal(USDC, address(this), oldTreasury);
        usdc.approve(address(successor), oldTreasury);
        successor.fundTreasury(oldTreasury);
        assertEq(successor.treasuryBalance(), oldTreasury, "old book carried over");
        assertEq(successor.depositCounter(), 0, "no Deposit");
    }

    function _pay(uint256 amount, uint256 nullifier) internal {
        uint256 a = uint256(uint160(RECIPIENT));
        bridge.withdrawByProof(
            hex"00",
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 0,
                amount: amount,
                recipientHi: a >> 80,
                recipientLo: a & ((1 << 80) - 1),
                dstChainId: block.chainid,
                senderAccFr: Bn254FrLib.toFr(uint256(keccak256("senderAcc"))),
                dappFr: DAPP_FR,
                accFr: ACC_FR,
                nullifier: nullifier,
                finalRoot: anchor,
                anchorLayer: 1,
                dstBridge: uint256(uint160(address(bridge)))
            })
        );
    }
}
