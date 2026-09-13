pragma gosh-solidity >=0.76.1;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./EthKeccak.sol";

/// @notice Sink for the canonicality anchor the light client proves. The
///         `USDCBridge` implements this so a proven finalized execution block
///         hash lands in the same `_acceptedBlockHash` set that
///         `finalizeDeposit` already consults — replacing the owner / M-of-N
///         attester writer with a trustless one. Address 0 disables the push
///         (the light client still records the anchor locally).
interface IAcceptedBlockHashSink {
    function acceptBlockHashFromLightClient(uint256 chainId, uint256 blockHash) external;
    /// Drop a hash the light client has aged out of its one-year window.
    /// Missing this function bounces; local eviction still stands.
    function forgetBlockHashFromLightClient(uint256 chainId, uint256 blockHash) external;
}

/// @title EthBeaconLightClient
/// @notice Trustless Ethereum → Acki Nacki canonicality oracle. Consumes the
///         beacon **sync-step** proof (`eth-light-client-prover`, milestone M5)
///         via the `ZKHALO2VERIFYWITHVK` TVM opcode (0xC7 0x4A) and, for every
///         accepted finality update, advances its head and registers the
///         finalized **execution** block hash as canonical on the tracked L1.
///
///         What the step proof establishes (all bound in-circuit — see
///         `eth-light-client-prover/src/step.rs`):
///           * the finalized beacon header is the one a supermajority
///             (≥ 2/3 of 512) of a sync committee signed, over the correct
///             `DOMAIN_SYNC_COMMITTEE` / fork / genesis;
///           * that committee's Poseidon commitment equals public input #5;
///           * the finalized block's `ExecutionPayloadHeader` merkleizes into
///             the signed `body_root`, exposing its `block_hash` (PI #6/#7).
///
///         What the proof CANNOT establish, and this contract asserts from
///         outside it:
///           * that the signing committee is the *chain-anchored* one for this
///             period — enforced by `committeeCommitment == _currentCommittee`
///             (the rotate ↔ step join). Bootstrapped from a weak-subjectivity
///             checkpoint; advanced by `submitRotate`. After
///             `disableOwnerRotation`, the owner hatch is `reAnchorCommittee`.
///
///         Checkpoint coverage: `submitUpdate` anchors the finalized execution
///         hash (1/32). `submitAncestry` walks that block's execution parent-hash
///         chain (keccak256(header RLP) + RLP `parentHash`, ≤ 31 parents) and
///         pushes those hashes into the same `_acceptedBlockHash` sink.
///         Proven hashes expire after `SLOTS_PER_YEAR` (365d of 12s slots) behind
///         head: `isProven` / the sink forget that window. Deposits older than
///         a year cannot `finalizeDeposit` against this oracle.
contract EthBeaconLightClient {
    string constant version = "0.1.0";

    // Sync committee size on Ethereum mainnet — the supermajority denominator.
    uint256 constant SYNC_COMMITTEE_SIZE = 512;
    // 365 * 24 * 3600 / 12. A proven execution hash older than this many
    // slots behind head is not canonical for `finalizeDeposit` (one-year SLA).
    uint64 constant SLOTS_PER_YEAR = 2_628_000;
    // Amortized compact of the FIFO; leftover expired keys stay unreadable
    // via `_isLive` until a later tx deletes them.
    uint16 constant MAX_EVICT_PER_TX = 128;

    uint64 constant MIN_BALANCE = 100 vmshell;
    uint constant bitCntAddress = 256;
    uint128 constant HeadUpdatedEmit = 701;
    uint128 constant CommitteeRotatedEmit = 702;
    uint128 constant CommitteeReAnchoredEmit = 703;
    uint128 constant AncestryAcceptedEmit = 704;
    uint128 constant CheckpointBackfilledEmit = 705;
    uint128 constant AnchorPushBouncedEmit = 706;
    uint128 constant AnchorsExpiredEmit = 707;

    // Error codes (kept clear of the shared USDCBridgeErrors range 204..229).
    uint16 constant ERR_NOT_OWNER          = 209;
    uint16 constant ERR_INVALID_SENDER     = 207;
    uint16 constant ERR_INVALID_ZKPROOF    = 220;
    uint16 constant ERR_BAD_PUBLIC_INPUTS  = 240;
    uint16 constant ERR_LOW_PARTICIPATION  = 241;
    uint16 constant ERR_WRONG_COMMITTEE    = 242;
    uint16 constant ERR_COMMITTEE_UNSET    = 243;
    uint16 constant ERR_STALE_UPDATE       = 244;
    uint16 constant ERR_STALE_PERIOD       = 245;
    uint16 constant ERR_OWNER_ROTATION_DISABLED = 246;
    uint16 constant ERR_REANCHOR_NOT_ARMED = 247;
    uint16 constant ERR_REANCHOR_NOOP      = 248;
    uint16 constant ERR_UNKNOWN_CHECKPOINT = 249;
    uint16 constant ERR_BAD_ANCESTRY       = 250;
    uint16 constant ERR_ANCESTRY_TOO_LONG  = 251;
    uint16 constant ERR_NOT_PROVEN         = 252;

    event HeadUpdated(
        uint64  finalizedSlot,
        uint256 finalizedBeaconRoot,
        uint256 executionBlockHash,
        uint256 committeeCommitment
    );
    event CommitteeRotated(
        uint64  period,
        uint256 fromCommitteeCommitment,
        uint256 toCommitteeCommitment
    );
    event CommitteeReAnchored(
        uint64  period,
        uint256 fromCommitteeCommitment,
        uint256 toCommitteeCommitment
    );
    event AncestryAccepted(
        uint256 checkpointHash,
        uint64  hashesAdded
    );
    /// Late-registered checkpoint (proof verified, head not rewound).
    event CheckpointBackfilled(
        uint64  finalizedSlot,
        uint256 executionBlockHash
    );
    /// USDCBridge rejected `acceptBlockHashFromLightClient` (bounce).
    event AnchorPushBounced();
    /// Hashes dropped from the one-year proven window (and forgotten at the sink).
    event AnchorsExpired(uint64 count);

    /// @notice Deposit-style struct of the 10 proven public inputs
    ///         (`step.rs::pack_step_instances`), all little-endian 32-byte Fr:
    ///           [0] attested_slot            [1] finalized_slot
    ///           [2] beacon_root_hi           [3] beacon_root_lo
    ///           [4] participation            [5] committee_commitment
    ///           [6] exec_block_hash_hi       [7] exec_block_hash_lo
    ///           [8] attested_state_root_hi   [9] attested_state_root_lo
    ///         `committee_commitment` is the **2-level** Poseidon scheme
    ///         (`commit_sync_committee_2level`), byte-identical to the value the
    ///         rotate proof emits as `next_commit` — so the committee that a
    ///         rotate advances into is exactly what the next step recomputes.
    ///         `attested_state_root` (indices 8|9) is the state the rotate proof
    ///         anchors its `next_sync_committee` branch against (bound in-circuit).
    struct StepPI {
        uint64  attestedSlot;
        uint64  finalizedSlot;
        uint256 finalizedBeaconRoot;    // hi << 128 | lo
        uint64  participation;          // count of committee bits set (0..512)
        uint256 committeeCommitment;    // 2-level Poseidon commitment to the sync committee
        uint256 executionBlockHash;     // hi << 128 | lo
        uint256 attestedStateRoot;      // hi << 128 | lo
    }

    /// @notice The proven public inputs of the **rotate** circuit. The rotate
    ///         root is a recursive 2-to-1 aggregation over 8 committee shards +
    ///         a step snark (`eth-light-client-prover`, milestone M6), so its
    ///         instance column is 15 little-endian 32-byte Fr:
    ///           [0..12)  KZG accumulator limbs (lhs/rhs G1, 3×88-bit per coord)
    ///           [12] current_committee_commitment  — 2-level Poseidon commitment
    ///               to the committee that SIGNED (must equal the trusted current
    ///               one); bound in-circuit to the folded step's committee
    ///           [13] next_committee_commitment     — 2-level Poseidon commitment
    ///               to the committee proven to sit in the signed state's
    ///               `next_sync_committee` slot; becomes the new trusted one
    ///           [14] period                        — the sync-committee period the
    ///               hand-off advances into (monotonic anti-replay)
    ///
    ///         ⚠ The 12 accumulator limbs are the recursion's KZG accumulator.
    ///         `ZKHALO2VERIFYWITHVK` verifies the aggregation proof with a plain
    ///         SHPLONK check but does NOT pair the accumulator against the SRS
    ///         `[s]·G2`, so acceptance does not (yet) cryptographically enforce
    ///         the folded shard/step proofs — see `submitRotate`.
    uint256 constant ROTATE_ACCUMULATOR_LIMBS = 12;
    struct RotatePI {
        uint256 currentCommitteeCommitment;
        uint256 nextCommitteeCommitment;
        uint64  period;
    }

    uint256 _ownerPubkey;

    // The Ethereum chain this light client follows, used as the anchor-set key
    // so it matches `USDCBridge._acceptedBlockHash[chainId][blockHash]`.
    uint256 _l1ChainId;

    // Optional sink (a USDCBridge) that receives every proven anchor. 0 = local
    // only. Owner-settable; carried through `updateCode` so a VkBlob rotation
    // does not drop the sink (unlike the previous fail-closed convention).
    address _usdcBridge;

    // Head (last accepted finality update).
    uint64  _finalizedSlot;
    uint256 _finalizedBeaconRoot;
    uint256 _finalizedExecutionBlockHash;
    uint64  _updatesApplied;

    // The trusted sync-committee Poseidon commitment for the current period.
    // Every step proof's committee_commitment (PI #5) must equal this — that is
    // the join that pins the aggregated signers to the chain-anchored committee.
    // Fail-closed: 0 rejects all updates until bootstrapped.
    uint256 _currentCommittee;

    // The sync-committee period `_currentCommittee` belongs to. Monotonic —
    // `submitRotate` only advances to a strictly later period. Bootstrapped with
    // the weak-subjectivity checkpoint's period.
    uint64 _committeePeriod;

    // Whether the owner may still advance the committee directly via
    // `setCommitteeCommitment`, bypassing the rotate proof. True while
    // bootstrapping (before the rotate VkBlob is emitted / while syncing from the
    // checkpoint); `disableOwnerRotation()` clears it permanently, which is the
    // step that reduces the committee-chain trust assumption from "owner key" to
    // "the rotate proof". One-way on purpose (mirrors USDCBridge's
    // `_ownerAnchorsEnabled`): a re-enable switch would leave the owner key on
    // the path regardless.
    bool _ownerRotationEnabled = true;

    // How many times `reAnchorCommittee` has fired (telemetry / alerting).
    // Under a live relayer SLA this stays 0.
    uint64 _reAnchorsApplied;

    // Canonicality registry: proven execution hashes with the Ethereum slot
    // they were admitted at. 0 = absent. Live iff slot >= head - SLOTS_PER_YEAR.
    mapping(uint256 => uint64) _provenEthSlot;
    // Insertion FIFO used to delete expired keys (lazy `_isLive` is authoritative).
    mapping(uint64 => uint256) _provenQueue;
    uint64 _provenQueueHead;
    uint64 _provenQueueTail;

    modifier onlyOwnerPubkey() {
        require(msg.pubkey() == _ownerPubkey, ERR_NOT_OWNER);
        _;
    }

    /// @param pubkey                — owner key for admin / bootstrap ops.
    /// @param l1ChainId             — EIP-155 id of the followed L1 (anchor key).
    /// @param bootstrapCommittee    — weak-subjectivity sync-committee Poseidon
    ///                                commitment for the starting period. Pass 0
    ///                                to leave unset (rejects all updates until
    ///                                `setCommitteeCommitment`).
    /// @param bootstrapPeriod       — the sync-committee period `bootstrapCommittee`
    ///                                belongs to (the rotate monotonicity baseline).
    constructor(
        uint256 pubkey,
        uint256 l1ChainId,
        uint256 bootstrapCommittee,
        uint64  bootstrapPeriod
    ) accept {
        _ownerPubkey = pubkey;
        _l1ChainId = l1ChainId;
        _currentCommittee = bootstrapCommittee;
        _committeePeriod = bootstrapPeriod;
    }

    modifier accept() {
        tvm.accept();
        _;
    }

    function ensureBalance() private pure {
        if (address(this).balance >= MIN_BALANCE) { return; }
        gosh.mintshellq(MIN_BALANCE);
    }

    // ========================================================
    // Light-client update (consumer side of opcode 0xC7 0x4A)
    // ========================================================

    /// @notice Verifies a beacon sync-step proof and, if it advances the head,
    ///         registers the finalized execution block hash as canonical. Fully
    ///         permissionless: the proof is the authorization, a bad proof only
    ///         wastes the light client's own gas.
    /// @param proof        — SHPLONK proof bytes (no header), the `proof_cell`
    ///                        operand of ZKHALO2VERIFYWITHVK.
    /// @param publicInputs — the circuit instance column: 10 × 32-byte LE Fr in
    ///                        the `step.rs::pack_step_instances` order. Verified
    ///                        verbatim; fields read at fixed offsets.
    function submitUpdate(bytes proof, bytes publicInputs) public {
        // Cheap parse + sanity BEFORE accept (pre-accept gas budget).
        StepPI pi = _parsePublicInputs(publicInputs);
        // Supermajority is enforced in-circuit; re-assert cheaply as documented
        // defence-in-depth (3·p ≥ 2·512 ⇔ p ≥ 342).
        require(3 * uint256(pi.participation) >= 2 * SYNC_COMMITTEE_SIZE, ERR_LOW_PARTICIPATION);
        // Fail-closed until bootstrapped, and pin the signers to the anchored
        // committee (rotate ↔ step join).
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        require(pi.committeeCommitment == _currentCommittee, ERR_WRONG_COMMITTEE);
        // Head only moves forward. A slot behind the head is still admitted
        // when its execution hash is not yet in the proven set (late-register
        // a skipped checkpoint of the *current* committee). Same-slot replay
        // and already-proven hashes revert ERR_STALE_UPDATE.
        bool advancing = pi.finalizedSlot > _finalizedSlot;
        bool late = pi.finalizedSlot < _finalizedSlot
            && !_isLive(pi.executionBlockHash);
        require(advancing || late, ERR_STALE_UPDATE);
        if (late) {
            require(_withinYear(pi.finalizedSlot), ERR_STALE_UPDATE);
        }

        // accept() must precede the halo2 verify: ZKHALO2VERIFYWITHVK is a
        // multi-second WASM extern that vastly exceeds the external-message
        // pre-accept gas limit.
        tvm.accept();
        require(
            gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof),
            ERR_INVALID_ZKPROOF
        );

        ensureBalance();

        if (advancing) {
            _finalizedSlot = pi.finalizedSlot;
            _finalizedBeaconRoot = pi.finalizedBeaconRoot;
            _finalizedExecutionBlockHash = pi.executionBlockHash;
            _updatesApplied += 1;
            _evictExpired();
            _pushExecHash(pi.executionBlockHash, pi.finalizedSlot);
            address addrExtern = address.makeAddrExtern(HeadUpdatedEmit, bitCntAddress);
            emit HeadUpdated{dest: addrExtern}(
                pi.finalizedSlot, pi.finalizedBeaconRoot, pi.executionBlockHash, pi.committeeCommitment
            );
        } else {
            _evictExpired();
            _pushExecHash(pi.executionBlockHash, pi.finalizedSlot);
            address addrExtern = address.makeAddrExtern(CheckpointBackfilledEmit, bitCntAddress);
            emit CheckpointBackfilled{dest: addrExtern}(pi.finalizedSlot, pi.executionBlockHash);
        }
    }

    // ========================================================
    // Committee rotation (rotate ↔ step join — trustless committee chain)
    // ========================================================

    /// @notice Advances the trusted sync committee by verifying a **rotate**
    ///         proof. This is what makes the committee chain trustless from the
    ///         weak-subjectivity checkpoint: `submitUpdate` only trusts a
    ///         committee equal to `_currentCommittee`, and this is the only
    ///         permissionless way that value moves.
    ///
    ///         The rotate proof (`eth-light-client-prover` recursive rotate, M6)
    ///         establishes, all bound in-circuit, that (a) the committee whose
    ///         2-level Poseidon commitment is `current_commit` signed an attested
    ///         beacon header with a supermajority — this commitment is bound to a
    ///         folded step proof, not witnessed freely (bls-bind); and (b) the
    ///         committee whose commitment is `next_commit` occupies that signed
    ///         state's `next_sync_committee` slot (`htr(SyncCommittee)` + branch @
    ///         gindex 87), anchored to the step's attested `state_root`
    ///         (bls-state-root). So proving `current_commit` == the current
    ///         trusted committee is proof `next_commit` is its legitimate
    ///         successor — no on-chain state_root plumbing needed.
    ///
    ///         ⚠ SOUNDNESS — TRUSTLESS ONLY ON A DECIDER-ENABLED NODE. The root
    ///         is a recursive 2-to-1 aggregation whose validity rests on its
    ///         12-limb KZG accumulator (PI [0..12)). A stock `ZKHALO2VERIFYWITHVK`
    ///         runs a plain SHPLONK verify and does NOT pair that accumulator
    ///         against the SRS `[s]·G2`. The opcode **decider extension**
    ///         (tvm-sdk#284: VkBlob byte 11 `accumulator_limbs = 12` ⇒ pairs
    ///         PI[0..12] vs the embedded Hermez `[s]·G2`; reference
    ///         `eth-light-client-prover/examples/rotate_decider_check.rs`) closes
    ///         this. The embedded `ROTATE_VK_BLOB` already sets that flag.
    ///         tvm-sdk#284 co-deploys with this contract, so an accepted rotate
    ///         enforces the folded shard/step proofs. Call
    ///         `disableOwnerRotation()` at the deposit flip
    ///         (`scripts/ursus/flip_deposit_to_light_client.md`).
    ///
    ///         Permissionless: the proof is the authorization.
    /// @param proof        — SHPLONK proof bytes, the `proof_cell` operand.
    /// @param publicInputs — 15 × 32-byte LE Fr: 12 accumulator limbs then
    ///                       [current_commit, next_commit, period].
    function submitRotate(bytes proof, bytes publicInputs) public {
        RotatePI pi = _parseRotatePublicInputs(publicInputs);
        // Chain to the trusted committee; fail-closed until bootstrapped.
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        require(pi.currentCommitteeCommitment == _currentCommittee, ERR_WRONG_COMMITTEE);
        // Monotonic period — reject stale / replayed hand-offs.
        require(pi.period > _committeePeriod, ERR_STALE_PERIOD);

        tvm.accept();
        require(
            gosh.zkhalo2VerifyWithVK(ROTATE_VK_BLOB, publicInputs, proof),
            ERR_INVALID_ZKPROOF
        );

        ensureBalance();

        uint256 from = _currentCommittee;
        _currentCommittee = pi.nextCommitteeCommitment;
        _committeePeriod = pi.period;

        address addrExtern = address.makeAddrExtern(CommitteeRotatedEmit, bitCntAddress);
        emit CommitteeRotated{dest: addrExtern}(pi.period, from, pi.nextCommitteeCommitment);
    }

    // ========================================================
    // Epoch ancestry (31/32 non-checkpoint execution hashes)
    // ========================================================

    /// @notice Pushes the execution parent-hash chain of an already-proven
    ///         checkpoint into the one-year window / `USDCBridge`.
    ///         `headerRlps[0]` must keccak256 to a proven checkpoint; each next
    ///         header's keccak256 must equal the previous header's RLP
    ///         `parentHash`. At most 32 headers (checkpoint + 31 parents).
    ///         Permissionless: the keccak + parent links are the authorization.
    ///         Parent links are compared in Ethereum byte order — the order
    ///         `parentHash` is written in — and re-packed by `_piForm` only
    ///         where they meet the store.
    ///
    ///         **Cannot execute on Acki Nacki today.** `EthKeccak` is software
    ///         keccak and one permutation measured 12.91M gas against a 10M
    ///         per-transaction limit (p20/p21), so a single 642-byte header
    ///         already exceeds the budget and a 32-header walk is ~2e9 gas.
    ///         `EthKeccak` also needs two sold fixes before it computes at all
    ///         (`uint64[5] bc` is zero-length, `_rotl` overflows on `uint64`).
    ///         Ancestry needs a keccak-256 builtin in the node, or the parent
    ///         chain proven in-circuit. Until then only the epoch checkpoint is
    ///         anchored, 1 block of 32. Measured on shellnet 2026-09-11,
    ///         gosh-sh/bridge#36.
    function submitAncestry(bytes[] headerRlps) public {
        require(headerRlps.length >= 2, ERR_BAD_ANCESTRY);
        require(headerRlps.length <= 32, ERR_ANCESTRY_TOO_LONG);
        tvm.accept();
        uint256 checkpoint = _piForm(EthKeccak.hash(headerRlps[0]));
        require(_isLive(checkpoint), ERR_UNKNOWN_CHECKPOINT);
        uint64 ckptSlot = _provenEthSlot[checkpoint];
        _evictExpired();
        uint256 want = EthKeccak.rlpParentHash(headerRlps[0]);
        uint64 added = 0;
        uint i;
        for (i = 1; i < headerRlps.length; i++) {
            uint256 h = EthKeccak.hash(headerRlps[i]);
            require(h == want, ERR_BAD_ANCESTRY);
            uint256 key = _piForm(h);
            if (!_isLive(key)) {
                _pushExecHash(key, ckptSlot);
                added += 1;
            }
            want = EthKeccak.rlpParentHash(headerRlps[i]);
        }
        ensureBalance();
        address addrExtern = address.makeAddrExtern(AncestryAcceptedEmit, bitCntAddress);
        emit AncestryAccepted{dest: addrExtern}(checkpoint, added);
    }

    function _yearCutoff() private view returns (uint64) {
        if (_finalizedSlot <= SLOTS_PER_YEAR) {
            return 0;
        }
        return _finalizedSlot - SLOTS_PER_YEAR;
    }

    function _withinYear(uint64 ethSlot) private view returns (bool) {
        return ethSlot >= _yearCutoff();
    }

    function _isLive(uint256 h) private view returns (bool) {
        uint64 s = _provenEthSlot[h];
        return s != 0 && _withinYear(s);
    }

    function _rev16(uint256 v) private pure returns (uint256 r) {
        uint i;
        for (i = 0; i < 16; i++) {
            r = (r << 8) | (v & 0xff);
            v >>= 8;
        }
    }

    /// @dev Anchors are keyed the way the step circuit publishes them, not the
    ///      way Ethereum writes them. `node_hi_lo` reads each 16-byte half of
    ///      the 32-byte hash little-endian, so `submitUpdate` stores
    ///      `(LE(h[0..16]) << 128) | LE(h[16..32])`, and that is also the word
    ///      the bridge holds and the deposit public inputs carry. `EthKeccak`
    ///      returns Ethereum byte order, so every keccak result must be
    ///      re-packed before it reaches `_provenEthSlot`. Name and body match
    ///      `acki-nacki` `181b0c6a`, the code deployed on shellnet.
    function _piForm(uint256 h) private pure returns (uint256) {
        return (_rev16(h >> 128) << 128) | _rev16(h & ((uint256(1) << 128) - 1));
    }

    function _pushExecHash(uint256 h, uint64 ethSlot) private {
        // Duplicate live writes are no-ops so `submitAncestry` can re-walk.
        if (_isLive(h)) {
            return;
        }
        require(ethSlot != 0, ERR_STALE_UPDATE);
        _provenEthSlot[h] = ethSlot;
        _provenQueue[_provenQueueHead] = h;
        _provenQueueHead += 1;
        _notifySink(h);
    }

    /// Compact the FIFO: expired hashes are deleted and forgotten at the sink;
    /// still-live hashes are re-queued. Caps work per tx so a long pause does
    /// not OOG; `_isLive` already hides anything past the cutoff.
    function _evictExpired() private {
        uint16 n = 0;
        uint64 dropped = 0;
        while (_provenQueueTail < _provenQueueHead && n < MAX_EVICT_PER_TX) {
            uint256 h = _provenQueue[_provenQueueTail];
            delete _provenQueue[_provenQueueTail];
            _provenQueueTail += 1;
            n += 1;
            uint64 s = _provenEthSlot[h];
            if (s != 0 && _withinYear(s)) {
                _provenQueue[_provenQueueHead] = h;
                _provenQueueHead += 1;
            } else if (s != 0) {
                delete _provenEthSlot[h];
                _forgetSink(h);
                dropped += 1;
            }
        }
        if (dropped != 0) {
            address addrExtern = address.makeAddrExtern(AnchorsExpiredEmit, bitCntAddress);
            emit AnchorsExpired{dest: addrExtern}(dropped);
        }
    }

    /// @notice Re-send an already-proven hash to `USDCBridge`. Recovers a
    ///         dropped `acceptBlockHashFromLightClient` (bounce, mis-set sink,
    ///         push that landed before `setLightClient`). Does not re-prove.
    ///         `blockHash` is the stored anchor key (`_piForm` packing), not
    ///         the Ethereum byte order a block explorer shows.
    function rePushAnchor(uint256 blockHash) public {
        require(_isLive(blockHash), ERR_NOT_PROVEN);
        tvm.accept();
        _notifySink(blockHash);
        ensureBalance();
    }

    function _notifySink(uint256 h) private {
        // An unset `_usdcBridge` is `addr_none` (never written), which is not
        // `address(0)`: comparing only against `address(0)` sent the message
        // to `addr_none`, the action phase aborted with result code 34 and
        // the whole `submitUpdate` rolled back on a standalone deployment.
        if (!_usdcBridge.isNone() && _usdcBridge != address(0)) {
            // bounce: true so a rejected sink returns the 1 vmshell and
            // `onBounce` emits. The hash stays proven locally — `rePushAnchor`
            // is the retry (submitUpdate would hit ERR_STALE_UPDATE).
            IAcceptedBlockHashSink(_usdcBridge).acceptBlockHashFromLightClient{
                value: 1 vmshell,
                bounce: true,
                flag: 1
            }(_l1ChainId, h);
        }
    }

    function _forgetSink(uint256 h) private {
        if (!_usdcBridge.isNone() && _usdcBridge != address(0)) {
            IAcceptedBlockHashSink(_usdcBridge).forgetBlockHashFromLightClient{
                value: 1 vmshell,
                bounce: true,
                flag: 1
            }(_l1ChainId, h);
        }
    }

    onBounce(TvmSlice /*body*/) external {
        address addrExtern = address.makeAddrExtern(AnchorPushBouncedEmit, bitCntAddress);
        emit AnchorPushBounced{dest: addrExtern}();
    }

    // ========================================================
    // Admin
    // ========================================================

    function setPubkey(uint256 pubkey) public onlyOwnerPubkey accept {
        ensureBalance();
        _ownerPubkey = pubkey;
    }

    /// @notice Sets the deposit-path sink that receives proven anchors (0 to
    ///         disable the push and keep the registry local-only). Carried
    ///         through `updateCode` / `onCodeUpgrade`.
    function setUsdcBridge(address usdcBridge) public onlyOwnerPubkey accept {
        ensureBalance();
        _usdcBridge = usdcBridge;
    }

    /// @notice Bootstraps / owner-advances the trusted sync-committee commitment.
    ///         This is the trust seam that `submitRotate` replaces: while
    ///         `_ownerRotationEnabled` the owner may set the committee directly
    ///         (weak-subjectivity checkpoint, or syncing before the rotate VkBlob
    ///         is emitted). `disableOwnerRotation()` turns this off permanently,
    ///         leaving the rotate proof as the only committee-advance path.
    /// @param committeeCommitment — the period's sync-committee Poseidon commitment.
    /// @param period             — the period it belongs to (rotate monotonicity baseline).
    function setCommitteeCommitment(uint256 committeeCommitment, uint64 period)
        public onlyOwnerPubkey accept
    {
        require(_ownerRotationEnabled, ERR_OWNER_ROTATION_DISABLED);
        ensureBalance();
        _currentCommittee = committeeCommitment;
        _committeePeriod = period;
    }

    /// @notice Permanently drops the *routine* owner committee path
    ///         (`setCommitteeCommitment`). After this, `submitRotate` is the only
    ///         permissionless writer; the owner retains `reAnchorCommittee` as the
    ///         exceptional weak-subjectivity recovery hop (logged, does not write
    ///         exec hashes). This is the call that turns everyday committee
    ///         advance from "trusted by the owner key" into "the rotate proof".
    /// @dev One-way, with no re-enable (mirrors USDCBridge.disableOwnerAnchors).
    ///      Requires the committee to be bootstrapped first. Do not call until
    ///      the opcode decider is on every node; the relayer SLA still matters
    ///      because re-anchor is a trust hop, not a substitute for liveness.
    function disableOwnerRotation() public onlyOwnerPubkey accept {
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        ensureBalance();
        _ownerRotationEnabled = false;
    }

    /// @notice Weak-subjectivity recovery: owner sets a fresh committee after
    ///         `disableOwnerRotation()`, when `submitRotate` cannot trustlessly
    ///         bridge a gap of more than one period. Not the routine bootstrap
    ///         path (`setCommitteeCommitment`); an exceptional, logged hop that
    ///         reintroduces owner trust for one checkpoint. Does NOT write
    ///         execution-block hashes — those still require a step proof.
    ///         Source the new commitment from ≥ 2 independent checkpoint-sync
    ///         providers (M0 §7).
    /// @dev Armed only after disable (`ERR_REANCHOR_NOT_ARMED` otherwise).
    ///      `period` must not go backwards. Same committee+period reverts
    ///      `ERR_REANCHOR_NOOP`.
    /// @param committeeCommitment — Poseidon commitment of the new WS committee.
    /// @param period             — period it belongs to (must be ≥ current).
    function reAnchorCommittee(uint256 committeeCommitment, uint64 period)
        public onlyOwnerPubkey accept
    {
        require(!_ownerRotationEnabled, ERR_REANCHOR_NOT_ARMED);
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        require(committeeCommitment != 0, ERR_COMMITTEE_UNSET);
        require(period >= _committeePeriod, ERR_STALE_PERIOD);
        require(
            committeeCommitment != _currentCommittee || period != _committeePeriod,
            ERR_REANCHOR_NOOP
        );
        ensureBalance();
        uint256 from = _currentCommittee;
        _currentCommittee = committeeCommitment;
        _committeePeriod = period;
        _reAnchorsApplied += 1;
        address addrExtern = address.makeAddrExtern(CommitteeReAnchoredEmit, bitCntAddress);
        emit CommitteeReAnchored{dest: addrExtern}(period, from, committeeCommitment);
    }

    /// @notice Replace code while keeping the anchored committee, head, and
    ///         proven-hash set. Needed because both VkBlobs live in code —
    ///         a step/rotate re-emit or an Ethereum gindex shift cannot be a
    ///         fresh deploy without spending the weak-subjectivity trust
    ///         step again. Mirrors `USDCBridge.updateCode`.
    function updateCode(TvmCell newcode, TvmCell userCell) public onlyOwnerPubkey accept {
        ensureBalance();
        TvmCell migrationCell = abi.encode(
            _ownerPubkey,
            _l1ChainId,
            _usdcBridge,
            _finalizedSlot,
            _finalizedBeaconRoot,
            _finalizedExecutionBlockHash,
            _updatesApplied,
            _currentCommittee,
            _committeePeriod,
            _ownerRotationEnabled,
            _reAnchorsApplied,
            _provenEthSlot,
            _provenQueue,
            _provenQueueHead,
            _provenQueueTail,
            userCell
        );
        tvm.commit();
        tvm.setcode(newcode);
        tvm.setCurrentCode(newcode);
        onCodeUpgrade(migrationCell);
    }

    function onCodeUpgrade(TvmCell cell) private {
        tvm.accept();
        tvm.resetStorage();
        (uint256 pubkey,
         uint256 l1ChainId,
         address usdcBridge,
         uint64 finalizedSlot,
         uint256 finalizedBeaconRoot,
         uint256 finalizedExecutionBlockHash,
         uint64 updatesApplied,
         uint256 currentCommittee,
         uint64 committeePeriod,
         bool ownerRotationEnabled,
         uint64 reAnchorsApplied,
         mapping(uint256 => uint64) provenEthSlot,
         mapping(uint64 => uint256) provenQueue,
         uint64 provenQueueHead,
         uint64 provenQueueTail,
         TvmCell userCell)
            = abi.decode(cell, (
                uint256, uint256, address, uint64, uint256, uint256, uint64,
                uint256, uint64, bool, uint64,
                mapping(uint256 => uint64), mapping(uint64 => uint256), uint64, uint64,
                TvmCell
            ));
        _ownerPubkey = pubkey;
        _l1ChainId = l1ChainId;
        _usdcBridge = usdcBridge;
        _finalizedSlot = finalizedSlot;
        _finalizedBeaconRoot = finalizedBeaconRoot;
        _finalizedExecutionBlockHash = finalizedExecutionBlockHash;
        _updatesApplied = updatesApplied;
        _currentCommittee = currentCommittee;
        _committeePeriod = committeePeriod;
        _ownerRotationEnabled = ownerRotationEnabled;
        _reAnchorsApplied = reAnchorsApplied;
        _provenEthSlot = provenEthSlot;
        _provenQueue = provenQueue;
        _provenQueueHead = provenQueueHead;
        _provenQueueTail = provenQueueTail;
    }

    // ========================================================
    // Getters
    // ========================================================

    /// @notice The current light-client head.
    function getHead() external view returns (
        uint64  finalizedSlot,
        uint256 finalizedBeaconRoot,
        uint256 executionBlockHash,
        uint256 committeeCommitment,
        uint64  updatesApplied
    ) {
        return (
            _finalizedSlot,
            _finalizedBeaconRoot,
            _finalizedExecutionBlockHash,
            _currentCommittee,
            _updatesApplied
        );
    }

    /// @notice Whether a finalized execution block hash has been proven canonical.
    function isProvenExecutionBlockHash(uint256 blockHash) external view returns (bool) {
        return _isLive(blockHash);
    }

    /// @notice Drop-in canonicality query matching `USDCBridge.isAcceptedBlockHash`:
    ///         true only for the followed L1 and a live (in-window) proven hash.
    function isAcceptedBlockHash(uint256 chainId, uint256 blockHash) external view returns (bool) {
        return chainId == _l1ChainId && _isLive(blockHash);
    }

    /// @notice Ethereum slots kept in the proven set (`365d / 12s`).
    function slotsPerYear() external pure returns (uint64) {
        return SLOTS_PER_YEAR;
    }

    /// @notice Occupancy of the eviction FIFO (live + not-yet-compacted expired).
    function provenQueueLen() external view returns (uint64) {
        return _provenQueueHead - _provenQueueTail;
    }

    function getConfig() external view returns (uint256 l1ChainId, address usdcBridge, uint256 ownerPubkey) {
        return (_l1ChainId, _usdcBridge, _ownerPubkey);
    }

    /// @notice The committee-chain trust state. `ownerRotationEnabled` is the one
    ///         that matters: while true the effective trust root for the committee
    ///         is the owner key regardless of the rotate proof. `reAnchorsApplied`
    ///         should stay 0 under a live relayer SLA — each increment is a
    ///         logged weak-subjectivity hop.
    function getCommitteeState() external view returns (
        uint256 currentCommittee,
        uint64  committeePeriod,
        bool    ownerRotationEnabled,
        uint64  reAnchorsApplied
    ) {
        return (_currentCommittee, _committeePeriod, _ownerRotationEnabled, _reAnchorsApplied);
    }

    function getVersion() external pure returns (string, string) {
        return (version, "EthBeaconLightClient");
    }

    // ========================================================
    // Halo2 public-inputs decode (consumer side of opcode 0xC7 0x4A)
    // ========================================================

    /// @dev Reads the 10 step public inputs out of the PROVEN blob (the proof was
    ///      verified over this exact byte string, so every value is proof-bound).
    ///      Layout = 10 × 32-byte LE Fr; 32-byte roots are split hi/lo (hi first)
    ///      exactly as `USDCBridge._parseBlockHash` reassembles the deposit hash.
    function _parsePublicInputs(bytes publicInputs) private pure returns (StepPI pi) {
        TvmSlice s = publicInputs.toSlice();
        uint256[] fr;
        for (uint k = 0; k < 10; k++) {
            uint256 v = 0;
            for (uint i = 0; i < 32; i++) {
                if (s.bits() < 8) { s = s.loadRef().toSlice(); }
                v |= (uint256(uint8(s.loadUint(8))) << (8 * i));   // little-endian
            }
            fr.push(v);
        }
        // Slots / participation are single u64-range field elements.
        require(fr[0] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);
        require(fr[1] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);
        require(fr[4] <= SYNC_COMMITTEE_SIZE, ERR_BAD_PUBLIC_INPUTS);

        pi.attestedSlot         = uint64(fr[0]);
        pi.finalizedSlot        = uint64(fr[1]);
        pi.finalizedBeaconRoot  = (fr[2] << 128) | fr[3];
        pi.participation        = uint64(fr[4]);
        pi.committeeCommitment  = fr[5];
        pi.executionBlockHash   = (fr[6] << 128) | fr[7];
        pi.attestedStateRoot    = (fr[8] << 128) | fr[9];
    }

    /// @dev Reads the 15 rotate public inputs out of the PROVEN blob. Layout =
    ///      15 × 32-byte LE Fr: 12 KZG accumulator limbs, then
    ///      [current_committee_commitment, next_committee_commitment, period].
    ///      Same little-endian Fr decoding as `_parsePublicInputs`; the two
    ///      commitments are full-width field elements (Poseidon outputs),
    ///      `period` is a small u64. The 12 accumulator limbs are skipped here
    ///      (they gate the recursion, not the committee hand-off) — see the
    ///      soundness note on `submitRotate`.
    function _parseRotatePublicInputs(bytes publicInputs) private pure returns (RotatePI pi) {
        TvmSlice s = publicInputs.toSlice();
        uint256[] fr;
        for (uint k = 0; k < ROTATE_ACCUMULATOR_LIMBS + 3; k++) {
            uint256 v = 0;
            for (uint i = 0; i < 32; i++) {
                if (s.bits() < 8) { s = s.loadRef().toSlice(); }
                v |= (uint256(uint8(s.loadUint(8))) << (8 * i));   // little-endian
            }
            fr.push(v);
        }
        uint acc = ROTATE_ACCUMULATOR_LIMBS;
        require(fr[acc + 2] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);
        pi.currentCommitteeCommitment = fr[acc];
        pi.nextCommitteeCommitment    = fr[acc + 1];
        pi.period                     = uint64(fr[acc + 2]);
    }

    // ZK verifying key (VkBlob) for the beacon light-client STEP circuit —
    // Base v1 (Blake2b transcript), 8 public inputs. Hermez SRS (k=19), Base
    // circuit shape num_advice_per_phase=[132]. Source:
    //   acki-nacki-bridge/eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin
    //   sha256 2d66c2058e615be1f6459d6a01f05dd21cf2ca99ba8ac9935739f5507d320fab
    // Magic "VKBLOB\x00\x00" + version 1.
    //
    // To rotate: regenerate the fixture in acki-nacki-bridge
    //   (cd eth-light-client-prover && cargo run --release --example export_step_vk_blob),
    // then run acki-nacki-bridge/scripts/embed_step_vk_blob.py <this file> and
    // refresh the tvm-sdk opcode fixtures
    //   (acki-nacki-bridge/scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh).
    // Do NOT hand-edit the hex.
    bytes constant VK_BLOB =
        hex"564b424c4f4200000100000000000000830000007b226b223a31392c226e756d5f6164766963655f7065725f7068617365223a5b3133325d2c226e75"
        hex"6d5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b342c302c305d2c226c6f6f6b75705f626974"
        hex"73223a31382c226e756d5f696e7374616e63655f636f6c756d6e73223a317d0a4400000213000000008600000009c4136feb037d5e5bd0beeadc2841"
        hex"61b2161d91f9d75162eb007d6068c2d12021f5b3c0e125f6b3c033dd30a9ababaddd126d3580e790612a7f34f9795c0523d819077e2116fe0dea6f59"
        hex"77db053ab97f0b2b0014b072d3dda1a6a7921e26188c97ad695658354d7f793a3d38c0cc75b641e14623e5e3eb917b724953a8ab13caf6084e3a31ea"
        hex"bff2618798f74fc55602310b40770c696cf8d820469913fe17028fe83020d4da7a1ba4954284016f8ea7bd4b7bc51882b99ecd0983c0f8550870704d"
        hex"f9576571fdd11fda61ce6ec123d14078b2dcaea8d097f6e33910ab5225dcabb167fcb5b7f00e1f4ebb3efc915c015a344054b0b75f7a7a424a822fc6"
        hex"03a44a66c0c3b311ec05243d43e50b129a0e9dec71675b9f3419d95801e4fa132428d31df64032f01b51028ec4d34a51e57a2349b06865bf54696e9d"
        hex"fe7414100f730e06dcc1cc9152250bea4047ad08f71abc3bf60ba968bbb98a80931b381319d456686158835cdbad5aa185cd89d7dcf9b8437012f403"
        hex"6026a42b1216826a24299cf0e96b9b449416c8a54f761b63cf9fe468d97822f4561f695cd77e26862afe16f546e9b6d2191906b952f04107c669dcb8"
        hex"d4da1cc23f14622e93557332026900cb1f2bc072a2808e69bfcd35cd6f525d6bf55300f7d52a8a4398ef356016271e653aa6acc7f26778dec05173f4"
        hex"f7f05f7224a9ffaae9c437f1785743df22f65f8a0db294ed9af6547ae810d689d24c460f277a14ccc6c794e2f5bc74d210d58ba13f759a068101ff34"
        hex"cc28dd0e3afc54eadf8eaec1675f2b2add339c7b25f9658ff39f506813802e67105ffb4cb095acc3d0a981a35318f5244535346907ace597e73b4523"
        hex"c7672f2009e844807aebbc7addf37be67f2bcb8ba07cbe972d8dab6885b2b0007609ea00f78782c738d185ea5177a8ac7a84a2c799f104182c93c10f"
        hex"bc91ee695d80d2bd552f29ef0887b919c4453cacf38244cc2e1c63972e3b650a188a05d91241d0d5692551a5eb65f382681cafe96eb0e0330a434e9c"
        hex"232a6de36b7eabd3c72583dbb408226db30867252b4dda616b1e521d6675f801171a09b85e7d4e0adb24e2b85c3f414f703801adb85e14440c1e231c"
        hex"1856e79f0ea9a5839f26c72701296768a69dbf0f76a17c0ad57f1df124fbc45b51382fec13c5161a6318b64f930b21b03a7ff8efa9e59dfd5f452a67"
        hex"b1758779ee477904233082f406b7385653179730cee8e1e39697be48acaa60ec9c6db265452050cb28e6093c9a8e1e40981135d03f6568af86163929"
        hex"a29c9f6c600a49b2877552c505dec2bca3d00a3a3e6a0dcf24d5b6d927e133ecf0b0865d7a76206d80f3a3632dda17e7e7a0b2697812393ae28a34be"
        hex"c0f7e0c3caec97c857146a03999dbe911e7cb6baf82fd317d55c7162b0efaf869323289477f0d2fa0f8e00e9f7367daf19e0c2e373cb791e05ff5445"
        hex"4385e776c1c60b0f0245101b5bd9d3ebbf35e888192ae0a229d9014b72387b3b4ad97e102fbc7a3f80179454053ec90a29025b7d0638af0fb42ced28"
        hex"ff858f283d3b9f940dc7d2e23c55cdc02a44e8eb80e8619c0faa95346b069e5353eeaf2413b744e3c8e79d6a6228fc4b01aa6e7ed0503bb60e63b019"
        hex"3877077447d75a7b37aa16ee78cfa3a37050ca5c080011f85bf56b3230674476dd35af8a8132ac4f5f5c095650569e0b6668125dec7f73e6cc4c69a7"
        hex"0927c098ffef8c61ed12c4fb2d890bb99cd9e884ed7d4772b09cb2afacd98ea90af0ebbdf22770519cb0151ff6be715ec013d0ffa8bf5bff870e51a4"
        hex"afd7a2cb1f818c933524e25cef7cc70cdcb68429c82c13d572974e43a22f56eb933b13e7166d361f04a548d4146373ebe8f1a9b3a80576901c4a636a"
        hex"5bc349fdcff8ec4424eecf11cf3d5a402f0f0d071a99df1ece8a8503d7be7eda42c905cc995db7901ccd305c76d3a65f1675b666d87df21215aec783"
        hex"43a56a32cbe8b45671574a3510c72f63af7da177543747517d3d0e27ceb0ffff994ca9bc7aec3c0a77828b5b16d23003691443171814e08e746e2639"
        hex"2f40af4d12c62453d845801e2bd7a0fe25487f363aa76f03cb6a4199858b8005405ef7e0b81e392dca1a5c961984d3012f99929c1ddd1773319c43c3"
        hex"7263c7440de0c1a28b9621a47d9dcbec1e36927510810745995055f4ef4b1978f19480899cb4835ff489a5a3f10228be5d0fc5851f02128ea6c3936f"
        hex"86f46df18d2aeacddebbdebaa0aea4902f39a12568a3f1cb2f2c97ca8361a192bd643b70237053cf5df07f0542f580c2e28f590d776601c825ae7620"
        hex"3657762ec5810b6d957b10abccc70d20ab4dc68d98877836d00e8c74073370936f6bdc0c19e8f377fd559b4d69bb4f6340a1612654a60613312ccb11"
        hex"293f406fb1fd4dd2c76ffd40997baae770a9dac28f9ae04366c737f26002c342109d9d05d9e0e62f07e235decf7b831efcc89f4224fdd038b79a7501"
        hex"12f9bb4f289d5c029adf9466b6ac82cc1930002b721be303facf199a4b9e575a346639bc01450757464203660c10e3658edb70fe2a109d2ad28c6189"
        hex"d84c8eb4826d3ef509d51c4d3286d9194d1c08a604c549d4308d65d7f20ceb3a2ef9c479943edb3112ce47f379954eb0728665d02c6ca3bd8abd0faa"
        hex"a3a04af3d82d745fd6a17df80425f302edb78faa00c8d51bb68ee64d7fdad0cc26ca4ded78d97d78773cc2451b39458ea1a9906c9d229db46ea2cda6"
        hex"773211df735ef0638f75f9aae016504329de72a60f77e7c5818ec2c39f5822aa586b1e94827545ed38b4fd461ad522090031f31ede533341063c6827"
        hex"d2b07fca0a34c6e968463f40d44d0a6edb11534f026ff0ab7e020acccb38f83b20c496921053eac68e75b913ca9805aeabdd6741031b35509e83f92e"
        hex"e0740c5b19fec22aa16726b508e63f3934d4e8fff30d8e9929eb123562fff27ab125af5cb9b3a1a186b83ac61477c2c8428b8652796a66ed22ec3970"
        hex"d70b493bdac579442c9a1d9a899c832f8842cccd9b70cc27b68382e904b83d79306ba83e37a38b5567512d44245833e981ca91e7248aee169e989831"
        hex"0b0559f1b4592bea5ce0df977129f28f84ce8caf23b520fb00fa3d5d8c6f94351e0939a3d586e2e404c9f90cf0a83154a2a79abd64ae3f2cd11fd144"
        hex"1e53108c26b9adafd86129b9c59fa6536fb165018a5173b0ecc0808d862ffe23969f9bde1346409427087f573f52d02c8fc083869f968d8543ad4103"
        hex"fab1c8ce97051cc30dbdea26246712b51dffef4a48b54363c885718453160e65e8060a7c8b6dedc00f4a401c3cd6d4a06dad779a4340ce4362175960"
        hex"175c76826116d9723b5441222c0434f3045070018ee54987b0aae3ddc80310c91720ce935c8da7050f3fbaaa162e77b1db82c7eb9eb3fff16a83d849"
        hex"31305be43f3a2cedabd4ba108710cfc02740c3e6a0489841e28b6442b072d43c06e321432139b76da806a59f7677bdbc0545863271b926e217d90ea9"
        hex"bc42c681cca840823a883573dbcb97716720e69b28463c81de682d1523ba066a89da45b7557f712e78481e216207e8a0c9181c4e0eb0624c16c26260"
        hex"d4b7be8a160470c8dfe0f443e978694d887439dea01f045d1f56372d05a93f31d790742cad03903f9312d4cdcfc47d0f360f2a0d55522c9f2952e95d"
        hex"7316b846f1ac1f6c717de29e8abd11fd5a6e34a7856e57a876190a6103cb50f4403874ad1e72b71c27a8b31d22269c35afc56554b5ccf03e0d0ec528"
        hex"2dffe388dbc7aa6003ae2f3ac898f3024b9cbff9488d47f38a16906b66bd97011720c4ff084b0a6e617bd804077618739a74e65a23b7c3b77ae6838d"
        hex"9001a49e040f01488e028c6bac3b3465ca5bd472d06a22173d4bd0b240f332c8de3fb2fb045d51c178f7fe3db7a648b021bb86725dc868480b43532f"
        hex"d0d846cb4458a736238cd770281912678e15e0b4d1b54479bdc8bef306c53586067d9faf87afc6b52c36ac18dba4bafaf75da09d879e4119c6070ab3"
        hex"8cfa998b76d937bae8541898210b7b23e3971a02a9ce3283ac85339d1fb03f316cc681a179881140d1e3b0081cb436a1de0f201d92c00c5b75dcc380"
        hex"c2207d7f3c1456fe70a7cf5259c4a0f31df4ba0321d59f769c6cea8b1b9e18be46ed1c5296a7d38d3e1acb73fff58f0c0e48cf777b5b64c6089fc2e4"
        hex"e1d33d2c1631b0c6d7b7ead2aac3d6004535cb241dab197396d84c1c68f9845b021f77656c3224c1d39511cb230bc0b49810c01d1565ccc99f447b8a"
        hex"5fabddd09f820d45ae5f325da318d0a7b00b21dadba5c17428680a6153e3c422f4b4f96f4b8097cd405f9aea2bfeda93a9a5e1b1b3e00aa71080a68d"
        hex"cbe4ae355541b0000fd7c42779303a279c02c6b794bda4e25abc7ec51249f68f0268d8e5e312b97e93863b813cf8280761fd90ccd8c50e3e087f1ef0"
        hex"040ce47914d49c2d5a382fd9dd1600bd8ec9f970609c516691271778f95140a0265ebcb8a31897a7735758a75a935ce9d69a8b70502e4c87b57fdc69"
        hex"a2f923981553012d61a53f2c18f8d88da96c80cc06eafe2888197d1a75904667625c938c28c186896eba77796d2dd59a022b60e2d7fd54c72c34b0e6"
        hex"00cec41675d6ae9f088f61341d281e29b73f13f9be9384f7e94f35b9f6fce930b94bd8e91fbf573c23edb45e0904907dc23a99432cc0e702bc07b1fc"
        hex"ac9fd54389d7e6bd8c042d7f05eb09466533e3a090d1146d323f6cf7db6ddacd782db464ee207a2ac380d0981bee7cfb589440acfcc900dcee024bc2"
        hex"ae44a5bba845c8433346a53ef4e8d2ce004dacac33618432d0c9cb217f9c98fe176566c5779205f55a5fb2e36d0ba7210dffd7fc115478cb08929a05"
        hex"93d90604974bea2dbb3d75fcceb0dfb2faf8210d1e264e83220ff06099322b0474fe02e500512e389f86956be92cb7216ebdeda11223fb2fe3362164"
        hex"80b096a8983962e8727a1a965a1879f966bfe1cfca57e2d82d4b6eb11c903a6a437839aa392096f90b9047bb90755c9147872e46fd4b8942238c451c"
        hex"76969c84337355a0928304bc04aa721769c36ae83ffb58586de29a231d250e8c4f552334dadcba44d7baf788f509bedfc6f2cc7f1b05b53e50d735a9"
        hex"2b4b8b4740b6c49e8a022354f0a9b97116bad98855088bcfa5d583eb1502df87199889b9046cbae62386fd4c66dd1113577af8e5fc7a8a467757a976"
        hex"973222b6173d56a77f809617268c4e685caa271b5e89c132a1f36fb6f068f154c9c5721b246ff9587095edbe188f11834237a4d9ab22bae4eefb5248"
        hex"a8ac1797b16ade661c72c862a3d55044a432e31ae96769ebcd69d2fd1ca24aefb141c3352f48afbb141b37873f79b71fe39fbf5f6308fcb751da0dfe"
        hex"f2b797113b2decd289d733fa2a5a520affac15254a589d80a2e6655e04149190de2a30d7fdccceeb107c5fa1288e26d4e5bd19aa253e92df8ebcfdfa"
        hex"9d1070c515381293cb6dfe935106296a0250e44eaf8b05a07f4933332ecb169c86d581b36b17dc49de7b54077305301016e771ba1bb82f3b6495061f"
        hex"66e412a82ae3f5f3e53fabcde28e9df5066e1d401fab83efbb6fb00961aea657eb01eba5f4dd7bb7fff4e079ceb917627d7a4dc31457637fc2e0bc29"
        hex"ad37f068de17c4a9224999c63d050661678082e559486d9616e91a6bfba1fad5cdef4d97fdd764bf5b2c38652be043d0c49843e601a8d751134967a6"
        hex"01d6253e4d3219baf4e62ce1354b319023ed915bf59bb1e03d65952324667c6fb33d6302fce6411ae23cd7e86575743faa1641dd15c6a2eabf766d94"
        hex"035607c74b7e563952bee6d8018015f0b1a4037779e1d5626d4ec0df4a4f6fbf027a9b3f47aa6f7743ff1119959b9a7bd6378bdf4b1cd27d154b1ee4"
        hex"044042ff109b24e73138a5c4c73f282e7865405100e5a837e513fb68254b246894ccd3731cf182fdbf4814d850cf1e35e2f19c478c91cb6340bfb237"
        hex"900cc1614706ccf51317e1e796d8b29018fab04aed31967a674c604d114195a9dcee9dd732ec5adb03406b740b4a0694a010a262e2f9fb0c7a81a2cc"
        hex"854334f1bc142ce9ecf280bd25b38cd39b9b09a2433e31f07ce1af6275bfdc9b199780b60bac595c2c24e80b1710830a58a1fa12c588c330a82a6f56"
        hex"2d4c216a739095c40d603f3406639cbd06e84b8cccd507ab338a781ff46f0e576793d11b10ccb5e07ee4b54ae17fa47310b14c7cfad95efff84e6b67"
        hex"784257434c278feff1a43b0dbb2f2de83488f2341587aa9ad9d99bc6fa5cc4d04443d98cd04420c12013d00417515013f4f7b7e429d6c2027c799787"
        hex"e4064f6074583ad364ba2bdfbecc90c7f3f90a7e594ac3781428df659f9e74941f331f0926422e7e3c200139752058bb2cd0a8550e77911f03936210"
        hex"8cf9885c9e39fd4586f6411116afe301cef4a72c907133a0e253a5752357de20aae439878fc8799b929014f583ceb31ed76715b5b619a5ce7b2c3fa4"
        hex"177678c8b02a15911955268a1aa34417df77e5586121de165b3399e71b41c7111517d20cb9d63adcbd93bcca58e835c40f2ccd86eb613df9999b087a"
        hex"286dc6b127d16a34ac9d837167cbba4bbeb3d354277c4c145e6b6a5b8ade24e96908e46301268f5989c2afeba51039ac2da364dc762717b6b29d11f2"
        hex"7421863ba48e884816b7286b15dee26ef09bb3c5f2b0fe65bea0daeee07030fded2b91d808b354b226d62fe6ca3452680fd10d5e70619fd11b71c1e1"
        hex"ccfd1ba18f3fbdc7dccd7eac2bd8822dd0a702cd7c4d02578194f875025ac3607507156045906792523328580e0e05deb87f80412355cbf0db84960d"
        hex"ff482a7130ac49fa4f41e2c4c957685d0ce4f4e410e3f3d01a63d08586ff83ce6209c058751196366a999c3368f6c6092700891cbf97fa07cbc5c7f5"
        hex"a1956999863f6e93cbde6ff28a09b5bc3059881308bfca2725a22e4b8565641a23e6607017f04a21e7824926d317375796c66e672f379b189d06f47d"
        hex"2e79cec4f04aeea07afeef2934b835cae27141d1084ccef506d1dc2a63668982e8e6c1d395da6d7fcbbdf981168f458a6ca09ef42501f14b16caa038"
        hex"58006f512698134619d84b522b989ec1e9de15d80bd6767110d3aa59138678f5271afa8e5cb133fd36ca932f40639f0e9d95b7cb0cd640198fcf498f"
        hex"1dda60c163c4e87421d7e3f6c89c0d958bb2afc8351f1001cb41525bbf4bc97d11563707601bc34e848dd1910fe8184f60db4ac4f13e22b8a0963c6c"
        hex"3d7072ce230fcc2639febf470a259e4b2b215200b3f630714c06d026be47e4c3bcc501e52e1e25c0e4d3f68547cbd05baa22371e6050dcc06f8f1f69"
        hex"6ff8f9cc548eaaa90e8a0b63c58c8c6ffefa89733a50dc47a170a47f27f019d2b15110408d97a8f3192b46f2d3c5aceafc63d25818915c3cb965b226"
        hex"0e4d5e5442fa62f508c1fb4c1a6d7cff13a976f280d89cae2a18ed8976562dd47d4ff79f7972800fc4fb833a0d4781203cbfb87126ce9cb5cb8db0ed"
        hex"576e27a4b1c68ff6c565c13477d3207b2e86c6c4f309464478113887669c890f6731332c6cfb5850a3af17560d31952d16b94c3b6299a13ebd222864"
        hex"d05df766689b24f22e529ebd57d2aac62d6fdb831f9d2cd05ece016b507ffba643ad2d3182cd76b01346d35c055ba0e18ad65e64011c252423cf0b8e"
        hex"95c5aba197594e8abfd799579490fb43f3dbdb9c1105c9880d60e0a1c254693adb8fab862257f6dd63a748b96e7805e41a672bac7f258c7303cf8dd7"
        hex"d234976538a2d69bc3a7aa95c0488a35ba841ae09d1cf2d26763e2a31726515c571c3ae2f67965398c7346ae499631c23d052b104ff4c5213d5d1fd0"
        hex"21f17a0e4fa700b22d9145623132c57e4fd378615fc1597680bfd7b3dffec813010908499835d92bd0e646e8b6fffefe10b62c46df2bc95cb80492fd"
        hex"b9d28b3327e7c995fa8bf8ea916a3836fc036b52ac3afb8197bf8efd3b8a933395cd703203c4b4e5e006489a409c75f5b849fbc2ee231865145c2f42"
        hex"72241591719cea152b7533600041a70a8f0497544091ba73f0894516424a47f6dcca7d042473136718af01f5bab178c262517c72472598b5fc1ea6c7"
        hex"bc2fac71f1ebf219351beeeb1a47be27105d4c96c53a0f41d68517b49fb8e293053647f8721b1819d1dc26eb1048ae18ca7e06548ecfce0191997e03"
        hex"16441febee5b13e956d1b352d1cbf16e15a781671cd77f4bc0e1d2dc5c209af2292dd642dfcc47d01c986d7062a49aea0959c25f11dbd954b8d5b4e8"
        hex"8375c494f850e68dbffe29ef5abaf7ca437f544f244c7128b588c304aabcf83a42f12535be5d1237cfd7848bf6fa67d33e3719591cad758592efe499"
        hex"55483c681cb85ce97ee6952f3a37ce118c9338893fd9fc31048a0cb08485a078c0d1122fb0ff532533a568ab0717ded34149a624f4bb93d22f3a03f7"
        hex"8636d55b2c204927cf3c1645552caa1e73027dbd9c187167097e9c410b6a73eda679ce72ee3ee824a06f77a5e89de8e1375b1ccf7047b891df6d4cf1"
        hex"177ddbe0abf8049654910e609989baa1351f8bcf13bbddc50bc3acadc0ff5c5e0aa8f5c871068af0e91b8ea0e85e8492b7d73c4655f5e49fb4912155"
        hex"55b530bc0542532acad6fc411755eb5f95cc0af0c8eddd5e417767c02ccf597d17afc4b610fd143aeeddceaeeb622f1c9c029f525a23aeaf62d13650"
        hex"eea3caf23d59085d18fd8105e17d0330a72b8de5049cf9edc16e5d472b953c8edf7a4b38f5387dcb2dfaca3dbe34b0e47910a294499bc29b95d1978d"
        hex"dc86d84aa3454658468f5bdb00ad6aab9af4d3cf3eca69236e950f388e41f6bb52b89802e2333ee2809f08ee076ceab3a203f10ae9772d72855ec607"
        hex"d7e8ef51a1aba6e58f302a09680250901c0c7e585065a01ad2a8d881cb0971f149f663ac242bf56baefa5e46a6fe722e2d6f481ba82b73991b69b852"
        hex"a4f2621d5acb685d8c5bd8408f25d285e4a7af672f919096e6236b1f22da5d1e9e4ccdf174a65f2594cc48a366d6bcaee79df9cc1e2abdec0b58d8b6"
        hex"68a17a11625339f69d9448a7b467a1640f1b555b185401d825498f80d4b689ac8433e2fe4f54e7a9c0c11e56aa43095ad2695219520163f81dd79c91"
        hex"4c39f72a467a925dbe73c8b3d5aa4163532f9523f3d667143ec8ec7e283608b954359b2b2644f525e1e8f63b329f6b40daa239e4c7741209f642c11b"
        hex"28dea5e9447a2ce5bd544a336564d0458f92b8483cb3a391219e794f4779206d2fd6f0431de32bd8a8fa85ea5df563e9f9725fa2f49b2713c7becd6f"
        hex"a603d5b9023c3e09b100258915c2d080146fa803a29a07dd5a57062494b4a64803f391c927964492d48b95bf5d201a7a13fe374333f24a96fd0441a2"
        hex"1ff2031b186d597b09f44560d175584aa35a1203a8cc892941082be7e69575c734dd39a57d2a47ae244bdf9dfcf908c8d69ecf0eb31f97174034550a"
        hex"f5c058778c762c6213887564002e7926035c443e2a8c7c05cc686ffbe9d34ac660ee6221715de70f518a6d9c206ba26b2ea2bd741bb2245b216765f9"
        hex"4262023f9f9d3256b0247d2822be77a4222dce376e0e98ab567cd97ab2ec5d41cec4172f02085bff160ff7a75a13cd9e2e16bf7aa8f8f18fb54745a2"
        hex"f3956537b6e5ba52290248a61763997ad9d944a70644fca30795848a65ed1cb976f9fe371135f6c1baddcf54159d77065a9eb95e304406705a069189"
        hex"61a627a45d6308ac4101d6a46a82322177c039c43dc843841405de511413c5780bb107b2bb1041bc6bdc0cbbca6c27cdeea77227735bc56e1fdafb04"
        hex"18c53423e2214cd5a08b619f08d1bceb95b202d68cd126282fbb8e930b2575d3b66b0971cd2ea53514ad8f82761a6e871a33df67b7d7b1e84a02f114"
        hex"03a162afed0ce90441e9ac9556e10d4bcda4ed0a3fe8638a30d8a48ee92c126e24c9dbe712cc360ef95cf98edb26a0a4dd1d309596e7848f22c1d516"
        hex"5217b06d0b1b714fb86345c16ef9d7d6b4fba62c6fbcaa0e1c8c550079ebb984c21d3c832039f50192eb03f12663d7e7af6d9fcc7047fcecbed96ff8"
        hex"d844eb73c5c6e8cd043e64b6b41fd242f385537c160e677cc50522f5defa5d9e2afc6fb58ac8e92926a022067ef11b0a5183c7c48556de3ce5d3fd27"
        hex"372165fefcd0c96a7e38b9ab176da01de8c4fda420c36f60dac41629b3bf3537c10619c062a948757c1d35a20adbedf567635785738d5cf801181051"
        hex"180d218621f90b8ee13023f4e2b0f33f14da4e3ab6d612bcd2800f8e503264d41a3d3e8ac78528cd61d2a2f5f386155f1b39b6e4d76baa555a0cf232"
        hex"df0e2d178effb6a3e93c54daffafe6804ed80e1710d665f6699942d36aec72be10c6295a4fd59b8b4f5f7cccec9f6475e899a11214ccf477a760303c"
        hex"ed8fd7180e1c7b4a9c90e9f43d46cd0aa3232c573e0fb23b00e1f20ff0c09e20a580e7656b20fc05e711bc64a42c65b96471d9272ba4600d30d1b0d6"
        hex"b66b18d2ca62e46dd391ba2b45d2aa4c53933fdbd246d25fed20f2b924fe67306c657cb46d690051e7e7345e0445bb7d9ea41b69c62ff0e8b452e315"
        hex"0bde8a8039d5a9895d042d2841d08a39af4d7c54468b2ffff666a1fc02b381de1fe3e81bec1df2e8cfd5dfac43bc0e3f7556ac716f0d4ebbb2c1280e"
        hex"6d27df8d04718c59410f2019376ff4542da9053acec9d0c33c925f2f97c4196a06d739ca001d64f25d83fae188f9d9c3095d2b00c9576baffc7d120a"
        hex"9be16bfc6fc8027225681eb0fa3b4a235448d0cd6ae05e5a12aec0f8eb1b0543ff7742d45de71d2c278b192e22828732179f76249c7c2b5e365c1768"
        hex"861b6191bd22de71eb410dd70a9155e2d532ceea6ac6fc9e14cd6d7c1efe93ed3cb6806f55cb39d75386eb110c9d284f69ff22dd65828256684d9b31"
        hex"ac3371f08e8f357535b10c670a77779a05eccee9c218bc6d797aa77ab6006b2c51d57cf4538a73cdf5160fcdcebcb0e206ec6445d13fe49f03127ad7"
        hex"7957cd46955b14b45991533ae865c8f8ff13fe153076823f492903766b64cc56e19ba78adb0ec6263cbfa76784bdb95e09715a6810d5503686594622"
        hex"7bb84d7c2a1bc28e0d33ef00b98d91aa054a513d8b5357e915ad6fcaff7c611ac89e01cccd76c137005ffe56539519fda34c62fd444717ce111beb02"
        hex"67fe8225565d9b30405f6834c9ed49049ec6b97fa4bb08f848c7a86d273686f0c744320191d2af937cd605ee802ff4bc5d7a1e51b03c9ca53d5992cf"
        hex"16d51569f224d9bbe2074544a814dd1b82b9f43bc15754495cbac72680bc155704b7e3335902e9985ad0cb513adbcf43fe16bed2bab97b34ed179df9"
        hex"b43a61e30a6c1188d4ed85ed692f91f82a6f5a55ebcd87cc752b27deec6afbf8ee9f5eb70f9dec447373daa35260ec5b03a17d6ef0ce8076117c4ac3"
        hex"154c17bf779f78a024620b7e901198d819055cc68690f0fba4b28c490e14664888d77700c2477c840e281686028fff09b41a79fe42e0a8cd6128aa02"
        hex"7fd43e14b9dddd627996c2d61d7bc9c9d2a5e02d4b3adc155837a9115a94fa473a19ad05ddae7b2a5aef7fa32c39e0a651ccb408ac4efa42bfe504ce"
        hex"17293b782b34daf1b21548d60c8443890f210842cf99edcb5d5df47b67e25c28aaaf6cda69ef40bda72e09a8b2dd8d88053ca17b84c232d7db9bd55a"
        hex"02632a56e52a7d418204c93ab2149a9e1a6659480786f5e5ef9c32cd25ab41e3703c6d9fb31b7d54e065305b9a1a05b9ef5169752a97e9d7514d2c1f"
        hex"d7e2cd41fb82eacb1b84b1c649f173fa811bb0f8b575c673166add2974f3d8c2ee4fd27263f7293dbfa1365472fe068e705efa7ab84d7cc609c27406"
        hex"fe921cc57142c227ce31a28aaa03cb20c9aa72b91bd1d638ca1e75950962008ac39259df0c0d5e8d572538085ee9ea97439fb80602b5fb4ea83a4ea8"
        hex"15024544caedc29aabface0728476e1a70ce970c8352357b62ddacc655f78d5510d5a1f8906e0c127914d774a6f4dc917580a95bacee46d2203569bc"
        hex"711401bc29033bb105f2bf366a6b32430ac42d3063ecab687d4f0a17292711c06e67ac9b005817ae2ffc1c31097e2a2754c332659bb9b40340c9a6f9"
        hex"dda649bafd4de0612a82611d2107adbb5144e85d8e71857b682a3be2fb433da9a1738626b8e4d5861e3071bad571c753f03acf13bc95efaffff52710"
        hex"fe251b19e71f38b618740cc2299351a7f7a422dec52ae7157b5f1a7c66ee48576c6742fbeffa302f7bf454220ab2414bc9bbf74e836d0ce8164486d5"
        hex"b78401a16f729130fd18785f500cc8df0fb407449b1fa9185e6cfb6129b3e5b3d2b213b4d32c55858d164c3ccc2bcf9e12489d5a91bb1cc514a1343d"
        hex"3b97c7c6e4aa08c3a7df9d565b0d944e6d9e1c1e26fce36779ace67f3ed559381a9ef8299286262b4ac823b599037e16264a6b0a035a3e51a5b940c2"
        hex"0556c2ce029b93c9ebf7eb556cf4a8742379fa55864a758e2606c951dd68ad9a9979a1bc4d2e8dc3508ac949d9d8f08b5618281236324b780fcfb453"
        hex"b928cf5370bb3beb01e336d9477dc8c7f9b55206e883d7c0adee18792a321b569759e1384b47579d932cae690bff16d011ee980c7a69eb9aea067162"
        hex"1147e4e707bb38d7c86c79a44585001dfe89fff244b3e0dac83c118962bb12b805ff403c934e0ea63d632d95fc6d814512963db3d873b34826b6500b"
        hex"41053f4e0702e6db3a239b0bbe3d4d7b8ce52b6ad3f1b1feec98c4c5b441305075501e4e07fbcce7d4f3042e0d2fe4dc8b4b07d8cd34de61b784f1a6"
        hex"37926752f2940af113cfd34dc92d7083942351c3e7c4e82096254ca213c926e4c373936699104d3a057ba62024541047c016d4f04f5ecbae2378249d"
        hex"9f6e6d4200614763738053252e4b7ea5c6a02c347488b968466d8aa728e46120696928749b77957a933e29f00be151ee5d4412b2b614ef2656a8f907"
        hex"62f0a36b1adc8855c93684fc4a7124890c35a7b2daf730086be00e04528d42cc3e2dc8b6b049bdace3a8dcaa9cfa23c32042c966fbed6cd177a556bc"
        hex"2437a6e40c021cc0a158029c64f6ca297b43eee71dc6236565f4b9d02336ba392d8037c82bed3cf10041f400f463f24da54d20fd2475dd05c6b2fb02"
        hex"6dfdd50b679b2d5e11510aa7f8bd5acd0c35c234799a87e92f05fc1f960d8e1d43e34615490042674a4dafa7ac5f43ced11a33081378bd1a0a12704a"
        hex"9828cea9367a7f385c71b86df5265044156905716f7c244f0c2ad71d254af39575b31bcb802e541d60b2e85a08058aa331abd7aa8b1e1a90699ace1f"
        hex"0543a0d780a2efecef47ff98eabd95c634c6eefcb2f788441bc46276d4746c111802b319fd5ecdecf14c39e64d6011302909f5bc43e1766c7b9689a8"
        hex"52436a2d2819f5d398e9a6a1787e8a542bd8c8304b77dacee859b2adf632445a0483df2110a14d2843e766155bbbdc65b8b2ee3ccc4aca117cc02de4"
        hex"3ae73558d67a52c40b1d124cf05b717af679b73e36cb18a2e77fc2a19d70d96d8a1a1a350885b3ff290d8ffc0ad0e87a235dc7355293cc12a1a7ca02"
        hex"3aee4fee7f2390dd1e05325127ec3bb97254c99409de0d3fe19572b6474366c52a0d4623c594924ce837859e2960b046ea8c6c422a7e848e027f4bd2"
        hex"6be0e23a3f9d726b61b0efe88b76e66a22b4a6a0644a80972ba5d13239b9cdab97ebacd2c18c89476a701a4cd950682f2d146d2e1cbf3d66643c2b63"
        hex"283f8108dfe16c5849816444694b382b526e8a8c19b0454687c395044bf5b8e3b0e6e307e7dc2cd4cee2d880a5c812a3f5396132267fbde297145a35"
        hex"c7e454c5549a4ff1fa2c579cff25a89161ae98eb5ecdd58f0be86127020068bcdeeaad9c7493b681461a887e8b34a958d95325862c3d139503be8a42"
        hex"ae65c3f224e0b24789c87ebaaadefe593134ec04a7a6d80d60c6a27206adfd14fe2e79d981c6fb27726306c6838442b0c8e5d4291e327cfff60677b7"
        hex"0811d3ba0ac8d5cbebf9e0c6b2dfef69a5297b75dd128040638289d9e617d7281fc23645fa542bd68433d7fbece427607e731f00ed86f5ffa91546a5"
        hex"8341b2960a5d0728284edbc1c1c591d0be761c4338ee29657f09f56c79c673c01292f8e0296766f35e29cd98230ba12f3b8bc6f95c5d0767a4e7a751"
        hex"21911dea45235f9c1b45416bfc81caaba7803a7c2e44d2c313d2329db50b9f89837473b906e616f90ee0d266b7ec67d0ea0361ef39806f71eaa422c8"
        hex"a1023dd8f4677d3a23d485c6105f147018c9b3e7f4c8f0fc39088b5099ee7b957e8b771f567f51e8410a558e1e15ed95ca6637649307d0789f41565f"
        hex"27db7ba84067b9dc5b56fd4adc09ef67176a1e215a941c59492ea708aad38c74926eb180faab8d7ea6ecd91c0fe36aae29be23728cdc2ef107fc389d"
        hex"188bf657045e6cc4319e12288e73e532ba093dc10aab45f797f67c67aa7c59d8c098acf130de1cc7342c497333b4ed5724adb3c90ea8328143754c05"
        hex"9efdf560ae14fc2b7e71e751cee7b2b5b247d52ae85e129d07b8ac45135a3ffaf87a34b188f3832f8842523a60dbe76c2695bd4fff4686461f5b4ea6"
        hex"ad5fd7d974869d17f7308deb120131003de2342377dee40da21e882928e9a62d0320baa5af94f92f17760b65e6aa4b12338b733086f3fe7a7f6b57a6"
        hex"1dc9336bc8f9dc19fad5779099fc63ecda4a65b1ac1b07388ab53c8fd1fb88cf1d2a91bee4a0de7763a7f31634afcb3c710ece0bd2bcfd3eafd441f2"
        hex"9eaa4495006e8bbb1123d1bd667892862712034191a1e11e8967fcf41d346039d6f56ed30ef6e53087a17488f05d3985407bc7a830b05ed75c265bce"
        hex"988ebc557d3375fa1e4a3ae846fc69f8bf7f7f3c42ee6539a6bec486266948c84a7f8aa19e4c356224724d732c2a0b61e0c70260a6b8b6a0d391ebee"
        hex"69d494683bf4a5e21cc53f5306801cfe73817c5acc99760882fd3733dc348bd5ac27aa5a6a504bb9ff0e09ca0f5d976071cfd84086ada9331d57b9b4"
        hex"13ec9a1738e1a52af432e5d29e3e3d6108b3bb15d64e85144d67745103def343e925a0f249da9df7a2326515a6971a95235b2f977e6ae23c11e0348b"
        hex"6ba426c2675cab052b4f8f85af3f7e240a4baca717833e44c520bff705065b85f602bcf305a73f9cc61f2ef2709abcf5affcbf5201c37cadc2382fd9"
        hex"f0424cdfb7134bad779f3ab1a97359718549e58f0846b4ce0574d93e89ac0f5645135802f7a7c2ba6531eca1cad4ac7c9705657250327dce2e170ef8"
        hex"0aa48cc42a866b32c310405c2ff3e19d80eb8fa2511488dcc5355d1b17f8cb62de29154f652192423a17d68c812a0ef4bca82268c64d5d72efb0d6a1"
        hex"18ebb3f1a2a6f7ef38050044b3d81d1221c302d12b92a84132f0eb0ffcb759762aea12c9235352f2d51501a196ab05c48906df5cc916626b6e14fe86"
        hex"f359adc9191844a2370f5e88130c51894bd7e00c4a80acb883fc81df8bee2b00232281ee13f7eddf982153f61619ac802992e9487ac95c4bd649062f"
        hex"8a7554496e4b18812bf89f9a1689e1a48f249a83b1cd7970c4b393b367b7436ef2ab36488ac2fdbd0c15749388a24d98550346baec0a97d03627f70f"
        hex"7e56a2f3175b599b1c1189ff1b575751833639d223a7936233212ebc4aaecefd1a2bcb200ec95c0de5ba4d9c17c240fa0053dcc3155f49cc9b81a248"
        hex"688976f7487ff48698cfe277ebc19d650139323a66349ae82bea200d6267fe23f745988b89b5d27c5a18d2a35f1282cf158513a28aa6276cfd4a16ae"
        hex"c17b7b64d715cc8d0c7995b0bffd10d242d3e4990e8a73db9966b8de5dd0206ed43c46f98f2d3294cfd879250603f89ebd08a6f10ba102912b35b999"
        hex"7d6c35c21906f081fd5350bd38b8b5fbd49ef46b94c0ae162d60b34651d6a02d9ad557dace03c15fb7d49ac800b8f3dfed586733e10c8b9d00ac01bf"
        hex"9a319d43da7e54f3f915e8ec5a1ab50d3b545c66d315df4ca34d1d7c2cef3f09410a6c7da30f92999fc0640913d75c618e4598b550b0a6d3c43937a5"
        hex"0ec244633fdedc65d18240f649af5c4fe55169d0a2fec1e13f9768b30147cd131910b3d0db970c0469b3cc75ebe3a9e571a698d7960704845eb4dc27"
        hex"ed4ef535213939735f1a1ffb0ef15dbc272ef0a3e630120e21754374da2fdc4249a50a5f23218a5da89d965ef0c42da04c74f53ff590a6841114c970"
        hex"c700ab3295c5635503e055e9a627d87ed496ce2bf880409d2239637803fc440e128d7d4b37651bbe1ada294530628c3b5d0ffde59f62229b4aeb8399"
        hex"4d46023c597cffaec681f28b09237b6a483f81d8144090952e5a372c6f8f2c5880a343e677a27fb731f8937d00043a1e00cd751f5f66c004559c0392"
        hex"09f116ccf4fd05ceebe7fe63f4f60f6517ce63853374b390d3568c005c3fcfd545840ca73d1ccb55447ffa47bd4178681a59e0cd4bc013323ab0f3fa"
        hex"f72f6a93d16cbecc1258c3a795d3953b8c214d812097aa07277e4ee0d1a1f010c69d56cda0e5e3d70e20f0b8bea988f4447799420d5258a8617b9516"
        hex"35cc21236aeb0e44acb5b6801b28dc099e2c2eb658a7a54e06d3894fe9ad6a8fc5b520b99f9d2db655a2e10875f2eb173b27c04b1a6f033b0445047a"
        hex"16b85859766507f3a55a115162aa99927c3f985a73f4f524f9972399124ef5f54dbccf59ca07a27d2763a819eedbb90c88ef8e9f0a0edcd280aefc7d"
        hex"16e133973369545cb11e0ebf5572a86e6adc7074b3834c51c6c31309b6b7604803ff2b78c420d3548349626490697ad5e9c5bd7fc705c1c60e207f01"
        hex"c65f10500320821a3a6e8ff2a9294badc4f9faa29519fdc71be0ecbcae12654b6f959bd90b4f8e07c1e9e32322541f1bbb3879de1081f3d0c330fd29"
        hex"49d1a86ae8581d9827284d59cb118a821c32fcaf581f5e94d2d4116d87de9b71597360091c0d9c301a766b174d9a3a312f2356fadd0e9e60e6d88b26"
        hex"404dfe50927dbdfdfc39b1cd20b9855262433c50b8fb550845cac3714afa5a71364c2e519dccdf184860d7d512faa5432f0990841844aac30c97835e"
        hex"d30b6dababd7a5c5a8b76a1680bb7c291d3df9dbef64011a5110b34037ad96e367694ff7472b71702ad124ad3aa5cc9b01fcabbfbfb3fe24fbd56dae"
        hex"5870eb36e567a8f88f9733c50cba374b8e1feedf03558c06a8bc51712a4a9c3f3e0b5896156c4b6e56f8a516a822b4a80cbfdfad1fa182b0065f279b"
        hex"2347b48c2438f589fd12c26a2cc88b0dcd776e0fc87bb59c2685b724205226a4547dc6b3365098c532196fefd4aba80b36517e744ceab6810a1a1507"
        hex"b39f98e5372780c7aa499c88d8dfc7a5f1b4d49ff639f8ab25321af203a23f0f28ee6a39264897883fc35247dc51d254b035f9305e707c0ba9dda1b5"
        hex"2fddd05e66de8efbebcedd654c6d9c5555cc363ea3c0ce241f8b45ad6254692e185cf15f82ee9e5d4da03e0ef391264e70269b9ca3561d1795def1f0"
        hex"d190620608993d2fbacb8111c35287343ad1f27879369c3bb6dd797d02b9e9626cd64d902eec4743505ae64dcd590004a0fb140d54e2c4d0913cccc1"
        hex"667da857d05672741e6a837bda7616408d48c419d4bd95959b3b4220a5f7b33484814f5a5d588da51ebd16d64fb216756f777803c7b1209f2de39126"
        hex"11f5665f6bbb12f0c11475012bf4914a69b72c265120672168dcb5cd7147ac42902c98f785d6d3eede743eb41435c19cfaa1c69aec4349668d275d4b"
        hex"31c2d95adbd2721d489bf468236112cf1d3ff07de51dd80a52e9212530d9d76226e60d0447c7f64a3f2fccb5717cf9e7138aacd2cb1b9fc45c34734d"
        hex"a8bdd27070b59bfec2034386727729f68baf93b62774195bd1a0103a8a4b937f10326e2d0dd8d47b94314f47935e07f9f26060821d40f85afa7f39c7"
        hex"9a416b0eded95f95754d10de1ade6dba0c65ae1492aedd802c5f777f2cf935108a8a95cddc09aaebc238990edc777b101e31258ee2469bac175a31de"
        hex"541b3ec66bb63ecb893573dbc9b107fa9dbc9cf004b7aee79aefa00c0161c3a49390d7b536f5a8bf0cb275f1aa9727a892a2c6cd8f5c515d26b38533"
        hex"0f4f7228cd3e6b67e9f4f1a67be3084b579750aefbe62903a58086386cb5807906c2609fe7294b07c959c4a544f10ff9f4374631cd5c35936f639de3"
        hex"61f01fd71cea04a5711b79c827a4fe77d288eeb1371465d23780be3894e64dc971b19c84222efd19ce0ed6d986c2d13416a37204151bbaf326d14577"
        hex"9712b47e59073e2a2b196d8c5d499b57de75577d0b577d563b825aeb776dc9509f428f2ca973fc0315708c48b9de845a99ec64f054986cd8346f6a82"
        hex"b48560dfcf9191f79eb3ce9a1c768205e01a53c9a3d60e6e5f9c55f8790f10ca3951917f048341c8a40c307d045edcefd477745c1b385e2dacdb5c69"
        hex"267b6712de44e583d027651f6e38be442ae4aaa80b662ef86f611504c7e71097649e7a30fc1fcce9f27ef04dab8e1b100ea71ac1bb223d973c79ac6d"
        hex"982fa14d9b4af05964f7a6a7aa0e2440ca1a80bc1aa1f3c91a03ff78f0f9fa771329177bb2f844885f5331e5edc41029e64426132c95de3fcfe8fa54"
        hex"4d024e6a0602b64b9262e023b9b06eea912a2c311d670a77163bae4d4ef982bc302261d5903b1439310c2c4ef82aafaf719930265bdbed1e1e288293"
        hex"b0d1892f220cb064088fed56961e9fa05c3c7c107d13d1bb542d445e2b9738a567b4c5adc3618d88821bf0363f5baec44f2cf9317faac1336627f8ba"
        hex"03d8c74132eae13b67f20ab23d68e72c73afd81dcbbba837a1c88a48a49401e7293ac3a3ecca7b0c64b6dc95fd243bd21830ac341ac64c81c3173363"
        hex"a6057fd422e54bea19f7202f9d219084a4fc45004ed7e3b0d224d2d2bf95181234681a9f075dd4e7db4c794908c7fb2c33ab8d9db14ec8ba0f0a4026"
        hex"dc958c4fc2af00c50b9978c3eb5fc310897c49f68fbea5575cd37794e2eac6c81f625a3d2d13a46d1f0ee06021a1c56dc55b698c677fdb4d53f3eb11"
        hex"884cadd7332e8c38f20596ad2fd4dca8ee40928c30dc5dadb8eac2e82183c3fc8be1831f060c99da9e3e10970a5dcd4e8520d3b9806c4cba21360ab0"
        hex"2bebd938c6c17499149f55e8d21e676606cdf8834de40e8f7e503890d5ab1ba6016450f8d0fac25f594137026a332ac7229bce3970f0738fc202699f"
        hex"0c345e2e1383b3b8121625e91e7083d93dda94f423401725bf55554c0c47221d15bd7489e65d743778b9878340d5ee01b1f404f11953b2f146246289"
        hex"9434952aa278148457676072342689b613f1c22d42cce48808f1d646117d3773a0bf0191b0fa795d80e016529dacce70b2edf2299eb22ca319ab7c29"
        hex"29ab05347031b3325805b0f13c99b736607a49041a0dd617acc845360f0ef4e712ce512c82c0beffc77526b4eaf63506149d617071fcc7b129a3a774"
        hex"28c53e3c687de44620aca01da06f95d7c36e3bdc4fe803692a45bd39339da7261acef0a437471a28c1afe98ece4ad8c389adddab064626966f83cc46"
        hex"3326653f1eeb6cb69f2c44db5351bc7a30ba444688cc674f21cad3def875435350faa7dc0f924340ab708bc140da617d414b93d0a51d6151ca1967d4"
        hex"adbfbf9d580834051779adccb61fa47bc03c10e08c34110cfb01a7c570082211375e1b578c71ce9717b40b782419baf8dfcf93fc917b516355ca527e"
        hex"d8ad24cd81abd578cd9428a41df99e38c0bfded1e5b5f2d73f450ab09c77dbfe28773167ce2c7c1093bc091915005a763ea74100ce8350759f21d2eb"
        hex"ce87c216b6275ed163afb7cd1682442b0228df7fbb090924d0eb32ff5cb5e16c3b2aaeb9c18fd0a13e22d1196770511b0e92de71f77ce7ea73d75180"
        hex"e99e9cdce5f864e3c12f726e0a4ee9f726b47fd503155e31b54ff2a917e47050249ec3d6a60044d5385637b98ab476bc0b6c99211687afbc19c904b9"
        hex"e1b19808cdcfa812131c0fde9a436d13916b7e90750089d0146400359ac615c098a4b5ef889909d4f710d395aef6b2421a7a7d64a092f3f2157fac8b"
        hex"d084bea2bd8485157a0c2f7c5747251b5e37b5d652ee538345cd6aa0263f56fc0026ef44e4c4433d42ca724d7d99c8d77f715e503d288696766d35b4"
        hex"0762ad7fc6ccda770dd7e51d53140afdcdbad14ecb08a6497f32be5a9301500d2bf0b7626c838dd8bb4a5a5b14c4404c3e2a54066d2cb850a87ea0f1"
        hex"1b8097ab0c599991ba7d911a67119648a2b2095326731e0c35cb8d9caa7a923ea28c61c91f729e952b74de3f5de9e33d9934d1b9cf492317438ae193"
        hex"9c9d81dec57b06d017cdeba8b97a2b76fc97930e5b3baf9addc3ae45e09f62bbf45b9d9a65db6a9b09a805a251639dc0720ba497adbed35562a90177"
        hex"ba1e465aaa550ce25a3cbc030614bca92f6ba0676043c466533350bb4250b7703fe2b848ad957aea0a34d5150363284ceedbcf18a0c1f3a47aea218a"
        hex"e4dc69e38e13899aed8761ad71153eeb071818e8e3a24a8a10198ef5408e57fd69597e832f3f51d40dc449ed2ee4a5b62d3ba535a07fd4543e8195d1"
        hex"046a5c4fdedc92bd3c7641d160f2479abda4c0cf0689fc81ddafc9e052250d500bf3a6195fe1e47637f982ade7a488509742353c0f40bd2b92d30031"
        hex"a6b008b3715f0f77a0323e034f728c874f43e11a6d7aae4423d61450e76e8a18c383c06ab38ac590f3482074f7c595da4ebb98ddaab1269300600969"
        hex"c091e19986aa9c580bb89202fb807b0c5f33eaa8bf3f8d631c1e81ab0d29d1a9beba92fd03e83bdee4f02a004d2a654cb80b182fac3fbf5ff2bc51f1"
        hex"0e34c4bd47d9e88459da035f923260d251b745bd76e3e599550ed99ed8036aa3207baf991da8ee8538cbc1df3463fd59c8e92494301f14be44f53b68"
        hex"fb2f6bac1ce23f605e942ca4d0b0046d561f00695901b25831d021a9fcbe2e0798bbe16010d22b169e4472cc75a1516105699c2401c8f32a64e40ed2"
        hex"e411994a2915581c29b1999e001c8ff6bb58d9857dcbe39954c1083d162047ede78bb6bdd20755dd11c36a0e62e6cda5bbb7b6c1f3bbc8d6ae87c1c1"
        hex"dcb6278ae93ababaa7ea6cf01a8fc73bc2482bc955276446f9af6428fcd40dfa86a7d9c99805d95c90d8902b1fe116178681878d3d8cd22d6460c1ad"
        hex"d8c726bd9544bedc44c81591713e5aa31c77b4fafada832098e164140b2851063f93e6f1343d0c0735819eb36e5361ed0e3d5d27d610b402411bc07e"
        hex"810aec0e49b9517cb63194077df542d6e579f5481a8b0839f3e4249d554c93181d94863359c95fbd1a779c3c68dc9ebe57bd1ca00d806b3b029fa68e"
        hex"5ac27a45df27bd89c1b3af949cd92faec73f6f1c69d58dfc0b40b3e72cb3bf74da475450d26679a4d510dc80afe2587fe0886f438f54148110ddfcf0"
        hex"85ac2f1d1b600d187cf9e096ca15ec18be6f9f4df5fd797ce97fd1ff1ae174f5098580f7548a46e84c47461ca98221e70b3b03c30cc84706a22e359a"
        hex"0134aec09128c514b564f6340e2e2561cd85a1daa33dce6f68750e3448f8791419c709ee7d957b215cd6089226a914df041860f598bb97600cb47cc4"
        hex"8c4895840b1cb0679227239b86b668d9e02c31dbec4080b91f92cce0395f1c9a4fd85dc72d86511521c1d961ef8ff680d8a9ce6609f64988f282adaa"
        hex"2a2d42fc374fec2423cc681e7b67ca0249a5b9eb0dadc1b13fe02ebe052959910e69fc94288d737b28ac5556f9fe72775d5c1a75399a3dffe94426cc"
        hex"9f19b46dcc3275b45e9602a22b988e00ea88b043bbe7ad75fdf03d7c5d25e1bd56a167ecdf5667a927572b261c373287f5a9b02695eff95e5359e42a"
        hex"05c7396e5f62bd4b448b6ff776f3d2a80096834bd18f0664a6dcc35ca2891dcb1524311855885168dd569b1a1f3f878a0a54b9a558aacc290a8a3c9f"
        hex"40514590744b3f53e0d0dbe31206a4bf7b0caf5022ed1c56572065be3f5e94df914e4276c6aaf6bc1417b72de1d08c43f411d6fa0ffb08339db11b2d"
        hex"da5552a92dc376ea485883f940f80966f256ce6e630150942bf23ffd1cbea80a21cc77925ba54ecac4a5ab4a7fb6f496eefe6a387b6849a01101a3bd"
        hex"5beba33a15705974937bac0a276f2265c64a5258a6fdc1b09952cf6a11cb0413e7d99879c9d222428a3a42b8eb7963c14e9d8c478cc8f29f083803d1"
        hex"294eb74ba0e947e8b27127094fab5ff8e544ba35851837ba15bd9d9c4f5cc2ac185222d6cf96ee1cb8d00d2c16557956b30c0a1a0188844609ce1fac"
        hex"d953991b2886f306e56c1af83ff89875846cd0074907f439b8d6243f49aefb884b4596982ec0a11b916b94907cb0db46334ecf9d292c4a0098725a0a"
        hex"4db79e6677899bb62ad4f5a54a5e524cade2c7451015398d2f235d5eb52d0e72f836227e2e2a162a2bdcac877f7ae2934de67b5bb0d98d6834e495d3"
        hex"75ad67ffbba8e1463ea9819522cd818dd0beca0eb2c3eebf03e21e24341fbcd147d755c58005fd1bdd270f931e05fb7a010ca1e255f9ecac08c97f83"
        hex"870a92fa2649f20300bbe9620e41943d033de6f6f13bce03da9c179b1d23d614760b4ff37c55a254f594cf47c90e15330f7a850e710b3fa3295a5dde"
        hex"8f2ef4c65c9a55a38c1f4d0bff0e7bb26f5bdb302849bbe63b364cc48b797cdf332ffcfe7119d9e02ccf32f58bb51ae9dfc24c1d1ad084df6c841c31"
        hex"96bb14e134f78251f38764cfbae4eec1d689f385acef00152b14007a212330b9b55ad280d706d6f3894fd1a05e0ea5973e625929be792d2503fb043d"
        hex"3ed6a2db02ebfa22572f425abb0b71ee2ab8e1d094b07f409a45c55e03c7cb3cd827e18d2f2bc1ae8ebf14c7075bc70552799421b7829b6390adf0a8"
        hex"0b694c014410205a63386f6d9045e6f67ca97aa7a74272283e1e6f30b20ecea822936ce906200d4780fc435fc5045ce0a3a3bbfd531919a3a03ae5b8"
        hex"c3d229fd056702b31c3b83ef4a98ed695a11ac4c5057889e59cf9f69e1b37cdb13b4c61e0bdac61c3a294817c31dde35da2a4b078ebb73c72c05d183"
        hex"d931859a67e1ac23299f7a24e522d47b61dc029a36f9444c689bf955f0ea0362c232ebac02dab2881813ab965910791449e988002bdae5cc22785f25"
        hex"9925dbceafb923c782aaa2141cf6c52ea3ebd8a79fa89fb9cdb01aba9b5f81fecf64beba1d83883ac94b276a02d867567ed92b367fc2b7e735a7d3ec"
        hex"a83a777bf2a4043c18afc83ca0702f56045afd6a84dc7861febc340731633c0a895d3390e31685487c21226d99bc382814a2b0bc52adf9f758410f5a"
        hex"03d374eede5802f991f58c23dbd94cc1235826cb25d10771ce8b169fd298eb37bdb198ddfabdb5abe5810d39499ae5027169229910f426b7ad012b0f"
        hex"a227d256abcf318d8b3c71f96ca6337ccac44063d2fe517d1117bfbf6219462dac8e7d5e6f7d779a0c2d009d2237fd0096c7c9be76413fa20344730f"
        hex"c45fac5039e6acbe995ba9c9b7e5f5c478c9aa34d1564d063217358b17be5a4fd5122317de2b1e91adffb4be5626eba235796e29b9063dc6c2da0940"
        hex"05f4f04309d6dd5b81ee8a6a728fd9fde9ab4638585e559e8f29bfa5944455690dcdd69e199fdfed4e04319e8a6db7d45d6b0501aaa3d496016cf286"
        hex"cc1d17b1040d45ee7c1c8c7953a0c8113a233d29544c92b6c6adb249f38527c31647e1340934f370affdda1a7d0be7f69d23300e026ccc34491ece14"
        hex"1940df19500e1dcf0a57877c64020700c87c19fbee98f469dc63e6fddc66b180da7e3aeb7bfac5db1d760665fe765942307b9c629ecb0cde950fd6d2"
        hex"6b15fb622013827ec1289c1c1b78953c188357d63d94b701db96edca28f8f3aa8dee89059a0839d60d87af5f28b946efbea88e59b65d0021de44f5d5"
        hex"9a4ad880530eae6c5e5405010693dcd62b46fd505d6d597b5a5ed10e39ef940e7687ef26d665f943740f94c2e43a23790748a49233d587b2a9828d85"
        hex"3ff07518c4573e298b5d23b860d9a34a2c1ca69a23fc9925c099cb22e583097f2ae42b63f76d2ab13f3536e6ef8950ba90ceaa891be2c7bf10b86da8"
        hex"c0b571a71010acf16d56e0f808e982ee01617ef6eb9018c827a45647c5fa6ac3ceb60283a931c91b6643c647a7ceb5876cc5fa67b285d15d211960a9"
        hex"9242d168df43ae3c75a0eb73cc819d15c1ac7a29ed05b5e57eb0643f0403ae8e2c7968c55209b50a4605da3be9239474b072a4a46bfe04db0f3c8991"
        hex"015b09b1c9bfa49eaa7ed8b93bef3c140ee6fab5ace8bed713961437188cd74315731487ebefa2c22be35571b7e90a0fba4c3c2bf47c61342fe3c192"
        hex"3b4cdf3a25838677e284e0d5e6a608bc5dc89aa011672c4fb22c027a8fc53563822d9ec20e14d5a1cbe4276a18bde4dd037d2c0f73799fdaffc60052"
        hex"b1cd34985ad118f506dbe2071b96e49b51eb6bfde1517392f4d04c5c947df4d859f941802068a4d42ce4f228d21b7dc7281ce4bc90de591bb98e8a20"
        hex"a95c55aa7d3ffc711057ed8929b14ed5c20aff5e82bbb73cb30ff74ea1d2bb2bfaeb3b90fee7d08430dbe2db017f044540112cc1c65e79c41b08e559"
        hex"70346cc140400cc6521efaf920bb278510945ac68ad9af007f53d6ae4444d3ca0e8575958f085375175964920fe29b211c527483645f114c17d8893d"
        hex"3121c09e2ac3e5e4ded3712d96ef397ad303a7ee1ce0887fa2b3e7c34a22ab52bbd8affb3f080353d732a0950f525654429147a908";

    // ZK verifying key (VkBlob) for the beacon light-client ROTATE circuit —
    // recursive 2-to-1 aggregation root over 8 committee shards + a step snark
    // (15 public inputs: 12 KZG accumulator limbs + current_commit, next_commit,
    // period). Base v1 (Blake2b transcript). Hermez SRS (outer k=21, shard
    // inners k=20). Source:
    //   acki-nacki-bridge/eth-light-client-prover/fixtures/rotate_vkblob/rotate_vk_blob.bin
    //
    // ⚠ SOUNDNESS: the opcode does a plain SHPLONK verify and does NOT pair the
    //   12 accumulator limbs against the SRS [s]·G2 unless the decider
    //   extension is present (tvm-sdk#284). That opcode co-deploys with this
    //   contract; `disableOwnerRotation()` is the intended flip once the
    //   committee is bootstrapped (see `scripts/ursus/flip_deposit_to_light_client.md`).
    //
    // To rotate: regenerate the fixture in acki-nacki-bridge on a big-RAM host
    //   (cd eth-light-client-prover &&
    //      EMIT_VKBLOB=1 cargo run --release --features aggregation --example rotate_tree_n8),
    // then run acki-nacki-bridge/scripts/embed_rotate_vk_blob.py <this file> and
    // refresh the tvm-sdk opcode fixtures
    //   (acki-nacki-bridge/scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh).
    // Do NOT hand-edit the hex.
    //   sha256 037cd274840dd5867e21b4f9d334e808a4db1b760f483fb3f306b1f44c57b81e
    bytes constant ROTATE_VK_BLOB =
        hex"564b424c4f4200000100000c000000007e0000007b226b223a32312c226e756d5f6164766963655f7065725f7068617365223a5b32315d2c226e756d"
        hex"5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b325d2c226c6f6f6b75705f62697473223a3230"
        hex"2c226e756d5f696e7374616e63655f636f6c756d6e73223a317d0a0c00000215000000001700000078d954a4152ede302e415658146c54f2100aa5cc"
        hex"7cf5347a457200b12e38de28d1331f3ae14f45dc9a79f4b9895f681a9c9daba02b61c0fa0bbf0166ae0c020e119309c58f9bb7529337d96b36277fa0"
        hex"426d4058ec2d1df6be7f35d104250e01a0e9e564e211e7950ca1d7c988582f074521211f1305ffdfce3fdac210827323f7729708653603842eb9fe4e"
        hex"c8326ce8cafea0e257c1be63568b918ae6ec361eb309b606de89e790a567641617daf13305f6a4bef3f779dd796169b08f8d402b8a1bebe283a8b1be"
        hex"67390514071840329780c648a5e4786a82276dd6d2a4571e6f11c6b2f316934ef3256879596acfb6353e475a6c02542bc1bd1a995a3ef50326b34cb7"
        hex"fb7847514c0159c52427b2c8b1f6b970b9f64c621cb6dd586c8316124c48151793217acf2524d92294234cef1033480573fa386bd66a9762c5cba824"
        hex"2953ec8d4118993075d1e6d018eb4248ce8846c907969b74467ca35b631ddc1bfaf77a601065506622233e8b39a91bb705a336290eb113991fcb25e5"
        hex"9fdad020b289b4858ec16e62b8fbfcbd02f4c0acd841121a8a748dc20745101aab84790fee310d124411f4382567c75671d0c4c65fb3a6493b789372"
        hex"8733269bf70291271897f4ed9dbff56cd445050ca1aefe35d0e97a52bc766ad543dc216cd6f600242c57bf13f6e8990d7d2feb6122025542272870a9"
        hex"44a98f31b278708c70b3df06c03fb7bd7649b0084e79841af815f5a6a9f01da9a5bae5bb0a9027aad3466b161358f76cebd12005acec9549d1b0a423"
        hex"dda279708255549ea0c7895eb6958022ea4b848fa295beb8dfd5b71a417009791e408a3a4e95ca1c15f8435a7c36e50fdffabbd185cbc64d6fdcebd4"
        hex"1a26533b4ddd228932967f0ea4bf23ad25ef972d779d93df767acacab77ab79461a189ac271e68ca6598c8e57c655db7758e1b210657be62d7261242"
        hex"d1a4a75a82d0da16542e26e735a07bf8ef61df6f60b83c0f33755b59b57cab77e324c4b1f89a97ff9d38c0f43f474b0308b1a29f4a854d075db267d0"
        hex"ff1d202ab73571213f100162c987c71117353258e7860d1701d439118d2d931f0b522317c657bede5a875e56298996ddb576bbf5baef2e90a569f81a"
        hex"56b159aa927dca05d100948680572c8d0bca65908165874cc025543964c7b0048ab4c93202ff8afe11867d80be66d9abf222c289b0d593d3376a4a60"
        hex"f8835005f0e323af1bb4289a4c580fc96d6df7478bad73ffe22711e0d2a71479fd919127fc9f55c18dc93291018decfb909114fad6a51d0ae4969b45"
        hex"6421b5f0b0e3df05bf7f9bbde63a45f76f4c724b1035b84751da8f44c6db3f7097d084bb9b26402aa2eed2982df68c8ac79d0691a09447a97994adc6"
        hex"99529d613065d93975cefa003bbf3508262bae61a878fcdc99e26eb2d453ed1e4f65dbcdcff683f21c58c80a6e094b5c6a22c062ca1ba7249a5cd31f"
        hex"988ba91579377e996e2bbe53ad060a0dc94272203dc55ff72fe44422fc91b7638642749fbfb409465492e61ae46e1d20d645e22a72bd9ec7c9ac3150"
        hex"c3378530700b514d490628c49b6b5e16258bd025f3a902cab309a93a61a4e8995985f4da2a46baf77f42fab66dc074f1a777112a42b21ac041bbe12a"
        hex"c07977c52872209e218f377fda6c02d25979ab21afab932c8ad7a3535081abf91e49362c510ce048777d8a5cdd6a9b995b1405d8df6fd30cb11ac808"
        hex"a7bb171bd3dee71efe86b8c7a6c3b88a74b0c02aad4b3c8196881708670de8e94f4ccd98edd5a64d80aff83cc8f4436faf0b6c99698774c0053cf30a"
        hex"d3847fa7e869e3cf6afcfac9a11b239ab102b093d2345f267f8a393fa3182f2b8eadf280dbea3c408fa7c277b18071e9614e05f064ac4e404ea03d11"
        hex"2c3bbf0b02994683fcab7cf21769254b401ab5c4df13cc3d50645c1f6b55c5c8fdeeec2bf736cea3b7fccf90211ba6098d372f9fe5fb39fc27f5f33c"
        hex"96eea0a4c78a1b0939b5a1a030f89e72cb9683f86ed544ad237173e31cac6ef2d268e50f5e600a0212cbc5cc9557e81e04c8448ade668d506d4f3a60"
        hex"cf6b23a6ce407ab7b274590614e0153a261dbda0e3c77d10624e29677cee614daf9a84df167fbcffd40b371f13d24c4a6359cd764bc19d092d22efab"
        hex"b0f68dabf9b4fae63c5f4c0ddc3788095c64ff61482fa43038e0f97f3c1a6a0d54a86334da930ee73a72101cca8d4b252063aac36e2434a31783cb9f"
        hex"b2b3f8818e708abb750b4dd88797f3dacd0b3f09658ddfc8889ad660a2dad5edbe876371ebd667573bf3acf33044a9b597a2b11e28241d5b45b1f37e"
        hex"046e1442012e8b5e3fad1f13e89939cb18863373c71f171d2ed959f5d126135d7fbbdfc990dd1afee6321dbc67113997d4c6c066178473202feb6ed1"
        hex"a90a2e4f2a6235ceef54d716bc06bd96625a733a2aa41e61ae036f1228565328736a2a6ffd25c850f529719797045f51c895b7244d6e7026e0f02e0a"
        hex"b129b1b6676b775f03203fb456e50563f0cc4c1d0a2ba44c2f2877a3c998b516e48be877e864d2f8bf270fa302305f07b681a18514cec480ed1fe2f0"
        hex"bda1ce2b928431a4772d1a6ab77d9af704458946af3e2a2d3df49f1a257ea8ef552bc127b4f9599ce5829b06ba75e0adb8f4b3a0b659d20c5bfae1bf"
        hex"1f6c1fbc40fae62de0355c60bbae1a291774a7f0cad116cd135f9cc8456f738d014f2ed52d13da187795dcc0d8149810464b4d6d39da7092831dd70d"
        hex"dda9b60b18fab5e70c0fed0dc4b5464ff8920b69758e7480d6b2a71122a0f02a7bb520e7ce00472f5943c71e4526c8deb7e80c0acf28c47f2935adab"
        hex"99225e98a1818cef4b4bc599058ca008c517bff698c47b1105d66cdb08a40f7835a324e6142156f536eb8773268f3226472f05d15e63c046b0cd7203"
        hex"1aace3e250d86f44115c7ff741496d35a8ae811788f5252af2f507a34fd3ba7d7b65984c4d19c0cf45c5de72f1ab59866af096105b501ba9dad3f730"
        hex"2ec0df327da10a2b0dbe300a10b7220430e15f6f38b9771d3d0849c899d71dabdcf9a9879a379c99ec762198d1d68b7090ef7c2694e69808334f5227"
        hex"6b73ec6febdd2558f8b02c88ba8cc506e3ca45600e33b5c68b7a880a2b2681fdae71243544e6d11dc84cd3377a24af6fd523106a8af481729950f02b"
        hex"347059b721b9f776d44d0b1f4e1987207c341c4ba4f0d670331e529b0ab37e0158e116b470cecd9033224a01bf2f968a12c22261a6a4831a09d5e3dd"
        hex"7595820fc8620f0c5218c76e9faff6d6dcfb3d711b405bf06725b4c4a7d416c996a3481f8d508103d4fd627f77e976043a343681f121879bcf6c7707"
        hex"75b4f3ac85e203156834604c6009eaa58fa60cef22b4cf0c0c6085b82507abb244abb40b836e9900b7863ba1de804679921e2b5d44b792420abde601"
        hex"c4267ba4a23bccc4f44c152310d2ebee8461db3a731994615c6be9bb201264b69e82ce0c12316f92727ab42b1b15bf2e6a4f212250ed0e5f3ee2011a"
        hex"d0d4966296580e63536fe1502c35b1278c69d6cce98e3213f5043958399a54d3b36e490924af9da8df4404ef751a3109dfea208af5dd6ce0d72eb6c6"
        hex"478a5017c0559129777b6ae6a0e67bb7d1a7da1fec34bbeaa2073a4471f57d159632ad25acd2c13fafcd55c92686577338ba0900c4a908d7c3047402"
        hex"002a263fa517df80710c028c83978d7a569b39efe45c520b6dde565c61e74ce0673741d51d46c0827bd702e3ebc31d181756779269cd2b28a512a312"
        hex"b19b22c088665cd36b558d3eb038813cd58b1510f95308777c1e71140648ec6b2390eb610bce05c85b4bec65c19cce515e3fab11b3446b7af19f5805"
        hex"d4a2ec70b69f07aa74c57af4893b90714314698afa7129e801114b0468e6f910ddba4b45f87d5a9a5711cd2035cd6dfff0b4f05dbe78adabae1c2bc8"
        hex"e427fa01db6e4c1a7b984e5941c11aa5b6aed9477fcd630b9a8a2e71f0b2f8862fa4af08fdac398c7b40b013e9a801b533fb1939669b4c95121f4624"
        hex"d71b2637eef2c52331d35b7b4dcdbf9f59f25c6e3c413aff4c9449a424626b769d7ff798d151702f7dabd9f14a62300a94d8bfc602d33a0d507633f2"
        hex"6aea68296f4a0897b605822a0eeed71ff792f33743087061161b1a05d935c910d6d1dc4324f5d9f620651e1b69a741c809039ad952c9087ccc2c80e3"
        hex"f777139301f8f8d9815f643f8cb700170d8df1ad05c6c97d5ee676103eece5e0c843d3e7a7d067be1db4780f96d9ff24ce910e6c79c0bf0bdd3cf950"
        hex"629e30320eb8d8f8dde915c7e4e14d55a1ec3501592f317e1d8a6e69fc0134bfd416636e694d680917cf3ee2bf8523f10a22090c";
}
