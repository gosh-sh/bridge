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
import "../src/MockBlockHeaderOracle.sol";
import "./helpers/Bn254FrLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockPrimaryVerifier.sol";

interface IAaveAToken {
    function UNDERLYING_ASSET_ADDRESS() external view returns (address);
    function POOL() external view returns (address);
}

interface IDebtToken {
    function totalSupply() external view returns (uint256);
}

/// @title AckiNackiBridgeAaveArbSepoliaForkTest
/// @notice Live Arbitrum Sepolia AAVE market (Circle USDC). Opt-in via
/// `FORK_URL`. Without it the test skips.
///
/// ```bash
/// FORK_URL=https://sepolia-rollup.arbitrum.io/rpc \
///   forge test --match-contract AckiNackiBridgeAaveArbSepoliaForkTest -vv
/// ```
///
/// The three addresses are the ones `DeployRealBridge` writes on chain id
/// 421614. Supply, harvest, withdraw, emergency unwind and a payout that
/// tops up from the pool all run against that market. The withdrawal proof
/// is the test mock: the pool pull is the real one.
contract AckiNackiBridgeAaveArbSepoliaForkTest is Test {
    address constant AAVE_V3_POOL = 0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff;
    address constant USDC = 0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d;
    address constant AUSDC = 0x460b97BD498E1157530AEb3086301d5225b91216;

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
        require(block.chainid == 421614, "fork must be Arbitrum Sepolia (chainid=421614)");

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

    function test_fork_supplyToAave() public {
        _deposit(10 * UsdcTestLib.UNIT);
        uint256 liquidBefore = usdc.balanceOf(address(bridge));
        bridge.supplyToAave(type(uint256).max);

        assertApproxEqAbs(
            bridge.suppliedPrincipal(), 9 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "90% supplied"
        );
        assertEq(
            usdc.balanceOf(address(bridge)),
            liquidBefore - bridge.suppliedPrincipal(),
            "liquid reserve stays"
        );
        assertApproxEqAbs(
            bridge.aUsdcBalance(), bridge.suppliedPrincipal(), DRIFT_TOLERANCE, "aUSDC"
        );
    }

    function test_fork_harvestYield_whileTheMarketHasABorrower() public {
        address debt = IAavePool(AAVE_V3_POOL).getReserveData(USDC).variableDebtTokenAddress;
        assertGt(IDebtToken(debt).totalSupply(), 0, "a borrower is already in the USDC reserve");

        _deposit(50 * UsdcTestLib.UNIT);
        _deposit(50 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        assertApproxEqAbs(
            bridge.suppliedPrincipal(), 90 * UsdcTestLib.UNIT, DRIFT_TOLERANCE, "principal"
        );

        vm.warp(block.timestamp + 365 days);
        vm.roll(block.number + 1);
        _deposit(1 * UsdcTestLib.UNIT);
        bridge.supplyToAave(500_000);

        uint256 yield = bridge.accruedYield();
        assertGt(yield, 0, "the borrowed reserve paid interest");
        uint256 take = yield / 2;
        if (take == 0) take = yield;

        uint256 sinkBefore = usdc.balanceOf(yieldSink);
        uint256 principalBefore = bridge.suppliedPrincipal();
        bridge.harvestYield(take);

        assertApproxEqAbs(usdc.balanceOf(yieldSink) - sinkBefore, take, DRIFT_TOLERANCE, "harvest");
        assertEq(bridge.suppliedPrincipal(), principalBefore, "harvest leaves principal");
    }

    function test_fork_withdrawFromAave() public {
        _deposit(10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 principalBefore = bridge.suppliedPrincipal();
        uint256 liquidBefore = usdc.balanceOf(address(bridge));

        bridge.withdrawFromAave(3 * UsdcTestLib.UNIT);

        assertEq(bridge.suppliedPrincipal(), principalBefore - 3 * UsdcTestLib.UNIT, "book");
        assertEq(usdc.balanceOf(address(bridge)) - liquidBefore, 3 * UsdcTestLib.UNIT, "USDC back");
    }

    function test_fork_emergencyWithdrawAll() public {
        _deposit(10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
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

    function test_fork_withdrawByProof_topsUpFromAave() public {
        _recordAnchor();
        _deposit(10 * UsdcTestLib.UNIT);
        bridge.supplyToAave(type(uint256).max);
        uint256 liquid = usdc.balanceOf(address(bridge));
        uint256 payout = 4 * UsdcTestLib.UNIT;
        assertLt(liquid, payout, "the payout must reach into AAVE");

        uint256 aBefore = bridge.aUsdcBalance();
        uint256 paidBefore = usdc.balanceOf(RECIPIENT);
        _pay(payout, 1);

        assertEq(usdc.balanceOf(RECIPIENT) - paidBefore, payout, "recipient paid");
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT - payout, "book of users");
        assertApproxEqAbs(
            aBefore - bridge.aUsdcBalance(), payout - liquid, DRIFT_TOLERANCE, "pool pull"
        );
        assertEq(usdc.balanceOf(address(bridge)), 0, "liquid spent");
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
                anchorLayer: 1
            })
        );
    }
}
