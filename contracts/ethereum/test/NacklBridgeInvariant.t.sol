// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./helpers/NacklBridgeFixture.sol";

/// @notice Drives payouts, queued executions, deposits, limit changes and time,
///         and checks every mint against the window and the delay as it happens.
contract NacklHandler is Test {
    AckiNackiBridge internal bridge;
    NacklToken internal nackl;
    uint256 internal anchor;
    uint256 internal immutable maxTransfer;
    address internal constant HOLDER = address(0xA11CE);

    struct Mint {
        uint256 at;
        uint256 amount;
    }

    Mint[] internal mints;
    uint256[] internal queued;
    mapping(uint256 => uint256) internal acceptedAt;
    mapping(uint256 => bool) internal minted;
    mapping(uint256 => bool) internal large;
    uint256 internal nextNullifier = 1;

    uint256 public accepted;
    uint256 public mintedTotal;
    uint256 public burned;
    bool public windowExceeded;
    bool public mintedEarly;
    bool public mintedTwice;

    constructor(AckiNackiBridge bridge_, NacklToken nackl_, uint256 anchor_, uint256 maxTransfer_) {
        bridge = bridge_;
        nackl = nackl_;
        anchor = anchor_;
        maxTransfer = maxTransfer_;
    }

    function pay(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, maxTransfer);
        uint256 nullifier = nextNullifier++;
        uint256 a = uint256(uint160(HOLDER));
        uint256 before = nackl.totalSupply();
        (, uint256 threshold,,,) = bridge.nacklState();
        large[nullifier] = amount > threshold;
        bridge.withdrawByProof(
            hex"c4",
            IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
                tokenId: 1,
                amount: amount,
                recipientHi: a >> 80,
                recipientLo: a & ((1 << 80) - 1),
                dstChainId: block.chainid,
                senderAccFr: 7,
                dappFr: bridge.bridgeWithdrawalDappFr(),
                accFr: bridge.bridgeWithdrawalAccFr(),
                nullifier: nullifier,
                finalRoot: anchor,
                anchorLayer: 1,
                dstBridge: uint256(uint160(address(bridge)))
            })
        );
        accepted += amount;
        acceptedAt[nullifier] = block.timestamp;
        if (nackl.totalSupply() > before) {
            _record(nullifier, amount);
        } else {
            queued.push(nullifier);
        }
    }

    function execute(uint256 indexSeed) external {
        if (queued.length == 0) return;
        uint256 i = bound(indexSeed, 0, queued.length - 1);
        uint256 nullifier = queued[i];
        (, uint256 amount,) = bridge.nacklPayout(nullifier);
        try bridge.executeNacklPayout(nullifier) {
            queued[i] = queued[queued.length - 1];
            queued.pop();
            _record(nullifier, amount);
        } catch { }
    }

    function deposit(uint256 amountSeed) external {
        uint256 balance = nackl.balanceOf(HOLDER);
        if (balance == 0) return;
        uint256 amount = bound(amountSeed, 1, balance < maxTransfer ? balance : maxTransfer);
        vm.startPrank(HOLDER);
        nackl.approve(address(bridge), amount);
        bridge.deposit(address(nackl), amount, bytes32(uint256(43)));
        vm.stopPrank();
        burned += amount;
    }

    function setLimits(uint256 dailySeed, uint256 thresholdSeed) external {
        uint256 daily = bound(dailySeed, maxTransfer, 8 * maxTransfer);
        bridge.setNacklMintLimits(daily, bound(thresholdSeed, 0, maxTransfer));
    }

    function warp(uint256 secondsSeed) external {
        vm.warp(block.timestamp + bound(secondsSeed, 0, 9 hours));
    }

    function _record(uint256 nullifier, uint256 amount) internal {
        if (minted[nullifier]) mintedTwice = true;
        minted[nullifier] = true;
        mintedTotal += amount;
        (uint256 daily,,,,) = bridge.nacklState();
        if (large[nullifier] && block.timestamp < acceptedAt[nullifier] + 24 hours) {
            mintedEarly = true;
        }
        mints.push(Mint(block.timestamp, amount));
        uint256 sum;
        for (uint256 k = mints.length; k > 0; --k) {
            Mint memory m = mints[k - 1];
            if (m.at + 24 hours <= block.timestamp) break;
            sum += m.amount;
        }
        if (sum > daily) windowExceeded = true;
    }
}

/// @title NacklBridgeInvariantTest
/// @notice P-2 … P-5 of docs/NACKL-bridge-extension.md over random sequences.
contract NacklBridgeInvariantTest is NacklBridgeFixture {
    NacklHandler internal handler;

    function setUp() public override {
        super.setUp();
        handler = new NacklHandler(bridge, nackl, anchor, MAX_TRANSFER);
        bridge.transferOwnership(address(handler));
        vm.prank(address(handler));
        bridge.acceptOwnership();
        targetContract(address(handler));
    }

    /// @notice P-2: the bridge mints only accepted payouts, each at most once.
    function invariant_P2_mintsOnlyAcceptedPayoutsOnce() public view {
        assertLe(handler.mintedTotal(), handler.accepted());
        assertEq(nackl.totalSupply(), handler.mintedTotal() - handler.burned());
        assertFalse(handler.mintedTwice());
    }

    /// @notice P-3: within any 24 hours the bridge mints at most the daily limit.
    function invariant_P3_mintsWithinAnyDayStayWithinTheLimit() public view {
        assertFalse(handler.windowExceeded());
    }

    /// @notice P-4: a payout above the threshold is minted no earlier than 24 hours after
    ///         the `withdrawByProof` that accepted it.
    function invariant_P4_largePayoutWaitsADay() public view {
        assertFalse(handler.mintedEarly());
    }

    /// @notice P-5: the bridge holds no NACKL.
    function invariant_P5_bridgeHoldsNoNackl() public view {
        assertEq(nackl.balanceOf(address(bridge)), 0);
    }
}
