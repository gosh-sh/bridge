// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "@src/AckiNackiBridge.sol";
import "@src/IBridgeWithdrawalVerifier.sol";
import "@bridge-test/mocks/MockERC20.sol";
import "@bridge-test/mocks/FeeOnTransferERC20.sol";
import "@bridge-test/mocks/MockAave.sol";
import "@bridge-test/helpers/Bn254FrLib.sol";

/// @title DepositHandler
/// @notice Handler for DEP-5 / DEP-3 — monotonic depositCounter and treasury ledger.
contract DepositHandler is Test {
    AckiNackiBridge public immutable bridge;
    MockERC20 public immutable usdc;

    uint256 public depositOps;
    uint256 public ghostDeposited;

    constructor(AckiNackiBridge _bridge, MockERC20 _usdc) {
        bridge = _bridge;
        usdc = _usdc;
    }

    /// @dev BOUNDS: amount ∈ [1, MAX_DEPOSIT_AMOUNT]
    function deposit(uint256 amountSeed, uint256 userSeed) external {
        uint256 amount = bound(amountSeed, 1, bridge.MAX_DEPOSIT_AMOUNT());
        address user =
            address(uint160(Bn254FrLib.toFr(uint256(keccak256(abi.encode("dep-inv", userSeed, depositOps))))));
        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
        depositOps++;
        ghostDeposited += amount;
    }
}

