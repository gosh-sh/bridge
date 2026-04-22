// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAckiNackiVerifier.sol";
import "./IBlockHeaderOracle.sol";
import "./IAavePool.sol";
import "./IWrappedTokenGatewayV3.sol";
import "./IERC20.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain
/// @dev Uses event-based proofs verified by ZK circuits (no on-chain Merkle tree).
///      Idle ETH can be supplied to AAVE V3 to earn yield.
///      - `deposit()` stays cheap: funds accumulate in the contract; an owner/keeper
///        batches supplies to AAVE with `supplyToAave()` to amortise gas.
///      - `withdraw()` transparently pulls from AAVE if the bridge's ETH balance is short.
///      - Owner can harvest accrued yield without touching user principal.
contract AckiNackiBridge {
    // ---------------------------------------------------------------------
    // Constants
    // ---------------------------------------------------------------------

    /// @notice Maximum deposit amount (prevents whale deposits)
    uint256 public constant MAX_DEPOSIT_AMOUNT = 100 ether;

    /// @notice Basis-point denominator
    uint256 public constant BPS_DENOMINATOR = 10_000;

    /// @notice Upper bound on the liquid reserve (50% of treasury kept as ETH)
    uint256 public constant MAX_LIQUID_RESERVE_BPS = 5_000;

    // ---------------------------------------------------------------------
    // Storage: core bridge state
    // ---------------------------------------------------------------------

    /// @notice Prevents double-spend of withdrawal proofs
    mapping(uint256 => bool) public processedDeposits;

    /// @notice Monotonic deposit identifier
    uint256 public depositCounter;

    /// @notice Total user principal currently held by the bridge (ETH + aWETH principal)
    /// @dev Yield accrued in AAVE is *not* reflected here — see `accruedYield()`.
    uint256 public treasuryBalance;

    /// @notice ZK verifier for withdrawal proofs
    IAckiNackiVerifier public verifier;

    /// @notice Oracle providing canonical Ethereum block hashes
    IBlockHeaderOracle public blockHeaderOracle;

    // ---------------------------------------------------------------------
    // Storage: AAVE integration
    // ---------------------------------------------------------------------

    /// @notice AAVE V3 Pool (set once at construction, immutable thereafter)
    IAavePool public immutable aavePool;

    /// @notice AAVE V3 WrappedTokenGateway (handles ETH<->WETH wrapping)
    IWrappedTokenGatewayV3 public immutable wethGateway;

    /// @notice aWETH token minted by AAVE to the bridge when supplying ETH
    IERC20 public immutable aWETH;

    /// @notice Whether new supplies to AAVE are permitted (withdrawals always allowed)
    bool public aaveEnabled;

    /// @notice Principal currently supplied to AAVE (book value, excludes yield)
    uint256 public suppliedPrincipal;

    /// @notice Fraction of the treasury to keep liquid as ETH, in basis points.
    ///         e.g. 500 = 5% of `treasuryBalance` stays as plain ETH to serve small
    ///         withdrawals without a round-trip through AAVE.
    uint256 public liquidReserveBps;

    /// @notice Address authorised to manage AAVE routing & harvest yield
    address public owner;

    /// @notice Address that receives harvested yield (defaults to owner)
    address public yieldRecipient;

    // Reentrancy guard (avoid pulling in OZ for a single uint256)
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _reentrancyStatus;

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    event Deposit(
        uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp
    );
    event Withdrawal(
        uint256 indexed depositId, address indexed recipient, uint256 amount, uint256 timestamp
    );
    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event WithdrawnFromAave(uint256 amountRequested, uint256 amountReceived);
    event YieldHarvested(address indexed recipient, uint256 amount);
    event AaveEnabledSet(bool enabled);
    event LiquidReserveBpsSet(uint256 bps);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event YieldRecipientSet(address indexed recipient);
    event EmergencyWithdrawAll(uint256 amount);

    // ---------------------------------------------------------------------
    // Errors
    // ---------------------------------------------------------------------

    error InvalidAmount();
    error DepositTooLarge();
    error DepositAlreadyProcessed();
    error InvalidProof();
    error InsufficientTreasury();
    error InvalidVerifier();
    error InvalidRecipient();
    error InvalidBlockHash();
    error InvalidOracle();
    error InvalidAaveAddress();
    error NotOwner();
    error Reentrancy();
    error AaveDisabled();
    error ReserveBpsTooHigh();
    error NothingToSupply();
    error AaveWithdrawFailed(uint256 requested, uint256 received);
    error NoYield();

    // ---------------------------------------------------------------------
    // Modifiers
    // ---------------------------------------------------------------------

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier nonReentrant() {
        if (_reentrancyStatus == _ENTERED) revert Reentrancy();
        _reentrancyStatus = _ENTERED;
        _;
        _reentrancyStatus = _NOT_ENTERED;
    }

    // ---------------------------------------------------------------------
    // Constructor
    // ---------------------------------------------------------------------

    /// @param _verifier           ZK verifier for withdrawal proofs
    /// @param _blockHeaderOracle  Oracle for canonical Ethereum block hashes
    /// @param _aavePool           AAVE V3 Pool address (mainnet: 0x8787...fA4E2)
    /// @param _wethGateway        AAVE V3 WrappedTokenGatewayV3
    /// @param _aWETH              aWETH token minted by AAVE for supplied WETH
    /// @dev Pass address(0) for `_aavePool`/`_wethGateway`/`_aWETH` to disable AAVE.
    ///      In that case, the bridge behaves as before (plain ETH custody).
    constructor(
        address _verifier,
        address _blockHeaderOracle,
        address _aavePool,
        address _wethGateway,
        address _aWETH
    ) {
        if (_verifier == address(0)) revert InvalidVerifier();
        if (_blockHeaderOracle == address(0)) revert InvalidOracle();

        // All three AAVE addresses must be provided together — or none at all.
        bool aaveWired =
            _aavePool != address(0) || _wethGateway != address(0) || _aWETH != address(0);
        bool aaveAllSet =
            _aavePool != address(0) && _wethGateway != address(0) && _aWETH != address(0);
        if (aaveWired && !aaveAllSet) revert InvalidAaveAddress();

        verifier = IAckiNackiVerifier(_verifier);
        blockHeaderOracle = IBlockHeaderOracle(_blockHeaderOracle);

        aavePool = IAavePool(_aavePool);
        wethGateway = IWrappedTokenGatewayV3(_wethGateway);
        aWETH = IERC20(_aWETH);

        owner = msg.sender;
        yieldRecipient = msg.sender;
        aaveEnabled = aaveAllSet;
        liquidReserveBps = 1_000; // default 10% liquid reserve
        _reentrancyStatus = _NOT_ENTERED;

        // Pre-approve the gateway to pull aWETH (once, for max).
        // AAVE V3's aWETH is a standard ERC-20 approve; rebasing does not
        // affect the allowance amount.
        if (aaveAllSet) {
            aWETH.approve(_wethGateway, type(uint256).max);
        }

        emit OwnershipTransferred(address(0), msg.sender);
    }

    // ---------------------------------------------------------------------
    // User-facing: deposit
    // ---------------------------------------------------------------------

    /// @notice Deposit ETH to be bridged to Acki Nacki.
    /// @dev Emits a `Deposit` event that is later proven by a ZK circuit.
    ///      Funds stay as ETH in this contract; a keeper supplies them to AAVE
    ///      in batches via `supplyToAave()`.
    function deposit() external payable nonReentrant {
        if (msg.value == 0) revert InvalidAmount();
        if (msg.value > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();

        uint256 depositId = depositCounter++;
        treasuryBalance += msg.value;

        emit Deposit(depositId, msg.sender, msg.value, block.timestamp);
    }

    // ---------------------------------------------------------------------
    // User-facing: withdraw
    // ---------------------------------------------------------------------

    /// @notice Withdraw ETH using a ZK proof of a prior Deposit event.
    /// @dev If the bridge's ETH balance is short, the shortfall is pulled from AAVE.
    function withdraw(
        address payable recipient,
        uint256 amount,
        uint256 depositId,
        uint256 blockNumber,
        bytes calldata proof
    ) external nonReentrant {
        if (recipient == address(0)) revert InvalidRecipient();

        bytes32 blockHash = blockHeaderOracle.getBlockHash(blockNumber);
        if (blockHash == bytes32(0)) revert InvalidBlockHash();

        uint256[] memory publicInputs = new uint256[](6);
        publicInputs[0] = depositId;
        publicInputs[1] = uint256(uint160(address(recipient)));
        publicInputs[2] = amount;
        publicInputs[3] = uint256(uint160(address(this)));
        publicInputs[4] = uint256(bytes32(blockHash) >> 128);
        publicInputs[5] = uint256(uint128(uint256(blockHash)));

        (bool isValid, bytes32 verifiedDepositId) =
            verifier.verifyWithdrawalProof(proof, publicInputs);
        if (!isValid) revert InvalidProof();
        require(uint256(verifiedDepositId) == depositId, "DepositId mismatch");

        if (processedDeposits[depositId]) revert DepositAlreadyProcessed();
        if (treasuryBalance < amount) revert InsufficientTreasury();

        // Effects (CEI)
        processedDeposits[depositId] = true;
        treasuryBalance -= amount;

        // Interaction: ensure we have enough liquid ETH, pulling from AAVE if needed.
        uint256 ethBalance = address(this).balance;
        if (ethBalance < amount) {
            uint256 shortfall;
            unchecked {
                shortfall = amount - ethBalance;
            }
            _pullFromAave(shortfall);
        }

        // Transfer to recipient (2300 gas stipend is safe for EOAs & standard wallets).
        recipient.transfer(amount);

        emit Withdrawal(depositId, recipient, amount, block.timestamp);
    }

    // ---------------------------------------------------------------------
    // AAVE management (owner-only)
    // ---------------------------------------------------------------------

    /// @notice Supply idle ETH from the bridge to AAVE, respecting the liquid reserve.
    /// @param amount Exact amount to supply, or `type(uint256).max` to supply
    ///               everything above the liquid reserve.
    function supplyToAave(uint256 amount) external onlyOwner nonReentrant {
        if (!aaveEnabled) revert AaveDisabled();

        uint256 available = _amountSupplyable();
        if (available == 0) revert NothingToSupply();

        uint256 toSupply = amount == type(uint256).max ? available : amount;
        if (toSupply == 0 || toSupply > available) revert InvalidAmount();

        suppliedPrincipal += toSupply;
        wethGateway.depositETH{ value: toSupply }(address(aavePool), address(this), 0);

        emit SuppliedToAave(toSupply, suppliedPrincipal);
    }

    /// @notice Withdraw ETH from AAVE back into the bridge (preemptively top up liquidity).
    /// @param amount Amount to withdraw, or `type(uint256).max` for the entire principal.
    function withdrawFromAave(uint256 amount) external onlyOwner nonReentrant {
        uint256 principalCap = suppliedPrincipal;
        if (principalCap == 0) revert InvalidAmount();

        uint256 target = amount == type(uint256).max ? principalCap : amount;
        if (target == 0 || target > principalCap) revert InvalidAmount();

        _pullFromAave(target);
    }

    /// @notice Emergency: pull *all* aWETH back into the bridge as ETH and disable supplies.
    /// @dev Useful if AAVE pauses/depegs. User withdrawals remain available.
    function emergencyWithdrawAll() external onlyOwner nonReentrant {
        aaveEnabled = false;
        emit AaveEnabledSet(false);

        uint256 before = address(this).balance;
        // max-value signals "withdraw full balance" to the gateway.
        wethGateway.withdrawETH(address(aavePool), type(uint256).max, address(this));

        uint256 received = address(this).balance - before;
        // In an emergency the actual received amount drives the book update.
        uint256 principal = suppliedPrincipal;
        suppliedPrincipal = 0;

        // Surplus (yield) stays in the bridge and can be harvested later.
        // Shortfall (e.g. AAVE insolvency) would reduce headroom — the owner
        // must top up treasury manually if that happens.
        emit EmergencyWithdrawAll(received);
        emit WithdrawnFromAave(principal, received);
    }

    /// @notice Harvest accrued yield (aWETH balance above principal) to `yieldRecipient`.
    /// @param amount Amount of yield to harvest (must be <= accruedYield()).
    function harvestYield(uint256 amount) external onlyOwner nonReentrant {
        uint256 yield = accruedYield();
        if (yield == 0 || amount == 0 || amount > yield) revert NoYield();

        // Withdraw the yield amount from AAVE as ETH (do not touch suppliedPrincipal).
        uint256 before = address(this).balance;
        wethGateway.withdrawETH(address(aavePool), amount, address(this));
        uint256 received = address(this).balance - before;
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        emit WithdrawnFromAave(amount, received);

        // Forward ETH to yield recipient.
        (bool ok,) = payable(yieldRecipient).call{ value: received }("");
        require(ok, "yield transfer failed");
        emit YieldHarvested(yieldRecipient, received);
    }

    /// @notice Enable or disable further supplies to AAVE.
    function setAaveEnabled(bool enabled) external onlyOwner {
        if (enabled && address(aavePool) == address(0)) revert InvalidAaveAddress();
        aaveEnabled = enabled;
        emit AaveEnabledSet(enabled);
    }

    /// @notice Set the liquid reserve fraction (in basis points, capped at 50%).
    function setLiquidReserveBps(uint256 bps) external onlyOwner {
        if (bps > MAX_LIQUID_RESERVE_BPS) revert ReserveBpsTooHigh();
        liquidReserveBps = bps;
        emit LiquidReserveBpsSet(bps);
    }

    function setYieldRecipient(address recipient) external onlyOwner {
        if (recipient == address(0)) revert InvalidRecipient();
        yieldRecipient = recipient;
        emit YieldRecipientSet(recipient);
    }

    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert InvalidRecipient();
        emit OwnershipTransferred(owner, newOwner);
        owner = newOwner;
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    /// @dev ETH that may be supplied to AAVE without dipping below the liquid reserve.
    function _amountSupplyable() internal view returns (uint256) {
        uint256 reserve = (treasuryBalance * liquidReserveBps) / BPS_DENOMINATOR;
        uint256 bal = address(this).balance;
        if (bal <= reserve) return 0;
        return bal - reserve;
    }

    /// @dev Pull `amount` ETH from AAVE via the WETH gateway. Reverts if short.
    function _pullFromAave(uint256 amount) internal {
        if (suppliedPrincipal == 0) revert InsufficientTreasury();

        // Don't over-withdraw principal; any more is yield which needs `harvestYield`.
        uint256 cap = suppliedPrincipal;
        uint256 toPull = amount > cap ? cap : amount;

        uint256 before = address(this).balance;
        wethGateway.withdrawETH(address(aavePool), toPull, address(this));
        uint256 received = address(this).balance - before;
        if (received < toPull) revert AaveWithdrawFailed(toPull, received);

        // The bridge may need slightly more if yield accrued; the caller (e.g.
        // withdraw()) can only spend up to `received`, so require the full amount.
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        suppliedPrincipal -= toPull;
        emit WithdrawnFromAave(amount, received);
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    function isDepositProcessed(uint256 depositId) external view returns (bool) {
        return processedDeposits[depositId];
    }

    /// @notice Current aWETH balance held by the bridge (principal + accrued interest).
    function aWethBalance() public view returns (uint256) {
        if (address(aWETH) == address(0)) return 0;
        return aWETH.balanceOf(address(this));
    }

    /// @notice Unharvested yield = aWETH balance above principal book value.
    /// @dev Returns 0 if the balance is below principal (shouldn't happen in practice
    ///      — AAVE only grows the balance — but keeps the function panic-free).
    function accruedYield() public view returns (uint256) {
        uint256 bal = aWethBalance();
        uint256 principal = suppliedPrincipal;
        return bal > principal ? bal - principal : 0;
    }

    /// @notice Total assets under management: ETH + aWETH (including yield).
    function totalAssets() external view returns (uint256) {
        return address(this).balance + aWethBalance();
    }

    // ---------------------------------------------------------------------
    // ETH receive hook
    // ---------------------------------------------------------------------

    /// @dev AAVE's WrappedTokenGateway unwraps WETH and sends ETH to the bridge
    ///      during `withdrawETH`. We must accept it. Direct transfers from any
    ///      other source are ignored for accounting purposes (they boost
    ///      `address(this).balance` but do not touch `treasuryBalance`).
    receive() external payable { }
}
