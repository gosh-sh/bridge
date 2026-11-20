// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./helpers/NacklBridgeFixture.sol";

contract NacklHandler is Test {
    AckiNackiBridge internal immutable bridge;
    NacklToken internal immutable nackl;
    uint256 internal immutable anchor;
    uint256 internal immutable dappFr;
    uint256 internal immutable accFr;
    uint256 internal immutable maxAmount;

    uint256 public nextNullifier = 1_000;
    uint256 public paidByProofs;
    uint256 public refusedButSpent;
    uint256[] public mintTimes;
    uint256[] public mintAmounts;

    constructor(
        AckiNackiBridge b,
        NacklToken t,
        uint256 anchor_,
        uint256 dapp,
        uint256 acc,
        uint256 maxAmount_
    ) {
        bridge = b;
        nackl = t;
        anchor = anchor_;
        dappFr = dapp;
        accFr = acc;
        maxAmount = maxAmount_;
    }

    function withdraw(uint256 amount) external {
        amount = bound(amount, 1, maxAmount);
        uint256 before = nackl.totalSupply();
        uint256 a = uint256(uint160(address(0x1111111111111111111111111111111111111111)));
        uint256 nullifier = nextNullifier++;
        try bridge.withdrawByProof(
            hex"00",
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 1,
                amount: amount,
                recipientHi: a >> 80,
                recipientLo: a & ((1 << 80) - 1),
                dstChainId: block.chainid,
                senderAccFr: 1,
                dappFr: dappFr,
                accFr: accFr,
                nullifier: nullifier,
                finalRoot: anchor,
                anchorLayer: 1,
                dstBridge: uint256(uint160(address(bridge)))
            })
        ) {
            paidByProofs += amount;
            mintTimes.push(block.timestamp);
            mintAmounts.push(nackl.totalSupply() - before);
        } catch {
            if (bridge.isNullifierUsed(nullifier) || nackl.totalSupply() != before) {
                refusedButSpent += 1;
            }
        }
    }

    function wait(uint256 secondsAhead) external {
        vm.warp(block.timestamp + bound(secondsAhead, 1, 30 hours));
    }

    function mintCount() external view returns (uint256) {
        return mintTimes.length;
    }
}

contract NacklBridgeInvariantTest is NacklBridgeFixture {
    NacklHandler internal handler;

    function setUp() public override {
        super.setUp();
        handler = new NacklHandler(bridge, nackl, seedAnchor, DAPP_FR, ACC_FR, DAILY_LIMIT);
        targetContract(address(handler));
    }

    function invariant_NB_P_3_supplyIsWhatProofsPaid() public view {
        assertEq(nackl.totalSupply(), _minted(bridge));
        assertEq(_minted(bridge), handler.paidByProofs());
        assertEq(handler.refusedButSpent(), 0);
    }

    function invariant_NB_P_4_any24HoursWithinTheLimit() public view {
        uint256 n = handler.mintCount();
        for (uint256 i = 0; i < n; i++) {
            uint256 sum;
            uint256 start = handler.mintTimes(i);
            for (uint256 j = i; j < n && handler.mintTimes(j) < start + 24 hours; j++) {
                sum += handler.mintAmounts(j);
            }
            assertLe(sum, DAILY_LIMIT);
        }
    }
}