/// @title TreasuryHandler
/// @notice Handler for TR-1 / TR-2 invariant campaigns (deposit, AAVE ops, withdraw).
/// @dev Ghost vars: `ghostDeposited`, `ghostWithdrawn`. TD-14 interleaves donation/skim/accrue.
contract TreasuryHandler is Test {
    AckiNackiBridge public immutable bridge;
    MockERC20 public immutable usdc;
    MockAUSDC public immutable aUSDC;
    MockAavePool public immutable pool;
    address public immutable owner;

    uint256 public ghostDeposited;
    uint256 public ghostWithdrawn;
    uint256 internal depositNonce;
    uint256 internal withdrawNonce;
    uint256 internal donateNonce;

    uint256 public immutable seedAnchor;
    uint256 public immutable dappFr;
    uint256 public immutable accFr;
    address public immutable recipient;

    constructor(
        AckiNackiBridge _bridge,
        MockERC20 _usdc,
        MockAUSDC _aUSDC,
        MockAavePool _pool,
        address _owner,
        uint256 _seedAnchor,
        uint256 _dappFr,
        uint256 _accFr,
        address _recipient,
        uint256 _initialGhostDeposited
    ) {
        bridge = _bridge;
        usdc = _usdc;
        aUSDC = _aUSDC;
        pool = _pool;
        owner = _owner;
        seedAnchor = _seedAnchor;
        dappFr = _dappFr;
        accFr = _accFr;
        recipient = _recipient;
        ghostDeposited = _initialGhostDeposited;
    }

    /// @dev BOUNDS: amount ∈ [1, MAX_DEPOSIT_AMOUNT]
    function deposit(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, bridge.MAX_DEPOSIT_AMOUNT());
        address user = address(uint160(Bn254FrLib.toFr(uint256(keccak256(abi.encode(depositNonce++, amountSeed))))));
        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();
        ghostDeposited += amount;
    }

    function supplyToAaveMax() external {
        vm.startPrank(owner);
        if (bridge.aaveEnabled()) {
            bridge.supplyToAave(type(uint256).max);
        }
        vm.stopPrank();
    }

    function withdrawFromAaveMax() external {
        vm.startPrank(owner);
        if (bridge.suppliedPrincipal() > 0) {
            bridge.withdrawFromAave(type(uint256).max);
        }
        vm.stopPrank();
    }

    function harvestAllYield() external {
        vm.startPrank(owner);
        uint256 y = bridge.accruedYield();
        if (y > 0) {
            bridge.harvestYield(y);
        }
        vm.stopPrank();
    }

    function emergencyWithdrawAll() external {
        vm.startPrank(owner);
        if (bridge.aUsdcBalance() > 0 || bridge.suppliedPrincipal() > 0) {
            bridge.emergencyWithdrawAll();
        }
        vm.stopPrank();
    }

    /// @dev TD-14 — accrue AAVE yield on supplied principal (pre-harvest path).
    function accrueAaveYield(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, 1_000_000);
        aUSDC.accrueYield(address(bridge), amount);
        usdc.mint(address(pool), amount);
    }

    /// @dev TD-14 — skim liquid excess above `treasuryBalance` (post-emergency yield).
    function skimExcessUsdcMax() external {
        vm.startPrank(owner);
        if (bridge.excessUsdc() > 0) {
            bridge.skimExcessUsdc(type(uint256).max);
        }
        vm.stopPrank();
    }

    function skimExcessPartial(uint256 amountSeed) external {
        vm.startPrank(owner);
        uint256 excess = bridge.excessUsdc();
        if (excess > 0) {
            bridge.skimExcessUsdc(bound(amountSeed, 1, excess));
        }
        vm.stopPrank();
    }

    /// @dev TD-14 / TR-3 cross — direct USDC transfer does not credit `treasuryBalance`.
    function donateDirectUsdc(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, bridge.MAX_DEPOSIT_AMOUNT());
        address donor =
            address(uint160(Bn254FrLib.toFr(uint256(keccak256(abi.encode("donate", donateNonce++, amountSeed))))));
        usdc.mint(donor, amount);
        vm.startPrank(donor);
        usdc.transfer(address(bridge), amount);
        vm.stopPrank();
    }

    /// @dev BOUNDS: amount ∈ [1, treasuryBalance]
    function withdrawByProof(uint256 amountSeed, uint256 nullifierSeed) external {
        uint256 tb = bridge.treasuryBalance();
        if (tb == 0) return;

        uint256 amount = bound(amountSeed, 1, tb);
        if (amount > tb) return;

        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256(abi.encode(withdrawNonce++, nullifierSeed))));
        if (bridge.isNullifierUsed(nullifier)) return;

        (uint256 hi, uint256 lo) = _split(recipient);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: amount,
            recipientHi: hi,
            recipientLo: lo,
            dstChainId: block.chainid,
            senderAccFr: Bn254FrLib.toFr(uint256(keccak256("inv-sender"))),
            dappFr: dappFr,
            accFr: accFr,
            nullifier: nullifier,
            finalRoot: seedAnchor
        });

        bridge.withdrawByProof(_dummyProof(), pub);
        ghostWithdrawn += amount;
    }

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & ((1 << 80) - 1);
    }

    function _dummyProof() internal pure returns (bytes memory) {
        return hex"00";
    }
}

/// @title FoTTreasuryHandler
/// @notice TD-23 / ETH-11 — FoT `deposit` must revert (`TransferAmountMismatch`).
/// @dev Each call attempts a fee-on-transfer deposit and expects it to fail closed.
contract FoTTreasuryHandler is Test {
    AckiNackiBridge public immutable bridge;
    FeeOnTransferERC20 public immutable fot;
    uint256 public immutable minDepositAmount;

    uint256 public ghostDeposited;
    uint256 public ghostCustodyReceived;
    uint256 public fotDepositOps;
    uint256 internal depositNonce;

    constructor(
        AckiNackiBridge _bridge,
        FeeOnTransferERC20 _fot,
        uint256 _initialGhostDeposited,
        uint256 _minDepositAmount
    ) {
        bridge = _bridge;
        fot = _fot;
        ghostDeposited = _initialGhostDeposited;
        minDepositAmount = _minDepositAmount;
    }

    /// @dev BOUNDS: amount ∈ [minDepositAmount, MAX_DEPOSIT_AMOUNT] — fee must be non-zero.
    function depositFoT(uint256 amountSeed) external {
        if (minDepositAmount > bridge.MAX_DEPOSIT_AMOUNT()) return;
        uint256 amount = bound(amountSeed, minDepositAmount, bridge.MAX_DEPOSIT_AMOUNT());
        address user =
            address(uint160(Bn254FrLib.toFr(uint256(keccak256(abi.encode("fot-dep", depositNonce++, amountSeed))))));
        fot.mint(user, amount);
        vm.startPrank(user);
        fot.approve(address(bridge), amount);
        try bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user)))) {
            revert("ETH-11: FoT deposit must revert");
        } catch {
            // fail closed — no treasury credit
        }
        vm.stopPrank();
        fotDepositOps++;
    }
}

