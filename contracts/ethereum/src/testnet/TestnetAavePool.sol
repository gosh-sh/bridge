// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../IAavePool.sol";
import "../IERC20.sol";
import "../cctp/ReentrancyGuard.sol";
import "../cctp/TwoStepOwnable.sol";
import "./IAaveTestnetFaucet.sol";

/// @title TestnetAToken
/// @notice The receipt token of `TestnetAavePool`. Only the pool mints and burns.
contract TestnetAToken is IERC20 {
    string public constant name = "Testnet aUSDC";
    string public constant symbol = "taUSDC";
    uint8 public constant decimals = 6;

    address public immutable pool;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    error NotPool();
    error InsufficientBalance();
    error InsufficientAllowance();

    constructor() {
        pool = msg.sender;
    }

    function mint(address to, uint256 amount) external {
        if (msg.sender != pool) revert NotPool();
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }

    function burn(address from, uint256 amount) external {
        if (msg.sender != pool) revert NotPool();
        if (balanceOf[from] < amount) revert InsufficientBalance();
        balanceOf[from] -= amount;
        totalSupply -= amount;
        emit Transfer(from, address(0), amount);
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) {
            if (allowed < amount) revert InsufficientAllowance();
            allowance[from][msg.sender] = allowed - amount;
        }
        _move(from, to, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        if (balanceOf[from] < amount) revert InsufficientBalance();
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}

/// @title TestnetAavePool
/// @notice Testnet only. The slice of the Aave V3 Pool `AckiNackiBridge` calls,
///         for a testnet where the real market cannot take a supply (on
///         Sepolia the USDC reserve is over its supply cap, and `supply`
///         reverts with Aave error 51). `supply` and `withdraw` move the
///         underlying 1:1 against `aToken`. `accrue` stands in for interest:
///         it mints aTokens to a holder and the faucet mints the underlying
///         that backs them, so `harvestYield` and the payout top-up have
///         something to pull.
contract TestnetAavePool is IAavePool, TwoStepOwnable, ReentrancyGuard {
    IERC20 public immutable underlying;
    TestnetAToken public immutable aToken;
    IAaveTestnetFaucet public immutable faucet;

    event Accrued(address indexed holder, uint256 amount);

    error WrongAsset(address asset);
    error TransferFailed();
    error InsufficientATokens(uint256 balance);

    constructor(address underlying_, address faucet_) {
        if (underlying_ == address(0) || faucet_ == address(0)) revert ZeroAddress();
        underlying = IERC20(underlying_);
        faucet = IAaveTestnetFaucet(faucet_);
        aToken = new TestnetAToken();
    }

    function supply(address asset, uint256 amount, address onBehalfOf, uint16)
        external
        nonReentrant
    {
        _checkAsset(asset);
        if (!underlying.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
        aToken.mint(onBehalfOf, amount);
    }

    function withdraw(address asset, uint256 amount, address to)
        external
        nonReentrant
        returns (uint256)
    {
        _checkAsset(asset);
        uint256 balance = aToken.balanceOf(msg.sender);
        uint256 burned = amount == type(uint256).max ? balance : amount;
        if (burned > balance) revert InsufficientATokens(balance);
        aToken.burn(msg.sender, burned);
        if (!underlying.transfer(to, burned)) revert TransferFailed();
        return burned;
    }

    /// @notice Simulate `amount` of interest for `holder`.
    function accrue(address holder, uint256 amount) external onlyOwner nonReentrant {
        faucet.mint(address(underlying), address(this), amount);
        aToken.mint(holder, amount);
        emit Accrued(holder, amount);
    }

    function getReserveData(address asset) external view returns (ReserveData memory data) {
        _checkAsset(asset);
        data.aTokenAddress = address(aToken);
    }

    function _checkAsset(address asset) internal view {
        if (asset != address(underlying)) revert WrongAsset(asset);
    }
}
