pragma gosh-solidity >=0.80;

import "./errors.sol";

abstract contract eccUSDCBridgeModifiers is eccUSDCBridgeErrors {
    uint64 constant MIN_BALANCE = 100 vmshell;

    uint32 constant USDC_ECC_ID = 3;
    uint128 constant USDC_DECIMALS_FACTOR = 1_000_000;

    address constant ACCUMULATOR_ADDRESS = address.makeAddrStd(0, 0x3535353535353535353535353535353535353535353535353535353535353535);
    address constant USDC_BRIDGE_ADDRESS = address.makeAddrStd(0, 0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a);

    // External address constants for directed events
    uint constant bitCntAddress = 256;
    uint128 constant UsdcMigratedEmit        = 615;
    uint128 constant UsdcMintedEmit          = 616;
    uint128 constant WithdrawalInitiatedEmit = 618;
    uint128 constant DepositFinalizedEmit    = 619;

    // int_msg_info$0 ihr_disabled:Bool bounce:Bool bounced:Bool src:MsgAddress
    // dest:MsgAddress value:CurrencyCollection ihr_fee:Grams fwd_fee:Grams
    // created_lt:uint64 created_at:uint32 src_dapp_id:(Maybe uint256) ...
    // The node stamps src_dapp_id from the sending account's own state, so it
    // is not caller-settable. Absent only where no account produced the
    // message; that is read as "this contract's own dapp".
    function _srcDappId() internal pure returns (uint256) {
        TvmSlice s = msg.data.toSlice();
        s.skip(4);
        s.load(address);
        s.load(address);
        s.load(varuint16);
        s.load(mapping(uint32 => varuint32));
        s.load(varuint16);
        s.load(varuint16);
        s.skip(64 + 32);
        if (s.loadUint(1) == 0) { return 0; }
        return s.loadUint(256);
    }

    /// @dev `dappId` 0 means this contract's own dapp. `address(this).dapp_id`
    ///      throws when the account has no dapp, so it is read only when the
    ///      answer actually depends on it.
    function _senderIsInDapp(uint256 dappId) internal pure returns (bool) {
        uint256 src = _srcDappId();
        if (src == 0) { return dappId == 0; }
        if (dappId == 0) { return src == address(this).dapp_id; }
        return src == dappId;
    }

    modifier accept() {
        tvm.accept();
        _;
    }

    modifier onlyOwnerPubkey(uint256 rootpubkey) {
        require(msg.pubkey() == rootpubkey, ERR_NOT_OWNER);
        _;
    }
}
