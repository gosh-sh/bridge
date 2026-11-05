// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./helpers/AaveBridgeFixture.sol";

/// @notice Drives deposits, payouts, owner AAVE paths, setters, direct USDC
///         and a pool that refuses supplies or runs short of liquidity, and
///         checks the rules each call must keep (VT-*, ST-*, HL-*) as it goes.
contract AaveBandHandler is AaveBridgeFixture {
    address internal constant USER = address(0xA11CE);

    uint256 public nextNullifier = 1;
    /// @dev Set when a call broke a rule; the invariant reads it.
    string public broken;
    /// @dev A refused best-effort pull left `S > B`: INV-3 holds only while
    ///      the pool serves that pull, and again once a later one lands.
    bool public overBookedByRefusal;

    bytes32 internal constant REFILL_FAILED = keccak256("AaveRefillFailed(uint256,bytes)");
    bytes32 internal constant SUPPLY_REFUSED = keccak256("AaveSupplyRefused(uint256,bytes)");

    constructor() {
        _deployFixture();
    }

    function exposedBridge() external view returns (AckiNackiBridge) {
        return bridge;
    }

    function exposedUsdc() external view returns (MockERC20) {
        return usdc;
    }

    // ---- user paths -----------------------------------------------------

    function deposit(uint256 amount) external {
        uint256 room = bridge.MAX_BRIDGE_TVL() - bridge.treasuryBalance();
        uint256 cap = bridge.MAX_TRANSFER_AMOUNT() < room ? bridge.MAX_TRANSFER_AMOUNT() : room;
        if (cap == 0) return;
        amount = bound(amount, 1, cap);
        (uint256 s, uint256 b, uint256 r, uint256 m) = _snapshot();

        usdc.mint(USER, amount);
        vm.prank(USER);
        usdc.approve(address(bridge), amount);
        vm.recordLogs();
        vm.prank(USER);
        try bridge.deposit(address(usdc), amount, bytes32(uint256(1))) { }
        catch {
            _break("HL-1: a deposit reverted");
            return;
        }
        bool refused = _emitted(SUPPLY_REFUSED);

        if (bridge.suppliedPrincipal() < s) _break("VT-1: S fell in deposit");
        if (bridge.treasuryBalance() != b + amount) _break("VT-2: B off in deposit");
        _sameBand(r, m);
        // Up to rounding: for a tiny book `H` floors to `T`, and `L = T` then.
        if (!refused && bridge.aaveEnabled() && _liquid() >= _threshold() && _liquid() > _target())
        {
            _break("ST-1: L >= H after a deposit");
        }
    }

    function pay(uint256 amount) external {
        uint256 b = bridge.treasuryBalance();
        if (b == 0) return;
        uint256 cap = bridge.MAX_TRANSFER_AMOUNT() < b ? bridge.MAX_TRANSFER_AMOUNT() : b;
        amount = bound(amount, 1, cap);
        uint256 s = bridge.suppliedPrincipal();
        (,, uint256 r, uint256 m) = _snapshot();
        uint256 u = usdc.balanceOf(address(bridge));
        uint256 a = bridge.aUsdcBalance();
        uint256 required = u < amount ? amount - u : 0;
        uint256 servable = a < pool.withdrawLimit() ? a : pool.withdrawLimit();
        uint256 liquidBefore = _liquid();
        uint256 want = liquidBefore >= amount
            ? 0
            : amount - liquidBefore + (s > b ? s - b : 0)
                + ((b - amount) * bridge.liquidReserveBps()) / bridge.BPS_DENOMINATOR();

        vm.recordLogs();
        try bridge.withdrawByProof(hex"00", _pub(amount, nextNullifier)) {
            nextNullifier++;
        } catch {
            if (required <= servable) _break("HL-2: a payout the pool could serve reverted");
            return;
        }
        bool refillFailed = _emitted(REFILL_FAILED);

        if (bridge.suppliedPrincipal() > s) _break("VT-1: S grew in a payout");
        if (bridge.treasuryBalance() != b - amount) _break("VT-2: B off in a payout");
        _sameBand(r, m);
        if (liquidBefore >= amount) {
            if (bridge.aUsdcBalance() != a) _break("HL-3: a covered payout called the pool");
        } else if (!refillFailed && want <= a && _liquid() != _target()) {
            _break("ST-2: L != T after a refill");
        }
        if (bridge.suppliedPrincipal() <= bridge.treasuryBalance()) {
            overBookedByRefusal = false;
        } else if (refillFailed) {
            overBookedByRefusal = true;
        }
    }

    function sendDirect(uint256 amount) external {
        amount = bound(amount, 1, 100 * UNIT);
        (uint256 s, uint256 b, uint256 r, uint256 m) = _snapshot();
        usdc.mint(address(bridge), amount);
        _sameBook(s, b);
        _sameBand(r, m);
    }

    // ---- pool behaviour -------------------------------------------------

    function accrue(uint256 amount) external {
        amount = bound(amount, 1, 5 * UNIT);
        aUSDC.accrueYield(address(bridge), amount);
        usdc.mint(address(pool), amount);
    }

    function setSupplyRefused(bool refused) external {
        pool.setSupplyRefused(refused);
    }

    function setWithdrawLimit(uint256 limit) external {
        pool.setWithdrawLimit(limit % 4 == 0 ? type(uint256).max : bound(limit, 0, 60 * UNIT));
    }

    // ---- owner paths ----------------------------------------------------

    function setReserve(uint256 bps) external {
        uint256 maxBps =
            (bridge.BPS_DENOMINATOR() * bridge.BPS_DENOMINATOR()) / bridge.supplyMultiplierBps();
        if (maxBps > bridge.MAX_LIQUID_RESERVE_BPS()) maxBps = bridge.MAX_LIQUID_RESERVE_BPS();
        bridge.setLiquidReserveBps(bound(bps, bridge.MIN_LIQUID_RESERVE_BPS(), maxBps));
    }

    function setMultiplier(uint256 bps) external {
        uint256 maxBps =
            (bridge.BPS_DENOMINATOR() * bridge.BPS_DENOMINATOR()) / bridge.liquidReserveBps();
        bridge.setSupplyMultiplierBps(bound(bps, bridge.BPS_DENOMINATOR() + 1, maxBps));
    }

    function setAaveEnabled(bool enabled) external {
        bridge.setAaveEnabled(enabled);
    }

    function withdrawFromAave(uint256 amount) external {
        uint256 a = bridge.aUsdcBalance();
        uint256 backed = bridge.suppliedPrincipal() < a ? bridge.suppliedPrincipal() : a;
        if (backed == 0 || backed > pool.withdrawLimit()) return;
        (uint256 s, uint256 b, uint256 r, uint256 m) = _snapshot();
        bridge.withdrawFromAave(bound(amount, 1, backed));
        if (bridge.suppliedPrincipal() > s) _break("VT-1: S grew in withdrawFromAave");
        if (bridge.treasuryBalance() != b) _break("VT-2: B moved in withdrawFromAave");
        _sameBand(r, m);
    }

    function harvest(uint256 amount) external {
        uint256 y = bridge.accruedYield();
        if (y == 0 || y > pool.withdrawLimit()) return;
        (uint256 s, uint256 b, uint256 r, uint256 m) = _snapshot();
        try bridge.harvestYield(bound(amount, 1, y)) { } catch { }
        _sameBook(s, b);
        _sameBand(r, m);
        _covered("HL-4: harvest took U + A below B");
    }

    function skim(uint256 amount) external {
        uint256 x = bridge.excessUsdc();
        if (x == 0) return;
        (uint256 s, uint256 b, uint256 r, uint256 m) = _snapshot();
        bridge.skimExcessUsdc(bound(amount, 1, x));
        _sameBook(s, b);
        _sameBand(r, m);
        _covered("HL-4: skim took U + A below B");
    }

    // ---- checks ---------------------------------------------------------

    function _snapshot() internal view returns (uint256, uint256, uint256, uint256) {
        return (
            bridge.suppliedPrincipal(),
            bridge.treasuryBalance(),
            bridge.liquidReserveBps(),
            bridge.supplyMultiplierBps()
        );
    }

    function _sameBook(uint256 s, uint256 b) internal {
        if (bridge.suppliedPrincipal() != s) _break("VT-1: S moved");
        if (bridge.treasuryBalance() != b) _break("VT-2: B moved");
    }

    function _sameBand(uint256 r, uint256 m) internal {
        if (bridge.liquidReserveBps() != r || bridge.supplyMultiplierBps() != m) {
            _break("VT-3: r or M moved outside a setter");
        }
    }

    function _covered(string memory rule) internal {
        if (usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance() < bridge.treasuryBalance()) {
            _break(rule);
        }
    }

    function _emitted(bytes32 topic) internal returns (bool) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == address(bridge) && logs[i].topics[0] == topic) return true;
        }
        return false;
    }

    function _break(string memory rule) internal {
        if (bytes(broken).length == 0) broken = rule;
    }
}

