// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockERC20.sol";

/// @title DepositEventSignatureTest
/// @notice TD-59 — `Deposit` event signature + indexed `depositId` regression (log scan).
contract DepositEventSignatureTest is Test {
    bytes32 internal constant DEPOSIT_EVENT_SIG =
        keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)");

    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    /// Same field layout but wrong name → different topic0; relayer `eth_getLogs` skips.
    event DepositLegacy(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;
    address internal user = address(0xA11CE);

    function setUp() public {
        usdc = new MockERC20("USDC", "USDC", 6);
        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );
        usdc.mint(user, 10 * UsdcTestLib.UNIT);
    }

    function _findDepositLog(Vm.Log[] memory logs) internal view returns (Vm.Log memory found) {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == address(bridge) && logs[i].topics.length >= 3) {
                if (logs[i].topics[0] == DEPOSIT_EVENT_SIG) {
                    return logs[i];
                }
            }
        }
        revert("TD-59: no Deposit log");
    }

    function test_td59_deposit_event_topic0_matches_canonical_hash() public {
        bytes32 anAccount = bytes32(uint256(uint160(user)));

        vm.recordLogs();
        vm.startPrank(user);
        usdc.approve(address(bridge), UsdcTestLib.UNIT);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();

        Vm.Log[] memory logs = vm.getRecordedLogs();
        Vm.Log memory depositLog = _findDepositLog(logs);

        assertEq(depositLog.topics[0], DEPOSIT_EVENT_SIG);
        assertEq(
            depositLog.topics[0],
            keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")
        );
    }

    function test_td59_indexed_depositId_is_topic1() public {
        bytes32 anAccount = bytes32(uint256(0xABCD));

        vm.startPrank(user);
        usdc.approve(address(bridge), 3 * UsdcTestLib.UNIT);

        vm.recordLogs();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        Vm.Log[] memory logs0 = vm.getRecordedLogs();
        Vm.Log memory log0 = _findDepositLog(logs0);
        assertEq(log0.topics[1], bytes32(uint256(0)), "TD-59 topic1 depositId=0");

        vm.recordLogs();
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        Vm.Log[] memory logs1 = vm.getRecordedLogs();
        Vm.Log memory log1 = _findDepositLog(logs1);
        assertEq(log1.topics[1], bytes32(uint256(1)), "TD-59 topic1 depositId=1");

        vm.stopPrank();
    }

    function test_td59_wrong_signature_log_not_decoded_as_deposit() public {
        bytes32 legacySig = keccak256("DepositLegacy(uint256,address,uint256,int8,bytes32,uint256)");
        assertTrue(legacySig != DEPOSIT_EVENT_SIG, "TD-59 legacy sig must differ");

        vm.recordLogs();
        emit DepositLegacy(
            0,
            user,
            UsdcTestLib.UNIT,
            int8(0),
            bytes32(uint256(uint160(user))),
            block.timestamp
        );
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], legacySig);
        assertTrue(logs[0].topics[0] != DEPOSIT_EVENT_SIG);
        // Production `EthLogSource` filters `Deposit::SIGNATURE_HASH` on topic0 only.
    }
}
