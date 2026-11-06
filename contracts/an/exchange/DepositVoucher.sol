pragma gosh-solidity >=0.80;
pragma AbiHeader expire;

import "./modifiers/modifiers.sol";
import "./eccBridge.sol";

/// @title DepositVoucher
/// @notice Deterministic one-shot marker contract for inbound bridge deposits
///         from any source chain. Its address is derived from `_depositHash`,
///         so re-deploying with the same hash collides with the existing
///         account and the second constructor call is a no-op — natural
///         replay protection.
contract DepositVoucher is eccBridgeModifiers {
    string constant version = "1.5.0";

    /// @notice Hash of the proof-bound deposit identity (deposit_id,
    ///         contract_addr, dapp_id, chain_id). Forms the deterministic
    ///         address of this voucher. amount/recipient are deliberately NOT
    ///         hashed: they are fixed by the proof, so keying the replay slot on
    ///         the identity alone makes one L1 deposit = one voucher = one mint,
    ///         and a replay can never re-route the payout (same identity ⇒ same
    ///         voucher).
    uint256 static _depositHash;

    /// @notice Deploy callback: forwards confirmation to eccBridge so it mints
    ///         the registered ECC for the proof-bound recipient. Only callable by eccBridge.
    /// @dev    The identity params (depositId, contractAddr, dappId, chainId)
    ///         are bound to `_depositHash`; amount + recipient (anAccount) ride
    ///         through with the proven token to `confirmTokenDeposit` for the payout.
    constructor(
        uint256 depositId,
        uint256 contractAddr,
        uint256 dappId,
        uint256 chainId,
        uint128 amount,
        uint256 anAccount,
        uint256 evmToken
    ) {
        tvm.accept();
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
        require(
            tvm.hash(abi.encode(depositId, contractAddr, dappId, chainId)) == _depositHash,
            ERR_HASH_MISMATCH
        );
        eccBridge(USDC_BRIDGE_ADDRESS).confirmTokenDeposit{value: 0.5 vmshell, flag: 1}(
            depositId, contractAddr, dappId, chainId, amount, anAccount, evmToken
        );
    }

    function getVersion() external pure returns (string, string) {
        return (version, "DepositVoucher");
    }
}
