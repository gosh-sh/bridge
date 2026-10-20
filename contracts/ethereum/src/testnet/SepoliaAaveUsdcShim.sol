// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../IERC20.sol";
import "../cctp/IDepositBridge.sol";
import "../cctp/ReentrancyGuard.sol";
import "../cctp/TwoStepOwnable.sol";
import "./IAaveTestnetFaucet.sol";

/// @title SepoliaAaveUsdcShim
/// @notice Testnet only. On Sepolia, CCTP mints Circle USDC, while the Aave V3
///         market lists only the faucet's own USDC. A bridge with AAVE enabled
///         must custody the faucet token, so a hub receiver cannot deposit
///         into it directly.
///
///         The shim takes the bridge's place for the receiver: `usdc()` is
///         Circle USDC, and `deposit` pulls that amount of Circle USDC and
///         deposits the same amount of faucet USDC into `bridge`. The faucet
///         USDC comes from the shim's balance first and is minted for the
///         rest. The Circle USDC stays here and backs `redeem`, which swaps
///         faucet USDC back 1:1, so a withdrawal can leave Sepolia through
///         CCTP.
///
///         The `Deposit` the bridge emits names the shim as `sender`. The
///         `forward` receipt holds the Circle `Transfer`, the faucet mint, the
///         faucet `Transfer` into the bridge (and its `Approval`, if the token
///         emits one), the `Deposit` and `Shimmed`.
contract SepoliaAaveUsdcShim is IDepositBridge, TwoStepOwnable, ReentrancyGuard {
    IERC20 public immutable circleUsdc;
    IERC20 public immutable aaveUsdc;
    IAaveTestnetFaucet public immutable faucet;
    IDepositBridge public immutable bridge;

    event Shimmed(address indexed caller, uint256 amount, uint256 minted);
    event Redeemed(address indexed caller, address indexed to, uint256 amount);
    event Rescued(address indexed token, address indexed to, uint256 amount);

    error ZeroAmount();
    error TransferFailed();
    error TransferAmountMismatch();
    error InsufficientCircleReserve(uint256 available);
    error WrongChain(uint256 chainId);

    uint256 internal constant SEPOLIA = 11_155_111;

    constructor(address bridge_, address circleUsdc_, address faucet_) {
        if (block.chainid != SEPOLIA) revert WrongChain(block.chainid);
        if (bridge_ == address(0) || circleUsdc_ == address(0) || faucet_ == address(0)) {
            revert ZeroAddress();
        }
        bridge = IDepositBridge(bridge_);
        address aaveUsdc_ = bridge.usdc();
        if (aaveUsdc_ == address(0)) revert ZeroAddress();
        aaveUsdc = IERC20(aaveUsdc_);
        circleUsdc = IERC20(circleUsdc_);
        faucet = IAaveTestnetFaucet(faucet_);
        if (!aaveUsdc.approve(bridge_, type(uint256).max)) revert TransferFailed();
    }

    /// @notice The token a hub receiver hands over: Circle USDC.
    function usdc() external view returns (address) {
        return address(circleUsdc);
    }

    /// @notice Pull `amount` of Circle USDC from the caller and deposit the
    ///         same amount of faucet USDC into `bridge` for `anAccount`.
    function deposit(uint256 amount, bytes32 anAccount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        _pullExact(circleUsdc, amount);

        uint256 held = aaveUsdc.balanceOf(address(this));
        uint256 minted;
        if (held < amount) {
            minted = amount - held;
            faucet.mint(address(aaveUsdc), address(this), minted);
            if (aaveUsdc.balanceOf(address(this)) < amount) revert TransferAmountMismatch();
        }

        bridge.deposit(amount, anAccount);
        emit Shimmed(msg.sender, amount, minted);
    }

    /// @notice Swap `amount` of faucet USDC for Circle USDC, paid to `to`.
    ///         Owner only: the faucet mints to anyone, so an open `redeem`
    ///         would drain the Circle USDC that backs the return CCTP leg.
    function redeem(uint256 amount, address to) external onlyOwner nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (to == address(0)) revert ZeroAddress();
        uint256 available = circleUsdc.balanceOf(address(this));
        if (amount > available) revert InsufficientCircleReserve(available);
        _pullExact(aaveUsdc, amount);
        if (!circleUsdc.transfer(to, amount)) revert TransferFailed();
        emit Redeemed(msg.sender, to, amount);
    }

    function rescue(address token, address to, uint256 amount) external onlyOwner nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        if (!IERC20(token).transfer(to, amount)) revert TransferFailed();
        emit Rescued(token, to, amount);
    }

    function _pullExact(IERC20 token, uint256 amount) internal {
        uint256 beforeBal = token.balanceOf(address(this));
        if (!token.transferFrom(msg.sender, address(this), amount)) revert TransferFailed();
        uint256 afterBal = token.balanceOf(address(this));
        if (afterBal < beforeBal || afterBal - beforeBal != amount) {
            revert TransferAmountMismatch();
        }
    }
}
