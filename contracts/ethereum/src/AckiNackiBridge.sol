// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./IBlockHeaderOracle.sol";
import "./IAavePool.sol";
import "./IWrappedTokenGatewayV3.sol";
import "./IERC20.sol";
import "./IPrimaryVerifier.sol";
import "./IFallbackVerifier.sol";
import "./ILayerHashesMovementVerifier.sol";
import "./IBridgeEventVerifier.sol";

/// @title AckiNackiBridge
/// @notice Bridge contract for depositing tokens to Acki Nacki blockchain.
/// @dev Holds user ETH on deposit and routes idle balance into AAVE V3 for yield.
///      - `deposit()` stays cheap: funds accumulate in the contract; an owner/keeper
///        batches supplies to AAVE with `supplyToAave()` to amortise gas.
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
///      AN→ETH event verification (Phase 4 + Circuit 4 scaffolding): every
///      successful `verifyBlock` also pushes the new top-of-chain anchor into
///      a rolling `_layerWindow` ring buffer (size `LAYER_WINDOW_SIZE = 100`,
///      matching partner Circuit 4's `NUM_LAYER_HASHES`). `verifyEvent`
///      consumes that window plus a Circuit 4 (`bridge-event-prove-circuit`)
///      Groth16 proof and asserts that a `WithdrawalInitiated` event for
///      `tokenId` was emitted by the AN-side `TokenBridge` identified by the
///      immutable `(bridgeEventDappFr, bridgeEventAccFr)` pair, anchored into
///      one of the windowed layer hashes (the circuit hides *which* one via
///      a private `hash_choice_index`).  **No `withdraw()` yet** — the
///      Phase A scope deliberately stops at attestation because the partner
///      circuit keeps `amount`/`recipient`/`dstChainId`/`sender` as private
///      witnesses. See `docs/circuit_4_open_questions.md` for the Phase B
///      circuit-extension shopping list.
contract AckiNackiBridge {
    // ---------------------------------------------------------------------
    // Constants
    // ---------------------------------------------------------------------

    /// @notice Maximum deposit amount (prevents whale deposits)
    uint256 public constant MAX_DEPOSIT_AMOUNT = 100 ether;

    /// @notice Basis-point denominator
    uint256 public constant BPS_DENOMINATOR = 10_000;

    /// @notice Upper bound on the liquid reserve (50% of treasury kept as ETH)
    uint256 public constant MAX_LIQUID_RESERVE_BPS = 5_000;

    /// @notice Maximum number of layer-hash slots per block (matches
    ///         partner Circuit 2's MAX_LAYERS).
    uint256 public constant MAX_LAYER_HASHES = 10;

    /// @notice Length of the rolling layer-hash window kept on-chain to back
    ///         Circuit 4 (Bridge Event Prove) proofs. Matches partner's
    ///         `NUM_LAYER_HASHES` in `bridge-event-prove-circuit/src/
    ///         bridge_event_prove_circuit.rs` — the circuit takes exactly
    ///         this many candidate layer hashes as public inputs and binds
    ///         (privately) to one of them via `hash_choice_index`. Increasing
    ///         it requires a partner-side circuit rebuild + new VK.
    uint256 public constant LAYER_WINDOW_SIZE = 100;

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

    /// @notice Total user principal currently held by the bridge (ETH + aWETH principal)
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

    /// @notice AAVE V3 Pool (set once at construction, immutable thereafter)
    IAavePool public immutable aavePool;

    /// @notice AAVE V3 WrappedTokenGateway (handles ETH<->WETH wrapping)
    IWrappedTokenGatewayV3 public immutable wethGateway;

    /// @notice aWETH token minted by AAVE to the bridge when supplying ETH
    IERC20 public immutable aWETH;

    /// @notice Whether new supplies to AAVE are permitted (withdrawals always allowed)
    bool public aaveEnabled;

    /// @notice Principal currently supplied to AAVE (book value, excludes yield)
    uint256 public suppliedPrincipal;

    /// @notice Fraction of the treasury to keep liquid as ETH, in basis points.
    ///         e.g. 500 = 5% of `treasuryBalance` stays as plain ETH to serve small
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
    // Storage: Circuit 4 (Bridge Event Prove) — AN→ETH event verification
    // ---------------------------------------------------------------------

    /// @notice Circuit 4 (Bridge Event Prove) verifier (Groth16 adapter).
    ///         May be `address(0)` if Circuit 4 verification is disabled at
    ///         deployment; in that case `verifyEvent` reverts with
    ///         `VerifyEventDisabled`. Independent of the `verifyBlock` toggle —
    ///         a deployment can wire Circuit 1A/1B/2 (state tracking) without
    ///         Circuit 4 (event attestation), or vice versa.
    IBridgeEventVerifier public immutable bridgeEventVerifier;

    /// @notice Fr-encoded AN-side bridge dApp identifier. Set at construction
    ///         and immutable. The Circuit 4 proof binds to a specific
    ///         `(dappFr, accFr)` pair via its `dapp_fr` / `acc_fr` public
    ///         inputs — pinning these on Ethereum at deploy time prevents a
    ///         caller from substituting an event emitted by a different AN
    ///         contract (e.g. a malicious lookalike `TokenBridge`).
    uint256 public immutable bridgeEventDappFr;

    /// @notice Fr-encoded AN-side bridge account identifier (see `bridgeEventDappFr`).
    uint256 public immutable bridgeEventAccFr;

    /// @notice Rolling ring buffer of the last `LAYER_WINDOW_SIZE`
    ///         top-of-chain anchors committed by successful `verifyBlock`
    ///         calls. Indexed by `layerWindowHead` modulo `LAYER_WINDOW_SIZE`.
    ///
    /// @dev Backs Circuit 4 — the contract supplies this entire array as the
    ///      `layerHashes` public input. The circuit privately picks one entry
    ///      via `hash_choice_index` and asserts its anchored chain root
    ///      equals the picked entry; the verifier learns *that* the proof
    ///      anchors to some known layer, but not *which* one.
    ///
    ///      Empty slots (before the buffer has filled up) are zero. A
    ///      well-formed Circuit 4 proof will (with overwhelming probability)
    ///      not match the zero hash, so leaving empties at zero is safe.
    uint256[LAYER_WINDOW_SIZE] private _layerWindow;

    /// @notice Number of writes to `_layerWindow` so far (monotonic, never
    ///         reset). The active slot for the next push is
    ///         `layerWindowHead % LAYER_WINDOW_SIZE`. Exposed publicly so
    ///         off-chain tooling can detect new entries.
    uint256 public layerWindowHead;

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    event Deposit(
        uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp
    );
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

    /// @notice Emitted whenever a new top-of-chain anchor is pushed into the
    ///         `_layerWindow` ring buffer (i.e. once per successful
    ///         `verifyBlock`). `slot` is the index modulo `LAYER_WINDOW_SIZE`
    ///         that received the write; `headAfter` is the new value of
    ///         `layerWindowHead` (monotonic).
    event LayerWindowPushed(uint256 indexed slot, uint256 anchor, uint256 headAfter);

    /// @notice Emitted on every successful `verifyEvent` call (Circuit 4).
    ///         A genuine cross-chain `withdraw()` is NOT exposed in Phase A
    ///         (the circuit keeps `amount` / `recipient` private). This event
    ///         lets off-chain consumers observe that *some* withdrawal event
    ///         was attested for `tokenId` under the configured AN-side
    ///         identity `(bridgeEventDappFr, bridgeEventAccFr)`, anchored to
    ///         one of the `LAYER_WINDOW_SIZE` recent layer hashes (the
    ///         circuit hides *which* one).
    /// @param tokenId Token id (uint32 packed from the event body).
    /// @param submitter `msg.sender` of the `verifyEvent` call (operational
    ///        breadcrumb; the proof itself doesn't bind to a submitter).
    event BridgeEventVerified(uint256 indexed tokenId, address indexed submitter);

    // ---------------------------------------------------------------------
    // Errors
    // ---------------------------------------------------------------------

    error InvalidAmount();
    error DepositTooLarge();
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

    // verifyBlock errors
    error VerifyBlockDisabled();
    error AttestationProofRejected();
    error LayerHashesProofRejected();
    error BkSetCommitmentMismatch(uint256 supplied, uint256 stored);
    error BlockSeqNoNotMonotonic(uint64 supplied, uint64 stored);
    error PrevAnchorMismatch(uint256 supplied, uint256 stored);
    error InvalidNumLayers(uint256 numLayers);
    error LayerHashTailNonZero(uint256 index);

    // verifyEvent (Circuit 4) errors
    error VerifyEventDisabled();
    error BridgeEventProofRejected();
    error InvalidBridgeEventIdentity();

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
    }

    /// @notice Argument bundle for the AN→ETH Circuit 4 (Bridge Event Prove)
    ///         wiring. Independent of `VerifyBlockConfig` — a deployment may
    ///         enable one without the other.
    /// @dev Passing `bridgeEventVerifier == address(0)` disables `verifyEvent`
    ///      (it reverts with `VerifyEventDisabled`). When enabled, both
    ///      `dappFr` and `accFr` must be non-zero — they pin the AN-side
    ///      identity the Circuit 4 proof must bind to.
    struct BridgeEventConfig {
        IBridgeEventVerifier bridgeEventVerifier;
        uint256 dappFr;
        uint256 accFr;
    }

    /// @param _blockHeaderOracle  Oracle for canonical Ethereum block hashes.
    ///                            Kept for future burn-proof anchoring; currently
    ///                            unused by the public surface but required at
    ///                            construction so re-deploys are unnecessary
    ///                            when the burn-proof flow lands.
    /// @param _aavePool           AAVE V3 Pool address (mainnet: 0x8787...fA4E2).
    /// @param _wethGateway        AAVE V3 WrappedTokenGatewayV3.
    /// @param _aWETH              aWETH token minted by AAVE for supplied WETH.
    /// @param _vb                 AN→ETH verifyBlock wiring (Phase 4). Pass all
    ///                            zeros to disable the AN→ETH path; the deposit/
    ///                            AAVE surface stays fully functional.
    /// @param _be                 Circuit 4 (Bridge Event Prove) wiring. Pass
    ///                            `BridgeEventConfig({...address(0), 0, 0})` to
    ///                            disable; can be enabled / disabled independently
    ///                            of `_vb`.
    /// @dev Pass address(0) for `_aavePool`/`_wethGateway`/`_aWETH` to disable AAVE.
    ///      In that case, the bridge behaves as before (plain ETH custody).
    constructor(
        address _blockHeaderOracle,
        address _aavePool,
        address _wethGateway,
        address _aWETH,
        VerifyBlockConfig memory _vb,
        BridgeEventConfig memory _be
    ) {
        if (_blockHeaderOracle == address(0)) revert InvalidOracle();

        // All three AAVE addresses must be provided together — or none at all.
        bool aaveWired =
            _aavePool != address(0) || _wethGateway != address(0) || _aWETH != address(0);
        bool aaveAllSet =
            _aavePool != address(0) && _wethGateway != address(0) && _aWETH != address(0);
        if (aaveWired && !aaveAllSet) revert InvalidAaveAddress();

        blockHeaderOracle = IBlockHeaderOracle(_blockHeaderOracle);

        aavePool = IAavePool(_aavePool);
        wethGateway = IWrappedTokenGatewayV3(_wethGateway);
        aWETH = IERC20(_aWETH);

        // verifyBlock wiring is all-or-nothing: any zero address disables it.
        primaryVerifier = _vb.primaryVerifier;
        fallbackVerifier = _vb.fallbackVerifier;
        layerHashesVerifier = _vb.layerHashesVerifier;
        storedBkSetCommitment = _vb.genesisBkSetCommitment;
        storedPrevMaxLevelLayerHash = _vb.genesisPrevMaxLevelLayerHash;

        // Circuit 4 (bridge event prove) wiring — independent of `_vb`.
        // Verifier address is the toggle; if non-zero, both Fr identifiers
        // must also be non-zero (otherwise no real proof could ever bind).
        if (address(_be.bridgeEventVerifier) != address(0)) {
            if (_be.dappFr == 0 || _be.accFr == 0) {
                revert InvalidBridgeEventIdentity();
            }
        }
        bridgeEventVerifier = _be.bridgeEventVerifier;
        bridgeEventDappFr = _be.dappFr;
        bridgeEventAccFr = _be.accFr;

        owner = msg.sender;
        yieldRecipient = msg.sender;
        aaveEnabled = aaveAllSet;
        liquidReserveBps = 1_000; // default 10% liquid reserve
        _reentrancyStatus = _NOT_ENTERED;

        // Pre-approve the gateway to pull aWETH (once, for max).
        // AAVE V3's aWETH is a standard ERC-20 approve; rebasing does not
        // affect the allowance amount.
        if (aaveAllSet) {
            aWETH.approve(_wethGateway, type(uint256).max);
        }

        emit OwnershipTransferred(address(0), msg.sender);
    }

    // ---------------------------------------------------------------------
    // User-facing: deposit
    // ---------------------------------------------------------------------

    /// @notice Deposit ETH to be bridged to Acki Nacki.
    /// @dev Emits a `Deposit` event that is later proven by a ZK circuit.
    ///      Funds stay as ETH in this contract; a keeper supplies them to AAVE
    ///      in batches via `supplyToAave()`.
    function deposit() external payable nonReentrant {
        if (msg.value == 0) revert InvalidAmount();
        if (msg.value > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();

        uint256 depositId = depositCounter++;
        treasuryBalance += msg.value;

        emit Deposit(depositId, msg.sender, msg.value, block.timestamp);
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
        uint256 newAnchor = layerHashes[numLayers - 1];
        storedPrevMaxLevelLayerHash = newAnchor;

        // Push the new top-of-chain anchor into the Circuit 4 ring buffer.
        // Even when Circuit 4 isn't wired we still maintain the window so a
        // later opt-in deployment doesn't have to back-fill 100 blocks. Cost
        // is one SSTORE per `verifyBlock` (overwriting a single slot).
        uint256 slot = layerWindowHead % LAYER_WINDOW_SIZE;
        _layerWindow[slot] = newAnchor;
        unchecked {
            // `layerWindowHead` is a monotonic counter; overflow at 2^256
            // is unreachable on any realistic timescale.
            layerWindowHead = layerWindowHead + 1;
        }
        emit LayerWindowPushed(slot, newAnchor, layerWindowHead);

        emit BlockVerified(blockId, blockSeqNo, finType, numLayers);
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

    // ---------------------------------------------------------------------
    // AN→ETH event verification — verifyEvent (Circuit 4, permissionless)
    // ---------------------------------------------------------------------

    /// @notice Verify a Circuit 4 (Bridge Event Prove) proof that a
    ///         `WithdrawalInitiated` event for `tokenId` was emitted on the
    ///         AN-side `TokenBridge` identified by `(bridgeEventDappFr,
    ///         bridgeEventAccFr)` and anchored into one of the most recent
    ///         `LAYER_WINDOW_SIZE` top-of-chain layer hashes.
    ///
    ///         Emits `BridgeEventVerified(tokenId, msg.sender)` on success.
    ///         Does NOT pay anything to anyone — the Phase A circuit keeps
    ///         `amount`/`recipient`/`dstChainId`/`sender` as private
    ///         witnesses, so the bridge has nothing actionable to do with
    ///         them on its own. A genuine `withdraw()` will land alongside a
    ///         circuit variant that exposes those fields (and adds a
    ///         replay-resistant nullifier — see
    ///         `docs/circuit_4_open_questions.md` Q-CIRC4-{1,2,3}).
    ///
    /// @dev Permissionless. The contract assembles the 103-element public
    ///      input vector itself (the 100 `layerHashes` are read from the
    ///      on-chain `_layerWindow`, not taken from the caller — so an
    ///      attacker can't substitute a known-good window from a different
    ///      bridge instance).
    ///
    /// @param proof    256-byte gnark Groth16 proof bytes (Circuit 4 wrap).
    /// @param tokenId  Token id committed by the event body[54..58).
    /// @return success Always `true` on a verified proof; reverts on failure
    ///         (matches `verifyBlock`'s pattern — no silent success).
    function verifyEvent(bytes calldata proof, uint256 tokenId)
        external
        nonReentrant
        returns (bool success)
    {
        if (address(bridgeEventVerifier) == address(0)) {
            revert VerifyEventDisabled();
        }

        // Snapshot the rolling window into memory (one copy, then the
        // adapter ABI takes a calldata array — using `memory` here matches
        // how Solidity passes fixed-size arrays to external calls).
        uint256[LAYER_WINDOW_SIZE] memory window;
        for (uint256 i = 0; i < LAYER_WINDOW_SIZE; i++) {
            window[i] = _layerWindow[i];
        }

        bool ok = bridgeEventVerifier.verifyBridgeEvent(
            proof, tokenId, bridgeEventDappFr, bridgeEventAccFr, window
        );
        if (!ok) revert BridgeEventProofRejected();

        emit BridgeEventVerified(tokenId, msg.sender);
        return true;
    }

    /// @notice Return the rolling layer-hash window as a memory snapshot.
    ///         Indices `0..LAYER_WINDOW_SIZE` map to ring-buffer slots,
    ///         *not* to time-ordered positions. To reconstruct chronological
    ///         order, use `layerWindowHead`:
    ///           `chronological[k] = window[(head - LAYER_WINDOW_SIZE + k) % LAYER_WINDOW_SIZE]`
    ///         for `k ∈ [0, min(head, LAYER_WINDOW_SIZE))`.
    ///         Empty slots (before the buffer fills) are zero.
    function getLayerWindow() external view returns (uint256[LAYER_WINDOW_SIZE] memory out) {
        for (uint256 i = 0; i < LAYER_WINDOW_SIZE; i++) {
            out[i] = _layerWindow[i];
        }
    }

    /// @notice Return one slot of the rolling layer-hash window.
    /// @param slot Ring-buffer slot index in `[0, LAYER_WINDOW_SIZE)`. Use
    ///        `(layerWindowHead - 1) % LAYER_WINDOW_SIZE` for the most recent.
    function layerWindowAt(uint256 slot) external view returns (uint256) {
        require(slot < LAYER_WINDOW_SIZE, "slot oob");
        return _layerWindow[slot];
    }

    /// @notice O(LAYER_WINDOW_SIZE) helper: is `hash` present in the current
    ///         window? Cheap to call off-chain; not used inside `verifyEvent`
    ///         itself (the ZK proof already enforces the membership check
    ///         via `hash_choice_index`).
    function isLayerHashInWindow(uint256 hash) external view returns (bool) {
        for (uint256 i = 0; i < LAYER_WINDOW_SIZE; i++) {
            if (_layerWindow[i] == hash) return true;
        }
        return false;
    }

    // ---------------------------------------------------------------------
    // AAVE management (owner-only)
    // ---------------------------------------------------------------------

    /// @notice Supply idle ETH from the bridge to AAVE, respecting the liquid reserve.
    /// @param amount Exact amount to supply, or `type(uint256).max` to supply
    ///               everything above the liquid reserve.
    function supplyToAave(uint256 amount) external onlyOwner nonReentrant {
        if (!aaveEnabled) revert AaveDisabled();

        uint256 available = _amountSupplyable();
        if (available == 0) revert NothingToSupply();

        uint256 toSupply = amount == type(uint256).max ? available : amount;
        if (toSupply == 0 || toSupply > available) revert InvalidAmount();

        suppliedPrincipal += toSupply;
        wethGateway.depositETH{ value: toSupply }(address(aavePool), address(this), 0);

        emit SuppliedToAave(toSupply, suppliedPrincipal);
    }

    /// @notice Withdraw ETH from AAVE back into the bridge (preemptively top up liquidity).
    /// @param amount Amount to withdraw, or `type(uint256).max` for the entire principal.
    function withdrawFromAave(uint256 amount) external onlyOwner nonReentrant {
        uint256 principalCap = suppliedPrincipal;
        if (principalCap == 0) revert InvalidAmount();

        uint256 target = amount == type(uint256).max ? principalCap : amount;
        if (target == 0 || target > principalCap) revert InvalidAmount();

        _pullFromAave(target);
    }

    /// @notice Emergency: pull *all* aWETH back into the bridge as ETH and disable supplies.
    /// @dev Useful if AAVE pauses/depegs. User withdrawals remain available.
    function emergencyWithdrawAll() external onlyOwner nonReentrant {
        aaveEnabled = false;
        emit AaveEnabledSet(false);

        uint256 before = address(this).balance;
        // max-value signals "withdraw full balance" to the gateway.
        wethGateway.withdrawETH(address(aavePool), type(uint256).max, address(this));

        uint256 received = address(this).balance - before;
        // In an emergency the actual received amount drives the book update.
        uint256 principal = suppliedPrincipal;
        suppliedPrincipal = 0;

        // Surplus (yield) stays in the bridge and can be harvested later.
        // Shortfall (e.g. AAVE insolvency) would reduce headroom — the owner
        // must top up treasury manually if that happens.
        emit EmergencyWithdrawAll(received);
        emit WithdrawnFromAave(principal, received);
    }

    /// @notice Harvest accrued yield (aWETH balance above principal) to `yieldRecipient`.
    /// @param amount Amount of yield to harvest (must be <= accruedYield()).
    function harvestYield(uint256 amount) external onlyOwner nonReentrant {
        uint256 yield = accruedYield();
        if (yield == 0 || amount == 0 || amount > yield) revert NoYield();

        // Withdraw the yield amount from AAVE as ETH (do not touch suppliedPrincipal).
        uint256 before = address(this).balance;
        wethGateway.withdrawETH(address(aavePool), amount, address(this));
        uint256 received = address(this).balance - before;
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        emit WithdrawnFromAave(amount, received);

        // Forward ETH to yield recipient.
        (bool ok,) = payable(yieldRecipient).call{ value: received }("");
        require(ok, "yield transfer failed");
        emit YieldHarvested(yieldRecipient, received);
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

    /// @dev ETH that may be supplied to AAVE without dipping below the liquid reserve.
    function _amountSupplyable() internal view returns (uint256) {
        uint256 reserve = (treasuryBalance * liquidReserveBps) / BPS_DENOMINATOR;
        uint256 bal = address(this).balance;
        if (bal <= reserve) return 0;
        return bal - reserve;
    }

    /// @dev Pull `amount` ETH from AAVE via the WETH gateway. Reverts if short.
    function _pullFromAave(uint256 amount) internal {
        if (suppliedPrincipal == 0) revert InsufficientTreasury();

        // Don't over-withdraw principal; any more is yield which needs `harvestYield`.
        uint256 cap = suppliedPrincipal;
        uint256 toPull = amount > cap ? cap : amount;

        uint256 before = address(this).balance;
        wethGateway.withdrawETH(address(aavePool), toPull, address(this));
        uint256 received = address(this).balance - before;
        if (received < toPull) revert AaveWithdrawFailed(toPull, received);

        // The bridge may need slightly more if yield accrued; the caller can
        // only spend up to `received`, so require the full amount.
        if (received < amount) revert AaveWithdrawFailed(amount, received);

        suppliedPrincipal -= toPull;
        emit WithdrawnFromAave(amount, received);
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    /// @notice Current aWETH balance held by the bridge (principal + accrued interest).
    function aWethBalance() public view returns (uint256) {
        if (address(aWETH) == address(0)) return 0;
        return aWETH.balanceOf(address(this));
    }

    /// @notice Unharvested yield = aWETH balance above principal book value.
    /// @dev Returns 0 if the balance is below principal (shouldn't happen in practice
    ///      — AAVE only grows the balance — but keeps the function panic-free).
    function accruedYield() public view returns (uint256) {
        uint256 bal = aWethBalance();
        uint256 principal = suppliedPrincipal;
        return bal > principal ? bal - principal : 0;
    }

    /// @notice Total assets under management: ETH + aWETH (including yield).
    function totalAssets() external view returns (uint256) {
        return address(this).balance + aWethBalance();
    }

    // ---------------------------------------------------------------------
    // ETH receive hook
    // ---------------------------------------------------------------------

    /// @dev AAVE's WrappedTokenGateway unwraps WETH and sends ETH to the bridge
    ///      during `withdrawETH`. We must accept it. Direct transfers from any
    ///      other source are ignored for accounting purposes (they boost
    ///      `address(this).balance` but do not touch `treasuryBalance`).
    receive() external payable { }
}
