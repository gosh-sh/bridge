// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/MockBlockHeaderOracle.sol";
import "@src/IPrimaryVerifier.sol";
import "@src/IFallbackVerifier.sol";
import "@src/ILayerHashesMovementVerifier.sol";
import "@src/IBridgeWithdrawalVerifier.sol";

import "@bridge-test/helpers/VerifyBlockConfigLib.sol";
import "@bridge-test/helpers/UsdcTestLib.sol";
import "@bridge-test/mocks/MockPrimaryVerifier.sol";
import "@bridge-test/mocks/MockFallbackVerifier.sol";
import "@bridge-test/mocks/MockLayerHashesMovementVerifier.sol";
import "@bridge-test/mocks/MockBridgeWithdrawalVerifier.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/mocks/CrossFnReentrantERC20.sol";

/// @title DepositCrossFnReentrancyTest
/// @notice TD-25 — token `transferFrom` (deposit / AAVE supply) reenters every mutating entrypoint.
/// @dev INV: DEP-CEI-RE / DEP-T09 — `nonReentrant` on public paths; owner paths fail `NotOwner`.
contract DepositCrossFnReentrancyTest is Test {
    AckiNackiBridge internal bridge;
    CrossFnReentrantERC20 internal token;
    MockAavePool internal pool;
    MockAUSDC internal aUsdc;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;
    uint256 internal constant R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    bytes32 internal constant SIB_H01 = bytes32(uint256(0x1234));
    bytes32 internal constant SIB_H4_7 = bytes32(uint256(0x5678));
    bytes32 internal constant SIB_H8_15 = bytes32(uint256(0x9ABC));

    address internal user = address(0xBEEF);
    uint256 internal seedAnchor;
    uint256 internal wdNullifier = 0xDEADBEEF;

    function setUp() public {
        MockPrimaryVerifier primary = new MockPrimaryVerifier();
        MockFallbackVerifier fallbackVerifier = new MockFallbackVerifier();
        MockLayerHashesMovementVerifier layerHashes = new MockLayerHashesMovementVerifier();
        MockBridgeWithdrawalVerifier withdrawal = new MockBridgeWithdrawalVerifier();
        primary.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashes.setShouldAccept(true);
        withdrawal.setShouldAccept(true);

        token = new CrossFnReentrantERC20();
        aUsdc = new MockAUSDC();
        pool = new MockAavePool(address(token), address(aUsdc));

        bridge = new AckiNackiBridge(
            address(new MockBlockHeaderOracle()),
            address(token),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primary)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashes)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawal)), DAPP_FR, ACC_FR
            )
        );
        bridge.setYieldRecipient(address(0xE1E11D));

        token.wireBridge(bridge, address(pool));
        seedAnchor = _seedVerifyBlock();
    }

    function _seedVerifyBlock() internal returns (uint256 l1Anchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("td25-seed-layer", i)));
        }
        l1Anchor = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("td25-seed-att")),
            abi.encodePacked(keccak256("td25-seed-lh")),
            0xC10C40001,
            BK_SET,
            1,
            ACTIVE_LAYERS,
            layers,
            GENESIS_PREV_ANCHOR
        );
    }

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & RECIPIENT_HALF_MASK;
    }

    function _dummyProof() internal pure returns (bytes memory) {
        bytes memory p = new bytes(256);
        for (uint256 i = 0; i < 256; i++) {
            p[i] = bytes1(uint8(i));
        }
        return p;
    }

    function _defaultWdPub(uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        (uint256 hi, uint256 lo) = _split(RECIPIENT);
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: amount,
            recipientHi: hi,
            recipientLo: lo,
            dstChainId: block.chainid,
            senderAccFr: uint256(keccak256("senderAcc")),
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: seedAnchor
        });
    }

    function _secondVerifyBlockLayers() internal view returns (uint256[10] memory layers) {
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = uint256(keccak256(abi.encode("td25-vb2-layer", i)));
        }
    }

    function _merkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        return _rawMerkleRoot(l2, l3) % R;
    }

    function _rawMerkleRoot(uint256 l2, uint256 l3) internal pure returns (uint256) {
        bytes32 h23 = sha256(abi.encodePacked(_le(l2), _le(l3)));
        bytes32 h0_3 = sha256(abi.encodePacked(SIB_H01, h23));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, SIB_H4_7));
        return uint256(sha256(abi.encodePacked(h0_7, SIB_H8_15)));
    }

    function _le(uint256 value) internal pure returns (bytes32) {
        uint256 reversed;
        for (uint256 i = 0; i < 32; i++) {
            reversed = (reversed << 8) | (value & 0xff);
            value >>= 8;
        }
        return bytes32(reversed);
    }

    struct Snap {
        uint256 treasury;
        uint256 counter;
        uint256 supplied;
        bool nullifierUsed;
        uint256 bridgeTokenBal;
    }

    function _snap() internal view returns (Snap memory s) {
        s.treasury = bridge.treasuryBalance();
        s.counter = bridge.depositCounter();
        s.supplied = bridge.suppliedPrincipal();
        s.nullifierUsed = bridge.isNullifierUsed(wdNullifier);
        s.bridgeTokenBal = token.balanceOf(address(bridge));
    }

    function _assertUnchanged(Snap memory before, Snap memory afterSnap) internal {
        assertEq(afterSnap.treasury, before.treasury, "treasury unchanged");
        assertEq(afterSnap.counter, before.counter, "depositCounter unchanged");
        assertEq(afterSnap.supplied, before.supplied, "suppliedPrincipal unchanged");
        assertEq(afterSnap.nullifierUsed, before.nullifierUsed, "nullifier unchanged");
        assertEq(afterSnap.bridgeTokenBal, before.bridgeTokenBal, "bridge token balance unchanged");
    }

    function _depositExpectRevert(bytes4 selector) internal {
        Snap memory before = _snap();
        bytes32 anAccount = bytes32(uint256(0x1234));
        token.mint(user, UsdcTestLib.UNIT);
        vm.startPrank(user);
        token.approve(address(bridge), UsdcTestLib.UNIT);
        vm.expectRevert(selector);
        bridge.deposit(UsdcTestLib.UNIT, int8(0), anAccount);
        vm.stopPrank();
        _assertUnchanged(before, _snap());
    }

    function _wireVerifyBlockReenter() internal {
        uint256[10] memory layers = _secondVerifyBlockLayers();
        token.wireVerifyBlock(
            0xC10C40002,
            2,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }

    function _wireApplyBkReenter() internal {
        uint256 newL3 = 0xB0B;
        token.wireApplyBkSetUpdate(
            _merkleRoot(BK_SET, newL3),
            42,
            BK_SET,
            newL3,
            SIB_H01,
            SIB_H4_7,
            SIB_H8_15
        );
    }

    function _wireWithdrawReenter() internal {
        token.wireWithdrawByProof(_dummyProof(), _defaultWdPub(UsdcTestLib.UNIT, wdNullifier));
    }

    function test_td25_depositPath_reenterDeposit_revertsReentrancy() public {
        token.wireDeposit(bytes32(uint256(0x1234)), UsdcTestLib.UNIT);
        _depositExpectRevert(AckiNackiBridge.Reentrancy.selector);
    }

    function test_td25_depositPath_reenterVerifyBlock_revertsReentrancy() public {
        _wireVerifyBlockReenter();
        _depositExpectRevert(AckiNackiBridge.Reentrancy.selector);
    }

    function test_td25_depositPath_reenterApplyBkSetUpdate_revertsReentrancy() public {
        _wireApplyBkReenter();
        _depositExpectRevert(AckiNackiBridge.Reentrancy.selector);
    }

    function test_td25_depositPath_reenterWithdrawByProof_revertsReentrancy() public {
        _wireWithdrawReenter();
        _depositExpectRevert(AckiNackiBridge.Reentrancy.selector);
    }

    function test_td25_depositPath_reenterSupplyToAave_revertsNotOwner() public {
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.SupplyToAave, UsdcTestLib.UNIT);
        _depositExpectRevert(AckiNackiBridge.NotOwner.selector);
    }

    function test_td25_depositPath_reenterWithdrawFromAave_revertsNotOwner() public {
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.WithdrawFromAave, UsdcTestLib.UNIT);
        _depositExpectRevert(AckiNackiBridge.NotOwner.selector);
    }

    function test_td25_depositPath_reenterEmergencyWithdrawAll_revertsNotOwner() public {
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.EmergencyWithdrawAll, 0);
        _depositExpectRevert(AckiNackiBridge.NotOwner.selector);
    }

    function test_td25_depositPath_reenterHarvestYield_revertsNotOwner() public {
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.HarvestYield, UsdcTestLib.UNIT);
        _depositExpectRevert(AckiNackiBridge.NotOwner.selector);
    }

    function test_td25_depositPath_reenterSkimExcessUsdc_revertsNotOwner() public {
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.SkimExcessUsdc, UsdcTestLib.UNIT);
        _depositExpectRevert(AckiNackiBridge.NotOwner.selector);
    }

    function test_td25_aaveSupplyPath_reenterDeposit_revertsReentrancy() public {
        UsdcTestLib.depositUsdc(vm, token, bridge, user, 10 * UsdcTestLib.UNIT);
        token.wireDeposit(bytes32(uint256(uint160(user))), UsdcTestLib.UNIT);

        Snap memory before = _snap();
        vm.expectRevert(AckiNackiBridge.Reentrancy.selector);
        bridge.supplyToAave(type(uint256).max);
        _assertUnchanged(before, _snap());
    }

    function test_td25_aaveSupplyPath_reenterVerifyBlock_revertsReentrancy() public {
        UsdcTestLib.depositUsdc(vm, token, bridge, user, 10 * UsdcTestLib.UNIT);
        _wireVerifyBlockReenter();

        Snap memory before = _snap();
        vm.expectRevert(AckiNackiBridge.Reentrancy.selector);
        bridge.supplyToAave(type(uint256).max);
        _assertUnchanged(before, _snap());
    }

    function test_td25_aaveSupplyPath_reenterSupplyToAave_revertsNotOwner() public {
        UsdcTestLib.depositUsdc(vm, token, bridge, user, 10 * UsdcTestLib.UNIT);
        token.wireOwnerTarget(CrossFnReentrantERC20.CrossFnTarget.SupplyToAave, UsdcTestLib.UNIT);

        Snap memory before = _snap();
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.supplyToAave(type(uint256).max);
        _assertUnchanged(before, _snap());
    }
}
