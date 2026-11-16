// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./helpers/NacklBridgeFixture.sol";

contract NacklHandler is Test {
    AckiNackiBridge internal immutable bridge;
    NacklToken internal immutable nackl;
    uint256 internal immutable anchor;
    uint256 internal immutable dappFr;
    uint256 internal immutable accFr;
    uint256 internal immutable maxTransfer;

    uint256 public nextNullifier = 1_000;
    uint256 public acceptedByProofs;
    uint256[] public mintTimes;
    uint256[] public mintAmounts;

    constructor(
        AckiNackiBridge b,
        NacklToken t,
        uint256 anchor_,
        uint256 dapp,
        uint256 acc,
        uint256 cap
    ) {
        bridge = b;
        nackl = t;
        anchor = anchor_;
        dappFr = dapp;
        accFr = acc;
        maxTransfer = cap;
    }

    function withdraw(uint256 amount) external {
        amount = bound(amount, 1, maxTransfer);
        uint256 before = nackl.totalSupply();
        uint256 a = uint256(uint160(address(0x1111111111111111111111111111111111111111)));
        bridge.withdrawByProof(
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
                nullifier: nextNullifier++,
                finalRoot: anchor,
                anchorLayer: 1,
                dstBridge: uint256(uint160(address(bridge)))
            })
        );
        acceptedByProofs += amount;
        _record(before);
    }

    function execute(uint256 id) external {
        (,,,,,, uint256 count,) = bridge.nacklState();
        if (count == 0) return;
        id = bound(id, 1, count);
        uint256 before = nackl.totalSupply();
        try bridge.executeNacklPayout(id) { } catch { }
        _record(before);
    }

    function wait(uint256 secondsAhead) external {
        vm.warp(block.timestamp + bound(secondsAhead, 1, 30 hours));
    }

    function mintCount() external view returns (uint256) {
        return mintTimes.length;
    }

    function _record(uint256 before) private {
        uint256 delta = nackl.totalSupply() - before;
        if (delta != 0) {
            mintTimes.push(block.timestamp);
            mintAmounts.push(delta);
        }
    }
}

contract NacklBridgeInvariantTest is NacklBridgeFixture {
    NacklHandler internal handler;

    uint256 internal constant NACKL_SLOT = 19;

    function setUp() public override {
        super.setUp();
        handler = new NacklHandler(bridge, nackl, seedAnchor, DAPP_FR, ACC_FR, MAX_TRANSFER);
        targetContract(address(handler));
    }

    function invariant_NB_P_3_supplyIsMintedAndAcceptedIsMintedPlusQueued() public view {
        (uint256 accepted, uint256 minted, uint256 pendingCount, uint256 payoutCount) = _state();
        assertEq(nackl.totalSupply(), minted);
        assertEq(accepted, handler.acceptedByProofs());
        uint256 queued;
        uint256 open;
        for (uint256 id = 1; id <= payoutCount; id++) {
            bytes32 base = keccak256(abi.encode(id, NACKL_SLOT + 4));
            uint256 word = uint256(vm.load(address(bridge), base));
            assertTrue(address(uint160(word)) != address(0), "a queued payout lost its record");
            if ((word >> 224) & 1 == 0) {
                queued += uint256(vm.load(address(bridge), bytes32(uint256(base) + 1)));
                open += 1;
            }
        }
        assertEq(accepted, minted + queued);
        assertEq(open, pendingCount);
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
