// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IAavePool.sol";
import "./IERC20.sol";

/// @title DemoAToken
/// @notice Receipt token of `DemoAavePool`. Only the pool mints and burns it, and it cannot
///         be transferred: the bridge reads nothing but `balanceOf`.
contract DemoAToken {
    string public constant name = "Demo aUSDC";
    string public constant symbol = "daUSDC";
    uint8 public constant decimals = 6;

    address public immutable pool;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;

    error OnlyPool();

    constructor() {
        pool = msg.sender;
    }

    function mint(address to, uint256 amount) external {
        if (msg.sender != pool) revert OnlyPool();
        balanceOf[to] += amount;
        totalSupply += amount;
    }

    function burn(address from, uint256 amount) external {
        if (msg.sender != pool) revert OnlyPool();
        balanceOf[from] -= amount;
        totalSupply -= amount;
    }
}

/// @title DemoAavePool
/// @notice Testnet stand-in for the AAVE V3 Pool, for a bridge whose USDC has no live AAVE
///         market. Holds every supplied token 1:1 against `DemoAToken`; there is no borrowing,
///         no supply cap and no interest rate. Yield exists only when the owner adds it with
///         `accrueYield`, which brings the USDC in with the receipt tokens, so every aToken is
///         always redeemable. Not for mainnet: the yield is whatever the owner pays in.
contract DemoAavePool is IAavePool {
    IERC20 public immutable underlying;
    DemoAToken public immutable aToken;
    address public immutable owner;

    event Supplied(address indexed onBehalfOf, uint256 amount);
    event Withdrawn(address indexed from, address indexed to, uint256 amount);
    event YieldAccrued(address indexed holder, uint256 amount);

    error UnsupportedAsset(address asset);
    error NotOwner();
    error InvalidAmount();
    error TransferFailed();

    constructor(address _underlying, address _owner) {
        if (_underlying == address(0) || _owner == address(0)) revert InvalidAmount();
        underlying = IERC20(_underlying);
        owner = _owner;
        aToken = new DemoAToken();
    }

    function supply(address asset, uint256 amount, address onBehalfOf, uint16) external override {
        if (asset != address(underlying)) revert UnsupportedAsset(asset);
        if (amount == 0) revert InvalidAmount();
        if (!underlying.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
        aToken.mint(onBehalfOf, amount);
        emit Supplied(onBehalfOf, amount);
    }

    function withdraw(address asset, uint256 amount, address to)
        external
        override
        returns (uint256)
    {
        if (asset != address(underlying)) revert UnsupportedAsset(asset);
        uint256 balance = aToken.balanceOf(msg.sender);
        uint256 toBurn = amount == type(uint256).max ? balance : amount;
        if (toBurn == 0 || toBurn > balance) revert InvalidAmount();
        aToken.burn(msg.sender, toBurn);
        if (!underlying.transfer(to, toBurn)) revert TransferFailed();
        emit Withdrawn(msg.sender, to, toBurn);
        return toBurn;
    }

    /// @notice Pay `amount` of the underlying in as yield to `holder` (the bridge).
    /// @dev The owner must have approved this pool for `amount`.
    function accrueYield(address holder, uint256 amount) external {
        if (msg.sender != owner) revert NotOwner();
        if (amount == 0) revert InvalidAmount();
        if (!underlying.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
        aToken.mint(holder, amount);
        emit YieldAccrued(holder, amount);
    }

    /// @notice Only `aTokenAddress` is filled, for the listed asset; `configuration` is zero,
    ///         which reads as "no supply cap" to `DeployRealBridge._checkAaveReserve`.
    function getReserveData(address asset)
        external
        view
        override
        returns (ReserveData memory data)
    {
        if (asset == address(underlying)) {
            data.aTokenAddress = address(aToken);
        }
    }
}
