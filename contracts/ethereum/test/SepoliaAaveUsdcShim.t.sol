// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/cctp/AnDestinationHook.sol";
import "../src/cctp/CctpDomains.sol";
import "../src/cctp/CctpHubReceiver.sol";
import "../src/cctp/CctpV2Message.sol";
import "../src/testnet/SepoliaAaveUsdcShim.sol";
import "./helpers/VerifyBlockConfigLib.sol";
import "./mocks/MockAave.sol";
import "./mocks/MockCctp.sol";
import "./mocks/MockERC20.sol";

/// @notice The Aave testnet faucet: open minting with a per-call cap.
contract MockAaveFaucet is IAaveTestnetFaucet {
    uint256 public constant CAP = 10_000e6;

    function mint(address token, address to, uint256 amount) external returns (uint256) {
        require(amount <= CAP, "Error: Mint limit transaction exceeded");
        MockERC20(token).mint(to, amount);
        return amount;
    }
}

/// @title SepoliaAaveUsdcShimTest
/// @notice A CCTP burn from Arbitrum Sepolia lands as Circle USDC, the shim
///         swaps it for faucet USDC, and the Aave-enabled bridge supplies it.
contract SepoliaAaveUsdcShimTest is Test {
    uint256 internal constant ARBITRUM_SEPOLIA_CHAIN = 421_614;
    uint256 internal constant AMOUNT = 2_000_000;
    uint256 internal constant FEE = 260;

    MockERC20 internal circleUsdc;
    MockERC20 internal aaveUsdc;
    MockERC20 internal arbUsdc;
    MockAUSDC internal aUsdc;
    MockAavePool internal pool;
    MockAaveFaucet internal faucet;
    AckiNackiBridge internal bridge;
    SepoliaAaveUsdcShim internal shim;
    MockMessageTransmitter internal transmitter;
    CctpHubReceiver internal receiver;

    event Shimmed(address indexed caller, uint256 amount, uint256 minted);

    address internal endpoint = address(0xE1);
    address internal user = address(0xA11);
    bytes32 internal anAccount = bytes32(uint256(0xabc));

    function setUp() public {
        circleUsdc = new MockERC20("Circle USDC", "USDC", 6);
        aaveUsdc = new MockERC20("Faucet USDC", "USDC", 6);
        arbUsdc = new MockERC20("Arb USDC", "USDC", 6);
        aUsdc = new MockAUSDC();
        pool = new MockAavePool(address(aaveUsdc), address(aUsdc));
        faucet = new MockAaveFaucet();

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(aaveUsdc),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        shim = new SepoliaAaveUsdcShim(address(bridge), address(circleUsdc), address(faucet));

        transmitter = new MockMessageTransmitter();
        receiver = new CctpHubReceiver(address(shim), address(transmitter), CctpDomains.ETHEREUM);
        receiver.setSource(
            CctpDomains.ARBITRUM,
            ARBITRUM_SEPOLIA_CHAIN,
            _b32(endpoint),
            _b32(address(arbUsdc)),
            true
        );
        receiver.setForwarder(address(this), true);
        transmitter.setLocalToken(_b32(address(arbUsdc)), address(circleUsdc));
    }

    function test_constructor_takesTheBridgeTokenAndOffersCircleUsdc() public {
        assertEq(address(shim.aaveUsdc()), address(aaveUsdc));
        assertEq(shim.usdc(), address(circleUsdc));
        assertEq(address(receiver.token()), address(circleUsdc));
        assertEq(aaveUsdc.allowance(address(shim), address(bridge)), type(uint256).max);

        vm.expectRevert(TwoStepOwnable.ZeroAddress.selector);
        new SepoliaAaveUsdcShim(address(bridge), address(0), address(faucet));
    }

    function test_cctpBurn_isDepositedAsFaucetUsdcAndSuppliedToAave() public {
        bytes32 key = _receive(bytes32(uint256(1)));

        vm.recordLogs();
        receiver.forward(key);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        uint256 net = AMOUNT - FEE;
        _assertDeposit(logs, address(shim), net);
        assertEq(bridge.depositCounter(), 1);
        assertEq(bridge.treasuryBalance(), net);
        assertEq(aaveUsdc.balanceOf(address(bridge)), net);
        assertEq(circleUsdc.balanceOf(address(shim)), net);
        assertEq(circleUsdc.balanceOf(address(receiver)), 0);

        bridge.supplyToAave(type(uint256).max);
        uint256 reserve = (net * bridge.liquidReserveBps()) / 10_000;
        assertEq(bridge.aUsdcBalance(), net - reserve);
        assertEq(bridge.suppliedPrincipal(), net - reserve);
        assertEq(aaveUsdc.balanceOf(address(bridge)), reserve);
    }

    function test_deposit_spendsHeldFaucetUsdcBeforeMinting() public {
        aaveUsdc.mint(address(shim), AMOUNT / 2);
        circleUsdc.mint(address(this), AMOUNT);
        circleUsdc.approve(address(shim), AMOUNT);

        vm.expectEmit(true, false, false, true, address(shim));
        emit Shimmed(address(this), AMOUNT, AMOUNT / 2);
        shim.deposit(AMOUNT, 0, anAccount);

        assertEq(aaveUsdc.balanceOf(address(shim)), 0);
        assertEq(aaveUsdc.balanceOf(address(bridge)), AMOUNT);
    }

    function test_deposit_aboveTheFaucetCapReverts() public {
        uint256 amount = faucet.CAP() + 1;
        circleUsdc.mint(address(this), amount);
        circleUsdc.approve(address(shim), amount);
        vm.expectRevert("Error: Mint limit transaction exceeded");
        shim.deposit(amount, 0, anAccount);
    }

    function test_deposit_rejectsZero() public {
        vm.expectRevert(SepoliaAaveUsdcShim.ZeroAmount.selector);
        shim.deposit(0, 0, anAccount);
    }

    function test_redeem_swapsFaucetUsdcBackToCircleOneToOne() public {
        receiver.forward(_receive(bytes32(uint256(1))));
        uint256 reserve = circleUsdc.balanceOf(address(shim));

        aaveUsdc.mint(user, reserve + 1);
        vm.startPrank(user);
        aaveUsdc.approve(address(shim), type(uint256).max);
        vm.expectRevert(
            abi.encodeWithSelector(SepoliaAaveUsdcShim.InsufficientCircleReserve.selector, reserve)
        );
        shim.redeem(reserve + 1, user);

        shim.redeem(reserve, address(0xB0B));
        vm.stopPrank();

        assertEq(circleUsdc.balanceOf(address(0xB0B)), reserve);
        assertEq(circleUsdc.balanceOf(address(shim)), 0);
        assertEq(aaveUsdc.balanceOf(address(shim)), reserve);
        assertEq(aaveUsdc.balanceOf(user), 1);
    }

    function test_rescue_isOwnerOnly() public {
        circleUsdc.mint(address(shim), 5);
        vm.prank(user);
        vm.expectRevert(TwoStepOwnable.NotOwner.selector);
        shim.rescue(address(circleUsdc), user, 5);

        shim.rescue(address(circleUsdc), user, 5);
        assertEq(circleUsdc.balanceOf(user), 5);
    }

    function _receive(bytes32 nonce) internal returns (bytes32 key) {
        bytes memory hook = AnDestinationHook.encode(0, anAccount, user);
        bytes memory message = CctpV2Message.format(
            CctpV2Message.Relay({
                sourceDomain: CctpDomains.ARBITRUM,
                destinationDomain: CctpDomains.ETHEREUM,
                nonce: nonce,
                sender: bytes32(uint256(0x111)),
                recipient: bytes32(uint256(0x222)),
                destinationCaller: _b32(address(receiver)),
                minFinalityThreshold: CctpDomains.FAST,
                finalityThresholdExecuted: CctpDomains.FAST,
                burnToken: _b32(address(arbUsdc)),
                mintRecipient: _b32(address(receiver)),
                amount: AMOUNT,
                messageSender: _b32(endpoint),
                maxFee: FEE,
                feeExecuted: FEE,
                expirationBlock: 0,
                hookData: hook
            })
        );
        (key,) = receiver.receiveBurn(message, "");
    }

    function _assertDeposit(Vm.Log[] memory logs, address sender, uint256 amount) internal view {
        bytes32 topic = keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)");
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != topic) continue;
            assertEq(logs[i].topics[2], bytes32(uint256(uint160(sender))));
            (uint256 loggedAmount,, bytes32 account,) =
                abi.decode(logs[i].data, (uint256, int8, bytes32, uint256));
            assertEq(loggedAmount, amount);
            assertEq(account, anAccount);
            return;
        }
        revert("Deposit log missing");
    }

    function _b32(address account) internal pure returns (bytes32) {
        return bytes32(uint256(uint160(account)));
    }
}
