// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./MockERC20.sol";
import "../../src/AckiNackiBridge.sol";
import "../../src/IBridgeWithdrawalVerifier.sol";

/// @title CrossFnReentrantERC20
/// @notice Malicious ERC20: reenters a wired bridge entrypoint from `transferFrom`.
/// @dev Deposit path: `msg.sender == bridge`, `to == bridge`.
///      AAVE supply path: `msg.sender == pool`, `from == bridge`.
contract CrossFnReentrantERC20 is MockERC20 {
    enum CrossFnTarget {
        None,
        Deposit,
        VerifyBlock,
        ApplyBkSetUpdate,
        WithdrawByProof,
        SupplyToAave,
        WithdrawFromAave,
        EmergencyWithdrawAll,
        HarvestYield,
        SkimExcessUsdc
    }

    AckiNackiBridge internal bridge;
    address internal pool;
    bytes32 internal reenterAccount;
    uint256 internal reenterAmount;
    CrossFnTarget internal target;
    bool internal armed;
    bool internal entered;

    // verifyBlock reenter bundle
    uint256 internal vbBlockId;
    uint64 internal vbSeqNo;
    uint8 internal vbNumLayers;
    uint256[10] internal vbLayers;
    uint256 internal vbPrevAnchor;

    // applyBkSetUpdate reenter bundle
    uint256 internal bkBlockId;
    uint64 internal bkSeqNo;
    uint64 internal bkLastSeen;
    uint256 internal bkOldL2;
    uint256 internal bkNewL3;
    bytes32 internal bkSib01;
    bytes32 internal bkSib4_7;
    bytes32 internal bkSib8_15;

    // withdrawByProof reenter bundle
    bytes internal wdProof;
    IBridgeWithdrawalVerifier.WithdrawalPublicInputs internal wdPub;

    constructor() MockERC20("X", "X", 6) { }

    function wireBridge(AckiNackiBridge _bridge, address _pool) external {
        bridge = _bridge;
        pool = _pool;
        armed = false;
        target = CrossFnTarget.None;
        entered = false;
    }

    function wireDeposit(bytes32 anAccount, uint256 amount) external {
        armed = true;
        target = CrossFnTarget.Deposit;
        reenterAccount = anAccount;
        reenterAmount = amount;
    }

    function wireVerifyBlock(
        uint256 blockId,
        uint64 seqNo,
        uint8 numLayers,
        uint256[10] calldata layers,
        uint256 prevAnchor
    ) external {
        armed = true;
        target = CrossFnTarget.VerifyBlock;
        vbBlockId = blockId;
        vbSeqNo = seqNo;
        vbNumLayers = numLayers;
        for (uint256 i = 0; i < 10; i++) {
            vbLayers[i] = layers[i];
        }
        vbPrevAnchor = prevAnchor;
    }

    function wireApplyBkSetUpdate(
        uint256 blockId,
        uint64 seqNo,
        uint256 oldL2,
        uint256 newL3,
        bytes32 sib01,
        bytes32 sib4_7,
        bytes32 sib8_15
    ) external {
        armed = true;
        target = CrossFnTarget.ApplyBkSetUpdate;
        bkBlockId = blockId;
        bkSeqNo = seqNo;
        bkLastSeen = 0;
        bkOldL2 = oldL2;
        bkNewL3 = newL3;
        bkSib01 = sib01;
        bkSib4_7 = sib4_7;
        bkSib8_15 = sib8_15;
    }

    function wireWithdrawByProof(
        bytes calldata proof,
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs calldata pub
    ) external {
        armed = true;
        target = CrossFnTarget.WithdrawByProof;
        wdProof = proof;
        wdPub = pub;
    }

    function wireOwnerTarget(CrossFnTarget t, uint256 amount) external {
        armed = true;
        target = t;
        reenterAmount = amount;
    }

    function transferFrom(address from, address to, uint256 amount)
        external
        override
        returns (bool)
    {
        bool depositPath = msg.sender == address(bridge) && to == address(bridge);
        bool supplyPath = pool != address(0) && msg.sender == pool && from == address(bridge);
        if (!entered && armed && (depositPath || supplyPath)) {
            entered = true;
            _reenter();
        }

        uint256 allowed = _allowances[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "CrossFnReentrantERC20: allowance");
            _allowances[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _reenter() internal {
        if (target == CrossFnTarget.None) return;
        if (target == CrossFnTarget.Deposit) {
            bridge.deposit(reenterAmount, int8(0), reenterAccount);
        } else if (target == CrossFnTarget.VerifyBlock) {
            bridge.verifyBlock(
                AckiNackiBridge.FinalizationType.Primary,
                abi.encodePacked(keccak256("td25-vb-att")),
                abi.encodePacked(keccak256("td25-vb-lh")),
                vbBlockId,
                bridge.storedBkSetCommitment(),
                vbSeqNo,
                vbNumLayers,
                vbLayers,
                vbPrevAnchor
            );
        } else if (target == CrossFnTarget.ApplyBkSetUpdate) {
            bridge.applyBkSetUpdate(
                AckiNackiBridge.FinalizationType.Primary,
                hex"00",
                bkBlockId,
                bkSeqNo,
                bkLastSeen,
                bkOldL2,
                bkNewL3,
                bkSib01,
                bkSib4_7,
                bkSib8_15
            );
        } else if (target == CrossFnTarget.WithdrawByProof) {
            bridge.withdrawByProof(wdProof, wdPub);
        } else if (target == CrossFnTarget.SupplyToAave) {
            bridge.supplyToAave(reenterAmount);
        } else if (target == CrossFnTarget.WithdrawFromAave) {
            bridge.withdrawFromAave(reenterAmount);
        } else if (target == CrossFnTarget.EmergencyWithdrawAll) {
            bridge.emergencyWithdrawAll();
        } else if (target == CrossFnTarget.HarvestYield) {
            bridge.harvestYield(reenterAmount);
        } else if (target == CrossFnTarget.SkimExcessUsdc) {
            bridge.skimExcessUsdc(reenterAmount);
        }
    }
}
