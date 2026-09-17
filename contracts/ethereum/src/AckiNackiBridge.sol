// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBlockHeaderOracle.sol";
import "./IAavePool.sol";
import "./IERC20.sol";
import "./IPrimaryVerifier.sol";
import "./IFallbackVerifier.sol";
import "./ILayerHashesMovementVerifier.sol";
import "./IBridgeWithdrawalVerifier.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain.
/// @dev Holds user USDC (ERC-20, 6 decimals) on deposit and routes idle balance
///      into AAVE V3 USDC market for yield.
///      - `deposit(uint256 amount)` stays cheap: funds accumulate in the contract;
///        an owner/keeper batches supplies to AAVE with `supplyToAave()` to amortise gas.
///      - Owner can harvest accrued yield without touching user principal.
///
///      AN→ETH state (Phase 4): the bridge stores a rolling commitment to the
///      Acki Nacki side (`block_seq_no`, `bk_set_poseidon`, layer-hash roots,
///      chain anchor). `verifyBlock` advances the commitment after verifying
///      a tuple of two cross-circuit-bound Halo2 SHPLONK aggregator proofs:
///        - **proof1**: Circuit 1A (Primary) or 1B (Fallback) attestation, BLS-aggregated.
///        - **proof2**: Circuit 2 (Layer hashes movement), Poseidon-Merkle-anchored.
///      Both proofs share a common `block_id` and `bk_set_poseidon` by
///      construction; the bridge enforces those equalities + the monotonic
///      `block_seq_no` and chain-anchor invariants on top.
///
///      AN→ETH event verification + payout (withdraw):
///      every successful `verifyBlock` appends the new top-of-chain anchor
///      to the per-layer rolling windows (`_layerWindows`).
///      `withdrawByProof` consumes those windows plus a Circuit 4
///      (`bridge-event-prove-circuit`) SHPLONK aggregator proof whose
///      10 public inputs include a single `finalRoot`. The bridge
///      calls `_isKnownAnchor(finalRoot)` off-circuit (this contract) — the
///      circuit only proves that the event's hash chain extends *into*
///      `finalRoot` via a dense-chain extension. The proof binds the
///      payout's `amount` and `recipient` (split-α 10/10 bytes) along
///      with the AN-side bridge identity `(bridgeWithdrawalDappFr,
///      bridgeWithdrawalAccFr)` and a Poseidon nullifier for replay
///      protection.
contract AckiNackiBridge {
    // ---------------------------------------------------------------------
    // Constants
    // ---------------------------------------------------------------------

    /// @notice One USDC base unit (USD Coin uses 6 decimals on Ethereum).
    uint256 public constant USDC_UNIT = 10 ** 6;

    /// @notice Maximum per-tx deposit amount. Capped at `type(uint64).max` so the
    ///         amount fits AN `USDCBridge` mint path (`fr[2]` as uint64). Not a
    ///         global TVL limit (QC-A1-1 / QC-AN-J1).
    uint256 public constant MAX_DEPOSIT_AMOUNT = type(uint64).max;

    /// @notice Basis-point denominator
    uint256 public constant BPS_DENOMINATOR = 10_000;

    /// @notice Upper bound on the liquid reserve (50% of treasury kept as USDC)
    uint256 public constant MAX_LIQUID_RESERVE_BPS = 5_000;

    /// @notice Maximum number of layer-hash slots per block (matches
    ///         partner Circuit 2's MAX_LAYERS).
    uint256 public constant MAX_LAYER_HASHES = 10;

    /// @notice Rolling-window length per layer (`GLOBAL_HISTORY_DATA_SPEC` §8.3).
    /// @dev WD-Q1: each successful `verifyBlock` appends one hash per
    ///      active layer. The oldest hash in that layer is evicted after 128
    ///      subsequent appends — this is a count of `verifyBlock` calls, not a
    ///      `blockSeqNo` span. A `withdrawByProof` whose `finalRoot` has been
    ///      evicted reverts `UnknownAnchor`; funds stay in the treasury.
    ///      The bridge is a stateless verifier and does not retry on anyone's
    ///      behalf: the withdrawing user is responsible for re-proving
    ///      Circuit 4 against a still-in-window descendant (dense chain
    ///      ≤ 11 rungs) before their anchor is evicted. Any relayer is
    ///      best-effort convenience infrastructure, not a privileged or
    ///      obligated actor. Fast-forward of `blockSeqNo` does **not** skip
    ///      extra slots — one call still writes one slot.
    uint256 public constant HISTORY_PROOF_WINDOW = 128;

    /// @notice BN254 scalar field order — the modulus every circuit public
    ///         input lives in.
    uint256 internal constant BN254_R =
        0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001;

    /// @notice Finalization type for a block being verified by `verifyBlock`.
    ///         Mirrors `attestation_bls_checker_circuit`'s `AttestationTargetType`
    ///         binary split: Primary (>= 2/3 quorum) or Fallback (>1/2 split).
    enum FinalizationType {
        Primary,
        Fallback
    }

    // ---------------------------------------------------------------------
    // Storage: core bridge state
    // ---------------------------------------------------------------------

    /// @notice Monotonic deposit identifier
    uint256 public depositCounter;

    /// @notice Total user principal currently held by the bridge (USDC + aUSDC principal)
    /// @dev Yield accrued in AAVE is *not* reflected here — see `accruedYield()`.
    uint256 public treasuryBalance;

    /// @notice Oracle providing canonical Ethereum block hashes.
    /// @dev Currently unused by the public surface; preserved for the future
    ///      burn-proof flow that will anchor cross-chain withdrawals to an
    ///      Ethereum block hash. Set in the constructor and never read.
    IBlockHeaderOracle public blockHeaderOracle;

    // ---------------------------------------------------------------------
    // Storage: AAVE integration
    // ---------------------------------------------------------------------

    /// @notice USDC (or test-USDC) accepted for deposits. Set at construction.
    IERC20 public immutable usdc;

    /// @notice AAVE V3 Pool (set once at construction, immutable thereafter)
    IAavePool public immutable aavePool;

    /// @notice aUSDC token minted by AAVE to the bridge when supplying USDC
    IERC20 public immutable aUSDC;

    /// @notice Whether new supplies to AAVE are permitted (withdrawals always allowed)
    bool public aaveEnabled;

    /// @notice Principal currently supplied to AAVE (book value, excludes yield)
    uint256 public suppliedPrincipal;

    /// @notice Fraction of the treasury to keep liquid as USDC, in basis points.
    ///         e.g. 500 = 5% of `treasuryBalance` stays as plain USDC to serve small
    ///         withdrawals without a round-trip through AAVE.
    uint256 public liquidReserveBps;

    /// @notice Address authorised to manage AAVE routing & harvest yield
    address public owner;
    /// @notice Two-step ownership. Set by `transferOwnership`; takes
    ///         effect only after `acceptOwnership` from this address.
    address public pendingOwner;

    /// @notice Address that receives harvested yield (defaults to owner)
    address public yieldRecipient;

    // Reentrancy guard (avoid pulling in OZ for a single uint256)
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _reentrancyStatus;

    // ---------------------------------------------------------------------
    // Storage: AN→ETH state (Phase 4 verifyBlock)
    // ---------------------------------------------------------------------

    /// @notice Circuit 1A (Primary attestation) verifier, consumed through the
    ///         `IPrimaryVerifier` interface (production backend: Halo2 SHPLONK
    ///         aggregator adapter `PrimaryAggregatorVerifier`).
    ///         May be `address(0)` if AN→ETH verification is disabled at
    ///         deployment; in that case `verifyBlock` reverts with `VerifyBlockDisabled`.
    IPrimaryVerifier public immutable primaryVerifier;

    /// @notice Circuit 1B (Fallback attestation) verifier, consumed through the
    ///         `IFallbackVerifier` interface (production backend: Halo2 SHPLONK
    ///         aggregator adapter `FallbackAggregatorVerifier`).
    ///         May be `address(0)` (see `primaryVerifier`).
    IFallbackVerifier public immutable fallbackVerifier;

    /// @notice Circuit 2 (Layer hashes movement) verifier, consumed through the
    ///         `ILayerHashesMovementVerifier` interface (production backend:
    ///         Halo2 SHPLONK aggregator adapter `LayerHashesAggregatorVerifier`).
    ///         May be `address(0)` (see `primaryVerifier`).
    ILayerHashesMovementVerifier public immutable layerHashesVerifier;

    /// @notice Active Acki Nacki BK-set Poseidon commitment.
    ///         Updated by `applyBkSetUpdate` after attestation + Merkle checks;
    ///         seeded from the constructor's `_genesisBkSetCommitment`.
    uint256 public storedBkSetCommitment;

    /// @notice Highest AN block sequence number whose BK-set rotation has been
    ///         applied on-chain via `applyBkSetUpdate`. Independent from
    ///         `storedLastSeenBlockSeqNo` (layer-bundle cursor).
    uint64 public storedLastBkSetUpdateSeqNo;

    /// @notice Highest AN block sequence number whose attestation has been
    ///         verified on-chain. Strictly monotonic via `verifyBlock`.
    uint64 public storedLastSeenBlockSeqNo;

    /// @notice Immutable genesis seed for the layer-hash chain anchor. Set
    ///         once at construction from `VerifyBlockConfig.genesisPrevMaxLevelLayerHash`
    ///         and never mutated post-deploy.
    /// @dev **Role narrowed in storage v2.0** (was: per-block max-layer cache
    ///      updated by every `verifyBlock`; now: immutable genesis seed only).
    ///      Still load-bearing — `_expectedPrevAnchor` reads it as the
    ///      pre-first-block bootstrap value, before any layer window is
    ///      populated. Every subsequent call sources the anchor from the
    ///      per-layer rolling windows in `_layerWindows` (see AB-Q4 /
    ///      `_expectedPrevAnchor`). For per-block per-layer state use
    ///      `getLatestPerLayer()`; for the chain anchor the next
    ///      `verifyBlock` will require use `expectedPrevAnchor(numLayers)`.
    ///
    ///      Storage v2.0 (2026-08-04): removed the hot-path SSTORE and made
    ///      this immutable; also removed the sibling `storedNumLayers` and
    ///      `storedLayerHashes[10]` flat cache — indexers migrate to
    ///      `getLatestPerLayer()` and `_highestActiveLayer()`.
    uint256 public immutable storedPrevMaxLevelLayerHash;

    // ---------------------------------------------------------------------
    // Storage: Circuit 4 (Bridge Withdrawal) — AN→ETH payout
    // ---------------------------------------------------------------------

    /// @notice Circuit 4 verifier, consumed through the
    ///         `IBridgeWithdrawalVerifier` interface (10-input
    ///         layout; production backend: Halo2 SHPLONK aggregator adapter).
    ///         May be `address(0)` if AN→ETH payout verification is
    ///         disabled at deployment; in that case `withdrawByProof` reverts
    ///         with `WithdrawByProofDisabled`. Independent of `verifyBlock`.
    IBridgeWithdrawalVerifier public immutable bridgeWithdrawalVerifier;

    /// @notice Fr-encoded AN-side bridge dApp identifier. Set at construction
    ///         and immutable. The Circuit 4 proof binds to a specific
    ///         `(dappFr, accFr)` pair via its public inputs — pinning these
    ///         on Ethereum at deploy time prevents a caller from substituting
    ///         an event emitted by a different AN contract (e.g. a malicious
    ///         lookalike `eccUSDCBridge`).
    uint256 public immutable bridgeWithdrawalDappFr;

    /// @notice Fr-encoded AN-side bridge account identifier (see `bridgeWithdrawalDappFr`).
    uint256 public immutable bridgeWithdrawalAccFr;

    /// @notice See `BridgeWithdrawConfig.altDstChainId`.
    uint256 public immutable bridgeWithdrawalAltDstChainId;

    /// @notice EVM `block.chainid` on which `altDstChainId` is honoured.
    ///         Zero disables the alias even when `altDstChainId` is set.
    uint256 public immutable bridgeWithdrawalAltDstHostChainId;

    /// @notice See `BridgeWithdrawConfig.altTokenId`.
    uint256 public immutable bridgeWithdrawalAltTokenId;

    /// @notice Replay-protection store. Keyed by `bytes32(nullifier)` from
    ///         the proof's public input slot [8]. The Circuit 4 nullifier is
    ///         `Poseidon(block_id_fr, tokenId, amount, recipientHi,
    ///         recipientLo, senderAccFr)` — uniqueness per event
    ///         is enforced inside the circuit, but the bridge still needs
    ///         the mapping to reject *re-submission* of an already-paid proof.
    ///         Keys must be canonical Fr (`nullifier < BN254_R`); the SHPLONK
    ///         Yul verifier reduces instances `mod BN254_R` (same modulus,
    ///         spelled `f_q` in the auto-generated Yul), so an unreduced
    ///         `N + k·BN254_R` would otherwise be a second mapping key for
    ///         the same field element.
    mapping(bytes32 => bool) private _nullifiers;

    /// @notice Set of per-layer rolling windows populated by `verifyBlock`.
    ///         Each `withdrawByProof` checks `finalRoot` against *any* layer
    ///         window via `_isKnownAnchor` (NB-Q1 2026-08-04 — was previously
    ///         pinned to L1 via a `WITHDRAW_ANCHOR_LAYER` constant that would
    ///         `revert UnknownAnchor` for every partner L≥2 witness). Every
    ///         window entry was written by a verified `verifyBlock`, so the
    ///         layer index adds specificity, not security. Option A (Circuit 4
    ///         PI slot `anchorLayer` + range-checked scan of the specific
    ///         window) remains the ultimate target once the Circuit 4
    ///         re-keygen lands.
    struct HistoryWindow {
        uint256[HISTORY_PROOF_WINDOW] data;
        /// @dev Not read by any on-chain check (`lastHeight` is the
        ///      monotonicity guard). Written so `getLayerWindow` can
        ///      resurrect the relayer `BridgeState` mirror. Dropping the
        ///      SSTORE would save ~29k gas on a ten-layer `verifyBlock`
        ///      and break that bootstrap.
        uint64[HISTORY_PROOF_WINDOW] heights;
        uint16 dataLen;
        uint16 writeCursor;
        uint64 lastHeight;
    }

    mapping(uint8 => HistoryWindow) private _layerWindows;

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    /// @notice Emitted on every deposit. `anAccount` alone is the Acki Nacki
    ///         destination: it is carried as ZK public inputs and credited on the
    ///         AN side (the EVM `sender` is kept only for provenance, since a
    ///         20-byte EVM address is not a valid AN recipient). `anWorkchain` is
    ///         inert — see `deposit` — and is emitted only so the event shape
    ///         stays stable for indexers.
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );

    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event WithdrawnFromAave(uint256 amountRequested, uint256 amountReceived);
    event YieldHarvested(address indexed recipient, uint256 amount);
    event AaveEnabledSet(bool enabled);
    event LiquidReserveBpsSet(uint256 bps);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    /// @notice Owner nominated `newOwner`; they must `acceptOwnership`.
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event YieldRecipientSet(address indexed recipient);
    event EmergencyWithdrawAll(uint256 amount);
    /// @notice Owner skimmed liquid USDC above `treasuryBalance` (post-emergency
    ///         yield / over-collateral) to `yieldRecipient` (QC-A1-3).
    event ExcessUsdcSkimmed(address indexed recipient, uint256 amount);

    /// @notice Emitted on every successful `verifyBlock` call.
    /// @param blockId AN block identifier (Merkle root committed by both proofs).
    /// @param blockSeqNo AN block sequence number whose attestation was verified.
    /// @param finType Primary (0) / Fallback (1) finalization path.
    /// @param numLayers Number of active layer slots in the new commitment.
    event BlockVerified(
        uint256 indexed blockId,
        uint64 indexed blockSeqNo,
        FinalizationType finType,
        uint8 numLayers
    );

    /// @notice Emitted whenever a layer anchor is appended by `verifyBlock`.
    ///         Carries the per-layer `(layer, hashValue, blockHeight)` so an
    ///         indexer can reconstruct each layer's rolling window (spec §8.3).
    event LayerAnchorAppended(uint8 indexed layer, uint256 hashValue, uint64 blockHeight);

    /// @notice Emitted when a BK-set rotation is applied via `applyBkSetUpdate`.
    event BkSetUpdated(
        uint256 indexed oldCommitment, uint256 indexed newCommitment, uint64 indexed blockSeqNo
    );

    /// @notice Emitted on every successful `withdrawByProof` call (Circuit 4),
    ///         A verified ZK proof releases
    ///         `amount` USDC to `recipient` exactly once (replay-protected by
    ///         `nullifier`).
    /// @param nullifier The Poseidon-derived nullifier from public input slot [8];
    ///        also the key in the `_nullifiers` mapping.
    /// @param recipient The 20-byte EVM address reconstructed from
    ///        `(recipientHi, recipientLo)`.
    /// @param amount Amount of USDC transferred to `recipient`.
    /// @param tokenId Token id from the event body (only `tokenId == 0` =
    ///        bridged USDC is currently supported; non-zero reserved for
    ///        multi-token wiring in a future milestone).
    /// @param submitter `msg.sender` of the `withdrawByProof` call (typically
    ///        a relayer; the payout goes to `recipient`, not `submitter`).
    event WithdrawalByProofExecuted(
        uint256 indexed nullifier,
        address indexed recipient,
        uint256 amount,
        uint256 indexed tokenId,
        address submitter
    );

    // ---------------------------------------------------------------------
    // Errors
    // ---------------------------------------------------------------------

    error InvalidAmount();
    error InvalidUsdc();
    error TransferFromFailed();
    /// @notice `transferFrom` returned true but custody did not grow
    ///         by `amount` (fee-on-transfer / rebasing). Fail closed.
    error TransferAmountMismatch();
    error DepositTooLarge();
    /// @notice Acki Nacki destination account was zero. A valid AN recipient
    ///         (256-bit TVM account) must be supplied at deposit time.
    error InvalidAnAccount();
    error InsufficientTreasury();
    /// @notice Zero EVM payout address. `withdrawByProof` rejects a reconstructed
    ///         `address(0)` before verify (WD-Q2). Do not remove — AN
    ///         `initiateWithdrawal` also rejects empty recipient (QC-AN-10).
    error InvalidRecipient();
    error InvalidOracle();
    error InvalidAaveAddress();
    error NotOwner();
    error Reentrancy();
    error AaveDisabled();
    error ReserveBpsTooHigh();
    error NothingToSupply();
    error AaveWithdrawFailed(uint256 requested, uint256 received);
    /// @notice `emergencyWithdrawAll` asked AAVE for `type(uint256).max`
    ///         but aUSDC still remains. Zeroing `suppliedPrincipal` would let
    ///         `harvestYield` treat leftover principal as yield.
    error EmergencyLeftoverAToken(uint256 leftover);
    /// @notice USDC `approve` returned false (do not ignore the bool).
    error ApproveFailed();
    /// @notice `msg.sender` is not `pendingOwner`.
    error OwnershipNotPending();
    error NoYield();

    // verifyBlock errors
    error VerifyBlockDisabled();
    error AttestationProofRejected();
    error LayerHashesProofRejected();
    error BkSetCommitmentMismatch(uint256 supplied, uint256 stored);
    error BlockSeqNoNotMonotonic(uint64 supplied, uint64 stored);
    error PrevAnchorMismatch(uint256 supplied, uint256 stored);
    error InvalidNumLayers(uint256 numLayers);
    error LayerHashTailNonZero(uint256 index);
    /// @notice Active layer slot (`i < numLayers`) must be non-zero (QC-A2-3).
    error LayerHashActiveZero(uint256 index);
    /// @notice No skimable liquid USDC above `treasuryBalance` (QC-A1-3).
    error NoExcessUsdc();

    // applyBkSetUpdate errors
    error BkUpdateDisabled();
    error StaleBkSetCommitment(uint256 supplied, uint256 stored);
    error BkUpdateSeqNoNotMonotonic(uint64 supplied, uint64 stored);
    error BkUpdateMerkleMismatch(uint256 computedRoot, uint256 blockId);
    /// @notice A zero `newCommitmentL3` would force every later
    ///         `verifyBlock` to attest a zero BK-set commitment.
    error ZeroBkSetCommitment();

    // withdrawByProof (Circuit 4) errors
    error WithdrawByProofDisabled();
    error WithdrawalProofRejected();
    error NullifierAlreadyUsed(uint256 nullifier);
    /// @notice A public input used as a mapping/window key is not a canonical
    ///         BN254 Fr (`value >= BN254_R`). The SHPLONK Yul verifier reduces
    ///         instances `mod BN254_R` (same modulus, spelled `f_q` in the
    ///         auto-generated Yul), so `x` and `x + k·BN254_R` verify as the
    ///         same field element while remaining distinct `uint256` keys.
    error FieldElementOutOfRange(uint256 value);
    error DstChainIdMismatch(uint256 supplied, uint256 expected);
    error RecipientHalfOutOfRange(uint256 value);
    error WithdrawIdentityMismatch();
    /// @notice The proof's `finalRoot` (PI slot [9]) is not in the bridge's
    ///         set of known anchors. Either the proof was generated against
    ///         a state the bridge has not yet observed (caller should wait
    ///         for the matching `verifyBlock` to land), or the anchor is
    ///         forged.
    error UnknownAnchor(uint256 finalRoot);
    error LayerOutOfRange(uint8 layer);
    error NonMonotonicLayerHeight(uint64 supplied, uint64 last);
    /// @notice Withdrawal verifier was wired but the AN-side
    ///         `(dappFr, accFr)` identity slots are zero — no real proof
    ///         could ever bind to a zero-identity bridge.
    error InvalidBridgeWithdrawalIdentity();
    /// @notice Only `tokenId == 0` (bridged USDC) is currently supported.
    ///         Non-zero token ids are reserved for multi-token wiring in a
    ///         future milestone.
    error UnsupportedTokenId(uint256 tokenId);
    /// @notice Circuit 4 withdraw is wired but the verifyBlock triple is
    ///         not. Without `verifyBlock`, no anchors ever land and every
    ///         `withdrawByProof` reverts `UnknownAnchor` — a silently dead
    ///         payout path. Wire all three attestation/layer verifiers, or
    ///         disable withdraw too.
    error WithdrawRequiresVerifyBlock();
    /// @notice verifyBlock is all-or-nothing. Partial wiring (1 or 2 of
    ///         the three verifier addresses set) is rejected at construction.
    error PartialVerifyBlockWiring();
    error WithdrawTransferFailed(address recipient, uint256 amount);
    /// @notice Bridge holds less USDC than the proof asks for. Should be
    ///         unreachable in steady state because deposits flow into
    ///         `treasuryBalance` and AAVE-supplied principal is auto-pulled
    ///         on demand. Surfaced as a distinct error to make debugging
    ///         easier than `WithdrawTransferFailed`.
    error WithdrawTreasuryShortfall(uint256 requested, uint256 available);

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

    /// @notice Argument bundle for the AN→ETH `verifyBlock` wiring.
    /// @dev Stored on the stack at construction; never persisted as a struct.
    ///      Passing the zero address for *any* of the three verifiers disables
    ///      `verifyBlock` (it reverts with `VerifyBlockDisabled`) — useful for
    ///      legacy deployments that only exercise the deposit/AAVE surface.
    struct VerifyBlockConfig {
        IPrimaryVerifier primaryVerifier;
        IFallbackVerifier fallbackVerifier;
        ILayerHashesMovementVerifier layerHashesVerifier;
        /// @notice Initial BK-set Poseidon commitment. Required when verifier
        ///         addresses are non-zero (otherwise no proof would ever pass
        ///         the BK-set check). Pass `0` only if `verifyBlock` is disabled.
        uint256 genesisBkSetCommitment;
        /// @notice Initial layer-hash chain anchor. Pass `0` for genesis
        ///         (no prior layer-hash chain to anchor against — the very
        ///         first verified block uses the zero anchor).
        uint256 genesisPrevMaxLevelLayerHash;
        /// @notice Initial `storedLastSeenBlockSeqNo`. Must equal the AN-side
        ///         `last_seen_block_seqno` baked into the very first
        ///         verifyBlock proof (i.e. the bootstrap seed seq_no emitted by
        ///         `compute_bridge_anchors`). Pass `0` only if the first proof
        ///         will also carry `last_seen = 0`; otherwise the first
        ///         `verifyBlock` reverts with `AttestationProofRejected` because
        ///         instance[15] (the proof's baked-in `last_seen`) will not
        ///         match `storedLastSeenBlockSeqNo`.
        uint64 genesisLastSeenBlockSeqNo;
    }

    /// @notice Argument bundle for the AN→ETH Circuit 4
    ///         wiring. verifyBlock-only deployments are legal; withdraw-only
    ///         is not (see `WithdrawRequiresVerifyBlock`).
    /// @dev Passing `bridgeWithdrawalVerifier == address(0)` disables
    ///      `withdrawByProof` (it reverts with `WithdrawByProofDisabled`).
    ///      When enabled, `accFr` must be non-zero and the verifyBlock triple
    ///      must be fully wired so anchors can land.
    struct BridgeWithdrawConfig {
        IBridgeWithdrawalVerifier bridgeWithdrawalVerifier;
        /// @notice Fr-encoded AN-side bridge dApp identifier. May be zero on
        ///         shellnet (zero `dapp_id` deployments) when `accFr` is set.
        uint256 dappFr;
        /// @notice Fr-encoded AN-side bridge account identifier. Must be
        ///         non-zero when `bridgeWithdrawalVerifier` is non-zero.
        uint256 accFr;
        /// @notice Optional shellnet/testnet alias for `pub.dstChainId` when
        ///         the AN orchestrator uses a logical id (e.g. `1`) distinct
        ///         from `block.chainid`. Honoured only when
        ///         `block.chainid == altDstHostChainId`; zero `altDstChainId`
        ///         or zero `altDstHostChainId` disables the alias.
        uint256 altDstChainId;
        /// @notice Host EVM chain id (e.g. Sepolia `11155111`) where the
        ///         `altDstChainId` mapping is active. Prevents a shellnet proof
        ///         destined for logical chain `1` from replaying on Arbitrum or
        ///         mainnet even if those deployments misconfigure `altDstChainId`.
        uint256 altDstHostChainId;
        /// @notice Optional shellnet alias for `pub.tokenId` (e.g. AN
        ///         `USDC_ECC_ID = 3`). Zero accepts only `tokenId == 0`.
        ///         Production mainnet deploys must leave this zero
        ///         (see `DeployRealBridge`); not enforced here because
        ///         Foundry tests `vm.chainId(1)` to bind Circuit 4 `dstChainId`.
        uint256 altTokenId;
    }

    /// @param _blockHeaderOracle  Oracle for canonical Ethereum block hashes.
    ///                            Kept for future burn-proof anchoring; currently
    ///                            unused by the public surface but required at
    ///                            construction so re-deploys are unnecessary
    ///                            when the burn-proof flow lands.
    /// @param _usdc               USDC (ERC-20, 6 decimals) accepted for deposits.
    ///                            Sepolia testnet: Aave-faucet USDC
    ///                            `0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8`.
    /// @param _aavePool           AAVE V3 Pool address.
    /// @param _aUSDC              aUSDC token minted by AAVE for supplied USDC.
    /// @param _vb                 AN→ETH verifyBlock wiring (Phase 4). Pass all
    ///                            zeros to disable the AN→ETH path; the deposit/
    ///                            AAVE surface stays fully functional.
    /// @param _bw                 Circuit 4 wiring. Pass
    ///                            `BridgeWithdrawConfig({...address(0), 0, 0})`
    ///                            to disable. When `bridgeWithdrawalVerifier`
    ///                            is non-zero, both `dappFr` and `accFr`
    ///                            must be non-zero.
    /// @dev Pass address(0) for `_aavePool`/`_aUSDC` to disable AAVE.
    ///      `_usdc` must always be non-zero — deposits pull USDC via `transferFrom`.
    constructor(
        address _blockHeaderOracle,
        address _usdc,
        address _aavePool,
        address _aUSDC,
        VerifyBlockConfig memory _vb,
        BridgeWithdrawConfig memory _bw
    ) {
        if (_blockHeaderOracle == address(0)) revert InvalidOracle();
        if (_usdc == address(0)) revert InvalidUsdc();

        // Both AAVE addresses must be provided together — or none at all.
        bool aaveWired = _aavePool != address(0) || _aUSDC != address(0);
        bool aaveAllSet = _aavePool != address(0) && _aUSDC != address(0);
        if (aaveWired && !aaveAllSet) revert InvalidAaveAddress();

        blockHeaderOracle = IBlockHeaderOracle(_blockHeaderOracle);

        usdc = IERC20(_usdc);
        aavePool = IAavePool(_aavePool);
        aUSDC = IERC20(_aUSDC);

        // verifyBlock wiring is all-or-nothing.
        {
            bool p = address(_vb.primaryVerifier) != address(0);
            bool f = address(_vb.fallbackVerifier) != address(0);
            bool l = address(_vb.layerHashesVerifier) != address(0);
            if ((p || f || l) && !(p && f && l)) revert PartialVerifyBlockWiring();
            if (address(_bw.bridgeWithdrawalVerifier) != address(0)) {
                if (_bw.accFr == 0) revert InvalidBridgeWithdrawalIdentity();
                if (!(p && f && l)) revert WithdrawRequiresVerifyBlock();
                // Identity slots are compared raw against Yul-reduced
                // instances. A non-canonical value makes every withdrawal
                // revert permanently — same invariant as the genesis anchors.
                _requireCanonicalFr(_bw.dappFr);
                _requireCanonicalFr(_bw.accFr);
                _requireCanonicalFr(_bw.altTokenId);
            }
            // Genesis anchors enter the same slots `applyBkSetUpdate`
            // guards, so they answer to the same invariant. Without this a
            // non-canonical `genesisPrevMaxLevelLayerHash` is self-contradictory:
            // `_expectedPrevAnchor` hands it back while no layer has data, but
            // `verifyBlock` gates the argument through `_requireCanonicalFr`
            // first, so the very first block can never match it. Zero is legal
            // for the layer hash — a first block may genuinely carry zero — so
            // only canonicity is required there.
            if (p && f && l) {
                if (_vb.genesisBkSetCommitment == 0) revert ZeroBkSetCommitment();
                _requireCanonicalFr(_vb.genesisBkSetCommitment);
                _requireCanonicalFr(_vb.genesisPrevMaxLevelLayerHash);
            }
        }
        primaryVerifier = _vb.primaryVerifier;
        fallbackVerifier = _vb.fallbackVerifier;
        layerHashesVerifier = _vb.layerHashesVerifier;
        storedBkSetCommitment = _vb.genesisBkSetCommitment;
        storedPrevMaxLevelLayerHash = _vb.genesisPrevMaxLevelLayerHash;
        storedLastSeenBlockSeqNo = _vb.genesisLastSeenBlockSeqNo;
        bridgeWithdrawalVerifier = _bw.bridgeWithdrawalVerifier;
        bridgeWithdrawalDappFr = _bw.dappFr;
        bridgeWithdrawalAccFr = _bw.accFr;
        bridgeWithdrawalAltDstChainId = _bw.altDstChainId;
        bridgeWithdrawalAltDstHostChainId = _bw.altDstHostChainId;
        bridgeWithdrawalAltTokenId = _bw.altTokenId;

        owner = msg.sender;
        yieldRecipient = msg.sender;
        aaveEnabled = aaveAllSet;
        liquidReserveBps = 1_000; // default 10% liquid reserve
        _reentrancyStatus = _NOT_ENTERED;

        emit OwnershipTransferred(address(0), msg.sender);
    }

    // ---------------------------------------------------------------------
    // User-facing: deposit
    // ---------------------------------------------------------------------

    /// @notice Deposit USDC to be bridged to Acki Nacki.
    /// @dev Caller must `approve` this contract for `amount` before calling.
    ///      Emits a `Deposit` event that is later proven by a ZK circuit.
    ///      Funds stay as USDC in this contract; a keeper supplies them to AAVE
    ///      in batches via `supplyToAave()`.
    /// @param amount      USDC amount (6 decimals) to bridge.
    /// @param anWorkchain Inert. Kept in the ABI and the event for compatibility,
    ///                    but Acki Nacki ignores it: the workchain concept is
    ///                    retired there and `dappId` replaced it (2026-06-02),
    ///                    so the recipient always lives in workchain 0. It is
    ///                    not a public input of the deposit proof. Not
    ///                    range-checked, because no supported set exists to
    ///                    check against — a wrong value changes nothing.
    ///                    The destination that
    ///                    does matter is `anAccount` below.
    /// @param anAccount   Acki Nacki destination account (256-bit TVM address).
    ///                    Must be non-zero (`InvalidAnAccount`). AN
    ///                    `finalizeDeposit` / `confirmDeposit` also reject zero
    ///                    (`ERR_ZERO_RECIPIENT`, QC-AN-10). Keep this ETH
    ///                    fail-fast — do not drop it. Carried as ZK public
    ///                    inputs; an EVM address cannot be an AN recipient.
    ///                    A wrong non-zero destination is one-way (no refund).
    function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) external nonReentrant {
        if (amount == 0) revert InvalidAmount();
        if (amount > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();
        if (anAccount == bytes32(0)) revert InvalidAnAccount();
        uint256 credited = _pullExactUsdc(amount);

        uint256 depositId = depositCounter++;
        treasuryBalance += credited;

        emit Deposit(depositId, msg.sender, amount, anWorkchain, anAccount, block.timestamp);
    }

    // ---------------------------------------------------------------------
    // AN→ETH state — verifyBlock (permissionless)
    // ---------------------------------------------------------------------

    /// @notice Advance the on-chain commitment to the Acki Nacki side after
    ///         verifying a tuple of two cross-circuit-bound ZK proofs.
    ///
    /// The two proofs MUST share `block_id` and `bkSetCommitment` (the partner's
    /// circuits already enforce those equalities at proof-generation time; this
    /// function relies on the ABI passing a single value to both verifiers).
    ///
    /// @dev Permissionless — anyone can submit; the contract only mutates state
    ///      after both verifiers (Halo2 SHPLONK aggregator adapters in
    ///      production) report success and every cross-circuit / monotonicity /
    ///      chain-anchor invariant holds.
    ///
    /// Invariants enforced (revert-on-violation):
    ///   - `bkSetCommitment == storedBkSetCommitment`           (BK-set anchor; rotated only by Phase 1.C Circuit 3 in future)
    ///   - `blockSeqNo > storedLastSeenBlockSeqNo`              (strictly monotonic)
    ///   - `prevMaxLevelLayerHash == _expectedPrevAnchor(numLayers)` (chain anchor — per-layer pick, guards against fork & replay)
    ///   - `1 <= numLayers <= MAX_LAYER_HASHES`                 (shape)
    ///   - `layerHashes[i] == 0` for `i >= numLayers`           (tail must be zero — defends against
    ///                                                            silent garbage in unused slots)
    ///   - `primaryVerifier` / `fallbackVerifier` / `layerHashesVerifier` all reject the proofs
    ///     ⇒ revert (no partial state mutation).
    ///
    /// State updates after success:
    ///   - `storedLastSeenBlockSeqNo = blockSeqNo`
    ///   - Per-layer rolling windows: for each `L in 1..=numLayers` with
    ///     `layerHashes[L-1] != 0`, `_layerWindows[L].append(layerHashes[L-1], blockSeqNo)`.
    ///     These are the authoritative per-layer state; observe via
    ///     `getLatestPerLayer()` / `isKnownLayerAnchor(L, hash)` and derive
    ///     the next block's expected anchor via `expectedPrevAnchor(numLayers)`.
    ///
    /// **Storage v2.0 (2026-08-04)**: the flat `storedNumLayers` +
    /// `storedLayerHashes[10]` cache and the `storedPrevMaxLevelLayerHash`
    /// SSTORE are no longer written on the hot path (SSTORE savings ≈ 32k gas
    /// per call). `storedPrevMaxLevelLayerHash` is now the immutable genesis
    /// seed. See `docs/EVM-contracts-spec.md` §4.
    ///   - each non-zero `layerHashes[i]` appended to its layer's rolling window
    ///
    /// @param finType            Primary or Fallback finalization path.
    /// @param attestationProof   Proof bytes accepted by `IPrimaryVerifier` /
    ///                           `IFallbackVerifier` for Circuit 1A or 1B
    ///                           (Halo2 SHPLONK aggregator proof in production).
    /// @param layerHashesProof   Proof bytes accepted by
    ///                           `ILayerHashesMovementVerifier` for Circuit 2
    ///                           (Halo2 SHPLONK aggregator proof in production).
    /// @param blockId            32-byte AN block identifier shared between both proofs.
    /// @param bkSetCommitment    Poseidon commitment to the active BK set; shared between both proofs.
    /// @param blockSeqNo         AN block sequence number being attested.
    /// @param numLayers          Number of active layer slots (1..=MAX_LAYER_HASHES).
    /// @param layerHashes        10 layer-hash field elements; tail (>= numLayers) must be zero.
    /// @param prevMaxLevelLayerHash Chain anchor — must equal `_expectedPrevAnchor(numLayers)`
    ///                           (the per-layer pick; query the `expectedPrevAnchor` view).
    function verifyBlock(
        FinalizationType finType,
        bytes calldata attestationProof,
        bytes calldata layerHashesProof,
        uint256 blockId,
        uint256 bkSetCommitment,
        uint64 blockSeqNo,
        uint8 numLayers,
        uint256[MAX_LAYER_HASHES] calldata layerHashes,
        uint256 prevMaxLevelLayerHash
    ) external nonReentrant {
        // Feature gate: all three verifier slots must be wired.
        if (
            address(primaryVerifier) == address(0) || address(fallbackVerifier) == address(0)
                || address(layerHashesVerifier) == address(0)
        ) {
            revert VerifyBlockDisabled();
        }

        // ---- Shape & range checks (cheap; before crypto). ----
        if (numLayers == 0 || numLayers > MAX_LAYER_HASHES) {
            revert InvalidNumLayers(numLayers);
        }
        for (uint256 i = numLayers; i < MAX_LAYER_HASHES; i++) {
            if (layerHashes[i] != 0) revert LayerHashTailNonZero(i);
        }
        // Active slots must be non-zero so `_appendLayerHashes` cannot skip a
        // layer and desync per-layer windows / `_highestActiveLayer` (QC-A2-3).
        for (uint256 i = 0; i < numLayers; i++) {
            if (layerHashes[i] == 0) revert LayerHashActiveZero(i);
            _requireCanonicalFr(layerHashes[i]);
        }
        _requireCanonicalFr(prevMaxLevelLayerHash);
        // `blockId` is only emitted, but Yul still reduces the instance
        // mod BN254_R. Reject unreduced words so logs match AN.
        _requireCanonicalFr(blockId);

        // ---- Anchor checks against stored state. ----
        if (bkSetCommitment != storedBkSetCommitment) {
            revert BkSetCommitmentMismatch(bkSetCommitment, storedBkSetCommitment);
        }
        // Strictly greater is enough: gaps (seq_no fast-forward) are permitted
        // so a relayer can catch up with a later valid proof. A jump does not
        // mass-evict the window — each call still appends one slot.
        // Sequential `last_seen+1` is an off-chain relayer policy, not an
        // on-chain cap (see `test_relayerLoop_seqNoFastForward_isPermittedByContract`).
        if (blockSeqNo <= storedLastSeenBlockSeqNo) {
            revert BlockSeqNoNotMonotonic(blockSeqNo, storedLastSeenBlockSeqNo);
        }
        // Chain anchor — derived PER LAYER from the rolling windows, mirroring
        // the partner prover's `BridgeState::prev_max_level_layer_hash_for`.
        // A flat `layerHashes[numLayers - 1]` anchor diverges from the prover
        // whenever `numLayers` *decreases* across consecutive key blocks, which
        // would halt `verifyBlock` forever (AB-Q4). See `_expectedPrevAnchor`.
        //
        // Scoped so `expectedAnchor` is freed before the tail `emit`, keeping
        // this function's live-stack-slot count under 16 so it also compiles
        // under `forge coverage` (which runs without the optimizer / `--via-ir`).
        {
            uint256 expectedAnchor = _expectedPrevAnchor(numLayers);
            if (prevMaxLevelLayerHash != expectedAnchor) {
                revert PrevAnchorMismatch(prevMaxLevelLayerHash, expectedAnchor);
            }
        }

        // ---- Crypto: verify both proofs. The shared (blockId, bkSetCommitment,
        //      blockSeqNo) values flow into both verifier calls, so any
        //      mismatch between the two proofs surfaces here as one of the two
        //      verifications failing (their public inputs are computed from
        //      these values byte-for-byte).
        //
        //      Each verification is in its own scope so the `bool` result is
        //      freed before the tail `emit` (same stack-slot reason as above).
        {
            bool attOk;
            if (finType == FinalizationType.Primary) {
                attOk = primaryVerifier.verifyPrimaryAttestation(
                    attestationProof,
                    blockId,
                    bkSetCommitment,
                    uint256(blockSeqNo),
                    uint256(storedLastSeenBlockSeqNo)
                );
            } else {
                attOk = fallbackVerifier.verifyFallbackAttestation(
                    attestationProof,
                    blockId,
                    bkSetCommitment,
                    uint256(blockSeqNo),
                    uint256(storedLastSeenBlockSeqNo)
                );
            }
            if (!attOk) revert AttestationProofRejected();
        }

        {
            bool lhOk = layerHashesVerifier.verifyLayerHashesMovement(
                layerHashesProof,
                blockId,
                bkSetCommitment,
                uint256(numLayers),
                layerHashes,
                prevMaxLevelLayerHash
            );
            if (!lhOk) revert LayerHashesProofRejected();
        }

        // ---- Effects (CEI): commit the new state. ----
        // Storage v2.0 (2026-08-04): the flat `storedNumLayers` + `storedLayerHashes[10]`
        // cache and the `storedPrevMaxLevelLayerHash` SSTORE are gone from the
        // hot path — the authoritative per-layer state lives in `_layerWindows`
        // and is written exclusively by `_appendLayer` below. Off-chain readers
        // migrate to `getLatestPerLayer()` / `_highestActiveLayer()` /
        // `expectedPrevAnchor(numLayers)`. See `docs/EVM-contracts-spec.md` §4.
        storedLastSeenBlockSeqNo = blockSeqNo;
        _appendLayerHashes(numLayers, layerHashes, blockSeqNo);

        emit BlockVerified(blockId, blockSeqNo, finType, numLayers);
    }

    /// @notice Apply an Acki Nacki BK-set rotation on Ethereum after verifying
    ///         a Circuit 1A/1B attestation and an open SHA-256 Merkle binding
    ///         `blockId == SHA256(SHA256(SHA256(SHA256(H01 ‖ SHA256(L2 ‖ L3)) ‖ H4_7) ‖ H8_15))`
    ///         against the depth-4 / 16-leaf block-id tree (`(L2, L3)` sit at
    ///         leaf positions 2 and 3).
    ///
    /// @dev Permissionless. Only the Poseidon **commitment** rotates on-chain;
    ///      the full pubkey table stays off-chain (prover working set).
    ///
    ///      `blockId` is the root of the canonical **16-leaf, depth-4**
    ///      block-id tree (`poseidon_profile_new`; leaves L2/L3 carry the old
    ///      and new BK-set Poseidon commitments). Opening that pair therefore
    ///      needs **three** siblings, and the fold is:
    ///
    ///      ```
    ///      h23  = SHA256(L2 ‖ L3)              // both in LE `Fr` repr
    ///      h0_3 = SHA256(siblingH01  ‖ h23)
    ///      h0_7 = SHA256(h0_3        ‖ siblingH4_7)
    ///      root = SHA256(h0_7        ‖ siblingH8_15)
    ///      blockId == root mod BN254_R         // canonical `Fr` image
    ///      ```
    ///
    ///      Mirrors `bridge-prover-lib/src/block_id_tree.rs`
    ///      (`siblings_for_l2_l3`) and the off-chain pre-flight in
    ///      `bridge-verifier-daemon`. The pre-16-leaf variant took two
    ///      siblings and folded one level less.
    ///
    /// @param finType Primary or Fallback attestation path for the update block.
    /// @param attestationProof SHPLONK attestation proof bytes.
    /// @param blockId Block identifier shared with the attestation public inputs,
    ///        so it is the canonical `Fr` image of the tree root (`root mod
    ///        BN254_R`) rather than the raw SHA-256 root — the same convention
    ///        `verifyBlock` uses.
    /// @param blockSeqNo Sequence number of the BK-update block (monotonic cursor).
    /// @param oldCommitmentL2 Must equal `storedBkSetCommitment`.
    /// @param newCommitmentL3 New BK-set Poseidon commitment after rotation.
    /// @param siblingH01 Merkle sibling `SHA256(L0 ‖ L1)` — depth-1 pair hash.
    /// @param siblingH4_7 Merkle sibling `SHA256(SHA256(L4 ‖ L5) ‖ SHA256(L6 ‖ L7))` — depth-2 quad hash.
    /// @param siblingH8_15 Merkle sibling covering leaves 8..15 — depth-3 oct hash.
    function applyBkSetUpdate(
        FinalizationType finType,
        bytes calldata attestationProof,
        uint256 blockId,
        uint64 blockSeqNo,
        uint256 oldCommitmentL2,
        uint256 newCommitmentL3,
        bytes32 siblingH01,
        bytes32 siblingH4_7,
        bytes32 siblingH8_15
    ) external nonReentrant {
        if (address(primaryVerifier) == address(0) || address(fallbackVerifier) == address(0)) {
            revert BkUpdateDisabled();
        }

        if (oldCommitmentL2 != storedBkSetCommitment) {
            revert StaleBkSetCommitment(oldCommitmentL2, storedBkSetCommitment);
        }
        if (blockSeqNo <= storedLastBkSetUpdateSeqNo) {
            revert BkUpdateSeqNoNotMonotonic(blockSeqNo, storedLastBkSetUpdateSeqNo);
        }
        if (newCommitmentL3 == 0) revert ZeroBkSetCommitment();
        // Stored commitment and attestation `blockId` must be canonical Fr.
        // Unreduced `newCommitmentL3` would otherwise land in
        // `storedBkSetCommitment` while adapters compare raw words.
        _requireCanonicalFr(blockId);
        _requireCanonicalFr(newCommitmentL3);

        // lastSeen is the live layer cursor, not `storedLastBkSetUpdateSeqNo`
        // (monotonicity only). The prover must bake the same word.
        bool attOk;
        if (finType == FinalizationType.Primary) {
            attOk = primaryVerifier.verifyPrimaryAttestation(
                attestationProof,
                blockId,
                oldCommitmentL2,
                uint256(blockSeqNo),
                uint256(storedLastSeenBlockSeqNo)
            );
        } else {
            attOk = fallbackVerifier.verifyFallbackAttestation(
                attestationProof,
                blockId,
                oldCommitmentL2,
                uint256(blockSeqNo),
                uint256(storedLastSeenBlockSeqNo)
            );
        }
        if (!attOk) revert AttestationProofRejected();

        // Depth-4 / 16-leaf block-id tree with (oldL2, newL3) at leaf positions 2 and 3.
        //   round 1: h23   = SHA(L2 ‖ L3)              — depth-1 pair hash
        //   round 2: h0_3  = SHA(siblingH01 ‖ h23)     — depth-2 quad hash (leaves 0..3)
        //   round 3: h0_7  = SHA(h0_3 ‖ siblingH4_7)   — depth-3 oct hash  (leaves 0..7)
        //   round 4: root  = SHA(h0_7 ‖ siblingH8_15)  — depth-4 root      (leaves 0..15)
        //
        // L2 and L3 are numeric `uint256` Fr scalars on the wire; the AN side
        // (bridge-prover-lib `block_id_tree.rs:30-31`) hashes them as canonical
        // 32-byte little-endian `Fr::to_repr()`. Solidity's default
        // `abi.encodePacked(uint256)` is big-endian, so we byte-reverse both
        // operands with `_frToLeBytes` before the round-1 SHA. Siblings at
        // rounds 2..4 are opaque SHA-256 outputs (already `bytes32`) and need
        // no reversal.
        bytes32 h23 = sha256(
            abi.encodePacked(_frToLeBytes(oldCommitmentL2), _frToLeBytes(newCommitmentL3))
        );
        bytes32 h0_3 = sha256(abi.encodePacked(siblingH01, h23));
        bytes32 h0_7 = sha256(abi.encodePacked(h0_3, siblingH4_7));
        bytes32 root = sha256(abi.encodePacked(h0_7, siblingH8_15));
        // The fold produces a raw 256-bit SHA-256 output, but `blockId` was
        // just handed to the attestation adapter, which compares it byte-for-byte
        // against the circuit's public instance — necessarily a canonical `Fr`,
        // i.e. `< BN254_R`. Only ~18.9% of 256-bit values are, so without this
        // reduction the two consumers of `blockId` disagree for roughly four out
        // of five rotations and no argument can satisfy both at once. Reducing
        // here keeps `blockId` meaning one thing everywhere (the field element
        // the circuit committed to, same as in `verifyBlock`) and leaves the
        // raw root confined to this fold.
        uint256 rootFr = uint256(root) % BN254_R;
        if (rootFr != blockId) {
            revert BkUpdateMerkleMismatch(rootFr, blockId);
        }

        storedBkSetCommitment = newCommitmentL3;
        storedLastBkSetUpdateSeqNo = blockSeqNo;

        emit BkSetUpdated(oldCommitmentL2, newCommitmentL3, blockSeqNo);
    }

    /// @dev Little-endian 32-byte image of a BN254 `Fr` — i.e. what
    ///      `Fr::to_repr()` produces on the Rust side.
    ///
    ///      The Acki Nacki block-id tree hashes BK-set Poseidon commitments in
    ///      that canonical LE repr, while every other on-chain use of a
    ///      commitment (`storedBkSetCommitment`, the attestation verifier's
    ///      public input) is the numeric field element. `applyBkSetUpdate` is
    ///      the single place where the two conventions meet, so the reversal
    ///      lives here and nowhere else. Deploy-time counterpart: the runbook
    ///      byte-reverses the prover's LE `bk_set_poseidon_hash_hex` to get
    ///      `GENESIS_BK_SET_COMMITMENT`.
    function _frToLeBytes(uint256 value) internal pure returns (bytes32) {
        uint256 reversed;
        for (uint256 i = 0; i < 32; i++) {
            reversed = (reversed << 8) | (value & 0xff);
            value >>= 8;
        }
        return bytes32(reversed);
    }

    /// @dev Append each non-zero layer hash from a successful `verifyBlock`
    ///      into the per-layer rolling windows (spec §8.3–8.4).
    function _appendLayerHashes(
        uint8 numLayers,
        uint256[MAX_LAYER_HASHES] calldata layerHashes,
        uint64 blockHeight
    ) internal {
        for (uint8 L = 1; L <= numLayers; L++) {
            uint256 hashValue = layerHashes[L - 1];
            if (hashValue != 0) {
                _appendLayer(L, hashValue, blockHeight);
            }
        }
    }

    /// @dev Append `hashValue` into layer `L`'s circular buffer.
    function _appendLayer(uint8 layer, uint256 hashValue, uint64 blockHeight) internal {
        if (layer == 0 || layer > MAX_LAYER_HASHES) {
            revert LayerOutOfRange(layer);
        }
        HistoryWindow storage w = _layerWindows[layer];
        if (blockHeight < w.lastHeight) {
            revert NonMonotonicLayerHeight(blockHeight, w.lastHeight);
        }

        w.data[w.writeCursor] = hashValue;
        w.heights[w.writeCursor] = blockHeight;
        w.writeCursor = uint16((uint256(w.writeCursor) + 1) % HISTORY_PROOF_WINDOW);
        if (w.dataLen < HISTORY_PROOF_WINDOW) {
            w.dataLen = w.dataLen + 1;
        }
        w.lastHeight = blockHeight;

        emit LayerAnchorAppended(layer, hashValue, blockHeight);
    }

    /// @dev O(W) membership test against layer `L`'s window.
    function _isKnownLayerAnchor(uint8 layer, uint256 hashValue) internal view returns (bool) {
        if (layer == 0 || layer > MAX_LAYER_HASHES) {
            return false;
        }
        HistoryWindow storage w = _layerWindows[layer];
        uint256 n = w.dataLen;
        for (uint256 i = 0; i < n; i++) {
            if (w.data[i] == hashValue) {
                return true;
            }
        }
        return false;
    }

    /// @dev Most-recently appended hash in layer `L`'s window, or 0 if empty.
    function _layerLatest(uint8 layer) internal view returns (uint256) {
        HistoryWindow storage w = _layerWindows[layer];
        if (w.dataLen == 0) {
            return 0;
        }
        uint256 lastIdx = (uint256(w.writeCursor) + HISTORY_PROOF_WINDOW - 1) % HISTORY_PROOF_WINDOW;
        return w.data[lastIdx];
    }

    /// @dev Highest 1-indexed layer that currently holds at least one hash, or
    ///      0 before any block is verified. Layers fill contiguously (layer L
    ///      only materialises once layers 1..L-1 exist), so this equals the
    ///      active-layer count `t` used by the partner prover's
    ///      `BridgeState::num_active_layers`.
    function _highestActiveLayer() internal view returns (uint8) {
        uint8 hi = 0;
        for (uint8 L = 1; L <= MAX_LAYER_HASHES; L++) {
            if (_layerWindows[L].dataLen > 0) {
                hi = L;
            }
        }
        return hi;
    }

    /// @dev Expected chain anchor for an incoming block that carries
    ///      `numLayers` non-empty layers — the exact mirror of the partner
    ///      prover's `BridgeState::prev_max_level_layer_hash_for`
    ///      (`bridge-prover-lib/src/bridge_state.rs`):
    ///        * `t = _highestActiveLayer()` (active-layer count);
    ///        * before any block (`t == 0`): the immutable genesis seed
    ///          (`storedPrevMaxLevelLayerHash`, set at construction — the
    ///          only remaining reader of the field after storage v2.0);
    ///        * otherwise `pick = min(numLayers, t)` and the anchor is the
    ///          latest hash of layer `pick`.
    ///      This is the AB-Q4 fix: a flat `layerHashes[numLayers - 1]` anchor
    ///      diverged from the prover whenever `numLayers` *decreased* between
    ///      consecutive key blocks (e.g. a 3-layer block followed by a 1-layer
    ///      block), permanently halting `verifyBlock`.
    function _expectedPrevAnchor(uint8 numLayers) internal view returns (uint256) {
        uint8 t = _highestActiveLayer();
        if (t == 0) {
            return storedPrevMaxLevelLayerHash; // immutable genesis bootstrap seed
        }
        uint8 pick = numLayers >= t ? t : numLayers;
        return _layerLatest(pick);
    }

    /// @notice The chain anchor a future `verifyBlock(.., numLayers, ..)` will
    ///         require as `prevMaxLevelLayerHash`. Relayers should thread this
    ///         exact value rather than guessing `layerHashes[numLayers - 1]` of
    ///         the previous block — it is the per-layer pick that mirrors the
    ///         prover's witness builder (`prev_max_level_layer_hash_for`).
    function expectedPrevAnchor(uint8 numLayers) external view returns (uint256) {
        return _expectedPrevAnchor(numLayers);
    }

    /// @dev Flat membership — true if `anchor` appears in any layer window.
    ///      This is the anchor check consumed by `withdrawByProof` (NB-Q1
    ///      2026-08-04): every window entry was written by a verified
    ///      `verifyBlock`, so the layer index adds specificity, not security.
    ///      Option A (Circuit 4 PI slot `anchorLayer` + range-checked scan
    ///      of the specific window) remains the ultimate target once the
    ///      Circuit 4 re-keygen lands.
    ///
    ///      **Soundness widening.** Dropping the layer index means the bridge
    ///      no longer asserts which layer a withdrawal is anchored in. A
    ///      Circuit 4 proof whose `finalRoot` equals a layer-2 window entry
    ///      is accepted even if the withdrawal event was intended to anchor
    ///      to layer 1 (or vice-versa). Correctness therefore rests entirely
    ///      on Circuit 4's own binding of `finalRoot` to the event — Option A
    ///      is what would restore per-layer specificity on-chain.
    ///
    ///      **Cost.** `_isKnownLayerAnchor` is O(W) with
    ///      `HISTORY_PROOF_WINDOW = 128`; this flat scan calls it for all
    ///      `MAX_LAYER_HASHES = 10` layers, so a miss is up to
    ///      `10 × 128 = 1280` cold SLOADs (~2.7M gas) — ~10× the single-
    ///      window scan it replaced — and is paid by the caller whose
    ///      `withdrawByProof` then reverts. An `mapping(uint256 => bool)`
    ///      written on append would give O(1) membership; the eviction on
    ///      window rollover must delete the map entry too, or the map
    ///      quietly becomes the unbounded bag the window was introduced to
    ///      avoid.
    function _isKnownAnchor(uint256 anchor) internal view returns (bool) {
        for (uint8 L = 1; L <= MAX_LAYER_HASHES; L++) {
            if (_isKnownLayerAnchor(L, anchor)) {
                return true;
            }
        }
        return false;
    }

    /// @notice Latest anchor written into each layer window. Entry `[L-1]`
    ///         is the head of `_layerWindows[L]`, i.e. the most recent
    ///         Poseidon Merkle root committed for layer `L` across all
    ///         `verifyBlock` calls so far — *not* just the last block's
    ///         array. Empty windows return zero.
    ///
    ///         Storage v2.0 (2026-08-04): replaces `getStoredLayerHashes()`
    ///         (removed). The per-layer view over `_layerWindows` is the
    ///         authoritative source; a shallow-successor-after-deep block no
    ///         longer overwrites deeper layers with zero. See
    ///         `docs/EVM-contracts-spec.md` §4.
    function getLatestPerLayer() external view returns (uint256[MAX_LAYER_HASHES] memory) {
        uint256[MAX_LAYER_HASHES] memory out;
        for (uint8 L = 1; L <= MAX_LAYER_HASHES; L++) {
            HistoryWindow storage w = _layerWindows[L];
            if (w.dataLen > 0) {
                uint16 head = (w.writeCursor + uint16(HISTORY_PROOF_WINDOW) - 1)
                    % uint16(HISTORY_PROOF_WINDOW);
                out[L - 1] = w.data[head];
            }
        }
        return out;
    }

    /// @notice O(1) helper: is `anchor` in the bridge's set of known anchors?
    ///         Cheap to call off-chain; mirrored inside `withdrawByProof`.
    function isKnownAnchor(uint256 anchor) external view returns (bool) {
        return _isKnownAnchor(anchor);
    }

    /// @notice Occupancy of layer `L`'s ring (`0..=HISTORY_PROOF_WINDOW`).
    ///         Saturates at 128 on first fill and stays there — it is not
    ///         headroom for a given `finalRoot`. Use `anchorRemainingAppends`.
    function layerWindowLen(uint8 layer) external view returns (uint16) {
        if (layer == 0 || layer > MAX_LAYER_HASHES) revert InvalidNumLayers(layer);
        return _layerWindows[layer].dataLen;
    }

    /// @notice Next write index of layer `L`'s ring (`0..=HISTORY_PROOF_WINDOW-1`).
    function layerWindowWriteCursor(uint8 layer) external view returns (uint16) {
        if (layer == 0 || layer > MAX_LAYER_HASHES) revert InvalidNumLayers(layer);
        return _layerWindows[layer].writeCursor;
    }

    /// @notice On which further `_appendLayer` call `anchor` is evicted from
    ///         `layer`: a return of N means the Nth append overwrites it, so the
    ///         anchor survives N-1. 0 = not in the window. When the ring is full
    ///         the oldest hash returns 1 — the next append takes it. Idle layers
    ///         never evict: remaining stays until that layer appends again.
    ///         Duplicate copies return the newest remaining.
    function anchorRemainingAppends(uint8 layer, uint256 anchor) external view returns (uint256) {
        if (layer == 0 || layer > MAX_LAYER_HASHES) revert InvalidNumLayers(layer);
        HistoryWindow storage w = _layerWindows[layer];
        uint256 n = w.dataLen;
        uint256 best = 0;
        for (uint256 i = 0; i < n; i++) {
            if (w.data[i] != anchor) continue;
            uint256 remaining =
                (i + HISTORY_PROOF_WINDOW - uint256(w.writeCursor)) % HISTORY_PROOF_WINDOW + 1;
            if (remaining > best) best = remaining;
        }
        return best;
    }

    /// @notice View helper: is `anchor` present in layer `L`'s rolling window?
    function isKnownLayerAnchor(uint8 layer, uint256 anchor) external view returns (bool) {
        return _isKnownLayerAnchor(layer, anchor);
    }

    /// @notice Full contents of layer `L`'s rolling window (data, heights, cursors).
    ///
    /// @dev Off-chain-only reader for daemon bootstrap / resurrect. Never
    ///      called on-chain (would be prohibitively gassy — returns
    ///      `HISTORY_PROOF_WINDOW * (32 + 8)` bytes plus scalars per call).
    ///      Used by the relayer daemon to reconstruct its `BridgeState`
    ///      mirror against an already-advanced contract — the scenario a
    ///      fresh install, a co-tester's daemon, or a mid-run machine
    ///      handoff hit when only `getLatestPerLayer()` was exposed
    ///      (heads-only). See
    ///      `crates/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md`
    ///      Case 6 (Chain-resurrect).
    ///
    ///      Struct return uses the ABI encoder v2 default in Solidity 0.8
    ///      and copies storage → memory; adds no hot-path cost since
    ///      `verifyBlock` does not touch this function.
    ///
    /// @param layer 1..=`MAX_LAYER_HASHES`. Zero or out-of-range reverts.
    /// @return  The layer's full `HistoryWindow` (unused slots read as zero).
    function getLayerWindow(uint8 layer) external view returns (HistoryWindow memory) {
        if (layer == 0 || layer > MAX_LAYER_HASHES) {
            revert LayerOutOfRange(layer);
        }
        return _layerWindows[layer];
    }

    // ---------------------------------------------------------------------
    // AN→ETH withdrawal payout — withdrawByProof (Circuit 4)
    // ---------------------------------------------------------------------

    /// @notice Mask for each half of a split-α `recipient` address (10 bytes = 80 bits).
    /// @dev Circuit 4 uses split-α (10/10) per
    ///      `bridge_event_prove_circuit::RECIPIENT_HI_*` / `RECIPIENT_LO_*`
    ///      offsets in the partner repo. The split is locked at deployment
    ///      via the immutable `bridgeWithdrawalVerifier` — its on-chain VK
    ///      only accepts proofs with the matching layout.
    uint256 private constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    /// @notice Pay out a withdrawal proven by a Circuit 4
    ///         Halo2 SHPLONK aggregator proof.
    ///
    /// Verifies that:
    ///   1. The proof witnesses a `WithdrawalInitiated(dstChainId, recipient,
    ///      amount, tokenId, sender)` event emitted by the AN-side TokenBridge
    ///      identified by `(bridgeWithdrawalDappFr, bridgeWithdrawalAccFr)`,
    ///      anchored — via the proof's dense-chain extension — to a
    ///      `finalRoot` the bridge has previously observed via `verifyBlock`
    ///      (`_isKnownAnchor(finalRoot) == true`).
    ///   2. `pub.dstChainId == block.chainid`, or — on shellnet E2E deploys
    ///      only — `pub.dstChainId == altDstChainId` while
    ///      `block.chainid == altDstHostChainId`. Cross-chain replay of the
    ///      same proof is rejected because nullifiers are per-contract *and*
    ///      `dstChainId` must match the executing chain (or its scoped alias).
    ///   3. `pub.dappFr == bridgeWithdrawalDappFr` and
    ///      `pub.accFr == bridgeWithdrawalAccFr` (defensive — also enforced
    ///      by the verifier under the same identity, but checked here so
    ///      the explicit `WithdrawIdentityMismatch` error surfaces before
    ///      the more opaque `WithdrawalProofRejected`).
    ///   4. `pub.nullifier` has not been used before (replay protection).
    ///   5. `pub.tokenId == 0` (only bridged USDC is currently supported;
    ///      non-zero token ids reserved for multi-token in a future milestone).
    ///   6. `pub.recipientHi` and `pub.recipientLo` both fit in 80 bits
    ///      (well-formedness check against malformed split inputs).
    ///
    /// State updates (CEI):
    ///   - **Effects**: mark `nullifier` used; decrement `treasuryBalance`.
    ///   - **Interactions**: (optional) `_pullFromAave(shortfall)` to top up
    ///     liquid USDC; `usdc.transfer(recipient, amount)` to pay out.
    ///
    /// @dev Permissionless. The caller pays gas but the payout goes to
    ///      `recipient` (reconstructed from `recipientHi`/`recipientLo`).
    ///      Typical caller is a relayer running `bridge-relayer-daemon`.
    ///
    /// @param proof   Circuit 4 proof bytes accepted by
    ///                `IBridgeWithdrawalVerifier` (Halo2 SHPLONK aggregator
    ///                proof in production).
    /// @param pub     Public-input slots [0..9]; see `IBridgeWithdrawalVerifier`.
    /// @return success Always `true` on a successful payout; reverts on failure.
    function withdrawByProof(
        bytes calldata proof,
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs calldata pub
    ) external nonReentrant returns (bool success) {
        if (address(bridgeWithdrawalVerifier) == address(0)) {
            revert WithdrawByProofDisabled();
        }

        // ---- Checks: identity, chain, replay, shape, anchor ----
        if (pub.dappFr != bridgeWithdrawalDappFr || pub.accFr != bridgeWithdrawalAccFr) {
            revert WithdrawIdentityMismatch();
        }
        bool dstOk = pub.dstChainId == block.chainid
            || (bridgeWithdrawalAltDstChainId != 0
                && bridgeWithdrawalAltDstHostChainId != 0
                && block.chainid == bridgeWithdrawalAltDstHostChainId
                && pub.dstChainId == bridgeWithdrawalAltDstChainId);
        if (!dstOk) {
            revert DstChainIdMismatch(pub.dstChainId, block.chainid);
        }
        bool tokenOk = pub.tokenId == 0
            || (bridgeWithdrawalAltTokenId != 0 && pub.tokenId == bridgeWithdrawalAltTokenId);
        if (!tokenOk) {
            revert UnsupportedTokenId(pub.tokenId);
        }
        if (pub.recipientHi > RECIPIENT_HALF_MASK) {
            revert RecipientHalfOutOfRange(pub.recipientHi);
        }
        if (pub.recipientLo > RECIPIENT_HALF_MASK) {
            revert RecipientHalfOutOfRange(pub.recipientLo);
        }
        // WD-Q2: reject recipient=0 before crypto / CEI so a stranded event
        // cannot burn gas on verify then strand forever on a real USDC reject.
        if (_reconstructRecipient(pub.recipientHi, pub.recipientLo) == address(0)) {
            revert InvalidRecipient();
        }
        // Yul reduces instances mod BN254_R; the mapping must not treat
        // N and N + k·BN254_R as distinct spent keys. Reject unreduced words
        // rather than reducing-and-keying (that would alias two caller-supplied keys).
        _requireCanonicalFr(pub.nullifier);
        _requireCanonicalFr(pub.finalRoot);
        bytes32 nullifierKey = bytes32(pub.nullifier);
        if (_nullifiers[nullifierKey]) {
            revert NullifierAlreadyUsed(pub.nullifier);
        }
        // NB-Q1 (2026-08-04): flat scan across every layer window. Option A
        // (Circuit 4 PI slot `anchorLayer` + range-checked scan of the
        // specific window) remains the ultimate target — this unblocks
        // partner L≥2 witnesses today without waiting for the Circuit 4
        // re-keygen. Every window entry was written by a verified
        // `verifyBlock`, so the layer index adds specificity, not security.
        if (!_isKnownAnchor(pub.finalRoot)) {
            revert UnknownAnchor(pub.finalRoot);
        }

        // ---- Crypto: verify the withdrawal proof. The 10 public inputs flow
        //      verbatim through the adapter; the anchor check above guards
        //      against a forged `finalRoot` that the circuit alone cannot
        //      bind to the bridge's view of AN state.
        bool ok = bridgeWithdrawalVerifier.verifyWithdrawal(proof, pub);
        if (!ok) revert WithdrawalProofRejected();

        // ---- Treasury check (must happen before the AAVE pull). ----
        if (pub.amount > treasuryBalance) {
            revert WithdrawTreasuryShortfall(pub.amount, treasuryBalance);
        }

        // ---- Effects (CEI: mutate state before any external call). ----
        _nullifiers[nullifierKey] = true;
        treasuryBalance -= pub.amount;

        // ---- Interactions ----
        // Top up liquid USDC from AAVE if the contract's plain USDC balance
        // is below the requested amount.
        uint256 liquid = usdc.balanceOf(address(this));
        if (liquid < pub.amount && suppliedPrincipal > 0) {
            uint256 shortfall = pub.amount - liquid;
            uint256 toPull = shortfall > suppliedPrincipal ? suppliedPrincipal : shortfall;
            _pullFromAave(toPull);
        }

        address recipient = _reconstructRecipient(pub.recipientHi, pub.recipientLo);
        _pushExactUsdc(recipient, pub.amount);

        emit WithdrawalByProofExecuted(
            pub.nullifier, recipient, pub.amount, pub.tokenId, msg.sender
        );
        return true;
    }

    /// @notice View helper: has `nullifier` already been consumed?
    /// @dev Useful for relayers / front-ends to skip re-submission of an
    ///      already-paid proof before paying gas for the verify call.
    function isNullifierUsed(uint256 nullifier) external view returns (bool) {
        return _nullifiers[bytes32(nullifier)];
    }

    /// @dev Revert unless `value` is a canonical BN254 Fr. Public inputs that
    ///      become mapping or window keys must match the Yul verifier's
    ///      `mod(calldataload, BN254_R)` image (the auto-generated Yul spells
    ///      this modulus `f_q`) — otherwise `x` and `x + k·BN254_R` verify as
    ///      one field element and occupy two keys.
    function _requireCanonicalFr(uint256 value) internal pure {
        if (value >= BN254_R) revert FieldElementOutOfRange(value);
    }

    /// @dev Credit exactly `amount` USDC. A fee-on-transfer or rebasing token
    ///      that moves a different custody delta reverts.
    function _pullExactUsdc(uint256 amount) internal returns (uint256 credited) {
        uint256 before = usdc.balanceOf(address(this));
        if (!usdc.transferFrom(msg.sender, address(this), amount)) {
            revert TransferFromFailed();
        }
        uint256 afterBal = usdc.balanceOf(address(this));
        if (afterBal < before || afterBal - before != amount) {
            revert TransferAmountMismatch();
        }
        return amount;
    }

    /// @dev Debit exactly `amount` USDC. Same FoT/rebase fail-closed.
    function _pushExactUsdc(address recipient, uint256 amount) internal {
        uint256 before = usdc.balanceOf(address(this));
        if (!usdc.transfer(recipient, amount)) {
            revert WithdrawTransferFailed(recipient, amount);
        }
        uint256 afterBal = usdc.balanceOf(address(this));
        if (before < afterBal || before - afterBal != amount) {
            revert WithdrawTransferFailed(recipient, amount);
        }
    }

    /// @dev Recombine a split-α `recipient` (10/10 byte halves) back into the
    ///      original 20-byte EVM address. The 80-bit range check on each half
    ///      is enforced by the caller before this is invoked.
    function _reconstructRecipient(uint256 hi, uint256 lo) internal pure returns (address) {
        // (hi << 80) | lo cannot overflow uint160 because both halves fit
        // in 80 bits (verified by RecipientHalfOutOfRange above).
        return address(uint160((hi << 80) | lo));
    }

    // ---------------------------------------------------------------------
    // AAVE management (owner-only)
    // ---------------------------------------------------------------------

    /// @notice Supply idle USDC from the bridge to AAVE, respecting the liquid reserve.
    /// @param amount Exact amount to supply, or `type(uint256).max` to supply
    ///               everything above the liquid reserve.
    function supplyToAave(uint256 amount) external onlyOwner nonReentrant {
        if (!aaveEnabled) revert AaveDisabled();

        uint256 available = _amountSupplyable();
        if (available == 0) revert NothingToSupply();

        uint256 toSupply = amount == type(uint256).max ? available : amount;
        if (toSupply == 0 || toSupply > available) revert InvalidAmount();

        if (!usdc.approve(address(aavePool), toSupply)) revert ApproveFailed();
        uint256 aBefore = aUsdcBalance();
        aavePool.supply(address(usdc), toSupply, address(this), 0);
        uint256 credited = aUsdcBalance() - aBefore;
        if (credited == 0) revert AaveWithdrawFailed(toSupply, 0);
        suppliedPrincipal += credited;

        emit SuppliedToAave(toSupply, suppliedPrincipal);
    }

    /// @notice Withdraw USDC from AAVE back into the bridge (preemptively top up liquidity).
    /// @param amount Amount to withdraw, or `type(uint256).max` for the entire principal.
    function withdrawFromAave(uint256 amount) external onlyOwner nonReentrant {
        uint256 principalCap = suppliedPrincipal;
        if (principalCap == 0) revert InvalidAmount();

        uint256 target = amount == type(uint256).max ? principalCap : amount;
        if (target == 0 || target > principalCap) revert InvalidAmount();

        _pullFromAave(target);
    }

    /// @notice Emergency: pull *all* aUSDC back into the bridge as USDC and disable supplies.
    /// @dev Useful if AAVE pauses/depegs. User withdrawals remain available.
    ///      If any aUSDC remains after `withdraw(max)`, revert — do not
    ///      zero `suppliedPrincipal` (that would make leftover shares look like
    ///      `accruedYield` and `harvestYield` would pay them to the owner).
    ///      If the drain is clean but `received < principal`, keep the
    ///      shortfall on the books instead of zeroing.
    function emergencyWithdrawAll() external onlyOwner nonReentrant {
        uint256 before = usdc.balanceOf(address(this));
        aavePool.withdraw(address(usdc), type(uint256).max, address(this));
        uint256 received = usdc.balanceOf(address(this)) - before;

        uint256 leftover = aUsdcBalance();
        if (leftover != 0) revert EmergencyLeftoverAToken(leftover);

        aaveEnabled = false;
        emit AaveEnabledSet(false);

        uint256 principal = suppliedPrincipal;
        suppliedPrincipal = received >= principal ? 0 : principal - received;

        emit EmergencyWithdrawAll(received);
        emit WithdrawnFromAave(principal, received);
    }

    /// @notice Harvest accrued yield (aUSDC balance above principal) to `yieldRecipient`.
    /// @param amount Amount of yield to harvest (must be <= accruedYield()).
    function harvestYield(uint256 amount) external onlyOwner nonReentrant {
        uint256 yield = accruedYield();
        if (yield == 0 || amount == 0 || amount > yield) revert NoYield();

        uint256 before = usdc.balanceOf(address(this));
        aavePool.withdraw(address(usdc), amount, address(this));
        uint256 received = usdc.balanceOf(address(this)) - before;
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        emit WithdrawnFromAave(amount, received);

        if (!usdc.transfer(yieldRecipient, received)) {
            revert WithdrawTransferFailed(yieldRecipient, received);
        }
        emit YieldHarvested(yieldRecipient, received);
    }

    /// @notice Liquid USDC held by the bridge above `treasuryBalance` (user
    ///         principal). Typically post-`emergencyWithdrawAll` yield that is
    ///         no longer tracked as AAVE `accruedYield()` (QC-A1-3).
    function excessUsdc() public view returns (uint256) {
        uint256 liquid = usdc.balanceOf(address(this));
        return liquid > treasuryBalance ? liquid - treasuryBalance : 0;
    }

    /// @notice Owner skim of `excessUsdc` to `yieldRecipient`. Does not touch
    ///         user principal (`treasuryBalance`).
    /// @param amount Amount to skim (`type(uint256).max` = all excess).
    function skimExcessUsdc(uint256 amount) external onlyOwner nonReentrant {
        if (yieldRecipient == address(0)) revert InvalidRecipient();
        uint256 excess = excessUsdc();
        uint256 toSkim = amount == type(uint256).max ? excess : amount;
        if (excess == 0 || toSkim == 0 || toSkim > excess) revert NoExcessUsdc();
        if (!usdc.transfer(yieldRecipient, toSkim)) {
            revert WithdrawTransferFailed(yieldRecipient, toSkim);
        }
        emit ExcessUsdcSkimmed(yieldRecipient, toSkim);
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

    /// @notice Nominate `newOwner`. They become `owner` only after
    ///         `acceptOwnership`. Replaces one-step transfer.
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert InvalidRecipient();
        pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner, newOwner);
    }

    /// @notice Complete a pending ownership transfer. Caller must be `pendingOwner`.
    /// @dev If `yieldRecipient` still tracks the outgoing owner (the
    ///      constructor default), move it with the role so a key-rotation
    ///      harvest cannot pay the compromised address. An explicitly set
    ///      recipient is left alone.
    function acceptOwnership() external {
        if (msg.sender != pendingOwner || msg.sender == address(0)) revert OwnershipNotPending();
        address previous = owner;
        emit OwnershipTransferred(previous, msg.sender);
        if (yieldRecipient == previous) {
            yieldRecipient = msg.sender;
            emit YieldRecipientSet(msg.sender);
        }
        owner = msg.sender;
        pendingOwner = address(0);
    }

    // ---------------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------------

    /// @dev USDC that may be supplied to AAVE without dipping below the liquid reserve.
    function _amountSupplyable() internal view returns (uint256) {
        uint256 reserve = (treasuryBalance * liquidReserveBps) / BPS_DENOMINATOR;
        uint256 bal = usdc.balanceOf(address(this));
        if (bal <= reserve) return 0;
        return bal - reserve;
    }

    /// @dev Pull `amount` USDC from AAVE. Reverts if short.
    function _pullFromAave(uint256 amount) internal {
        if (suppliedPrincipal == 0) revert InsufficientTreasury();

        uint256 cap = suppliedPrincipal;
        uint256 poolBal = aUsdcBalance();
        uint256 toPull = amount > cap ? cap : amount;
        if (toPull > poolBal) toPull = poolBal;
        if (toPull == 0) revert InsufficientTreasury();

        uint256 before = usdc.balanceOf(address(this));
        aavePool.withdraw(address(usdc), toPull, address(this));
        uint256 received = usdc.balanceOf(address(this)) - before;
        if (received < toPull) revert AaveWithdrawFailed(toPull, received);
        if (received < amount && toPull == amount) revert AaveWithdrawFailed(amount, received);

        suppliedPrincipal -= received;
        emit WithdrawnFromAave(amount, received);
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    /// @notice Current aUSDC balance held by the bridge (principal + accrued interest).
    function aUsdcBalance() public view returns (uint256) {
        if (address(aUSDC) == address(0)) return 0;
        return aUSDC.balanceOf(address(this));
    }

    /// @notice Unharvested yield = aUSDC balance above principal book value.
    function accruedYield() public view returns (uint256) {
        uint256 bal = aUsdcBalance();
        uint256 principal = suppliedPrincipal;
        return bal > principal ? bal - principal : 0;
    }

    /// @notice Total assets under management: USDC + aUSDC (including yield).
    function totalAssets() external view returns (uint256) {
        return usdc.balanceOf(address(this)) + aUsdcBalance();
    }
}