/// @title AaveHandler
/// @notice Handler for TR-3 — supply / accrue / harvest interleaving.
contract AaveHandler is Test {
    AckiNackiBridge public immutable bridge;
    MockERC20 public immutable usdc;
    MockAUSDC public immutable aUSDC;
    MockAavePool public immutable pool;
    address public immutable owner;

    constructor(AckiNackiBridge _bridge, MockERC20 _usdc, MockAUSDC _aUSDC, MockAavePool _pool, address _owner) {
        bridge = _bridge;
        usdc = _usdc;
        aUSDC = _aUSDC;
        pool = _pool;
        owner = _owner;
    }

    function depositAndSupply(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, bridge.MAX_DEPOSIT_AMOUNT());
        address user = address(uint160(Bn254FrLib.toFr(uint256(keccak256(abi.encode("aave-dep", amountSeed))))));
        usdc.mint(user, amount);
        vm.startPrank(user);
        usdc.approve(address(bridge), amount);
        bridge.deposit(amount, int8(0), bytes32(uint256(uint160(user))));
        vm.stopPrank();

        vm.startPrank(owner);
        if (bridge.aaveEnabled()) {
            bridge.supplyToAave(type(uint256).max);
        }
        vm.stopPrank();
    }

    function accrueYield(uint256 amountSeed) external {
        uint256 amount = bound(amountSeed, 1, 1_000_000);
        aUSDC.accrueYield(address(bridge), amount);
        usdc.mint(address(pool), amount);
    }

    function harvestPartial(uint256 amountSeed) external {
        vm.startPrank(owner);
        uint256 y = bridge.accruedYield();
        if (y > 0) {
            uint256 amt = bound(amountSeed, 1, y);
            bridge.harvestYield(amt);
        }
        vm.stopPrank();
    }
}

/// @title VerifyBlockHandler
/// @notice Handler for LH-6 / seq monotonicity fuzz (F-VB-1).
contract VerifyBlockHandler is Test {
    AckiNackiBridge public immutable bridge;
    uint256 public immutable bkSet;
    uint8 public immutable activeLayers;

    uint64 public nextSeq;

    constructor(AckiNackiBridge _bridge, uint256 _bkSet, uint8 _activeLayers, uint64 startSeq) {
        bridge = _bridge;
        bkSet = _bkSet;
        activeLayers = _activeLayers;
        nextSeq = startSeq;
    }

    function submitPrimaryBlock() external {
        uint64 seq = nextSeq;
        nextSeq = seq + 1;

        uint256[10] memory layers;
        for (uint256 i = 0; i < activeLayers; i++) {
            layers[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("vb-handler", seq, i))));
        }

        uint256 anchor = bridge.expectedPrevAnchor(activeLayers);
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256(abi.encode("att", seq))),
            abi.encodePacked(keccak256(abi.encode("lh", seq))),
            0x70000000 + seq,
            bkSet,
            seq,
            activeLayers,
            layers,
            anchor
        );
    }
}

