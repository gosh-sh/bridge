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
///      - **Withdraw on ETH side**: deliberately not exposed in this milestone.
///        A genuine cross-chain withdrawal will land alongside a burn-proof
///        circuit + state-anchored verification in a future milestone (see
///        `docs/an_partner_integration_plan.md` §3 Phase 4 open design question
///        and Decision Log entry 2026-05-17). The legacy v1 refund-style
///        `withdraw(depositId, recipient, amount, blockNumber, proof)` was
///        retired in Phase 4.3 (2026-05-17) — see Decision Log.
///
///      AN→ETH state (Phase 4): the bridge stores a rolling commitment to the
///      Acki Nacki side (`block_seq_no`, `bk_set_poseidon`, layer-hash roots,
///      chain anchor). `verifyBlock` advances the commitment after verifying
///      a tuple of two cross-circuit-bound Halo2/Groth16 proofs:
///        - **proof1**: Circuit 1A (Primary) or 1B (Fallback) attestation, BLS-aggregated.
///        - **proof2**: Circuit 2 (Layer hashes movement), Poseidon-Merkle-anchored.
///      Both proofs share a common `block_id` and `bk_set_poseidon` by
///      construction; the bridge enforces those equalities + the monotonic
///      `block_seq_no` and chain-anchor invariants on top.
///
///      AN→ETH event verification + payout (Circuit 4, single-final-root):
///      every successful `verifyBlock` also records the new top-of-chain
///      anchor in a set of `knownAnchors`. `withdrawByProof` consumes that
///      set plus a Circuit 4 (`bridge-event-prove-circuit`) Groth16 proof
///      whose 10 public inputs include a single `finalRoot`. The bridge
///      checks `finalRoot ∈ knownAnchors` off-circuit (this contract) — the
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

    /// @notice Maximum deposit amount (100 USDC; prevents whale deposits)
    uint256 public constant MAX_DEPOSIT_AMOUNT = 100 * USDC_UNIT;

    /// @notice Basis-point denominator
    uint256 public constant BPS_DENOMINATOR = 10_000;

    /// @notice Upper bound on the liquid reserve (50% of treasury kept as USDC)
    uint256 public constant MAX_LIQUID_RESERVE_BPS = 5_000;

    /// @notice Maximum number of layer-hash slots per block (matches
    ///         partner Circuit 2's MAX_LAYERS).
    uint256 public constant MAX_LAYER_HASHES = 10;

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

    /// @notice Address that receives harvested yield (defaults to owner)
    address public yieldRecipient;

    // Reentrancy guard (avoid pulling in OZ for a single uint256)
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _reentrancyStatus;

    /// @notice Global pause flag. When `true`, all user-facing entrypoints
    ///         (`deposit`, `verifyBlock`, `withdrawByProof`) revert with
    ///         `BridgePaused`. Owner-only AAVE management remains
    ///         available so funds can still be evacuated in an incident.
    /// @dev Toggled via `pause()` / `unpause()` (owner-only).
    bool public paused;

    // ---------------------------------------------------------------------
    // Storage: AN→ETH state (Phase 4 verifyBlock)
    // ---------------------------------------------------------------------

    /// @notice Circuit 1A (Primary attestation) verifier (Groth16 adapter).
    ///         May be `address(0)` if AN→ETH verification is disabled at
    ///         deployment; in that case `verifyBlock` reverts with `VerifyBlockDisabled`.
    IPrimaryVerifier public immutable primaryVerifier;

    /// @notice Circuit 1B (Fallback attestation) verifier (Groth16 adapter).
    ///         May be `address(0)` (see `primaryVerifier`).
    IFallbackVerifier public immutable fallbackVerifier;

    /// @notice Circuit 2 (Layer hashes movement) verifier (Groth16 adapter).
    ///         May be `address(0)` (see `primaryVerifier`).
    ILayerHashesMovementVerifier public immutable layerHashesVerifier;

    /// @notice Active Acki Nacki BK-set Poseidon commitment.
    ///         Updated only by future Circuit 3 (BK-set rotation, Phase 1.C);
    ///         seeded from the constructor's `_genesisBkSetCommitment`.
    uint256 public storedBkSetCommitment;

    /// @notice Highest AN block sequence number whose attestation has been
    ///         verified on-chain. Strictly monotonic via `verifyBlock`.
    uint64 public storedLastSeenBlockSeqNo;

    /// @notice Number of active layer slots committed by the most recent
    ///         layer-hashes proof. Range 1..=`MAX_LAYER_HASHES` (`MAX_LAYERS`).
    uint8 public storedNumLayers;

    /// @notice Per-layer Poseidon Merkle roots committed by the most recent
    ///         layer-hashes proof. Indices `>= storedNumLayers` are zero.
    uint256[MAX_LAYER_HASHES] public storedLayerHashes;

    /// @notice Chain anchor — the Poseidon root of the previous chain that
    ///         the next layer-hashes proof must extend. Equal to the
    ///         `(storedNumLayers - 1)`-th entry of `storedLayerHashes` after
    ///         each successful `verifyBlock` (the new top of the chain).
    uint256 public storedPrevMaxLevelLayerHash;

    // ---------------------------------------------------------------------
    // Storage: Circuit 4 (Bridge Withdrawal, single-final-root) — AN→ETH payout
    // ---------------------------------------------------------------------

    /// @notice Circuit 4 verifier (Groth16 adapter; 10-input single-final-root
    ///         layout). May be `address(0)` if AN→ETH payout verification is
    ///         disabled at deployment; in that case `withdrawByProof` reverts
    ///         with `WithdrawByProofDisabled`. Independent of `verifyBlock`.
    IBridgeWithdrawalVerifier public immutable bridgeWithdrawalVerifier;

    /// @notice Fr-encoded AN-side bridge dApp identifier. Set at construction
    ///         and immutable. The Circuit 4 proof binds to a specific
    ///         `(dappFr, accFr)` pair via its public inputs — pinning these
    ///         on Ethereum at deploy time prevents a caller from substituting
    ///         an event emitted by a different AN contract (e.g. a malicious
    ///         lookalike `TokenBridge`).
    uint256 public immutable bridgeWithdrawalDappFr;

    /// @notice Fr-encoded AN-side bridge account identifier (see `bridgeWithdrawalDappFr`).
    uint256 public immutable bridgeWithdrawalAccFr;

    /// @notice See `BridgeWithdrawConfig.altDstChainId`.
    uint256 public immutable bridgeWithdrawalAltDstChainId;

    /// @notice See `BridgeWithdrawConfig.altTokenId`.
    uint256 public immutable bridgeWithdrawalAltTokenId;

    /// @notice Replay-protection store. Keyed by `bytes32(nullifier)` from
    ///         the proof's public input slot [8]. The Circuit 4
    ///         single-final-root nullifier is
    ///         `Poseidon(block_id_fr, tokenId, amount, recipientHi,
    ///                   recipientLo, senderAccFr)` — uniqueness per event
    ///         is enforced inside the circuit, but the bridge still needs
    ///         the mapping to reject *re-submission* of an already-paid proof.
    mapping(bytes32 => bool) private _nullifiers;

    /// @notice Set of anchors (Fr) observed via successful `verifyBlock`
    ///         calls. Each `withdrawByProof` proof exposes one `finalRoot`
    ///         (slot [9]) and the bridge requires `_knownAnchors[finalRoot]`
    ///         to be `true` before accepting the proof.
    ///
    /// @dev This replaces the legacy `_layerWindow[100]` ring-buffer +
    ///      private-index design from Circuit 4 v1/v2. v3
    ///      (`circuit4-single-final-root`) exposes the anchor as a single
    ///      public input rather than as a 1-of-N private selection, so the
    ///      on-chain check is a flat set membership.
    ///
    ///      Anchors stay valid forever once recorded — partner's dense-chain
    ///      construction makes each `finalRoot` unique per (layer, block-
    ///      height range), and a withdrawal proven against any *previously
    ///      observed* `finalRoot` is by definition a valid withdrawal that
    ///      we missed paying out earlier (re-org-resistant by virtue of AN's
    ///      finalization model).
    mapping(uint256 => bool) private _knownAnchors;

    /// @notice Monotonic counter of distinct anchors recorded by
    ///         `verifyBlock` calls (never reset; useful for off-chain
    ///         tooling to detect new state).
    uint256 public anchorsRecorded;

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    /// @notice Emitted on every deposit. `anWorkchain` + `anAccount` are the
    ///         Acki Nacki destination (TVM `workchain:account`) chosen by the
    ///         depositor; they are carried as ZK public inputs and credited on
    ///         the AN side (the EVM `sender` is kept only for provenance, since
    ///         a 20-byte EVM address is not a valid AN recipient).
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        int8 anWorkchain,
        bytes32 anAccount,
        uint256 timestamp
    );
    /// @notice Bridge paused — emitted when the owner sets `paused = true`.
    /// @param by Owner address that triggered the pause (`msg.sender`).
    event Paused(address indexed by);
    /// @notice Bridge unpaused — emitted when the owner sets `paused = false`.
    /// @param by Owner address that lifted the pause (`msg.sender`).
    event Unpaused(address indexed by);
    event SuppliedToAave(uint256 amount, uint256 suppliedPrincipalAfter);
    event WithdrawnFromAave(uint256 amountRequested, uint256 amountReceived);
    event YieldHarvested(address indexed recipient, uint256 amount);
    event AaveEnabledSet(bool enabled);
    event LiquidReserveBpsSet(uint256 bps);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event YieldRecipientSet(address indexed recipient);
    event EmergencyWithdrawAll(uint256 amount);

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

    /// @notice Emitted whenever a new top-of-chain anchor is recorded by a
    ///         successful `verifyBlock` call. `anchor` is the new
    ///         `storedPrevMaxLevelLayerHash`; `totalAnchors` is the new
    ///         value of `anchorsRecorded` (monotonic, never resets).
    event AnchorRecorded(uint256 indexed anchor, uint256 totalAnchors);

    /// @notice Emitted on every successful `withdrawByProof` call (Circuit 4,
    ///         single-final-root layout). A verified ZK proof releases
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
    error DepositTooLarge();
    /// @notice Acki Nacki destination account was zero. A valid AN recipient
    ///         (256-bit TVM account) must be supplied at deposit time.
    error InvalidAnAccount();
    error InsufficientTreasury();
    error InvalidRecipient();
    error InvalidOracle();
    error InvalidAaveAddress();
    error NotOwner();
    error Reentrancy();
    error AaveDisabled();
    error ReserveBpsTooHigh();
    error NothingToSupply();
    error AaveWithdrawFailed(uint256 requested, uint256 received);
    error NoYield();
    /// @notice User-facing entrypoints are paused. Owner-only AAVE
    ///         management (`emergencyWithdrawAll`, `withdrawFromAave`,
    ///         `harvestYield`) remains available regardless of the pause
    ///         state so funds can still be evacuated in an incident.
    error BridgePaused();
    error AlreadyInThatPauseState();

    // verifyBlock errors
    error VerifyBlockDisabled();
    error AttestationProofRejected();
    error LayerHashesProofRejected();
    error BkSetCommitmentMismatch(uint256 supplied, uint256 stored);
    error BlockSeqNoNotMonotonic(uint64 supplied, uint64 stored);
    error PrevAnchorMismatch(uint256 supplied, uint256 stored);
    error InvalidNumLayers(uint256 numLayers);
    error LayerHashTailNonZero(uint256 index);

    // withdrawByProof (Circuit 4, single-final-root) errors
    error WithdrawByProofDisabled();
    error WithdrawalProofRejected();
    error NullifierAlreadyUsed(uint256 nullifier);
    error DstChainIdMismatch(uint256 supplied, uint256 expected);
    error RecipientHalfOutOfRange(uint256 value);
    error WithdrawIdentityMismatch();
    /// @notice The proof's `finalRoot` (PI slot [9]) is not in the bridge's
    ///         set of known anchors. Either the proof was generated against
    ///         a state the bridge has not yet observed (caller should wait
    ///         for the matching `verifyBlock` to land), or the anchor is
    ///         forged.
    error UnknownAnchor(uint256 finalRoot);
    /// @notice Withdrawal verifier was wired but the AN-side
    ///         `(dappFr, accFr)` identity slots are zero — no real proof
    ///         could ever bind to a zero-identity bridge.
    error InvalidBridgeWithdrawalIdentity();
    /// @notice Only `tokenId == 0` (bridged USDC) is currently supported.
    ///         Non-zero token ids are reserved for multi-token wiring in a
    ///         future milestone.
    error UnsupportedTokenId(uint256 tokenId);
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

    /// @dev User-facing entrypoints revert when the bridge is paused.
    ///      Owner-only AAVE management is deliberately *not* gated by
    ///      this modifier so the owner can still pull liquidity in an
    ///      incident.
    modifier whenNotPaused() {
        if (paused) revert BridgePaused();
        _;
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
    }

    /// @notice Argument bundle for the AN→ETH Circuit 4 (single-final-root)
    ///         wiring. Independent of `VerifyBlockConfig` — a deployment may
    ///         enable one without the other.
    /// @dev Passing `bridgeWithdrawalVerifier == address(0)` disables
    ///      `withdrawByProof` (it reverts with `WithdrawByProofDisabled`).
    ///      When enabled, both `dappFr` and `accFr` must be non-zero — they
    ///      pin the AN-side identity the Circuit 4 proof must bind to.
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
        ///         from `block.chainid`. Zero disables the alias.
        uint256 altDstChainId;
        /// @notice Optional shellnet alias for `pub.tokenId` (e.g. AN
        ///         `USDC_ECC_ID = 3`). Zero accepts only `tokenId == 0`.
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
    /// @param _bw                 Circuit 4 (single-final-root) wiring. Pass
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

        // verifyBlock wiring is all-or-nothing: any zero address disables it.
        primaryVerifier = _vb.primaryVerifier;
        fallbackVerifier = _vb.fallbackVerifier;
        layerHashesVerifier = _vb.layerHashesVerifier;
        storedBkSetCommitment = _vb.genesisBkSetCommitment;
        storedPrevMaxLevelLayerHash = _vb.genesisPrevMaxLevelLayerHash;

        // Circuit 4 (single-final-root) wiring — independent of `_vb`.
        // Verifier address is the toggle; if non-zero, both Fr identifiers
        // must also be non-zero (otherwise no real proof could ever bind).
        if (address(_bw.bridgeWithdrawalVerifier) != address(0)) {
            if (_bw.accFr == 0) {
                revert InvalidBridgeWithdrawalIdentity();
            }
        }
        bridgeWithdrawalVerifier = _bw.bridgeWithdrawalVerifier;
        bridgeWithdrawalDappFr = _bw.dappFr;
        bridgeWithdrawalAccFr = _bw.accFr;
        bridgeWithdrawalAltDstChainId = _bw.altDstChainId;
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
    /// @param anWorkchain Acki Nacki destination workchain id (TVM, e.g. 0).
    /// @param anAccount   Acki Nacki destination account (256-bit TVM address).
    ///                    Must be non-zero. Carried as ZK public inputs and
    ///                    credited on the AN side — an EVM address cannot be an
    ///                    AN recipient, so the destination is supplied explicitly.
    function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)
        external
        nonReentrant
        whenNotPaused
    {
        if (amount == 0) revert InvalidAmount();
        if (amount > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();
        if (anAccount == bytes32(0)) revert InvalidAnAccount();
        if (!usdc.transferFrom(msg.sender, address(this), amount)) {
            revert TransferFromFailed();
        }

        uint256 depositId = depositCounter++;
        treasuryBalance += amount;

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
    ///      after both gnark Groth16 verifiers report success and every cross-
    ///      circuit / monotonicity / chain-anchor invariant holds.
    ///
    /// Invariants enforced (revert-on-violation):
    ///   - `bkSetCommitment == storedBkSetCommitment`           (BK-set anchor; rotated only by Phase 1.C Circuit 3 in future)
    ///   - `blockSeqNo > storedLastSeenBlockSeqNo`              (strictly monotonic)
    ///   - `prevMaxLevelLayerHash == storedPrevMaxLevelLayerHash` (chain anchor — guards against fork & replay)
    ///   - `1 <= numLayers <= MAX_LAYER_HASHES`                 (shape)
    ///   - `layerHashes[i] == 0` for `i >= numLayers`           (tail must be zero — defends against
    ///                                                            silent garbage in unused slots)
    ///   - `primaryVerifier` / `fallbackVerifier` / `layerHashesVerifier` all reject the proofs
    ///     ⇒ revert (no partial state mutation).
    ///
    /// State updates after success:
    ///   - `storedLastSeenBlockSeqNo = blockSeqNo`
    ///   - `storedNumLayers = numLayers`
    ///   - `storedLayerHashes[i] = layerHashes[i]` for all 0..MAX_LAYER_HASHES
    ///   - `storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1]`
    ///     (the new top-of-chain layer becomes the anchor for the *next* call)
    ///
    /// @param finType            Primary or Fallback finalization path.
    /// @param attestationProof   gnark Groth16 proof bytes for Circuit 1A or 1B (256 bytes).
    /// @param layerHashesProof   gnark Groth16 proof bytes for Circuit 2 (256 bytes).
    /// @param blockId            32-byte AN block identifier shared between both proofs.
    /// @param bkSetCommitment    Poseidon commitment to the active BK set; shared between both proofs.
    /// @param blockSeqNo         AN block sequence number being attested.
    /// @param numLayers          Number of active layer slots (1..=MAX_LAYER_HASHES).
    /// @param layerHashes        10 layer-hash field elements; tail (>= numLayers) must be zero.
    /// @param prevMaxLevelLayerHash Chain anchor — must equal `storedPrevMaxLevelLayerHash`.
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
    ) external nonReentrant whenNotPaused {
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

        // ---- Anchor checks against stored state. ----
        if (bkSetCommitment != storedBkSetCommitment) {
            revert BkSetCommitmentMismatch(bkSetCommitment, storedBkSetCommitment);
        }
        if (blockSeqNo <= storedLastSeenBlockSeqNo) {
            revert BlockSeqNoNotMonotonic(blockSeqNo, storedLastSeenBlockSeqNo);
        }
        if (prevMaxLevelLayerHash != storedPrevMaxLevelLayerHash) {
            revert PrevAnchorMismatch(prevMaxLevelLayerHash, storedPrevMaxLevelLayerHash);
        }

        // ---- Crypto: verify both proofs. The shared (blockId, bkSetCommitment,
        //      blockSeqNo) values flow into both verifier calls, so any
        //      mismatch between the two proofs surfaces here as one of the two
        //      verifications failing (their public inputs are computed from
        //      these values byte-for-byte).
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

        bool lhOk = layerHashesVerifier.verifyLayerHashesMovement(
            layerHashesProof,
            blockId,
            bkSetCommitment,
            uint256(numLayers),
            layerHashes,
            prevMaxLevelLayerHash
        );
        if (!lhOk) revert LayerHashesProofRejected();

        // ---- Effects (CEI): commit the new state. ----
        storedLastSeenBlockSeqNo = blockSeqNo;
        storedNumLayers = numLayers;
        for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
            storedLayerHashes[i] = layerHashes[i];
        }
        // The new top-of-chain becomes the anchor for the next call; keeps
        // `storedPrevMaxLevelLayerHash` co-located with the canonical layer.
        // Also push it into the Circuit 4 ring buffer (via a helper to keep
        // this function's stack frame small enough to compile cleanly under
        // `forge coverage`, which runs without `--via-ir`).
        storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1];
        _recordAnchor(layerHashes[numLayers - 1]);

        emit BlockVerified(blockId, blockSeqNo, finType, numLayers);
    }

    /// @dev Record `anchor` in the `_knownAnchors` set and bump the
    ///      monotonic `anchorsRecorded` counter. Extracted from
    ///      `verifyBlock` so the latter compiles without `--via-ir`
    ///      (needed for `forge coverage`). Cost: one SSTORE for the
    ///      mapping (new key) and one for the counter. The same anchor
    ///      may be recorded multiple times safely (re-write to true is a
    ///      no-op in terms of correctness; the counter still bumps so
    ///      off-chain tooling sees every `verifyBlock` event).
    function _recordAnchor(uint256 anchor) internal {
        _knownAnchors[anchor] = true;
        unchecked {
            anchorsRecorded = anchorsRecorded + 1;
        }
        emit AnchorRecorded(anchor, anchorsRecorded);
    }

    /// @notice View helper: returns the full `storedLayerHashes` array as a
    ///         memory copy. Public mappings give per-index access; this is
    ///         convenient for off-chain reads in one RPC call.
    function getStoredLayerHashes() external view returns (uint256[MAX_LAYER_HASHES] memory) {
        uint256[MAX_LAYER_HASHES] memory out;
        for (uint256 i = 0; i < MAX_LAYER_HASHES; i++) {
            out[i] = storedLayerHashes[i];
        }
        return out;
    }

    /// @notice O(1) helper: is `anchor` in the bridge's set of known anchors?
    ///         Cheap to call off-chain; mirrored inside `withdrawByProof`.
    function isKnownAnchor(uint256 anchor) external view returns (bool) {
        return _knownAnchors[anchor];
    }

    // ---------------------------------------------------------------------
    // AN→ETH withdrawal payout — withdrawByProof (Circuit 4, single-final-root)
    // ---------------------------------------------------------------------

    /// @notice Mask for each half of a split-α `recipient` address (10 bytes = 80 bits).
    /// @dev Circuit 4 single-final-root uses split-α (10/10) per
    ///      `bridge_event_prove_circuit::RECIPIENT_HI_*` / `RECIPIENT_LO_*`
    ///      offsets in the partner repo. The split is locked at deployment
    ///      via the immutable `bridgeWithdrawalVerifier` — its on-chain VK
    ///      only accepts proofs with the matching layout.
    uint256 private constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    /// @notice Pay out a withdrawal proven by a Circuit 4 (single-final-root)
    ///         Groth16 proof.
    ///
    /// Verifies that:
    ///   1. The proof witnesses a `WithdrawalInitiated(dstChainId, recipient,
    ///      amount, tokenId, sender)` event emitted by the AN-side TokenBridge
    ///      identified by `(bridgeWithdrawalDappFr, bridgeWithdrawalAccFr)`,
    ///      anchored — via the proof's dense-chain extension — to a
    ///      `finalRoot` the bridge has previously observed via `verifyBlock`
    ///      (`_knownAnchors[finalRoot] == true`).
    ///   2. `pub.dstChainId == block.chainid` (the event was destined for
    ///      *this* chain, not a sibling EVM chain that shares the bridge VK).
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
    /// @param proof   256-byte gnark Groth16 proof bytes (Circuit 4 wrap).
    /// @param pub     Public-input slots [0..9]; see `IBridgeWithdrawalVerifier`.
    /// @return success Always `true` on a successful payout; reverts on failure.
    function withdrawByProof(
        bytes calldata proof,
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs calldata pub
    ) external nonReentrant whenNotPaused returns (bool success) {
        if (address(bridgeWithdrawalVerifier) == address(0)) {
            revert WithdrawByProofDisabled();
        }

        // ---- Checks: identity, chain, replay, shape, anchor ----
        if (pub.dappFr != bridgeWithdrawalDappFr || pub.accFr != bridgeWithdrawalAccFr) {
            revert WithdrawIdentityMismatch();
        }
        bool dstOk = pub.dstChainId == block.chainid
            || (bridgeWithdrawalAltDstChainId != 0
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
        bytes32 nullifierKey = bytes32(pub.nullifier);
        if (_nullifiers[nullifierKey]) {
            revert NullifierAlreadyUsed(pub.nullifier);
        }
        // Off-circuit anchor check. The proof exposes `finalRoot` as a
        // public input (slot [9]) — we accept it only if we have previously
        // observed it through a successful `verifyBlock` extension. This
        // replaces the legacy `_layerWindow[100]` private-index design with
        // an O(1) set lookup that matches the v3 circuit's single-final-root
        // layout.
        if (!_knownAnchors[pub.finalRoot]) {
            revert UnknownAnchor(pub.finalRoot);
        }

        // ---- Crypto: verify the Groth16 proof. The 10 public inputs flow
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
        if (!usdc.transfer(recipient, pub.amount)) {
            revert WithdrawTransferFailed(recipient, pub.amount);
        }

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

        suppliedPrincipal += toSupply;
        usdc.approve(address(aavePool), toSupply);
        aavePool.supply(address(usdc), toSupply, address(this), 0);

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
    function emergencyWithdrawAll() external onlyOwner nonReentrant {
        aaveEnabled = false;
        emit AaveEnabledSet(false);

        uint256 before = usdc.balanceOf(address(this));
        aavePool.withdraw(address(usdc), type(uint256).max, address(this));

        uint256 received = usdc.balanceOf(address(this)) - before;
        uint256 principal = suppliedPrincipal;
        suppliedPrincipal = 0;

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

    // ---------------------------------------------------------------------
    // Pause controls (owner-only)
    // ---------------------------------------------------------------------

    /// @notice Halt all user-facing entrypoints (`deposit`, `verifyBlock`,
    ///         `withdrawByProof`). Reverts if the bridge is already paused.
    /// @dev Owner-only AAVE management is *not* gated — the owner must
    ///      still be able to evacuate funds via `emergencyWithdrawAll` /
    ///      `withdrawFromAave` / `harvestYield` while paused.
    function pause() external onlyOwner {
        if (paused) revert AlreadyInThatPauseState();
        paused = true;
        emit Paused(msg.sender);
    }

    /// @notice Re-enable user-facing entrypoints. Reverts if not paused.
    function unpause() external onlyOwner {
        if (!paused) revert AlreadyInThatPauseState();
        paused = false;
        emit Unpaused(msg.sender);
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
        uint256 toPull = amount > cap ? cap : amount;

        uint256 before = usdc.balanceOf(address(this));
        aavePool.withdraw(address(usdc), toPull, address(this));
        uint256 received = usdc.balanceOf(address(this)) - before;
        if (received < toPull) revert AaveWithdrawFailed(toPull, received);
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        suppliedPrincipal -= toPull;
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
