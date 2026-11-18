pragma gosh-solidity >=0.80;
pragma AbiHeader expire;

import "./modifiers/modifiers.sol";

// The one call the voucher makes. eccBridge itself is not imported: it links
// EthTrie, whose KECCAK256 the voucher's compiler cannot assemble.
interface IDepositConfirmation {
    function confirmTokenDeposit(
        uint256 depositId, uint256 contractAddr, uint256 dappId,
        uint256 chainId, uint128 amount, uint256 anAccount, uint256 evmToken
    ) external;
}

/// @title DepositVoucher
/// @notice Forwards an inbound deposit confirmation from a deterministic
///         address. The bridge checks this address against the pending record
///         and marks the deposit minted when it pays the proven recipient.
contract DepositVoucher is eccBridgeModifiers {
    string constant version = "1.5.0";

    /// @notice Hash of the proof-bound deposit identity (deposit_id,
    ///         contract_addr, dapp_id, chain_id). Forms the deterministic
    ///         address of this voucher. The bridge stores the proven amount
    ///         and recipient in the pending record under the same hash.
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
        IDepositConfirmation(USDC_BRIDGE_ADDRESS).confirmTokenDeposit{value: 0.5 vmshell, flag: 1}(
            depositId, contractAddr, dappId, chainId, amount, anAccount, evmToken
        );
    }

    function getVersion() external pure returns (string, string) {
        return (version, "DepositVoucher");
    }
}
