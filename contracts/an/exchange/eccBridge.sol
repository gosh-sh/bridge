pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./modifiers/modifiers.sol";
import "./modifiers/replayprotection.sol";
import "./DepositVoucher.sol";
import "./EthBeaconLightClient.sol";
import "./ArbitrumBlockVerifierDeploy.sol";
import "./EthTrie.sol";
import "../token/interface/ISubscriber.sol";

interface IShellAccumulator {
    function buyShellFor(address buyer) external;
}

/// @title eccBridge
/// @notice The name covers three distinct flows that share storage / owner key
///         but are otherwise independent:
///
///         1. TIP-3 USDC → ECC[3] stripe-mint gateway
///            `onTransferReceived` (ISubscriber callback) — fires when the
///            bridge's TIP-3 USDC TokenWallet receives a transfer. Mints
///            equivalent ECC[3] USDC and forwards to the original depositor.
///            One-way: TIP-3 → ECC. Counter: `_totalMinted`.
///
///         2. Owner-mint admin path
///            `mintAndSend` / `mintAndSendAccumulator` / `mintAndSendBridged`
///            — owner-key (pubkey) mints ECC[3] USDC and dispatches to
///            recipient / Accumulator buyShellFor. Nonce-protected
///            (`_mintNonce`, `_mintAccumulatorNonce`) against off-chain
///            replay of signed mint requests. `mintAndSend` and
///            `mintAndSendAccumulator` increment `_totalMinted` only.
///            `mintAndSendBridged` matches `confirmDeposit`: it increments
///            `_totalMintedBridgeByToken[USDC_ECC_ID]` and is the recovery
///            mint after a voucher-consumed `confirmDeposit` abort (CLI
///            exit 37). Do not use it for ordinary owner mint.
///
///         3. Cross-chain bridge — registered ERC-20 / ECC currency pairs
///            `initiateWithdrawal` (burn ECC + emit `WithdrawalInitiated`) and
///            `finalizeDeposit` / `confirmDeposit` (prove the deposit receipt
///            under an accepted block, deploy deterministic `DepositVoucher`
///            for anti-replay, mint ECC).
///            The destination chain of a withdrawal is opaque; the SOURCE of a
///            deposit is the chain whose accepted block holds the receipt and
///            the contract that emitted the log, gated by the owner-managed
///            `_trustedL1Bridge` allowlist.
///            Counters: per-tokenId `_totalMintedBridgeByToken` /
///            `_totalBurnedBridgeByToken`. `initiateWithdrawal` refuses a
///            burn that would take burned above minted for that token
///            (`ERR_BURN_EXCEEDS_BRIDGED`). Owner-mint (`mintAndSend`,
///            `mintAndSendAccumulator`) and TIP-3 mint do not increment
///            the bridged counters; `mintAndSendBridged` does.
///
///         Deployed at fixed address in zerostate.
contract eccBridge is eccBridgeModifiers, BridgeReplayProtection, ISubscriber {
    string constant version = "1.17.0";

    uint128 constant MAX_TRANSFER_AMOUNT = 50_000_000;
    uint128 constant MAX_BRIDGE_TVL = 10_000_000_000;

    event UsdcMigrated(address from, uint128 value);
    event UsdcMinted(address recipient, uint128 value);
    event WithdrawalInitiated(
        uint256 dstChainId,
        bytes recipient,
        uint128 amount,
        uint32 tokenId,
        uint160 dstBridge,
        address sender
    );
    event WithdrawalRefused(
        uint256 dstChainId,
        bytes recipient,
        uint128 amount,
        uint32 tokenId,
        address sender,
        uint16 reason
    );
    event DepositFinalized(
        uint256 depositId,
        uint256 contractAddr,
        uint256 dappId,
        uint256 chainId,
        uint128 amount,
        uint256 anAccount
    );

    /// @notice Deposit fields read out of the proven `Deposit` log.
    struct DepositPI {
        uint256 depositId;     // topic 1 — anti-replay anchor (per source chain/contract/dapp)
        uint128 amount;        // data word 0
        uint256 contractAddr;  // the log's emitter
        uint256 chainId;       // the chain whose accepted block holds the receipt
        uint256 dappId;        // pinned to 0: deposits land in dapp 0
        uint256 anAccount;     // data word 1 — AN recipient
        uint256 evmToken;      // data word 3 — deposited ERC-20 address
    }

    // `AckiNackiBridge.Deposit(uint256 indexed depositId, address indexed sender,
    // uint256 amount, bytes32 anAccount, uint256 timestamp, address token)`.
    // Its log has one RLP encoding and nothing else is accepted:
    //   f8fc                list of 252 bytes
    //   94 <20 bytes>       emitter
    //   f863 a0 <topic0> a0 <depositId> a0 <sender>
    //   b880 <4 words>      amount, anAccount, timestamp, token
    uint256 constant DEPOSIT_TOPIC0 = 0x935bca73ba823197b8e5564da08c2c294de3811fc4a4cb482e6af4fd08623c0e;
    uint16 constant DEPOSIT_LOG_HEADER = 0xf8fc;
    uint8 constant DEPOSIT_EMITTER_HEADER = 0x94;
    uint16 constant DEPOSIT_TOPICS_HEADER = 0xf863;
    uint8 constant DEPOSIT_TOPIC_HEADER = 0xa0;
    uint16 constant DEPOSIT_DATA_HEADER = 0xb880;

    uint256 _ownerPubkey;

    // TokenWallet address for TIP-3 USDC bridge (one-way: TIP-3 -> ECC[3])
    address _usdcWallet;

    // Total ECC[3] USDC minted by the stripe (TIP-3) bridge
    uint128 _totalMinted;

    // Nonces for double-spend protection
    uint64 _mintNonce;
    uint64 _mintAccumulatorNonce;

    // Cross-chain bridge accounting per tokenId (independent of stripe
    // `_totalMinted`). `initiateWithdrawal` keeps burned <= minted for that
    // token. ECC[3] is fungible, so this is an inventory cap, not a
    // provenance fence: owner/TIP-3 ECC can still consume deposit headroom.
    mapping(uint32 => uint128) _totalMintedBridgeByToken;
    mapping(uint32 => uint128) _totalBurnedBridgeByToken;
    mapping(uint256 => uint128) _outstandingByChain;
    mapping(uint256 => mapping(uint32 => BridgeTokenConfig)) _bridgeTokens;
    mapping(uint256 => mapping(uint256 => uint32)) _bridgeTokenIds;

    // Code of DepositVoucher contract — deployed per inbound deposit for replay protection
    TvmCell _depositVoucherCode;

    // Source-chain allowlist: L1 chainId -> SET of bridge contracts on that
    // chain whose deposit events this bridge accepts. A deposit passes if its
    // chainId and the emitter of its proven log hit a present entry — so an L1
    // bridge rotation can keep the old and the new address trusted at once for
    // a migration window, then drop the old one. An absent entry (false) means
    // that address is not accepted. Owner-managed via `setTrustedL1Bridge`;
    // deliberately NOT carried through `onCodeUpgrade` — after a code upgrade
    // the bridge accepts no deposits until the owner re-seeds it (fail-closed).
    mapping(uint256 => mapping(uint256 => bool)) _trustedL1Bridge;

    // Destination-chain EVM bridge: dstChainId -> the one `AckiNackiBridge`
    // that may pay this bridge's withdrawals to that chain. `initiateWithdrawal`
    // writes it into `WithdrawalInitiated`, Circuit 4 makes it a public input,
    // and the EVM bridge refuses a proof that names another address, so a
    // proof pays out on one bridge only. Zero = unset: the withdrawal is
    // refunded (`ERR_NO_EVM_BRIDGE`). Owner-managed via `setEvmBridge`; like
    // `_trustedL1Bridge` it is NOT carried through `onCodeUpgrade`.
    mapping(uint256 => uint160) _evmBridgeByChain;

    // Canonicality anchor: source chain id -> block hash -> admitted. A
    // deposit's receipt is proven against the receipts root of a header that
    // hashes to one of these, and nothing else says that header is a block of
    // that chain's canonical history: a privately mined block carrying a
    // fabricated deposit event would prove just as well. This mapping is that
    // assertion, made from outside the deposit.
    //
    // Fail-closed: an unset entry rejects the deposit. Like `_trustedL1Bridge`
    // this is NOT threaded through the `onCodeUpgrade` migration cell, so after
    // a code upgrade the anchors start empty and have to be re-established.
    mapping(uint256 => mapping(uint256 => bool)) _acceptedBlockHash;

    // Code of the beacon light client (`EthBeaconLightClient`), the only
    // non-owner writer of `_acceptedBlockHash`. The bridge is the sole account
    // that may deploy it, and its address follows from this code, so there is
    // no address to set and none to spoof. Empty = no light client yet; like
    // `_trustedL1Bridge` it is not carried through `onCodeUpgrade`.
    TvmCell _lightClientCode;

    // Light client per source-chain id, derived from `_lightClientCode`
    // salted with that id, never set directly. One code, one client per
    // chain, each at its own address.
    mapping(uint256 => address) _lightClients;

    // Chain id of the light client deployed last, written by every
    // `deployLightClient`. 0 until the first one. Only the no-argument
    // `disableOwnerAnchors` and `getAnchorConfig` read it; every other path
    // takes the chain id from its caller. Not carried through
    // `onCodeUpgrade`.
    uint256 _lightClientL1ChainId;

    // Owner writer of `_acceptedBlockHash`, keyed by source-chain id.
    // Missing = owner may still write. The bit is set per chain, for any
    // chain that has a deployed light client. Like `_trustedL1Bridge` it is
    // NOT carried through `onCodeUpgrade`.
    mapping(uint256 => bool) _ownerAnchorsDisabled;

    // Code every Arbitrum block verifier (`ArbitrumBlockVerifier`) is deployed
    // from, the second non-owner writer of `_acceptedBlockHash`. Like the
    // light client, a verifier's address follows from this code salted with
    // its chain id, so there is no address to set. Not carried through
    // `onCodeUpgrade`.
    TvmCell _l2VerifierCode;

    // Verifier per Arbitrum chain id, and the chain whose accepted blocks it
    // reads the assertions from. Set only by `deployL2Verifier`. Not carried
    // through `onCodeUpgrade`.
    mapping(uint256 => address) _l2Verifiers;
    mapping(uint256 => uint256) _l2VerifierParent;

    // Owner-operated stop of the cross-chain lane: `finalizeDeposit` and
    // `initiateWithdrawal` refuse while it is set. Like `_trustedL1Bridge` and
    // `_acceptedBlockHash` it is NOT carried through `onCodeUpgrade`, so an upgraded bridge starts
    // unpaused — with an empty allowlist and empty anchors, which hold deposits
    // anyway until the owner re-seeds them.
    bool _paused;

    /// @notice Contract constructor.
    /// @dev `_depositVoucherCode` is intentionally NOT a constructor arg:
    ///       in the only deploy path that matters (zerostate premine stub +
    ///       `updateCode` upgrade) the voucher code arrives via the
    ///       `onCodeUpgrade` payload. There is no standalone setter (B2 fix),
    ///       so the only way to populate / rotate `_depositVoucherCode` is a
    ///       full `updateCode` upgrade of eccBridge.
    /// @param pubkey — owner public key for admin operations
    /// @param usdcWallet — address of the Exchange's TIP-3 USDC TokenWallet (subscriber target)
    constructor(
        uint256 pubkey,
        address usdcWallet
    ) accept {
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _usdcWallet = usdcWallet;
    }

    /// @notice Ensures contract balance stays above MIN_BALANCE by minting vmshell if needed.
    function ensureBalance() private pure {
        if (address(this).balance >= MIN_BALANCE) { return; }
        gosh.mintshellq(MIN_BALANCE);
    }

    // ========================================================
    // TIP-3 USDC -> ECC[3] bridge (ISubscriber callback)
    // ========================================================

    /// @notice ISubscriber callback invoked by the bridge's TIP-3 USDC TokenWallet
    ///         when it receives a TIP-3 transfer. Mints equivalent ECC[3] USDC and sends
    ///         it to the original depositor. Only callable by _usdcWallet.
    /// @param from — address of the original depositor (wallet owner who sent TIP-3 USDC)
    /// @param value — amount of TIP-3 USDC received (in micro-USDC, 6 decimals)
    function onTransferReceived(
        address from,
        address /*to*/,
        uint128 value,
        uint128 /*balance*/
    ) external override {
        require(msg.sender == _usdcWallet, ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();

        // TIP-3 USDC deposited -> mint ECC[3] and send to depositor
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        from.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMigratedEmit, bitCntAddress);
        emit UsdcMigrated{dest: addrExtern}(from, value);
    }

    // ========================================================
    // Mint ECC[3] USDC and send to recipient (owner only)
    // ========================================================

    /// @notice Mints ECC[3] USDC and sends it to the specified recipient address.
    ///         Only callable by the owner (by public key). Increments
    ///         `_totalMinted` only — not bridged inventory. After CLI exit 37
    ///         (voucher spent, `confirmDeposit` aborted) use
    ///         `mintAndSendBridged` instead, or the USDC already in the
    ///         Ethereum treasury never becomes withdrawable headroom.
    /// @param recipient — address to receive the minted ECC[3] USDC
    /// @param value — amount of ECC[3] USDC to mint and send (in micro-USDC)
    function mintAndSend(address recipient, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        _mintNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        recipient.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(recipient, value);
    }

    // ========================================================
    // Owner recovery mint that credits bridged inventory
    // ========================================================

    /// @notice Owner recovery mint matching `confirmDeposit` accounting:
    ///         `gosh.mintecc` plus `_totalMintedBridgeByToken[USDC_ECC_ID]`.
    ///         Call this after CLI exit 37: the deposit voucher is spent so
    ///         `finalizeDeposit` cannot mint again, Ethereum already holds
    ///         the USDC, and ordinary `mintAndSend` would leave
    ///         `getTotalBridged` short — last withdrawers then hit 233.
    ///         Shares `_mintNonce` with `mintAndSend`. The call is not bound
    ///         to a deposit id: paying twice, or typing the wrong `value`,
    ///         still credits bridged inventory the Ethereum treasury does
    ///         not hold, and last withdrawers then lose the difference.
    ///         Match the aborted deposit's recipient and amount, once.
    ///         Do not use it for ordinary owner mint.
    /// @param recipient — address to receive the minted ECC[3] USDC
    /// @param value — amount of ECC[3] USDC to mint and send (in micro-USDC)
    /// @param nonce — next `_mintNonce` (shared with `mintAndSend`)
    /// @param chainId — EIP-155 source chain whose aborted deposit is being paid
    function mintAndSendBridged(address recipient, uint128 value, uint64 nonce, uint256 chainId) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        require(value <= MAX_TRANSFER_AMOUNT, ERR_TRANSFER_TOO_LARGE);
        require(_outstandingByChain[chainId] + value <= MAX_BRIDGE_TVL, ERR_TVL_CAP);
        _mintNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMintedBridgeByToken[USDC_ECC_ID] += value;
        _outstandingByChain[chainId] += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        recipient.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(recipient, value);
    }

    /// @notice Owner recovery for an aborted registered-token deposit.
    ///         Match the source receipt, currency, recipient and amount exactly once.
    function mintAndSendTokenBridged(address recipient, uint128 value, uint64 nonce,
                                     uint256 chainId, uint32 tokenId)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(nonce == _mintNonce + 1, ERR_INVALID_NONCE);
        require(value > 0 && value <= uint128(type(uint64).max), ERR_OVERFLOW);
        BridgeTokenConfig config = _bridgeTokens[chainId][tokenId];
        require(_requireDepositToken(chainId, config.evmToken, value, false) == tokenId,
                ERR_TOKEN_CONFIG);
        _mintNonce = nonce;
        ensureBalance();
        gosh.mintecc(uint64(value), tokenId);
        _creditToken(chainId, tokenId, value);
        mapping(uint32 => varuint32) ecc;
        ecc[tokenId] = varuint32(value);
        recipient.transfer({value: 1 vmshell, bounce: false, flag: 1, currencies: ecc});
    }

    // ========================================================
    // Mint USDC and send to Accumulator for a buyer
    // ========================================================

    /// @notice Mints ECC[3] USDC and sends it to the Accumulator's buyShellFor,
    ///         which will process the purchase and send ECC[2] Shell to the buyer.
    /// @param buyer — address to receive Shell from the Accumulator
    /// @param value — amount of ECC[3] USDC to mint (in micro-USDC)
    function mintAndSendAccumulator(address buyer, uint128 value, uint64 nonce) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        require(nonce == _mintAccumulatorNonce + 1, ERR_INVALID_NONCE);
        require(value > 0, ERR_ZERO_AMOUNT);
        require(value % USDC_DECIMALS_FACTOR == 0, ERR_NOT_WHOLE_USDC);
        require(value <= uint128(type(uint64).max), ERR_OVERFLOW);
        _mintAccumulatorNonce = nonce;

        gosh.mintecc(uint64(value), USDC_ECC_ID);
        _totalMinted += value;

        mapping(uint32 => varuint32) ecc;
        ecc[USDC_ECC_ID] = varuint32(value);
        IShellAccumulator(ACCUMULATOR_ADDRESS).buyShellFor{value: 1 vmshell, bounce: false, flag: 1, currencies: ecc}(buyer);

        address addrExtern = address.makeAddrExtern(UsdcMintedEmit, bitCntAddress);
        emit UsdcMinted{dest: addrExtern}(buyer, value);
    }

    // ========================================================
    // Cross-chain bridge — outbound (AN -> any chain): burn ECC, emit proof-source event
    // ========================================================

    /// @notice Burns the ECC currency attached to this message and emits an event
    ///         carrying the data needed to mint the equivalent on the destination
    ///         chain. Exactly one ECC currency must be attached; its id and amount
    ///         are taken from `msg.currencies`. The destination chain is opaque
    ///         to this contract — `dstChainId` is just passed through to the event.
    /// @param dstChainId — opaque destination chain identifier (passed through to event)
    /// @param recipient  — destination-chain recipient bytes (≤64 bytes)
    function initiateWithdrawal(uint256 dstChainId, bytes recipient) public {
        require(!_paused, ERR_PAUSED);
        require(recipient.length > 0, ERR_RECIPIENT_EMPTY);
        require(recipient.length <= 64, ERR_RECIPIENT_TOO_LONG);
        require(!_isZeroRecipient(recipient), ERR_ZERO_RECIPIENT);

        mapping(uint32 => varuint32) currencies = msg.currencies;
        uint32[] keys = currencies.keys();
        require(keys.length >= 1, ERR_NO_ECC);
        require(keys.length == 1, ERR_MULTIPLE_ECC);

        uint32 tokenId = keys[0];
        BridgeTokenConfig config = _bridgeTokens[dstChainId][tokenId];
        if (config.evmToken == 0 && tokenId != USDC_ECC_ID) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId, ERR_TOKEN_UNREGISTERED);
            return;
        }
        uint160 dstBridge = _evmBridgeByChain[dstChainId];
        if (dstBridge == 0) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_NO_EVM_BRIDGE);
            return;
        }
        uint128 payoutCap = config.evmToken == 0 ? MAX_TRANSFER_AMOUNT : config.maxWithdrawal;
        if (currencies[tokenId] > varuint32(payoutCap)) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_TRANSFER_TOO_LARGE);
            return;
        }
        uint128 amount = uint128(currencies[tokenId]);
        require(amount > 0, ERR_ZERO_AMOUNT);
        require(amount <= uint128(type(uint64).max), ERR_OVERFLOW);
        uint128 minted = _totalMintedBridgeByToken[tokenId];
        uint128 burned = _totalBurnedBridgeByToken[tokenId];
        if (burned > minted || amount > minted - burned) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_BURN_EXCEEDS_BRIDGED);
            return;
        }
        if (amount > _tokenOutstanding(dstChainId, tokenId)) {
            _refundWithdrawal(currencies, dstChainId, recipient, tokenId,
                              ERR_CHAIN_BALANCE_EXCEEDED);
            return;
        }

        tvm.accept();
        ensureBalance();

        gosh.burnecc(uint64(amount), tokenId);
        _totalBurnedBridgeByToken[tokenId] += amount;
        if (tokenId == USDC_ECC_ID) {
            _outstandingByChain[dstChainId] -= amount;
        } else {
            _bridgeTokens[dstChainId][tokenId].outstanding -= amount;
        }

        address addrExtern = address.makeAddrExtern(WithdrawalInitiatedEmit, bitCntAddress);
        emit WithdrawalInitiated{dest: addrExtern}(dstChainId, recipient, amount, tokenId,
                                                   dstBridge, msg.sender);
    }

    /// @dev Returns the ECC and says why on `WithdrawalRefused`, so a refusal
    ///      leaves a record a relayer or a UI can read: `reason` is the error
    ///      code the call would have thrown if a refusal threw —
    ///      `ERR_TOKEN_UNREGISTERED` (241) when the currency is not registered
    ///      for the destination chain, `ERR_NO_EVM_BRIDGE` (239) when no EVM
    ///      bridge is set for it, `ERR_TRANSFER_TOO_LARGE` (234) over the
    ///      per-transfer cap,
    ///      `ERR_BURN_EXCEEDS_BRIDGED` (233) when the burn would take bridged
    ///      burned above bridged minted, `ERR_CHAIN_BALANCE_EXCEEDED` (236)
    ///      when the destination chain accounts for less than this amount.
    ///      An amount too large for `uint128` is reported saturated; it is
    ///      already far above the cap that refused it.
    function _refundWithdrawal(mapping(uint32 => varuint32) currencies, uint256 dstChainId,
                               bytes recipient, uint32 tokenId, uint16 reason) private pure {
        tvm.accept();
        ensureBalance();
        varuint32 sent = currencies[tokenId];
        uint128 shown = sent > varuint32(type(uint128).max)
            ? type(uint128).max : uint128(sent);
        address addrExtern = address.makeAddrExtern(WithdrawalRefusedEmit, bitCntAddress);
        emit WithdrawalRefused{dest: addrExtern}(dstChainId, recipient, shown, tokenId,
                                                msg.sender, reason);
        msg.sender.transfer({value: math.min(msg.value, 0.1 vmshell),
                             bounce: false, flag: 1, currencies: currencies});
    }

    /// @dev True when every byte of `recipient` is zero. The destination chain
    ///      is opaque here, so this cannot compare against one chain's zero
    ///      address: a 20-byte EVM zero and a 32-byte one both name no one.
    function _isZeroRecipient(bytes recipient) private pure returns (bool) {
        TvmSlice s = recipient.toSlice();
        uint i;
        for (i = 0; i < recipient.length; i++) {
            if (s.bits() < 8) {
                s = s.loadRef().toSlice();
            }
            if (s.loadUint(8) != 0) {
                return false;
            }
        }
        return true;
    }

    // ========================================================
    // Cross-chain bridge — inbound (any chain -> AN): verify proof, deploy DepositVoucher, mint ECC
    // ========================================================

    /// @notice Owner-managed source-chain allowlist entry: add or remove ONE L1
    ///         bridge contract from the trusted SET of `chainId`. `l1Bridge` is
    ///         the L1 address left-padded to uint256, exactly as the circuit
    ///         exposes it in the public inputs (fr[3]). `allowed=true` trusts it,
    ///         `false` revokes it; several addresses may be trusted on the same
    ///         chain at once (rotation window). Applies to `finalizeDeposit`
    ///         only — the outbound path and the TIP-3/owner-mint flows are
    ///         unaffected.
    function setTrustedL1Bridge(uint256 chainId, uint256 l1Bridge, bool allowed) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        if (allowed) {
            _trustedL1Bridge[chainId][l1Bridge] = true;
        } else {
            delete _trustedL1Bridge[chainId][l1Bridge];
        }
    }

    /// @notice Owner-managed destination bridge: the EVM `AckiNackiBridge` on
    ///         `dstChainId` that withdrawals to that chain are bound to. It goes
    ///         into every `WithdrawalInitiated`, and a withdrawal proof pays out
    ///         only on that address. Zero clears it, and withdrawals to the chain
    ///         are refunded until it is set again. Changing it leaves earlier
    ///         burns bound to the bridge that was set when they were made.
    function setEvmBridge(uint256 dstChainId, uint160 evmBridge) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        if (evmBridge == 0) {
            delete _evmBridgeByChain[dstChainId];
        } else {
            _evmBridgeByChain[dstChainId] = evmBridge;
        }
    }

    /// @notice The EVM bridge withdrawals to `dstChainId` are bound to; 0 if unset.
    function getEvmBridge(uint256 dstChainId) external view returns (uint160) {
        return _evmBridgeByChain[dstChainId];
    }

    /// @notice Returns the trusted L1 bridge SET for `chainId` (address -> true).
    function getTrustedL1Bridges(uint256 chainId) external view returns (mapping(uint256 => bool)) {
        return _trustedL1Bridge[chainId];
    }

    /// @notice True if `l1Bridge` is in the trusted set of `chainId`.
    function isTrustedL1Bridge(uint256 chainId, uint256 l1Bridge) external view returns (bool) {
        return _trustedL1Bridge[chainId][l1Bridge];
    }

    /// @notice Admits (or retracts) a source-chain block hash as canonical.
    ///         Whoever holds the owner key MUST:
    ///           * verify the block against an independent node, not against
    ///             the relayer that produced the proof, and
    ///           * wait for enough confirmations, since a reorged-out block
    ///             loses the funds exactly like a dishonest one.
    ///
    ///         Accepts a hash rather than a header so that swapping this writer
    ///         for the light client later needs no change to `finalizeDeposit`.
    /// @param chainId   — EIP-155 chain id of the source L1/L2.
    /// @param blockHash — the block's 256-bit hash, as the circuit binds it into
    ///                    public inputs #9/#10 (`hi << 128 | lo`).
    /// @param accepted  — true to admit, false to retract (e.g. on discovering
    ///                    the block was reorged out before any deposit landed).
    function setAcceptedBlockHash(uint256 chainId, uint256 blockHash, bool accepted)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(!_ownerAnchorsDisabled[chainId], ERR_OWNER_ANCHORS_DISABLED);
        ensureBalance();
        if (accepted) {
            _acceptedBlockHash[chainId][blockHash] = true;
        } else {
            delete _acceptedBlockHash[chainId][blockHash];
        }
    }

    /// @notice Installs the code every light client is deployed from. It
    ///         changes the address the next `deployLightClient` derives;
    ///         clients already recorded in `_lightClients` keep their
    ///         addresses and stay recognized as writers. This remains owner-
    ///         callable after the anchor flip; a subsequent `deployLightClient`
    ///         can replace the recorded writer for a flipped chain.
    function setLightClientCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _lightClientCode = code;
    }

    /// @notice Deploys the beacon light client. The bridge is the only account
    ///         allowed to do so: the contract's constructor refuses any other
    ///         sender, so nothing else can occupy that address. Always writes
    ///         `l1ChainId` into `_lightClientL1ChainId`, even if the
    ///         constructor message bounces because the account already exists.
    function deployLightClient(
        uint256 pubkey,
        uint256 l1ChainId,
        uint256 bootstrapCommittee,
        uint64  bootstrapPeriod
    ) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        require(_lightClientCode.depth() != 0, ERR_LIGHT_CLIENT_UNSET);
        require(l1ChainId != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        ensureBalance();
        TvmCell stateInit = abi.encodeStateInit({
            contr: EthBeaconLightClient,
            varInit: {},
            code: abi.setCodeSalt(_lightClientCode, abi.encode(l1ChainId))
        });
        address lightClient = address.makeAddrStd(0, tvm.hash(stateInit));
        new EthBeaconLightClient{
            stateInit: stateInit,
            value: 10 vmshell,
            flag: 1
        }();
        EthBeaconLightClient(lightClient).initialize{
            value: 2 vmshell,
            flag: 1
        }(pubkey, l1ChainId, bootstrapCommittee, bootstrapPeriod);
        _lightClients[l1ChainId] = lightClient;
        _lightClientL1ChainId = l1ChainId;
    }

    /// @notice Address the light client for `l1ChainId` is deployed at
    ///         (`addr_none`, not `address(0)`, until `deployLightClient`
    ///         ran for that chain).
    function getLightClient(uint256 l1ChainId) external view returns (address) {
        return _lightClients[l1ChainId];
    }

    /// @notice Disables direct owner anchor writes for the chain whose light
    ///         client was deployed last. Other chains keep their own setting;
    ///         use `disableOwnerAnchorsOn` to select one explicitly.
    /// @dev There is no direct re-enable function in this code, but the owner
    ///      retains `updateCode`, which resets these flags, and can replace
    ///      the recorded client through `setLightClientCode` followed by
    ///      `deployLightClient`. This call does not revoke owner trust or
    ///      upgrade authority. Requires a recorded light client or L2
    ///      verifier for the chain; it does not check that the account is
    ///      Active. Idempotent.
    function disableOwnerAnchors() public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        _disableOwnerAnchors(_lightClientL1ChainId);
    }

    /// @notice Same as `disableOwnerAnchors()`, for an explicit chain id.
    ///         Any chain with a light client in `_lightClients` or an L2
    ///         verifier in `_l2Verifiers` is accepted; a chain with neither
    ///         reverts `ERR_LIGHT_CLIENT_UNSET` (226),
    ///         and chain id 0 reverts `ERR_NOT_LIGHT_CLIENT_CHAIN` (227).
    ///         A chain with an Arbitrum block verifier reverts
    ///         `ERR_PARENT_OWNER_ANCHORS_ENABLED` (240) until the owner
    ///         writer is off for the verifier's parent chain: the verifier
    ///         admits blocks under any parent-chain block the bridge accepts,
    ///         so an owner who still anchors the parent still reaches the
    ///         Arbitrum chain. Turn off the parent chain first.
    ///         Sold does not allow a public overload of
    ///         `disableOwnerAnchors`.
    function disableOwnerAnchorsOn(uint256 chainId) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        _disableOwnerAnchors(chainId);
    }

    function _disableOwnerAnchors(uint256 chainId) private {
        require(chainId != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        // `exists`, not `!= address(0)`: a missing key reads as addr_none,
        // which is not address(0), so that comparison passed for any chain.
        require(_lightClients.exists(chainId) || _l2Verifiers.exists(chainId),
                ERR_LIGHT_CLIENT_UNSET);
        require(!_l2Verifiers.exists(chainId) || _ownerAnchorsDisabled[_l2VerifierParent[chainId]],
                ERR_PARENT_OWNER_ANCHORS_ENABLED);
        ensureBalance();
        _ownerAnchorsDisabled[chainId] = true;
    }

    /// @notice Stops and resumes the cross-chain lane: while paused,
    ///         `finalizeDeposit` and `initiateWithdrawal` throw
    ///         `ERR_PAUSED` (231) before doing any work. Reversible, and
    ///         deliberately narrow — the TIP-3 inbound callback, the owner
    ///         mints, `confirmDeposit` and every anchor and allowlist call stay
    ///         available, so a deposit already proven can still be paid out and
    ///         the owner can keep the bridge's configuration current while it
    ///         is stopped.
    function setPaused(bool paused) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _paused = paused;
    }

    /// @notice Register USDC for a source chain once, then update its deposit limits.
    ///         ECC id 3 is the only id accepted: Acki Nacki has no other currency
    ///         for the bridge to mint, and a new one takes a new version of this code.
    ///         The token/id association is immutable; TVL can only increase.
    ///         Closing or throttling deposits preserves pending confirmations and withdrawals.
    function setBridgeToken(uint256 chainId, uint256 evmToken, uint32 tokenId,
                            uint128 maxTransfer, uint128 maxTvl, bool depositsEnabled)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(chainId != 0 && evmToken != 0 && evmToken < (uint256(1) << 160), ERR_TOKEN_CONFIG);
        require(tokenId == USDC_ECC_ID, ERR_TOKEN_CONFIG);
        require(maxTransfer > 0 && maxTransfer <= uint128(type(uint64).max)
                && maxTvl >= maxTransfer, ERR_TOKEN_CONFIG);
        if (tokenId == USDC_ECC_ID) {
            require(maxTransfer <= MAX_TRANSFER_AMOUNT && maxTvl <= MAX_BRIDGE_TVL, ERR_TOKEN_CONFIG);
        }
        BridgeTokenConfig config = _bridgeTokens[chainId][tokenId];
        require(maxTvl >= config.maxTvl, ERR_TOKEN_CONFIG);
        require(config.evmToken == 0 || config.evmToken == evmToken, ERR_TOKEN_CONFIG);
        uint32 previousId = _bridgeTokenIds[chainId][evmToken];
        require(previousId == 0 || previousId == tokenId, ERR_TOKEN_CONFIG);
        config.evmToken = evmToken;
        config.maxTransfer = maxTransfer;
        config.maxTvl = maxTvl;
        uint128 payoutFloor = tokenId == USDC_ECC_ID ? MAX_TRANSFER_AMOUNT : maxTransfer;
        config.maxWithdrawal = math.max(config.maxWithdrawal, payoutFloor);
        config.depositsEnabled = depositsEnabled;
        _bridgeTokens[chainId][tokenId] = config;
        _bridgeTokenIds[chainId][evmToken] = tokenId;
        ensureBalance();
    }

    function _requireDepositToken(uint256 chainId, uint256 evmToken, uint128 amount,
                                  bool newDeposit) private view returns (uint32 tokenId) {
        tokenId = _bridgeTokenIds[chainId][evmToken];
        BridgeTokenConfig config = _bridgeTokens[chainId][tokenId];
        require(evmToken != 0 && config.evmToken == evmToken, ERR_TOKEN_UNREGISTERED);
        require(!newDeposit || config.depositsEnabled, ERR_TOKEN_DEPOSITS_DISABLED);
        uint128 cap = newDeposit ? config.maxTransfer : config.maxWithdrawal;
        require(amount <= cap, ERR_TRANSFER_TOO_LARGE);
        uint128 outstanding = _tokenOutstanding(chainId, tokenId);
        require(outstanding <= config.maxTvl && amount <= config.maxTvl - outstanding, ERR_TVL_CAP);
    }

    function _creditToken(uint256 chainId, uint32 tokenId, uint128 amount) private {
        _totalMintedBridgeByToken[tokenId] += amount;
        if (tokenId == USDC_ECC_ID) {
            _outstandingByChain[chainId] += amount;
        } else {
            _bridgeTokens[chainId][tokenId].outstanding += amount;
        }
    }

    function _tokenOutstanding(uint256 chainId, uint32 tokenId) private view returns (uint128) {
        return tokenId == USDC_ECC_ID ? _outstandingByChain[chainId]
                                     : _bridgeTokens[chainId][tokenId].outstanding;
    }

    function getTokenDepositConfig(uint256 chainId, uint256 evmToken) external view returns (
        bool listed, uint32 tokenId, uint128 maxTransfer, uint128 maxTvl,
        uint128 outstanding, bool depositsEnabled, uint128 maxWithdrawal
    ) {
        tokenId = _bridgeTokenIds[chainId][evmToken];
        BridgeTokenConfig config = _bridgeTokens[chainId][tokenId];
        listed = evmToken != 0 && config.evmToken == evmToken;
        return (listed, tokenId, config.maxTransfer, config.maxTvl,
                _tokenOutstanding(chainId, tokenId), config.depositsEnabled, config.maxWithdrawal);
    }

    function getTokenWithdrawalConfig(uint256 chainId, uint32 tokenId) external view returns (
        bool listed, uint256 evmToken, uint128 maxTransfer, uint128 maxTvl,
        uint128 outstanding, bool depositsEnabled, uint128 maxWithdrawal
    ) {
        BridgeTokenConfig config = _bridgeTokens[chainId][tokenId];
        return (config.evmToken != 0, config.evmToken, config.maxTransfer, config.maxTvl,
                _tokenOutstanding(chainId, tokenId), config.depositsEnabled, config.maxWithdrawal);
    }

    /// @notice States how much of one chain's principal this bridge still
    ///         accounts for, which is the ceiling `initiateWithdrawal` pays out
    ///         to that chain and the room left under `MAX_BRIDGE_TVL` for a
    ///         credit to it. `confirmDeposit` raises the counter, a withdrawal
    ///         spends it, and `onCodeUpgrade` carries it between builds that
    ///         both have it. What leaves it empty is a migration from a build
    ///         that had none, while the principal stays locked on the source
    ///         chain. This is how the owner states what is locked there.
    ///
    ///         The figure to write is the principal L1 holds less the deposits
    ///         not yet credited here and the burns L1 has not yet paid out —
    ///         not L1's balance, which still carries both. The counter is also
    ///         the deposit room, since `finalizeDeposit` and `confirmDeposit`
    ///         bound `outstanding + amount` by `MAX_BRIDGE_TVL`, so writing
    ///         more than that figure buys withdrawal room by refusing deposits
    ///         with `ERR_TVL_CAP`, and a deposit caught between finalization
    ///         and confirmation aborts with its voucher already consumed.
    ///         Writing less is the safe direction: it only refuses
    ///         withdrawals, which return the ECC.
    ///
    ///         It moves this counter alone. A withdrawal is still bounded by
    ///         bridged minted less bridged burned, so where that inventory is
    ///         empty too — a fresh deployment — this pays nothing out on its
    ///         own: the withdrawal is refused with `ERR_BURN_EXCEEDS_BRIDGED`
    ///         instead. It recovers a deployment that carried its inventory
    ///         across an upgrade but not its per-chain counter. It is no new
    ///         authority: `mintAndSend` has no ceiling at all, and a call that
    ///         writes a counter without minting is weaker than one that mints.
    ///         See `contracts/an/README.md`, *Restoring a chain's outstanding
    ///         amount*.
    /// @param chainId — EIP-155 chain id of the destination L1/L2.
    /// @param amount  — principal outstanding to that chain, in token units,
    ///                  at most `MAX_BRIDGE_TVL`.
    function setOutstandingByChain(uint256 chainId, uint128 amount)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(amount <= MAX_BRIDGE_TVL, ERR_TVL_CAP);
        ensureBalance();
        _outstandingByChain[chainId] = amount;
    }

    /// @notice Admits a block hash the light client proved final on the source
    ///         chain. Authorized solely by being the light client this bridge
    ///         — no human asserts canonicality, the proof does.
    function acceptBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClients[chainId] != address(0) && msg.sender == _lightClients[chainId],
                ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        _acceptedBlockHash[chainId][blockHash] = true;
    }

    /// @notice Drops a hash the light client has aged out of its window.
    function forgetBlockHashFromLightClient(uint256 chainId, uint256 blockHash) public {
        require(_lightClients[chainId] != address(0) && msg.sender == _lightClients[chainId],
                ERR_INVALID_SENDER);
        tvm.accept();
        ensureBalance();
        delete _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice Installs the code every Arbitrum block verifier is deployed
    ///         from. Verifiers already recorded keep their addresses.
    function setL2VerifierCode(TvmCell code) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        _l2VerifierCode = code;
    }

    /// @notice Deploys the block verifier of Arbitrum chain `l2ChainId`. It
    ///         proves that chain's blocks from the assertions `rollup` posts on
    ///         `parentChainId`, whose blocks must reach `_acceptedBlockHash`
    ///         through that chain's light client. Re-running it with new
    ///         verifier code replaces the recorded verifier; with the code of
    ///         the recorded one it throws `ERR_L2_VERIFIER_DEPLOYED` (238).
    ///         On a chain whose owner writer is already off it throws
    ///         `ERR_PARENT_OWNER_ANCHORS_ENABLED` (240) unless the owner
    ///         writer is off for `parentChainId` too, so a new verifier cannot
    ///         reopen the chain through a parent the owner still anchors.
    /// @param l2ChainId     — EIP-155 id of the Arbitrum chain (42161, 421614).
    /// @param parentChainId — EIP-155 id of the chain its rollup lives on (1, 11155111).
    /// @param rollup        — the rollup contract that emits `AssertionCreated`.
    function deployL2Verifier(uint256 l2ChainId, uint256 parentChainId, uint160 rollup)
        public onlyOwnerPubkey(_ownerPubkey) accept saveMsg
    {
        require(_l2VerifierCode.depth() != 0, ERR_L2_VERIFIER_UNSET);
        require(l2ChainId != 0 && parentChainId != 0 && rollup != 0, ERR_NOT_LIGHT_CLIENT_CHAIN);
        TvmCell stateInit = abi.encodeStateInit({
            contr: ArbitrumBlockVerifierDeploy,
            varInit: {},
            code: abi.setCodeSalt(_l2VerifierCode, abi.encode(l2ChainId))
        });
        address verifier = address.makeAddrStd(0, tvm.hash(stateInit));
        // The same code and chain id reach the recorded verifier, whose
        // `initialize` refuses a second configuration (311) in a message the
        // bridge never hears back from: recording this call would leave the
        // bridge and the verifier on different parent chains.
        require(!_l2Verifiers.exists(l2ChainId) || _l2Verifiers[l2ChainId] != verifier,
                ERR_L2_VERIFIER_DEPLOYED);
        require(!_ownerAnchorsDisabled[l2ChainId] || _ownerAnchorsDisabled[parentChainId],
                ERR_PARENT_OWNER_ANCHORS_ENABLED);
        ensureBalance();
        new ArbitrumBlockVerifierDeploy{
            stateInit: stateInit,
            value: 10 vmshell,
            flag: 1
        }();
        ArbitrumBlockVerifierDeploy(verifier).initialize{
            value: 2 vmshell,
            flag: 1
        }(l2ChainId, parentChainId, rollup);
        _l2Verifiers[l2ChainId] = verifier;
        _l2VerifierParent[l2ChainId] = parentChainId;
    }

    /// @notice The verifier of `l2ChainId` and its parent chain. Until
    ///         `deployL2Verifier` ran for it: no address (addr_none, not the
    ///         zero address) and 0.
    function getL2Verifier(uint256 l2ChainId) external view returns (address verifier, uint256 parentChainId) {
        return (_l2Verifiers[l2ChainId], _l2VerifierParent[l2ChainId]);
    }

    /// @notice Admits an Arbitrum block hash the chain's verifier proved from
    ///         an assertion in parent-chain block `parentBlockHash`. Only that
    ///         verifier may call it, and only once the parent-chain block is
    ///         accepted here, so the trust reaches back to the light client.
    function acceptBlockHashFromL2Verifier(uint256 l2ChainId, uint256 parentBlockHash, uint256 blockHash) public {
        require(_l2Verifiers.exists(l2ChainId) && msg.sender == _l2Verifiers[l2ChainId],
                ERR_INVALID_SENDER);
        require(_acceptedBlockHash[_l2VerifierParent[l2ChainId]][parentBlockHash], ERR_UNKNOWN_BLOCK);
        tvm.accept();
        ensureBalance();
        _acceptedBlockHash[l2ChainId][blockHash] = true;
    }

    /// @notice True if `blockHash` is admitted as canonical for `chainId`.
    function isAcceptedBlockHash(uint256 chainId, uint256 blockHash) external view returns (bool) {
        return _acceptedBlockHash[chainId][blockHash];
    }

    /// @notice Anchor-path configuration of the chain whose light client was
    ///         deployed last, and whether the owner may still admit anchors
    ///         there. With clients on several chains this names only that one:
    ///         read `getLightClient(chainId)` and
    ///         `isOwnerAnchorsEnabled(chainId)` for a specific chain.
    function getAnchorConfig() external view returns (address lightClient, bool ownerAnchorsEnabled) {
        return (_lightClients[_lightClientL1ChainId],
                _lightClientL1ChainId == 0 || !_ownerAnchorsDisabled[_lightClientL1ChainId]);
    }

    /// @notice Whether the owner may still admit anchors for `chainId`.
    ///         False after `disableOwnerAnchors` / `disableOwnerAnchorsOn`
    ///         for that chain; every chain is independent.
    function isOwnerAnchorsEnabled(uint256 chainId) external view returns (bool) {
        return !_ownerAnchorsDisabled[chainId];
    }

    /// @notice EIP-155 chain id the deployed light client follows (0 until
    ///         `deployLightClient`).
    function getLightClientL1ChainId() external view returns (uint256) {
        return _lightClientL1ChainId;
    }

    /// @notice Whether the cross-chain lane is stopped by the owner.
    function isPaused() external view returns (bool) {
        return _paused;
    }

    /// @notice Finalizes a deposit made on chain `chainId`: the `Deposit` log
    ///         number `logIndex` of transaction `txIndex` in the block whose
    ///         header is `header`. The block must be accepted for that chain
    ///         (`_acceptedBlockHash`); the receipt is proven against the
    ///         header's receipts root, and every deposit field is read out of
    ///         the proven log, so nothing but the choice of log is caller-set.
    ///         Permissionless. A deterministic `DepositVoucher` keyed on the
    ///         deposit identity gives replay protection; it calls back
    ///         `confirmTokenDeposit` to mint and pay.
    /// @param chainId       — source chain id; picks the accepted blocks and
    ///                         the trusted bridges the deposit is checked against.
    /// @param header        — RLP of the block header.
    /// @param txIndex       — index of the deposit transaction in the block.
    /// @param receiptProof  — receipt-trie nodes from the header's receipts
    ///                         root down to the leaf at `rlp(txIndex)`.
    /// @param logIndex      — index of the `Deposit` log within that receipt.
    function finalizeDeposit(uint256 chainId, bytes header, uint16 txIndex,
                             bytes[] receiptProof, uint16 logIndex) public {
        // Every check runs before accept, within the external message's gas
        // credit, so a refused deposit costs the bridge nothing. A deposit
        // whose proof walk exceeds the credit cannot be finalized; the CLI and
        // deposit-relayer refuse to send one.
        require(!_paused, ERR_PAUSED);
        uint256 blockHash = gosh.keccak256(header);
        require(_acceptedBlockHash[chainId][blockHash], ERR_UNKNOWN_BLOCK);
        DepositPI f = _provenDeposit(header, blockHash, txIndex, receiptProof, logIndex);
        f.chainId = chainId;
        require(f.amount > 0, ERR_ZERO_AMOUNT);
        // The != 0 guard keeps a stray `_trustedL1Bridge[chainId][0]=true`
        // from ever admitting a zero contract.
        require(f.contractAddr != 0 && _trustedL1Bridge[f.chainId][f.contractAddr],
                ERR_UNSUPPORTED_SRC_CHAIN);
        require(f.anAccount != 0, ERR_ZERO_RECIPIENT);
        _requireDepositToken(f.chainId, f.evmToken, f.amount, true);

        tvm.accept();
        _saveMsg();
        tvm.commit();
        ensureBalance();

        // Anti-replay anchor = (deposit_id, emitting contract, source chain);
        // the dapp component is pinned to 0. chainId is IN the key: two
        // different chains may legitimately emit the same (deposit_id,
        // contract) pair. amount/recipient are NOT in the key — the emitter
        // fixes them per deposit_id, so a replay can never re-route or
        // re-mint: same key ⇒ same voucher ⇒ no-op.
        uint256 depositHash = tvm.hash(abi.encode(f.depositId, f.contractAddr, f.dappId, f.chainId));

        TvmCell stateInit = abi.encodeStateInit({
            contr: DepositVoucher,
            varInit: { _depositHash: depositHash },
            code: _depositVoucherCode
        });

        new DepositVoucher{
            stateInit: stateInit,
            value: 2 vmshell,
            flag: 1
        }(f.depositId, f.contractAddr, f.dappId, f.chainId, f.amount, f.anAccount, f.evmToken);
    }

    /// @notice Internal callback from a freshly deployed `DepositVoucher`. Mints
    ///         registered ECC and sends it to the proof-bound AN recipient. The caller
    ///         must be the deterministic voucher address derived from the
    ///         deposit identity — replay attempts hit the existing voucher
    ///         account whose constructor was already consumed. If this call
    ///         aborts after that constructor has run, the voucher is spent
    ///         and the owner pays the recipient with the matching recovery mint.
    function confirmDeposit(
        uint256 depositId, uint256 contractAddr, uint256 dappId,
        uint256 chainId, uint128 amount, uint256 anAccount
    ) public {
        _confirmTokenDeposit(depositId, contractAddr, dappId, chainId, amount,
                             anAccount, _bridgeTokens[chainId][USDC_ECC_ID].evmToken);
    }

    function confirmTokenDeposit(
        uint256 depositId, uint256 contractAddr, uint256 dappId,
        uint256 chainId, uint128 amount, uint256 anAccount, uint256 evmToken
    ) public {
        _confirmTokenDeposit(depositId, contractAddr, dappId, chainId, amount, anAccount, evmToken);
    }

    function _confirmTokenDeposit(
        uint256 depositId, uint256 contractAddr, uint256 dappId,
        uint256 chainId, uint128 amount, uint256 anAccount, uint256 evmToken
    ) private {
        uint256 depositHash = tvm.hash(abi.encode(depositId, contractAddr, dappId, chainId));
        TvmCell stateInit = abi.encodeStateInit({
            contr: DepositVoucher,
            varInit: { _depositHash: depositHash },
            code: _depositVoucherCode
        });
        require(msg.sender == address.makeAddrStd(0, tvm.hash(stateInit)), ERR_INVALID_SENDER);
        require(anAccount != 0, ERR_ZERO_RECIPIENT);
        require(amount > 0, ERR_ZERO_AMOUNT);
        uint32 tokenId = _requireDepositToken(chainId, evmToken, amount, false);

        tvm.accept();
        ensureBalance();

        gosh.mintecc(uint64(amount), tokenId);
        _creditToken(chainId, tokenId, amount);

        mapping(uint32 => varuint32) ecc;
        ecc[tokenId] = varuint32(amount);
        address.makeAddrStd(0, anAccount).transfer({
            value: 1 vmshell,
            bounce: false,
            flag: 1,
            currencies: ecc
        });

        address addrExtern = address.makeAddrExtern(DepositFinalizedEmit, bitCntAddress);
        emit DepositFinalized{dest: addrExtern}(
            depositId, contractAddr, dappId, chainId, amount, anAccount
        );
    }

    // DepositVoucher code rotation is intentionally not exposed as a
    // standalone setter. The only way to change `_depositVoucherCode` is via
    // a full `updateCode` upgrade of eccBridge (the new code+layout pass
    // through `onCodeUpgrade`). This removes the "owner can swap voucher
    // logic in one tx and free-mint" backdoor flagged in PR2112 review (B2).

    // ========================================================
    // Admin
    // ========================================================

    /// @notice Replaces the owner public key. Only callable by the current owner.
    /// @param pubkey — new owner public key (uint256)
    function setPubkey(uint256 pubkey) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        ensureBalance();
        _ownerPubkey = pubkey;
    }

    /// @notice Sends a plain transfer to the given address from the bridge.
    ///         Used to trigger Transaction contracts deployed by the bridge's USDC wallet
    ///         (e.g. SET_SUBSCRIBER_TYPE). Only callable by the owner.
    /// @param txAddr — address of the Transaction contract to trigger
    function triggerTransaction(address txAddr) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        txAddr.transfer({value: 1 vmshell, bounce: true, flag: 1});
    }

    // ========================================================
    // On-chain code upgrade (owner only)
    // ========================================================

    /// @notice Upgrades the contract code on-chain. Only callable by the owner.
    ///         Remains available after the anchor flip. This migration clears
    ///         the owner-anchor flags and recorded clients; operators must
    ///         restore configuration and repeat each intended chain's flip.
    /// @param newcode — new contract code TvmCell
    /// @param userCell — replacement voucher code, or an empty cell to retain
    ///        the current code. Registry configuration is preserved separately.
    function updateCode(TvmCell newcode, TvmCell userCell) public onlyOwnerPubkey(_ownerPubkey) accept saveMsg {
        ensureBalance();
        TvmCell registryState = abi.encode(
            userCell.toSlice().empty() ? _depositVoucherCode : userCell,
            _bridgeTokens, _bridgeTokenIds);
        TvmCell migrationCell = abi.encode(
            _ownerPubkey, _usdcWallet, _totalMinted, _mintNonce, _mintAccumulatorNonce,
            _totalMintedBridgeByToken, _totalBurnedBridgeByToken, _depositVoucherCode,
            registryState, _outstandingByChain, _messages
        );
        tvm.commit();
        tvm.setcode(newcode);
        tvm.setCurrentCode(newcode);
        onCodeUpgrade(migrationCell);
    }

    /// @notice Initializes state after code upgrade. Resets all storage and re-initializes
    ///         from the provided cell. Called by UpdateZeroContract (zerostate) and updateCode().
    /// @param cell — ABI-encoded tuple:
    ///                 (uint256 pubkey,
    ///                  address usdcWallet,
    ///                  uint128 totalMinted,
    ///                  uint64  mintNonce,
    ///                  uint64  mintAccumulatorNonce,
    ///                  mapping(uint32 => uint128) totalMintedBridgeByToken,
    ///                  mapping(uint32 => uint128) totalBurnedBridgeByToken,
    ///                  TvmCell depositVoucherCode,
    ///                  TvmCell userCell,
    ///                  mapping(uint256 => uint128) outstandingByChain,
    ///                  mapping(uint32 => mapping(uint256 => bool)) messages)
    ///         `depositVoucherCode` supplies the zerostate voucher code.
    ///         A non-empty `userCell` contains abi.encode(voucherCode,
    ///         bridgeTokens, bridgeTokenIds). For an upgrade from 1.15.0,
    ///         construct it with BridgeZerostateData.getTokenUpgradeData;
    ///         subsequent updateCode calls construct it automatically.
    ///
    ///         `_trustedL1Bridge` is deliberately absent from the tuple. After
    ///         an upgrade the allowlist starts empty (deposits fail closed)
    ///         until the owner re-seeds it via `setTrustedL1Bridge`.
    ///         This format requires a fresh 1.13.0 deployment; it does not
    ///         decode the previous nine-field upgrade tuple.
    function onCodeUpgrade(TvmCell cell) private {
        tvm.accept();
        tvm.resetStorage();
        (uint256 pubkey,
         address usdcWallet,
         uint128 totalMinted,
         uint64  mintNonce,
         uint64  mintAccumulatorNonce,
         mapping(uint32 => uint128) totalMintedBridgeByToken,
         mapping(uint32 => uint128) totalBurnedBridgeByToken,
         TvmCell depositVoucherCode,
         TvmCell userCell,
         mapping(uint256 => uint128) outstandingByChain,
         mapping(uint32 => mapping(uint256 => bool)) messages)
            = abi.decode(cell, (uint256, address, uint128, uint64, uint64,
                                mapping(uint32 => uint128), mapping(uint32 => uint128),
                                TvmCell, TvmCell, mapping(uint256 => uint128),
                                mapping(uint32 => mapping(uint256 => bool))));
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _usdcWallet = usdcWallet;
        _totalMinted = totalMinted;
        _mintNonce = mintNonce;
        _mintAccumulatorNonce = mintAccumulatorNonce;
        _totalBurnedBridgeByToken = totalBurnedBridgeByToken;
        _totalMintedBridgeByToken = totalMintedBridgeByToken;
        _outstandingByChain = outstandingByChain;
        _messages = messages;
        _depositVoucherCode = depositVoucherCode;
        if (!userCell.toSlice().empty()) {
            (_depositVoucherCode, _bridgeTokens, _bridgeTokenIds) = abi.decode(userCell, (
                TvmCell, mapping(uint256 => mapping(uint32 => BridgeTokenConfig)),
                mapping(uint256 => mapping(uint256 => uint32))));
        }
        // `_lightClients`, `_lightClientL1ChainId` and `_ownerAnchorsDisabled`
        // are absent from the tuple: after an upgrade the owner writer is
        // on for every chain until the owner re-deploys the light client
        // and flips L1 again.
    }

    // ========================================================
    // Getters
    // ========================================================

    /// @notice Returns the TIP-3 USDC TokenWallet address used for the bridge.
    function getUsdcWallet() external view returns (address) {
        return _usdcWallet;
    }

    /// @notice Returns the owner public key.
    function getOwnerPubkey() external view returns (uint256) {
        return _ownerPubkey;
    }

    /// @notice Returns ECC[3] counted by `mintAndSend`, `mintAndSendAccumulator`,
    ///         and the migration path. `confirmDeposit` and `mintAndSendBridged`
    ///         mint ECC[3] without incrementing this; bridged inventory is
    ///         `getTotalBridged`.
    function getTotalMinted() external view returns (uint128) {
        return _totalMinted;
    }

    /// @notice Returns total ECC minted/burned via the cross-chain bridge path
    ///         for a specific tokenId. `initiateWithdrawal` uses these as an
    ///         inventory cap (`ERR_BURN_EXCEEDS_BRIDGED` when burned would
    ///         exceed minted).
    function getTotalBridged(uint32 tokenId) external view returns (uint128 minted, uint128 burned) {
        return (_totalMintedBridgeByToken[tokenId], _totalBurnedBridgeByToken[tokenId]);
    }

    function getDepositLimits(uint256 chainId) external view returns (
        uint128 maxTransfer, uint128 maxTvl, uint128 outstanding
    ) {
        BridgeTokenConfig config = _bridgeTokens[chainId][USDC_ECC_ID];
        return (config.evmToken == 0 ? MAX_TRANSFER_AMOUNT : config.maxTransfer,
                config.evmToken == 0 ? MAX_BRIDGE_TVL : config.maxTvl,
                _outstandingByChain[chainId]);
    }

    /// @notice Returns the hash of the currently installed DepositVoucher code.
    function getDepositVoucherCodeHash() external view returns (uint256) {
        return tvm.hash(_depositVoucherCode);
    }

    /// @notice Returns current nonces for double-spend protection.
    function getNonces() external view returns (uint64 mintNonce, uint64 mintAccumulatorNonce) {
        return (_mintNonce, _mintAccumulatorNonce);
    }

    /// @notice Returns contract version and name.
    function getVersion() external pure returns (string, string) {
        return (version, "eccBridge");
    }

    // ========================================================
    // Deposit receipt
    // ========================================================

    /// @dev The deposit in log `logIndex` of the receipt of transaction
    ///      `txIndex`, proven against the receipts root of `header`, which
    ///      hashes to `blockHash`. `chainId` is left for the caller.
    function _provenDeposit(bytes header, uint256 blockHash, uint16 txIndex,
                            bytes[] receiptProof, uint16 logIndex) private pure returns (DepositPI f) {
        (, uint256 receiptsRoot) = EthTrie.headerRoots(header, blockHash);
        (uint256 key, uint nibbles) = EthTrie.receiptKey(txIndex);
        (TvmSlice receipt, uint len) = EthTrie.prove(receiptsRoot, key, nibbles, receiptProof);
        TvmSlice log = EthTrie.receiptLog(receipt, len, logIndex);

        uint256 v;
        (v, log) = EthTrie._uint(log, 2);
        require(v == DEPOSIT_LOG_HEADER, ERR_NOT_DEPOSIT_LOG);
        (v, log) = EthTrie._uint(log, 1);
        require(v == DEPOSIT_EMITTER_HEADER, ERR_NOT_DEPOSIT_LOG);
        (f.contractAddr, log) = EthTrie._uint(log, 20);
        (v, log) = EthTrie._uint(log, 2);
        require(v == DEPOSIT_TOPICS_HEADER, ERR_NOT_DEPOSIT_LOG);
        (v, log) = _depositTopic(log);
        require(v == DEPOSIT_TOPIC0, ERR_NOT_DEPOSIT_LOG);
        (f.depositId, log) = _depositTopic(log);
        (v, log) = _depositTopic(log);              // sender
        (v, log) = EthTrie._uint(log, 2);
        require(v == DEPOSIT_DATA_HEADER, ERR_NOT_DEPOSIT_LOG);
        (v, log) = EthTrie._uint(log, 32);
        require(v <= uint256(type(uint64).max), ERR_OVERFLOW);
        f.amount = uint128(v);
        (f.anAccount, log) = EthTrie._uint(log, 32);
        (v, log) = EthTrie._uint(log, 32);          // timestamp
        (f.evmToken, log) = EthTrie._uint(log, 32);
        // Deposits into AN always land in dapp 0. The workchain concept is
        // retired on AN, so the recipient always lives in workchain 0 (see
        // `_confirmTokenDeposit`'s makeAddrStd).
        f.dappId = 0;
    }

    function _depositTopic(TvmSlice s) private pure returns (uint256 v, TvmSlice) {
        (v, s) = EthTrie._uint(s, 1);
        require(v == DEPOSIT_TOPIC_HEADER, ERR_NOT_DEPOSIT_LOG);
        return EthTrie._uint(s, 32);
    }
}