/// @title AckiNackiBridgeAaveInvariantTest
/// @notice TC-14: INV-1..3 after every call; ST-1, ST-2, VT-1..3, HL-1, HL-2,
///         HL-4 inside the handler.
contract AckiNackiBridgeAaveInvariantTest is Test {
    AaveBandHandler internal handler;
    AckiNackiBridge internal bridge;
    MockERC20 internal usdc;

    function setUp() public {
        handler = new AaveBandHandler();
        bridge = handler.exposedBridge();
        usdc = handler.exposedUsdc();
        targetContract(address(handler));
    }

    function invariant_rulesKeptByEveryCall() public view {
        assertEq(handler.broken(), "");
    }

    /// @dev INV-1, with a pool that returns what it holds.
    function invariant_inv1_holdingsCoverTheBook() public view {
        assertGe(usdc.balanceOf(address(bridge)) + bridge.aUsdcBalance(), bridge.treasuryBalance());
    }

    /// @dev INV-2.
    function invariant_inv2_bandInRange() public view {
        uint256 r = bridge.liquidReserveBps();
        uint256 m = bridge.supplyMultiplierBps();
        assertGe(r, bridge.MIN_LIQUID_RESERVE_BPS());
        assertLe(r, bridge.MAX_LIQUID_RESERVE_BPS());
        assertGt(m, bridge.BPS_DENOMINATOR());
        assertLe(r * m, bridge.BPS_DENOMINATOR() * bridge.BPS_DENOMINATOR());
    }

    /// @dev INV-3, unless a refused best-effort pull left the book short.
    function invariant_inv3_onlyPrincipalIsBooked() public view {
        if (handler.overBookedByRefusal()) return;
        assertLe(bridge.suppliedPrincipal(), bridge.treasuryBalance());
    }
}