/// @title WithdrawReplayHandler
/// @notice Handler for WD-7 — records spent nullifiers; replay attempts must fail.
contract WithdrawReplayHandler is Test {
    AckiNackiBridge public immutable bridge;

    uint256 public immutable seedAnchor;
    uint256 public immutable dappFr;
    uint256 public immutable accFr;
    address public immutable recipient;

    uint256[] public spentNullifiers;
    uint256 internal withdrawNonce;

    constructor(AckiNackiBridge _bridge, uint256 _seedAnchor, uint256 _dappFr, uint256 _accFr, address _recipient) {
        bridge = _bridge;
        seedAnchor = _seedAnchor;
        dappFr = _dappFr;
        accFr = _accFr;
        recipient = _recipient;
    }

    function withdrawOnce(uint256 amountSeed) external {
        uint256 tb = bridge.treasuryBalance();
        if (tb == 0) return;
        uint256 amount = bound(amountSeed, 1, tb);

        uint256 nullifier = Bn254FrLib.toFr(uint256(keccak256(abi.encode("wd7", withdrawNonce++))));
        if (bridge.isNullifierUsed(nullifier)) return;

        _withdraw(amount, nullifier);
        spentNullifiers.push(nullifier);
    }

    function _withdraw(uint256 amount, uint256 nullifier) internal {
        (uint256 hi, uint256 lo) = _split(recipient);
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub = IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: 0,
            amount: amount,
            recipientHi: hi,
            recipientLo: lo,
            dstChainId: block.chainid,
            senderAccFr: 1,
            dappFr: dappFr,
            accFr: accFr,
            nullifier: nullifier,
            finalRoot: seedAnchor
        });
        bridge.withdrawByProof(hex"00", pub);
    }

    function spentNullifiersLength() external view returns (uint256) {
        return spentNullifiers.length;
    }

    function _split(address addr) internal pure returns (uint256 hi, uint256 lo) {
        uint256 a = uint256(uint160(addr));
        hi = a >> 80;
        lo = a & ((1 << 80) - 1);
    }
}

/// @title OwnerOpsHandler
/// @notice Handler for A4-INV-1 — owner-only paths must not reduce `treasuryBalance`.
contract OwnerOpsHandler is Test {
    AckiNackiBridge public immutable bridge;
    MockAUSDC public immutable aUSDC;
    address public immutable owner;

    constructor(AckiNackiBridge _bridge, MockAUSDC _aUSDC, address _owner) {
        bridge = _bridge;
        aUSDC = _aUSDC;
        owner = _owner;
    }

    function supplyMax() external {
        vm.startPrank(owner);
        if (bridge.aaveEnabled() && bridge.treasuryBalance() > 0) {
            bridge.supplyToAave(type(uint256).max);
        }
        vm.stopPrank();
    }

    function withdrawFromAaveMax() external {
        vm.startPrank(owner);
        if (bridge.suppliedPrincipal() > 0) {
            bridge.withdrawFromAave(type(uint256).max);
        }
        vm.stopPrank();
    }

    function harvestPartial(uint256 seed) external {
        vm.startPrank(owner);
        uint256 y = bridge.accruedYield();
        if (y > 0) {
            bridge.harvestYield(bound(seed, 1, y));
        }
        vm.stopPrank();
    }

    function emergencyExit() external {
        vm.startPrank(owner);
        if (bridge.aUsdcBalance() > 0 || bridge.suppliedPrincipal() > 0) {
            bridge.emergencyWithdrawAll();
        }
        vm.stopPrank();
    }

    function skimExcessMax() external {
        vm.startPrank(owner);
        if (bridge.excessUsdc() > 0) {
            bridge.skimExcessUsdc(type(uint256).max);
        }
        vm.stopPrank();
    }

    function setLiquidReserve(uint256 seed) external {
        vm.startPrank(owner);
        bridge.setLiquidReserveBps(uint16(bound(seed, 0, 5_000)));
        vm.stopPrank();
    }

    function setYieldRecipient(uint256 seed) external {
        vm.startPrank(owner);
        bridge.setYieldRecipient(address(uint160(bound(seed, 1, type(uint160).max))));
        vm.stopPrank();
    }
}
