pragma gosh-solidity >=0.80;
pragma AbiHeader expire;

import "./modifiers/modifiers.sol";
import "./eccUSDCBridge.sol";

/// @title DepositVoucher
/// @notice Deterministic one-shot marker contract for inbound bridge deposits
///         from any source chain. Its address is derived from `_depositHash`,
///         so re-deploying with the same hash collides with the existing
///         account and the second constructor call is a no-op — natural
///         replay protection.
contract DepositVoucher is eccUSDCBridgeModifiers {
    string constant version = "1.4.0";

    /// @notice Hash of the deposit identity (deposit_id, contract_addr,
    ///         dapp_id, chain_id) from the proof's public inputs. Forms the
    ///         deterministic address of this voucher, so one identity = one
    ///         voucher = one mint. amount/recipient are NOT hashed: the first
    ///         proof that verifies for an identity deploys the voucher and
    ///         decides both, and any later proof for it is a no-op.
    uint256 static _depositHash;

    /// @notice Deploy callback: forwards confirmation to eccUSDCBridge so it mints
    ///         USDC for the AN recipient from the proof's public inputs. Only callable by eccUSDCBridge.
    /// @dev    The identity params (depositId, contractAddr, dappId, chainId)
    ///         are bound to `_depositHash`; amount + recipient (anAccount) ride
    ///         through to `confirmDeposit` for the payout.
    constructor(
        uint256 depositId,
        uint256 contractAddr,
        uint256 dappId,
        uint256 chainId,
        uint128 amount,
        uint256 anAccount
    ) {
        tvm.accept();
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
        require(
            tvm.hash(abi.encode(depositId, contractAddr, dappId, chainId)) == _depositHash,
            ERR_HASH_MISMATCH
        );
        eccUSDCBridge(USDC_BRIDGE_ADDRESS).confirmDeposit{value: 0.5 vmshell, flag: 1}(
            depositId, contractAddr, dappId, chainId, amount, anAccount
        );
    }

    function getVersion() external pure returns (string, string) {
        return (version, "DepositVoucher");
    }
}
