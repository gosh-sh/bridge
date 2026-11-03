pragma gosh-solidity >=0.80;
pragma AbiHeader expire;
pragma AbiHeader pubkey;

import "./EthKeccak.sol";
import "./eccUSDCBridge.sol";

/// @notice Sink for the canonicality anchor the light client proves. The
///         `USDCBridge` implements this so a proven finalized execution block
///         hash lands in the same `_acceptedBlockHash` set that
///         `finalizeDeposit` already consults — replacing the owner / M-of-N
///         attester writer with a trustless one. Address 0 disables the push
///         (the light client still records the anchor locally).

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
///           * the finalized block's execution block hash sits in the signed
///             `body_root` (PI #6/#7). Before Gloas that is the payload
///             header's `block_hash`; from Gloas on, the payload bid's
///             `parent_block_hash`;
///           * the Merkle indices were chosen by the header slots against the
///             Gloas fork slot in PI #10, which must equal `_gloasForkSlot`.
///
///         What the proof CANNOT establish, and this contract asserts from
///         outside it:
///           * that the signing committee is the *chain-anchored* one —
///             enforced by matching `committeeCommitment` to the commitment
///             this contract holds: the one in force, or the one a rotation
///             replaced, for the period that replaced committee served
///             (`attestedSlot / 8192 + 1 == _committeePeriod`). The
///             replaced committee may still move the head in that period,
///             so an early `/updates` best-update hand-off does not freeze
///             the remaining checkpoints of the old period. Bootstrapped
///             from a weak-subjectivity checkpoint; advanced by
///             `submitRotate`. After `disableOwnerRotation`, the owner
///             hatch is `reAnchorCommittee`.
///
///             The installed committee serves the hand-off's attested
///             slot's period plus one (`attestedSlot / 8192 + 1`). There is no
///             end-of-period window: a beacon `/updates` best update (start
///             of the old period) is a valid hand-off.
///
///         Checkpoint coverage: `submitUpdate` anchors the finalized execution
///         hash (1/32). The head only moves forward, but a skipped checkpoint
///         can still be late-registered behind it, with its own proof and
///         its own slot (`CheckpointBackfilled`), signed by the committee in
///         force or by the one a rotation replaced, which is what rescues a
///         skip at a period edge in the honest case.
///         `submitAncestry` fills the span between two already-proven anchors:
///         headers oldest first, 3 to 33 of them (two anchors and up to the 31
///         blocks of an epoch), each hashed with `gosh.keccak256` and linked by
///         RLP `parentHash`. The blocks between the anchors go into the same
///         `_acceptedBlockHash` sink. Anchors further apart than one span need
///         the checkpoint between them registered first, or the owner's
///         `setAcceptedBlockHash`.
///         Proven hashes expire after `SLOTS_PER_YEAR` (365d of 12s slots) behind
///         head: `isProven` / the sink forget that window. Deposits older than
///         a year cannot `finalizeDeposit` against this oracle.
contract EthBeaconLightClient {
    string constant version = "1.7.0";

    // Sync committee size on Ethereum mainnet — the supermajority denominator.
    uint256 constant SYNC_COMMITTEE_SIZE = 512;
    // 365 * 24 * 3600 / 12. A proven execution hash older than this many
    // slots behind head is not canonical for `finalizeDeposit` (one-year SLA).
    uint64 constant SLOTS_PER_YEAR = 2_628_000;
    // Sync-committee period: 256 epochs of 32 slots, about 27 hours.
    uint64 constant SLOTS_PER_PERIOD = 8192;
    // Amortized compact of the FIFO; leftover expired keys stay unreadable
    // via `_isLive` until a later tx deletes them.
    uint16 constant MAX_EVICT_PER_TX = 128;
    // Separate from the examination cap above, and the one that matters for
    // the action phase: every forget is an internal message carrying
    // `SINK_VALUE`, and so is every push. The worst case a single call can
    // queue has to fit under `MIN_BALANCE` without relying on a mint —
    // `gosh.mintshellq` is clamped to the dapp's credit and quietly yields
    // nothing when that is spent, which is also what it does on a self-rooted
    // test deployment. 32 forgets + 33 pushes = 65 vmshell against a 100
    // vmshell floor, leaving the gas of a full span (about 2) inside it.
    // Examinations stay at 128, so compaction keeps its rate; only the
    // messages are bounded.
    uint64 constant MAX_FORGETS_PER_TX = 32;
    uint64 constant SINK_VALUE = 1 vmshell;

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
    uint16 constant ERR_SELF_ROTATION      = 253;
    uint16 constant ERR_ZERO_PUBKEY        = 254;
    uint16 constant ERR_WRONG_FORK_SLOT    = 255;
    uint16 constant ERR_FORK_SLOT_UNSET    = 256;
    uint16 constant ERR_ALREADY_INITIALIZED = 257;
    uint16 constant ERR_WRONG_CODE_SALT     = 258;

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
    /// `checkpointHash` is the newer of the two anchors the span ended on.
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

    /// @notice Deposit-style struct of the 11 proven public inputs
    ///         (`step.rs::pack_step_instances`), all little-endian 32-byte Fr:
    ///           [0] attested_slot            [1] finalized_slot
    ///           [2] beacon_root_hi           [3] beacon_root_lo
    ///           [4] participation            [5] committee_commitment
    ///           [6] exec_block_hash_hi       [7] exec_block_hash_lo
    ///           [8] attested_state_root_hi   [9] attested_state_root_lo
    ///           [10] gloas_fork_slot
    ///         `committee_commitment` is the **2-level** Poseidon scheme
    ///         (`commit_sync_committee_2level`), byte-identical to the value the
    ///         rotate proof emits as `next_commit` — so the committee that a
    ///         rotate advances into is exactly what the next step recomputes.
    ///         `attested_state_root` (indices 8|9) is the state the rotate proof
    ///         anchors its `next_sync_committee` branch against; `submitRotate`
    ///         checks the two proofs agree on it.
    struct StepPI {
        uint64  attestedSlot;
        uint64  finalizedSlot;
        uint256 finalizedBeaconRoot;    // hi << 128 | lo
        uint64  participation;          // count of committee bits set (0..512)
        uint256 committeeCommitment;    // 2-level Poseidon commitment to the sync committee
        uint256 executionBlockHash;     // hi << 128 | lo
        uint256 attestedStateRoot;      // hi << 128 | lo
        uint64  gloasForkSlot;          // first Gloas slot the proof assumed
    }

    /// @notice The proven public inputs of the **rotate** circuit: the committee
    ///         circuit (`eth-light-client-prover` `committee_table`) wrapped in
    ///         one aggregation, so its instance column is 17 little-endian
    ///         32-byte Fr:
    ///           [0..12)  KZG accumulator limbs (lhs/rhs G1, 3×88-bit per coord)
    ///           [12] next_committee_commitment — 2-level Poseidon commitment to
    ///               the committee whose SSZ root sits in `state_root`'s
    ///               `next_sync_committee` slot; becomes the new trusted one
    ///           [13] state_root_hi, [14] state_root_lo — the beacon state that
    ///               slot is proven against, split as the step's PI #8|#9
    ///           [15] attested_slot — picks the slot's index with
    ///           [16] gloas_fork_slot — 87 before Gloas, 2946 from it
    ///
    ///         The proof says nothing about who signed `state_root`: that is the
    ///         step proof `submitRotate` takes with it.
    uint256 constant ROTATE_ACCUMULATOR_LIMBS = 12;
    struct RotatePI {
        uint256 nextCommitteeCommitment;
        uint256 attestedStateRoot;      // hi << 128 | lo
        uint64  attestedSlot;
        uint64  gloasForkSlot;
    }

    uint256 _ownerPubkey;

    // The Ethereum chain this light client follows, used as the anchor-set key
    // so it matches `USDCBridge._acceptedBlockHash[chainId][blockHash]`.
    uint256 _l1ChainId;

    // The sink that receives every proven anchor: the bridge at its zerostate
    // address, which is also the only account allowed to deploy this contract.
    address constant USDC_BRIDGE_ADDRESS =
        address.makeAddrStd(0, 0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a);

    // Head (last accepted finality update).
    uint64  _finalizedSlot;
    uint256 _finalizedBeaconRoot;
    uint256 _finalizedExecutionBlockHash;
    uint64  _updatesApplied;

    // The trusted sync-committee Poseidon commitment for the current period.
    // A step proof's committee_commitment (PI #5) must equal this, or the
    // commitment this one replaced (`_prevCommittee`) for the period that
    // committee served. That is the join that pins the aggregated signers to a
    // chain-anchored committee.
    // Fail-closed: 0 rejects all updates until bootstrapped.
    uint256 _currentCommittee;
    // The commitment `_currentCommittee` replaced. A checkpoint skipped near
    // the end of a period would otherwise be lost the moment the committee
    // rotates: its proof carries the old commitment. Accepted for the period
    // that committee served (`attestedSlot / 8192 + 1 == _committeePeriod`),
    // including to move the head. 0 before the first rotation.
    uint256 _prevCommittee;
    // The finalized slot the head stood on when `_prevCommittee` left office.
    // Nothing reads it; it is kept for the storage layout.
    uint64 _prevCommitteeHeadSlot;

    // The sync-committee period `_currentCommittee` serves. `submitRotate`
    // stores the hand-off's attested slot's period plus one. The replaced committee
    // may late-register or advance the head while its attested slot sits
    // in that period. `reAnchorCommittee` compares its argument against
    // it. Bootstrapped with the weak-subjectivity checkpoint's period.
    uint64 _committeePeriod;

    // Whether the owner may still advance the committee directly via
    // `setCommitteeCommitment`, bypassing the rotate proof. True while
    // bootstrapping (before the rotate VkBlob is emitted / while syncing from the
    // checkpoint); `disableOwnerRotation()` clears it with no direct re-enable
    // function. Normal migration preserves it, but owner-controlled recovery
    // (`reAnchorCommittee`) and arbitrary code upgrades remain available.
    // Disabling this routine path does not remove the owner from the trust model.
    bool _ownerRotationEnabled = true;

    // How many times `reAnchorCommittee` has fired (telemetry / alerting).
    // Under a live relayer SLA this stays 0.
    uint64 _reAnchorsApplied;

    // First slot of the Gloas fork on the followed network (`type(uint64).max`
    // if not scheduled). The circuits accept any value and pick their Merkle
    // indices by it, so a proof is only meaningful against the network's real
    // one. Not in the `updateCode` migration cell: the owner passes it in
    // `userCell`. Fail-closed while unset.
    uint64 _gloasForkSlot;
    bool _gloasForkSlotSet;

    // Canonicality registry: proven execution hashes with the Ethereum slot
    // they were admitted at. 0 = absent. Live iff slot >= head - SLOTS_PER_YEAR.
    mapping(uint256 => uint64) _provenEthSlot;
    // Insertion FIFO used to delete expired keys (lazy `_isLive` is authoritative).
    mapping(uint64 => uint256) _provenQueue;
    uint64 _provenQueueHead;
    uint64 _provenQueueTail;

    modifier onlyOwnerPubkey() {
        require(_ownerPubkey != 0 && msg.pubkey() == _ownerPubkey, ERR_NOT_OWNER);
        _;
    }

    constructor() accept {
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
    }

    /// @param pubkey                — owner key for admin / bootstrap ops.
    /// @param l1ChainId             — EIP-155 id of the followed L1 (anchor key).
    /// @param bootstrapCommittee    — weak-subjectivity sync-committee Poseidon
    ///                                commitment for the starting period. Pass 0
    ///                                to leave unset (rejects all updates until
    ///                                `setCommitteeCommitment`).
    /// @param bootstrapPeriod       — the sync-committee period `bootstrapCommittee`
    ///                                belongs to (recorded as-is on this owner
    ///                                path; `submitRotate` stores the
    ///                                hand-off's attested slot / 8192 + 1).
    function initialize(
        uint256 pubkey,
        uint256 l1ChainId,
        uint256 bootstrapCommittee,
        uint64  bootstrapPeriod
    ) public accept {
        require(msg.sender == USDC_BRIDGE_ADDRESS, ERR_INVALID_SENDER);
        require(_ownerPubkey == 0, ERR_ALREADY_INITIALIZED);
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        optional(TvmCell) salt = abi.codeSalt(tvm.code());
        require(salt.hasValue() && abi.decode(salt.get(), uint256) == l1ChainId,
                ERR_WRONG_CODE_SALT);
        _ownerPubkey = pubkey;
        _l1ChainId = l1ChainId;
        _currentCommittee = bootstrapCommittee;
        _committeePeriod = bootstrapPeriod;
    }

    modifier accept() {
        tvm.accept();
        _;
    }

    /// `MIN_BALANCE` covers the gas, not the sink messages. One call can queue
    /// up to `MAX_FORGETS_PER_TX` forgets plus one notify per header, each
    /// carrying `SINK_VALUE`. Queued messages are paid in the action phase in creation order,
    /// so a mint queued after them cannot help — the first send that crosses
    /// the balance fails with action code 37 and the whole transaction aborts
    /// after a compute phase that reported success, with the gas spent and
    /// nothing written. The relayer then resends the same valid payload until
    /// its budget runs out and gives the span up, so an honest span is lost to
    /// permissionless coverage. Mint for the worst case before anything is
    /// queued. This bites on an ordinary chain, with no attacker: expired keys
    /// are contiguous in the FIFO, so the first time a year's worth comes due
    /// the forgets arrive 128 to a transaction.
    function ensureBalanceForSinks(uint64 messages) private pure {
        uint64 needed = MIN_BALANCE + messages * SINK_VALUE;
        if (address(this).balance >= needed) { return; }
        gosh.mintshellq(needed);
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
    /// @param publicInputs — the circuit instance column: 11 × 32-byte LE Fr in
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
        require(_gloasForkSlotSet, ERR_FORK_SLOT_UNSET);
        require(pi.gloasForkSlot == _gloasForkSlot, ERR_WRONG_FORK_SLOT);
        // Head only moves forward. A slot behind the head is still admitted
        // when its execution hash is not yet in the proven set (late-register
        // a skipped checkpoint). Same-slot replay and already-proven hashes
        // revert ERR_STALE_UPDATE.
        bool advancing = pi.finalizedSlot > _finalizedSlot;
        bool late = pi.finalizedSlot < _finalizedSlot
            && !_isLive(pi.executionBlockHash);
        require(advancing || late, ERR_STALE_UPDATE);
        if (late) {
            require(_withinYear(pi.finalizedSlot), ERR_STALE_UPDATE);
        }
        // The signer is the committee in force, or the one a rotation
        // replaced, in the period that replaced committee served
        // (`attestedSlot / 8192 + 1` equals `_committeePeriod`). That
        // path may advance the head, so an early rotate does not freeze
        // the rest of the old period.
        bool currentSigner = pi.committeeCommitment == _currentCommittee;
        bool prevSigner = _prevCommittee != 0
            && pi.committeeCommitment == _prevCommittee
            && pi.attestedSlot / SLOTS_PER_PERIOD + 1 == _committeePeriod;
        require(currentSigner || prevSigner, ERR_WRONG_COMMITTEE);

        // accept() must precede the halo2 verify: ZKHALO2VERIFYWITHVK is a
        // multi-second WASM extern that vastly exceeds the external-message
        // pre-accept gas limit.
        tvm.accept();
        require(
            gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof),
            ERR_INVALID_ZKPROOF
        );

        // `_evictExpired` can queue `MAX_FORGETS_PER_TX` forgets and the push one
        // notify, which is more than the bare floor; see
        // `ensureBalanceForSinks`.
        ensureBalanceForSinks(MAX_FORGETS_PER_TX + 1);

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

    /// @notice Advances the trusted sync committee. This is what makes the
    ///         committee chain trustless from the weak-subjectivity checkpoint:
    ///         `submitUpdate` only trusts the commitment this contract holds,
    ///         and this is the only permissionless way that value moves.
    ///
    ///         Two proofs of one light-client update, verified together:
    ///           * the step proof (`VK_BLOB`, as `submitUpdate` takes it): the
    ///             committee in force signed the attested header with a
    ///             supermajority, and that header's `state_root`;
    ///           * the rotate proof (`ROTATE_VK_BLOB`): the committee whose
    ///             commitment is `next_commit` sits in that state's
    ///             `next_sync_committee` slot (gindex 87, or 2946 from Gloas).
    ///         The two meet on the attested state root and slot, checked here.
    ///         The step proof is not taken as a head update.
    ///
    ///         ⚠ SOUNDNESS — TRUSTLESS ONLY ON A DECIDER-ENABLED NODE. The
    ///         rotate proof is an aggregation whose validity rests on its
    ///         12-limb KZG accumulator (PI [0..12)). The opcode pairs it against
    ///         the embedded Hermez `[s]·G2` because `ROTATE_VK_BLOB` byte 11 is
    ///         `accumulator_limbs = 12` (tvm-sdk#284); a node without that
    ///         extension accepts the proof without the committee circuit behind
    ///         it. Call `disableOwnerRotation()` at the deposit flip
    ///         (`scripts/ursus/flip_deposit_to_light_client.md`).
    ///
    ///         Permissionless: the proofs are the authorization.
    /// @param stepProof        — step SHPLONK proof bytes of the hand-off update.
    /// @param stepPublicInputs — its 11 × 32-byte LE Fr, as for `submitUpdate`.
    /// @param rotateProof      — rotate SHPLONK proof bytes.
    /// @param rotatePublicInputs — 17 × 32-byte LE Fr: 12 accumulator limbs then
    ///                       [next_commit, state_root_hi, state_root_lo,
    ///                       attested_slot, gloas_fork_slot].
    function submitRotate(
        bytes stepProof,
        bytes stepPublicInputs,
        bytes rotateProof,
        bytes rotatePublicInputs
    ) public {
        StepPI step = _parsePublicInputs(stepPublicInputs);
        RotatePI pi = _parseRotatePublicInputs(rotatePublicInputs);
        require(3 * uint256(step.participation) >= 2 * SYNC_COMMITTEE_SIZE, ERR_LOW_PARTICIPATION);
        // Chain to the trusted committee; fail-closed until bootstrapped.
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        require(step.committeeCommitment == _currentCommittee, ERR_WRONG_COMMITTEE);
        require(_gloasForkSlotSet, ERR_FORK_SLOT_UNSET);
        require(step.gloasForkSlot == _gloasForkSlot, ERR_WRONG_FORK_SLOT);
        require(pi.gloasForkSlot == _gloasForkSlot, ERR_WRONG_FORK_SLOT);
        require(
            pi.attestedSlot == step.attestedSlot && pi.attestedStateRoot == step.attestedStateRoot,
            ERR_BAD_PUBLIC_INPUTS
        );
        // The hand-off must move the committee. The last header of a period
        // is signed in the next one, and its state's `next_sync_committee` is
        // still the signer, so a genuine proof with `next == current` exists
        // for every boundary. Without this it would pass, change nothing
        // visible, and overwrite `_prevCommittee` with `_currentCommittee` —
        // silently stranding every checkpoint the replaced committee had not
        // registered yet.
        require(pi.nextCommitteeCommitment != _currentCommittee, ERR_SELF_ROTATION);
        tvm.accept();
        require(
            gosh.zkhalo2VerifyWithVK(VK_BLOB, stepPublicInputs, stepProof),
            ERR_INVALID_ZKPROOF
        );
        require(
            gosh.zkhalo2VerifyWithVK(ROTATE_VK_BLOB, rotatePublicInputs, rotateProof),
            ERR_INVALID_ZKPROOF
        );

        ensureBalance();

        uint256 from = _currentCommittee;
        _prevCommittee = from;
        _prevCommitteeHeadSlot = _finalizedSlot;
        _currentCommittee = pi.nextCommitteeCommitment;
        // The period the installed committee serves: the attested slot's
        // period plus one. There is no end-of-period window: a `/updates`
        // best update is a valid hand-off, and the replaced committee can
        // still advance the head and late-register for this period.
        _committeePeriod = pi.attestedSlot / SLOTS_PER_PERIOD + 1;

        address addrExtern = address.makeAddrExtern(CommitteeRotatedEmit, bitCntAddress);
        emit CommitteeRotated{dest: addrExtern}(
            _committeePeriod, from, pi.nextCommitteeCommitment
        );
    }

    // ========================================================
    // Epoch ancestry (31/32 non-checkpoint execution hashes)
    // ========================================================

    /// @notice Fills in the blocks between two proven anchors. Headers oldest
    ///         first; `headerRlps[0]` and the last one must each already be
    ///         proven, and only the blocks between them are admitted.
    ///         Permissionless: the keccak + parent links are the authorization.
    function submitAncestry(bytes[] headerRlps) public {
        require(headerRlps.length >= 3, ERR_BAD_ANCESTRY);
        require(headerRlps.length <= 33, ERR_ANCESTRY_TOO_LONG);
        // Both anchors before `accept`, inside the pre-accept credit, so a
        // span that cannot admit anything — a far end that is not an anchor —
        // is refused without the light client paying for the walk. The call
        // is permissionless and the contract pays for everything after
        // `accept`, on a reverted transaction too.
        //
        // This does not bound a payload whose two ends are both real. The
        // forged headers between them link to each other and break only at
        // the last link: measured at 1,917,462 gas for 31 of them, charged to
        // the client, with nothing admitted. No cheap check bounds that,
        // because verifying the chain is the work the gas pays for. The same
        // exposure is in `submitUpdate` since 1.4.1 — `accept` must precede
        // the halo2 verify — and the answer there is the same as here: this
        // contract mints its own gas, and `rePushAnchor` is permissionless and
        // cheap and reaches `ensureBalance`, so anyone can restore the floor
        // and the client cannot be stranded.
        uint256 firstKey = _piForm(gosh.keccak256(headerRlps[0]));
        uint256 lastKey = _piForm(gosh.keccak256(headerRlps[headerRlps.length - 1]));
        require(_isLive(firstKey) && _isLive(lastKey), ERR_UNKNOWN_CHECKPOINT);
        tvm.accept();
        // Before `_evictExpired` and the walk, because both queue messages.
        ensureBalanceForSinks(MAX_FORGETS_PER_TX + uint64(headerRlps.length));
        uint last = headerRlps.length - 1;
        uint256 prev = gosh.keccak256(headerRlps[0]);
        uint256 anchor = _piForm(prev);
        require(_isLive(anchor), ERR_UNKNOWN_CHECKPOINT);
        uint64 anchorSlot = _provenEthSlot[anchor];
        _evictExpired();
        uint64 added = 0;
        uint i;
        for (i = 1; i <= last; i++) {
            require(EthKeccak.rlpParentHash(headerRlps[i]) == prev, ERR_BAD_ANCESTRY);
            prev = gosh.keccak256(headerRlps[i]);
            uint256 key = _piForm(prev);
            if (i == last) {
                require(_isLive(key), ERR_UNKNOWN_CHECKPOINT);
            } else if (!_isLive(key)) {
                _pushExecHash(key, anchorSlot);
                added += 1;
            }
        }
        ensureBalance();
        address addrExtern = address.makeAddrExtern(AncestryAcceptedEmit, bitCntAddress);
        emit AncestryAccepted{dest: addrExtern}(_piForm(prev), added);
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
        while (
            _provenQueueTail < _provenQueueHead
                && n < MAX_EVICT_PER_TX
                && dropped < MAX_FORGETS_PER_TX
        ) {
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
    ///         push that landed before the bridge recognized this light
    ///         client). Does not re-prove.
    ///         `blockHash` is the stored anchor key (`_piForm` packing), not
    ///         the Ethereum byte order a block explorer shows. The bridge is
    ///         sent `_piForm(blockHash)`, the Ethereum-order hash
    ///         `finalizeDeposit` looks up.
    function rePushAnchor(uint256 blockHash) public {
        require(_isLive(blockHash), ERR_NOT_PROVEN);
        tvm.accept();
        // Mint first: the message is paid in the action phase, and a mint
        // queued behind it is too late to cover it. This is also the call that
        // restores the floor after a griefer has burned it, so it has to work
        // from a balance below `SINK_VALUE`.
        ensureBalanceForSinks(1);
        _notifySink(blockHash);
    }

    // `h` is the stored key. The bridge keys `_acceptedBlockHash` by the hash
    // in Ethereum byte order, which is what `_parseBlockHash` rebuilds from
    // the deposit public inputs, so both sink calls send `_piForm(h)`:
    // `_piForm` is its own inverse. Sending `h` itself admits a word no
    // deposit ever looks up.
    function _notifySink(uint256 h) private {
        // bounce: true so a rejected sink returns the 1 vmshell and
        // `onBounce` emits. The hash stays proven locally — `rePushAnchor`
        // is the retry (submitUpdate would hit ERR_STALE_UPDATE).
        eccUSDCBridge(USDC_BRIDGE_ADDRESS).acceptBlockHashFromLightClient{
            value: SINK_VALUE,
            bounce: true,
            flag: 1
        }(_l1ChainId, _piForm(h));
    }

    function _forgetSink(uint256 h) private {
        eccUSDCBridge(USDC_BRIDGE_ADDRESS).forgetBlockHashFromLightClient{
            value: SINK_VALUE,
            bounce: true,
            flag: 1
        }(_l1ChainId, _piForm(h));
    }

    onBounce(TvmSlice /*body*/) external {
        address addrExtern = address.makeAddrExtern(AnchorPushBouncedEmit, bitCntAddress);
        emit AnchorPushBounced{dest: addrExtern}();
    }

    // ========================================================
    // Admin
    // ========================================================

    function setPubkey(uint256 pubkey) public onlyOwnerPubkey accept {
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        ensureBalance();
        _ownerPubkey = pubkey;
    }

    /// @notice Bootstraps / owner-advances the trusted sync-committee commitment.
    ///         This is the trust seam that `submitRotate` replaces: while
    ///         `_ownerRotationEnabled` the owner may set the committee directly
    ///         (weak-subjectivity checkpoint, or syncing before the rotate VkBlob
    ///         is emitted). `disableOwnerRotation()` disables this routine
    ///         owner path; `reAnchorCommittee` and `updateCode` remain owner-
    ///         controlled recovery and upgrade paths.
    /// @param committeeCommitment — the period's sync-committee Poseidon commitment.
    /// @param period             — the period it belongs to, recorded as-is:
    ///                             this is an owner path. `submitRotate`
    ///                             stores the hand-off's attested slot / 8192 + 1.
    function setCommitteeCommitment(uint256 committeeCommitment, uint64 period)
        public onlyOwnerPubkey accept
    {
        require(_ownerRotationEnabled, ERR_OWNER_ROTATION_DISABLED);
        ensureBalance();
        // A correction, not a hand-off: the committee being replaced is the
        // one the owner is rejecting, so it must not keep write access.
        _prevCommittee = 0;
        _prevCommitteeHeadSlot = 0;
        _currentCommittee = committeeCommitment;
        _committeePeriod = period;
    }

    /// @notice Disables the routine owner committee path
    ///         (`setCommitteeCommitment`). After this, `submitRotate` is the only
    ///         permissionless writer; the owner retains `reAnchorCommittee` as the
    ///         exceptional weak-subjectivity recovery hop (logged, does not write
    ///         exec hashes). This is the call that turns everyday committee
    ///         advance from "trusted by the owner key" into "the rotate proof".
    /// @dev This code has no direct re-enable function, and its migration
    ///      preserves the disabled flag. The owner still controls upgrades;
    ///      this call does not make the contract immutable or remove owner trust.
    ///      Requires the committee to be bootstrapped first. Do not call until
    ///      the opcode decider is on every node; the relayer SLA still matters
    ///      because re-anchor is a trust hop, not a substitute for liveness.
    function disableOwnerRotation() public onlyOwnerPubkey accept {
        require(_currentCommittee != 0, ERR_COMMITTEE_UNSET);
        ensureBalance();
        _ownerRotationEnabled = false;
    }

    /// @notice Sets the Gloas fork slot proofs must have been made for
    ///         (`GLOAS_FORK_EPOCH × SLOTS_PER_EPOCH`; `type(uint64).max` if the
    ///         network has not scheduled Gloas). Read back with
    ///         `getGloasForkSlot()` → `(slot, isSet)`. Owner-callable for the
    ///         contract's lifetime, including after `disableOwnerRotation`.
    /// @dev Freezing this field would not shrink the owner's power:
    ///      `updateCode` stays `onlyOwnerPubkey` with no other gate, so the
    ///      same key can already replace the whole contract. A `u64::MAX` pin
    ///      after the flip is therefore a setting, corrected by this call
    ///      when a network publishes Gloas. The daemon refuses a prove whose
    ///      beacon spec slot disagrees (`ProofFailed`) before any 255; 255 is
    ///      only a manual `prove-one` + `submit-one` against the wrong pin.
    function setGloasForkSlot(uint64 gloasForkSlot) public onlyOwnerPubkey accept {
        ensureBalance();
        _gloasForkSlot = gloasForkSlot;
        _gloasForkSlotSet = true;
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
        // Same reasoning as `setCommitteeCommitment`, and a re-anchor may jump
        // over several periods, so what it replaces is not the period before.
        _prevCommittee = 0;
        _prevCommitteeHeadSlot = 0;
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
    /// @param userCell — `abi.encode(uint64 gloasForkSlot)` for the new code, or
    ///                   an empty cell to keep the current one (unset stays
    ///                   unset: fail-closed until `setGloasForkSlot`).
    function updateCode(TvmCell newcode, TvmCell userCell) public onlyOwnerPubkey accept {
        ensureBalance();
        if (userCell.toSlice().empty() && _gloasForkSlotSet) {
            userCell = abi.encode(_gloasForkSlot);
        }
        TvmCell migrationCell = abi.encode(
            _ownerPubkey,
            _l1ChainId,
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
                uint256, uint256, uint64, uint256, uint256, uint64,
                uint256, uint64, bool, uint64,
                mapping(uint256 => uint64), mapping(uint64 => uint256), uint64, uint64,
                TvmCell
            ));
        require(pubkey != 0, ERR_ZERO_PUBKEY);
        _ownerPubkey = pubkey;
        _l1ChainId = l1ChainId;
        _finalizedSlot = finalizedSlot;
        _finalizedBeaconRoot = finalizedBeaconRoot;
        _finalizedExecutionBlockHash = finalizedExecutionBlockHash;
        _updatesApplied = updatesApplied;
        _currentCommittee = currentCommittee;
        // Not in the migration cell: the cell is built by the code being
        // replaced, and the live 1.4.1 client knows nothing of this field.
        // Adding it here would read 256 bits from where the period and the
        // flags sit and corrupt the trust root. An upgraded client starts
        // with no previous committee, which only means no catch-up across the
        // rotation already behind it.
        _prevCommittee = 0;
        _prevCommitteeHeadSlot = 0;
        _committeePeriod = committeePeriod;
        _ownerRotationEnabled = ownerRotationEnabled;
        _reAnchorsApplied = reAnchorsApplied;
        _provenEthSlot = provenEthSlot;
        _provenQueue = provenQueue;
        _provenQueueHead = provenQueueHead;
        _provenQueueTail = provenQueueTail;
        TvmSlice user = userCell.toSlice();
        if (!user.empty()) {
            _gloasForkSlot = user.load(uint64);
            _gloasForkSlotSet = true;
        }
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
    ///         Takes the stored anchor key (`_piForm` packing), as `getHead`
    ///         returns it, not the Ethereum byte order.
    function isProvenExecutionBlockHash(uint256 blockHash) external view returns (bool) {
        return _isLive(blockHash);
    }

    /// @notice True only for the followed L1 and a live (in-window) proven hash.
    ///         Takes the stored anchor key, like `isProvenExecutionBlockHash`.
    ///         Not interchangeable with `USDCBridge.isAcceptedBlockHash`: for
    ///         the same block, the bridge's getter takes `_piForm` of the key
    ///         passed here, the hash in Ethereum byte order.
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
        return (_l1ChainId, USDC_BRIDGE_ADDRESS, _ownerPubkey);
    }

    /// @notice The committee-chain trust state. `ownerRotationEnabled` is the one
    ///         that matters: while true the effective trust root for the committee
    ///         is the owner key regardless of the rotate proof. `reAnchorsApplied`
    ///         should stay 0 under a live relayer SLA — each increment is a
    ///         logged weak-subjectivity hop.
    function getCommitteeState() external view returns (
        uint256 currentCommittee,
        uint256 prevCommittee,
        uint64  committeePeriod,
        bool    ownerRotationEnabled,
        uint64  reAnchorsApplied
    ) {
        return (
            _currentCommittee,
            _prevCommittee,
            _committeePeriod,
            _ownerRotationEnabled,
            _reAnchorsApplied
        );
    }

    /// @notice The Gloas fork slot proofs are checked against, and whether it is
    ///         set (no update or rotate is accepted until it is).
    function getGloasForkSlot() external view returns (uint64 gloasForkSlot, bool isSet) {
        return (_gloasForkSlot, _gloasForkSlotSet);
    }

    function getVersion() external pure returns (string, string) {
        return (version, "EthBeaconLightClient");
    }

    // ========================================================
    // Halo2 public-inputs decode (consumer side of opcode 0xC7 0x4A)
    // ========================================================

    /// @dev Reads the 11 step public inputs out of the PROVEN blob (the proof was
    ///      verified over this exact byte string, so every value is proof-bound).
    ///      Layout = 11 × 32-byte LE Fr; 32-byte roots are split hi/lo (hi first)
    ///      and recombined `hi << 128 | lo`, as `USDCBridge._parseBlockHash`
    ///      does. The step circuit fills each half little-endian (`node_hi_lo`),
    ///      so `executionBlockHash` comes out as the stored key (`_piForm`), not
    ///      the Ethereum-order hash the bridge reads out of a deposit.
    function _parsePublicInputs(bytes publicInputs) private pure returns (StepPI pi) {
        TvmSlice s = publicInputs.toSlice();
        uint256[] fr;
        for (uint k = 0; k < 11; k++) {
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
        require(fr[10] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);

        pi.attestedSlot         = uint64(fr[0]);
        pi.finalizedSlot        = uint64(fr[1]);
        pi.finalizedBeaconRoot  = (fr[2] << 128) | fr[3];
        pi.participation        = uint64(fr[4]);
        pi.committeeCommitment  = fr[5];
        pi.executionBlockHash   = (fr[6] << 128) | fr[7];
        pi.attestedStateRoot    = (fr[8] << 128) | fr[9];
        pi.gloasForkSlot        = uint64(fr[10]);
    }

    /// @dev Reads the 17 rotate public inputs out of the PROVEN blob. Layout =
    ///      17 × 32-byte LE Fr: 12 KZG accumulator limbs, then
    ///      [next_committee_commitment, state_root_hi, state_root_lo,
    ///      attested_slot, gloas_fork_slot].
    ///      Same little-endian Fr decoding as `_parsePublicInputs`; the
    ///      commitment is a full-width field element (Poseidon output), the
    ///      root halves are 128-bit, the slots u64. The 12 accumulator limbs
    ///      are skipped here (the opcode's decider reads them) — see the
    ///      soundness note on `submitRotate`.
    function _parseRotatePublicInputs(bytes publicInputs) private pure returns (RotatePI pi) {
        TvmSlice s = publicInputs.toSlice();
        uint256[] fr;
        for (uint k = 0; k < ROTATE_ACCUMULATOR_LIMBS + 5; k++) {
            uint256 v = 0;
            for (uint i = 0; i < 32; i++) {
                if (s.bits() < 8) { s = s.loadRef().toSlice(); }
                v |= (uint256(uint8(s.loadUint(8))) << (8 * i));   // little-endian
            }
            fr.push(v);
        }
        uint acc = ROTATE_ACCUMULATOR_LIMBS;
        require(fr[acc + 1] <= uint256(type(uint128).max), ERR_BAD_PUBLIC_INPUTS);
        require(fr[acc + 2] <= uint256(type(uint128).max), ERR_BAD_PUBLIC_INPUTS);
        require(fr[acc + 3] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);
        require(fr[acc + 4] <= uint256(type(uint64).max), ERR_BAD_PUBLIC_INPUTS);
        pi.nextCommitteeCommitment = fr[acc];
        pi.attestedStateRoot       = (fr[acc + 1] << 128) | fr[acc + 2];
        pi.attestedSlot            = uint64(fr[acc + 3]);
        pi.gloasForkSlot           = uint64(fr[acc + 4]);
    }

    // ZK verifying key (VkBlob) for the beacon light-client STEP circuit —
    // Base v1 (Blake2b transcript), 11 public inputs. Hermez SRS (k=19), Base
    // circuit shape num_advice_per_phase=[92]. Source:
    //   acki-nacki-bridge/eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin
    //   sha256 581910a739788e1e4269c9cbf848c29f57a9e9a49e60f5e0178cc1ed67f397db
    // Magic "VKBLOB\x00\x00" + version 1.
    //
    // To rotate: regenerate the fixture in acki-nacki-bridge
    //   (cd eth-light-client-prover && cargo run --release --example export_step_vk_blob),
    // then run acki-nacki-bridge/scripts/embed_step_vk_blob.py <this file> and
    // refresh the tvm-sdk opcode fixtures
    //   (acki-nacki-bridge/scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh).
    // Do NOT hand-edit the hex.
    bytes constant VK_BLOB =
        hex"564b424c4f4200000100000000000000820000007b226b223a31392c226e756d5f6164766963655f7065725f7068617365223a5b39325d2c226e756d"
        hex"5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b342c302c305d2c226c6f6f6b75705f62697473"
        hex"223a31382c226e756d5f696e7374616e63655f636f6c756d6e73223a317d0a3000000213000000005e00000009c4136feb037d5e5bd0beeadc284161"
        hex"b2161d91f9d75162eb007d6068c2d12021f5b3c0e125f6b3c033dd30a9ababaddd126d3580e790612a7f34f9795c0523daa2bb5850a5300e983f8c55"
        hex"726cf08284ba896fafc48400255fc7d87ac33026fc29a1bb0ebbeaf33dbb30152b5cf27d61adf8307bb3dde8b08f33815149271d471b3846130e471b"
        hex"eeb69637d0609d0b73549d3bf6d9bc0d82233bcb846c6e18f9194150043821b3adf8b85f2ec4f5220554bdb2a0b1d75943ec31ce0540901556b964e8"
        hex"614c9607110f3e8d66389ca8fcc6fac5316fc5432afe687bd5aa5d0f63908575c7d4784715bea6bb7b53cda0149f98f4550a9d1a5a00736904450209"
        hex"c54bfd4e48b85f54467fdb54ef20dcaf97eadcd4e6f94d839b367246b61a300f113d6579f6e73ad26a5e8d45916c94088b99c90b83d0b71800dd2513"
        hex"16367501dc90dec2528ff691a2d039cb81b489720774737161591733c35b9db525b0d513baee3df64e690ffd2659948d6fba91a41a7c31d51631b3d4"
        hex"70fb5b9c00894f0c07d2e388e2035d2ad78241a4d22651ab9fba259c313e212b2b261e2c259acd02e18b4503e15455175cdb420cd205b46393d4295c"
        hex"eeddf9025f1c8181e153a71121dacc90719ff94b11f1c6ab8f097b49333426b1afca4bd372bcc647aa3bb12c695aeba9cc0d1ac23c473dbd7c7400d4"
        hex"5451e56b0093012c24407a4e71744430ac990c0f8ede73c066da2f42136a831391ce559c6dfde838d82261fcd71aa62fc0ce2d5a01ba5b9e7fc69670"
        hex"cd9857ab32c7189567b453859897be9e92f6e91b110766e4a47e8395e7ae97a676cc0e239354db3cca5ce7d1470da2d0ac42c81c3bc34dee778ff4de"
        hex"9cb45d3999e285ec3a79a5ae8e14b7f38da90cf9896eac097e12115e9ffbedb8db68db925b93fb5870e295de8f4abcc214948cdb70960b28366367d7"
        hex"043a0f3d587759b23cda0d4475048702c153d56310b3e2c4fa89241177bb9047edfecac610d9dd97f04f63797b4bced76b925e8908a48519aebbeb1c"
        hex"e9281eb80f988820a8b29a5a794345a156e88da6bccb70a28c8140fd37cc9419959a0465c61d5e5cbf68e4f20e7d445d495529b238585abecf2742ca"
        hex"ff1ef722dfdd515f1cd6b047bde4876410f18cff6e8de27002b1a6d5c6031917d0656d2b8fa494d47bc914af45ce76a8c67be2c13424729f246ff93d"
        hex"a4d06f4b7551210fe6deafa508fef6aae62eb4dd490ce3e2f0e35dfd667d73667a9431b749e4b40135a1dbc0ba9cef46de31b6206a2da88466f03bf4"
        hex"67b71ce2d9db8b8918192c2256055a5aafca57813c7b8aea5c7c2f344461280b4a0e3f4190113fada5402c2eba638e07620541c1c97f04b00b04ef43"
        hex"f354945d829c28e1911d85001df4b80ce5ae66039e292801b6b4f9c0d27281da28c4b4e40f866f79c47499c0d9da8c2022878f19eaf4e40dac9c33e1"
        hex"dadaf8dbf21cd05d5b505caa5d9dfa420093ad1a42606ad931a724891ee96ffb1a61c328cb5d252893c50b190d4fcb400fd63f123c47b06c3e011617"
        hex"208ce8b5d331d1db955cc888d5491a8e45c1e2f984347823e6ad7d40862aa02990b2cfb9d987ce4ca64c7a771ac737b3de1d983a8e12272a08a6768c"
        hex"d05406a5d35d454aca470121dd7fca8a2f594040672290a8f7504806dfafe6d3df98ac219fa86304aab4adaf1f04d9fff504ba27c2a0b4f474cf921c"
        hex"9a28b5e93b1036b5b5dc954e4d24c8b5e27c5a6c0140b0da5a586e592d3410108886fced574d8b577c0e927b320963d203e9b028f3566342e6380e1c"
        hex"403b770d026a8c693287d86cc10a7c9885456902bdac75b91627f073936ebe05bb5d5e25b15e48482302abc6ac25b70e490c7642c823693da1decb8e"
        hex"908115ad8c07f72e9d3c74fb6eb7915dfa21cb30cbdf9793ea108d5bfc3222aca0a0e68ba72e7d07e54bf5e7d26fc306d6a313ca94177c777b0ae4a8"
        hex"ff0d95135f78fc66989daa1af40a0a8e5bdd700f4fb09ffa84a45dab0688db8553d652119593fc3fa2d5c31852946f3175e2e320ba9e8b98cfd413c7"
        hex"174d3b06d9f8f7811a8c613d090a680a0352bb20372f5d188546153c3ede55ba51d56a1b2975ff065456d5d6f8a19521727168eba11e9728cf8068a5"
        hex"6f34d6e7992452d631f5bb0730d1eb0c4e3a5f1d26afbdda716f9dadd2f94ed965add2821ca9715e5b95aac38c64b66ad983110906b740f2d56a2bd5"
        hex"5f9bdc3b93956dfa2cf97e667635a30a67a2423668c6c70a201b20fb03a69780a3b1f34f57494296849c837cbe689d7929dd0f1dbbfcd909e4ab3977"
        hex"fd5b7edd0f0d009419d6d549a4d05bfd75f78036ae7fedfcab2c3700481cac94edbc905a225dd7d7074742bc93d0254ab0e8bcc9e02dc190719d5114"
        hex"f2c1455aa88db251d6d147df5bd9b6e50d57280c0597927e7d08aa8a4543c50aaac53bf6481ea44d9af6e8f814f0b6cf2c6f33418ce80702e882e343"
        hex"d7c1662642e2db28b5dc2f9c22738a2a16c0c1557f7e439a41059551acd2f326283c821a64fcf8a29654e0a5bbfb19b9d80956433b610ebe6b305da8"
        hex"538175792ec1e11e0da9884debfbd5e7dd64d219fcf03210e282914423302ea7f8a6251d7836b10422e1d2bf20631c96fc46f1b1d43ff698d41fcec7"
        hex"683b864639a7232dc091f32898e8f3472eba0570698d1ff98c9033dd4a65c76df444b8068b96fe33c026a510005627785d14ba53edf9a095d7721e0d"
        hex"8c8af0aecb056aa1b553441b5ac9611a8e1b88af6420a1c9168ba546d9df7e8ffb5f2334c63554fc798c7626b4f56f1c53d5d81dec59784529768767"
        hex"65116cae2bb5f79a74f06d9ee5cf02a78e9dba1028b72b9d24f9391115615913f2e573c796ec7dd7527400b37be90d77f087f41d974ffae60424701c"
        hex"9a3282f496df46f8cc98229975a70b2c3c70f33b3c410f21a137734831f1cee1c5452bd11792f9f7b939313ab964126cc99dd0fc7617a303f54b5dfb"
        hex"c0e0f7c5e454a0db289d053ad7ee51491c6a28f9226cee1389fd062e0c86ddd4c50c55d95ed297edfc3b6cf1143e7f848e775dc4df4120f609127327"
        hex"80ef9de7d91a30b6795adbb5b8f9ed3b9f5e18d663e51c4d0b57ce4b16c24a116d2c5a2a1023fb41c56cb398e9bd10f78d7c5423c1e960670f6926d6"
        hex"f2d9732bc52b4d41d99c8b2bb0a115f8e65e24b227b3ff1025fdf2ad2bd467a000be082d0315f19422f46b528766d8ed36efaf925720faf8eafb0d06"
        hex"0a9b6a70e004121ce6bc683b418947b4801e3e17ec544ca5b6302149b2fa9457b6df48c37809171abb4f3197fd5f3bc4727da607a0c166748275e095"
        hex"bd1dcff35fbf50ca1b95ad0510b338c3a6a74511a590b748bee7d9f0a7fb8b358412bfcc83a456d0fb63061fe75957c395b2ac7df0348029cb288b2b"
        hex"b4c4b167a2c0ebdcca3a97e4ab8ff4264c0f647f5365e495704a4e2fa7b2658e3f6578ea25c0cf54588047556f7ee821dbcd9eefd3b779ef6eba2d4f"
        hex"50bf08d78c4d924e46215c3e3521b5c6357770111c0b2073af09f6133702d8412e601fd3d7f1027f488a502466dca93db353590432f9ab8da991b062"
        hex"7318121e689c60d4816f794aa9bb214f57865f9a9f0bfa2d440dd35fb7aef8e1d577e0c92a4e82abf7788bcb99c1a33774d0d241be9c9c1e5d96e83a"
        hex"effd3ad725669f2043d541e2ada6d8c62f1858cdcb459c9197cf4f2190f47296e8cf3eb075bd919efaa6bd83bdc16e0ee872916df1072f67164b2222"
        hex"94af030b7e2b73216720f90e704586423fae7dad01f829617675312f18ec990303e462dda15f38cf66b5d44527118f3cba5bf9da07faf601af624cbf"
        hex"f08cfb1eeea1c6ea1fbe22c9d337a60ca0006ea1327ecf732568b594aa798140edf03b1d012195480865e095c5a3752fe41d855494c45b83d0f77d44"
        hex"33900fb8a6536f1934c0abe660b4708dc4ced131bf1d1596048925e973b49042b325a86bedd89205f3c417b26bb8babe5594ba9d350184697cbf2901"
        hex"1eb818746cfc4ab796b0b20f1fe4124d182a0c11ba64977dd792070b0ab80013e1723208d5f2444cbd44fd146e3c7c507fd1efd2fa77da3290c1197c"
        hex"cb2a9f0a2899a304d708b2e90ac5140c9c08927a8e75f387eeee3248168cf94e0a0186b48e9bea80f91dd06995649f1001dba8c545cb84ef2bae4a99"
        hex"3b02e11863d28551b0a16ec46fc42ea097f05927634b504c9b4c74a9db04fb5bf30fb8160b0b884f5b580faefdb24e2415a51212cdea07c4124c4e20"
        hex"66f83d09740472ce4b509246335da8c1d46b1a3bc25749176de6eab8ab5ed3b156cdb2895287e245f62484bd26199e6ff1c4541c5d8afd2ee9cbc820"
        hex"c7841a8b7f11b014edea5903f390dca48bfa23ff69b9fe0d14f8632c921641b0d6a8f039b44ab6c5142da58ccef9cd1f413b8e4aab41b7778c122210"
        hex"fe82224d7109635d6196952357a73ac09f579a0a9ad4ec486583c069f1dbb20fb8e6675af6431f90c9295ec67963f902f0c14d1ff90c5bf34bfec5a4"
        hex"79aec72e85410e921d82116b7fc138979bbdaef4b9460797c3042025d452302b78e4850990a2284a419b7895c316e16b489895c33036e65b872c2c6d"
        hex"cb58e71718c694232bda8f5a753aa585f6f85465714daaf39f1c38e740a41f5f8d3e7f01e0788c1ff48acbe4c80918fc020eb1d93dbd6855ee462dac"
        hex"c2fc9c6f68d6a658a7b85c0dfa1471c80c3017fd4ccd275083b6ba95cb7de23ee3c15a309b3e04bceb5e6502c565f67e614859b0572fedfbfc58953a"
        hex"9343a50f3373f7db1f89cd96efa22c0d2b3822c94a605c98613c14c93e280187d0fa362eb97c402a2c660493de80b71920886baebf935b322ab5f393"
        hex"5d3daaffeef7ebf3f2025adab412ec075570bb033a01f15ff008f7d5f7289971a5953b60207190c3f9e68c356d7a65c80c0edc19384f4e68f435a87a"
        hex"3ba3f61f7e581e156986a0fd881e64f6525296c5f4d3bb0c10c672521b60807a9af2f0c4c55a755b66a9a0f578d7d9bcc67eaf21d77f4330812612dc"
        hex"db8c04b448be54826fe06ad5d385e463bca621d132cc3ba8727c06210e21a5ed867ac5aaa4c3e8d4f00f2f96cd83148dbc396d57658747861dc02625"
        hex"e5a499a5926e9139ffc51c8e38d0f0ff355f6de0b29a81efb2b5be1e8af6d40a898c2a360d54652ccf6e929df6cf588dc8936b2d3d0de2dd33cdb80a"
        hex"ae2ca10aab7f584fc3a25eef7b48011f46b9ef7815b6a6bcf6bdc99345750bfa7a59bf0bcddbd08601ab9f8db07f6c2be3adaaf48210e5ed81cee23d"
        hex"19a518ec4a029e116e10af5ba3d14ff6e2c97a5b64e8b5cced1a34ae1ed5bf4d8e0be6fcf899930752671c2cc7220292d32dfb68a3120c6429289a34"
        hex"f688077f082e307f5e117b28c10e3e94e70738995c8d8d71bc976272a6395b57781b2336ca88776bc4ac020111b937af8e4d47bfe15c02412172ca53"
        hex"b235996b104a03c4edaad520b8672a075bd210220e3c9abf488552cd05f68bac8a11e667cc783158ed0b9a610f489027e8ec7d4046334294f65efd1c"
        hex"dccf8b4cd74cce4711e6b4668e086511f5c330164bc3cefa72f25489c5bae5d7dc515fd9dea8a6e5138e802271c40f0239a5120ed0d0df54b7ea3316"
        hex"1174fe4251fd9ec4cd1ba9f027a645d92673f01074cb5e2e08798c2dfdd5fd4201ffbf8ef3bca48d558729bda04d75893e345f78dd99ee189bec7053"
        hex"474fc6eb6a85c86cee2ad67a1e64442724e04dd343b1fe4bd37f4013e67fa4e59c232ad5fa1deb09f69853a55100b3a050232716d69120f03f72fe2b"
        hex"1fb47bd938b038b9558d6ec736ee02f6d6a22c75944f0b01273a11725d989b1b69e7e853b23f16c02aeb29480ee3d2721f2a8d0e7e1dbadd8032af40"
        hex"336c4830b5baa69dfc8133bf9473c63ed69f33c4700e3d629383fd380045b62d386215255d0e0b79dfad3cacd441ba65eecb39c924153db182313b0e"
        hex"07e34ab5f2ed202f86c0dc27d919f9238affe24c3a66a428f99d0b358e41a42ef52584ea2413eb267441add740845bddc793901621db5bb773c466ff"
        hex"c370d1884ed3106871f85226b7c941adbab954dd04340e2636becc40dfcd99c2039fa4971e2be07144b00e00e72f33460671f10421525e5be0a2a208"
        hex"a111aa757beef4ce6f18396a4c6a6a0dcb40775b25c9ae4c78d4eed495300568e9ae9562a4dfb4db9f45c7c8eaa3221310061cdd874f08762ac22d0a"
        hex"e5dbd499fc9760035f7130dba2a3acebf6a50e08ed18784d71dcffc65e37eb7a18cacc68ff3399adff6e46d83ebb485c9c130010314f37aab469af65"
        hex"739dca5b2cbeef15b610347e03f1b228ada84cc2a50e80230782ed3a6645ad429a2354b69279c86e609d28f50b0f1194479334f9fe443a0cbbea8e81"
        hex"0134cbcc38fc5140009df2deb96aa813c52d1b50f6ccd9d75cb19b08d99659cd71b8f4df8ca95a24db129d408e6a9bb4915a7ab63630a9a01bb1260d"
        hex"41251e3eefe155125a608425219d801c988e8dfff4deb75d98162ba93da58a26f46ba1edb8d13eebcae293b41ebf2def90eb292e3349d59494d1da12"
        hex"cf54a50dea82f246f0ea00d4c6590a5a6a9834d349e2bd59f3d4cbb8d3009ab10650832bd62249b1778d6d60b2a35ca8b639df7c264051d7f2598021"
        hex"85d0108c6f5903267551960066ae8d32397e01d341c079311f3744dd6d772ad58154d9cb14c9621a69d6dab946050199c6cc4fcdf199d87b9312bf4b"
        hex"0c320ad7bd741b110124e914d5fec9bf22f80d0b8a373fca2e1d08a814bfcb09ae2bedb56a2cf581edab120b0291ec6a863394b50e16ebeb6ad514bb"
        hex"dcb412da41de27087f0891e9fd902025ec2b7aef0a48eff85161bcf2ea291a43044f96c2fc93fe47eb5520e481c29526be32f0f18b46514309eb7312"
        hex"d4bb07355dc3a6a6bf36e63b4e080f023ecfc103824af90dc887a5a1903909261bd67d098e9263d662970fa5a238c3905c00531e27911773d07b5467"
        hex"7a0a69eff8cacc76ab0870f593a1c7fbf284f29744f6902bc4f710985ab43cd200c8faf7e67b4f14710d4feed4f4ccc1ce396ceaae270a002ade4585"
        hex"59d7531c99559d3c6dfa8995e0b40f05d07b4f2eb030f98981f154098c614ea15bda0d855376971fc7af3ddae9c8f3fdc47c6b11bfb00d523a475c30"
        hex"fb45b1487a2e06ad2ac9a79753e0d4498b49d80c0e223ad6dc284f8038e0f01b2a08bbe46f8f9082c265f24ff2e9c1f9176bf2d39cb6ad5aa8fbffec"
        hex"f8bac52d7c988543b08e4c0c38c2b2fa42752cbd2440a5afeae604595803ecdcc475a30a7c8fca2e2b04df82fa2350ea5b781a086079fc6c866bd849"
        hex"8cba135e45be680a8d8c442044f3d6fbc10b34b73635eba40198289f1ce50f9ce432b9f1fccc0e077e2332573416576895922c9d63762f4a547130ab"
        hex"ef4e4810d54ce86ff5f68e0478f4b6403ad9500d46386dc970b18925640775e0d8be2a24974c40d6794caf02d8dc262db08442a5568fea318fc3fb9f"
        hex"4613242f160be7cff6ef5afbc3ea6905428b718f864af1a0a771dbda8deef229c9de345f357ace0e1db91b1bbf9f481b8f529ad6988825b979bc4355"
        hex"09e8d92f31402e070030640b3b564b74e5c0e005ad9070fb41fbe650c33f5e0d78109fedad24dbab06eff22330abc9b5156dc62382ef6d8edcdc291b"
        hex"bf98bff9381bec37ff225e0c6312f2a87687f92976ab1028b8d6ab50a1cc8c1f7918c4afcf4260b3db8fa5e1d0197ce7d526f6b709185a1e93c9e50e"
        hex"c90f80e845b062c8e85f07fc651cd0ca7edccdc266211462a89e07238c365683bbb8a63f40b89b0629a592382db7c10c2bfc7bf79b27814a43d29a22"
        hex"b9846e80dd73d1e1fa43ac45e46f3c1ae0d2a7c14cb0f66f060035a020a55d2dbfbdb2184ad4ca1af01ae39e140b7695c388d00c085b8e96354a1a12"
        hex"d7b28601cf0bce5116032fdc09023748240bc03ade780d5adc4c68db009fea4b3ef04516cdc1be5f41487d530fb4b1a9cd4cbaf27f0230b6f22c1978"
        hex"275a410f2765c3015688487bc3987d390474567dd6b3980384f3379bd0812eec9dd21c9d0c73101773c98df1dd91620d86949490034bae2fdb223842"
        hex"69e1d28baaf2496bcf768d1810ce2a5d423198220fbe6bdb84f639a55717425e96ea1b7d622a0807af13eb03ca03cdadb60573ded518ee3463f04c93"
        hex"ef118bbbeb81fcf8a449e7e8b1c5de14d98861c46db120335d72379b5bfcde6d98bf3acab969cb49382c5e6b9cdd8a0187bff940e213c409785057e3"
        hex"cf93add94dc0ed18f35d8b33a541c9335067ca26ee10eb8c35228e160cffa1d4d64dbb119d2898e623bd4be41ad6fee54be2d906261a96500880e693"
        hex"754816d0accb468e79700bdb84fd7155067051bb46510f2bc4b56caaca10f956119b16286b5f0dbd748ba28818b40737741b5c66f90dd80aa387db9f"
        hex"ae6984c8a9722d49c1383e505b130c94be9d9196dea91db76db62212225a39cbd48634f84fb7866eec6a304003d8aa72d463d13b45e722e48c81481d"
        hex"ce4977e67e6500e0b318c17d86b786e30a5039c44198cbd1ceda77674a4c470e5807e129fab8fca9ab52d91c483511c33bf0f4456b3341bf3d29c1da"
        hex"ba59d10b2b7f0c621cd92a381fc6c51cba0e4d95a78f286385f0ba0ccf4fc5753fd7b51fba56b6f2e3e773ac293846684f461794b933852878fb89bc"
        hex"a5c57883ce3c002a2a9124c73c526daec2d68db77e7b0f50c67684c86e441d452c09e5be971f942b75b87dbc5473190dfcd0c18817aafee6bd83068e"
        hex"8de1dae508e88822c5ce110e1b0de305b9d384a286a63be62d2e72be3904854e571a4098ff28efe1a5f3752642ad86bb25ce0e3a4b8bfb772fb6d0ac"
        hex"3234316fcf72dce162079d2af57cf425a6110918e737f3aa22e120a5a1927da1f93403197c2fa4364dda4d34c201db1bcb26d44e66591a1aa23681fb"
        hex"7f267224c9cc1ee997690e7e02a0771266a4962d3aff9fef75ceebc86b3f08935fd4292965dad84ad6c02aff68ff8f88100bc311640367e7af812934"
        hex"501ce5a36de55ce0faab9f450bbfb31add8580803e5e8d097ebebb4ebf58d95ec9a5f6d7f3e750daba36cf1dbff35b16ca62a23ce55282248598ec38"
        hex"6712ed6d06db2b0957f3b0f6cfe379f9c2e4b2bcd70387fb2e4b301b4df7dfed0af096f94c1e6924f4aa7c21012ec87bd64512986a2c9737a1f59210"
        hex"e20a282ec8c342e5d511af71e32f66d5473a1bd7e1070e08b48a1328faa83211aada0da0015aaf9c670f028f82e9fb3754a8ccab234fa53217cd0631"
        hex"461e221f285c573d5a3fdca8f5cef23b33991b418c47270861cfd8770a85e1f327955f093d21372746396460d08457ed604dc0fb237ad6bd953450e6"
        hex"2215c05f8f348d225b7c924d2e4fc93677c0ee9749ffbf05fafc65ef6a4ca41865325bdbbfa49e03540a83b3ac904866aa78e6bbbdd5700bdea051b9"
        hex"85098dcf5b520f7104bd8609df01e70da79c098fe4b5c7abca7060a7a0a0675ecf043eca0e43d61badbd4d24b0c780e31a2293eae55b0a60b17d58ec"
        hex"1282f868d15528102065fea562fb5c201da18c34b973dcd98412e28ba302b9c5f0b8c08df7db3f993ad13afe164a731c190291f253de8b8852f66f9a"
        hex"9b10d087c861fa641b01c347650d7d676d34161e528a6476ec8c97004d8f6a066a7c8f8a75f43d7bc5ca8dc7e8f53fb6718d8c01f0a61d29bacff8a5"
        hex"2f869595c035dbc34e182d0f018d466bf864a8d1bea483012d7b279a942979c973c505eb793250c288664d12719bf6fbd7582a4bc73f5802dd937c0c"
        hex"dab112fb6f0aea19111d45146c6b72cd9f2ee161a3b54d5635e890261999e034574ca2ea4111a6265adb5f508dea24016eb71ece834ea2343c21660c"
        hex"f6faa57f67af0d5341871d2d1efc0cb4c186f0c8911fb38af69dfb9912e84f1bd1e942a3e28c6865db617f17784b057e91e233478eb7da14cedc2133"
        hex"f1c0c3181f5ed394ded8b82704ceaed430fa3ecfe10a98cd75d92bc75f069af8757e240ff086665ac3fb29153f12c29ea74a2ca63ae476d277bee32a"
        hex"21e5f9ff7ca1330b56c6b339ebe9fd68a680384ab5d13cf7bbbd43eb378a1b1069e049baae2b540c854c92cef6c567123db58e3c371d6438cfa16b8a"
        hex"eddaaa927111dd2281a6e8125e8a5bb76f84e206c4b2c1ef210237777784060558f82dbaefb1711e293f850e86549d303c4b587db2d759501e53d377"
        hex"b4e625309e3cd38fce3fea16c01529082695edd602bab731f4c71c2f4bd21cfb098040bba3b895644b8fffddd106d227de4e357334b411d6e8875245"
        hex"9114fff8747dd5ff16f93d19dcbb0f5e958c4d05bd0d65f23cee72b24b957f5553c71d699e68b77d128d56e6416746c7d605a21ddda9fc0646e77e27"
        hex"6b801fdaa8908cd8deafbc7c9150452158e7d740a49d1215e7a6fa5f464bf32eff710b21304e8e13f5158cac8df2e69b037cb72bf551393047471023"
        hex"fbcee9257da3f4937067881d74eafb3cfc38f2122e21c159d7ae7112a552be13c86793be2ecc20787e71a5c73215997dfaa887bcde56216e1c0c0413"
        hex"02a5b0f7829d2559bbbbcd205e025aa3996cb63c0a23e0397f16507f75554e1fb2502bd9c92a4181290253e2df05f80a4a50ab535da62dcfffb39aa6"
        hex"bee9580596b613416236a083508ec31eb4acfa890a912f4fb1b9351fafe64b1b53a4e307842b918199eeba095b5c7a49291e6eecc35aa804e0baa04b"
        hex"9bcf969cf8769c07182ee82d2281263661536524032cbec1386c594525b1b1a7a106b6ba177405010f76ea1afd85b8b70b34abf794a1e350ee8aa9a7"
        hex"c5e55555f083b755322bea22284d816b1501f6aef6fffefdde614d9773043d514173447f2fb595377a9d9909b3729304d514d7f8d13da09d59b220a2"
        hex"00f071316a0e8bb79738584b4f6e340e10e780e689ac5f26e081164f2dc64b2a5be43dc573845a0f3dc13d0007a4a3161de1c2495963766553e1f624"
        hex"930f2ef6f57bbb508aeb0f940af0eee9d69ef11a7fc6b43ee93ebfc46e423bb5d0e0087e8cc78eaf70238e9d04fe318128c2c402df9214f35f0bcff8"
        hex"3527e4fad5d61038c51fb15678305072823c1f22d3ee811f8eb1f44273f2c9bff3cea85286e0f7e24188e3cd43492db4b506424eeaf3c3139740c9cf"
        hex"ca8b34c0f929a36af725c6c5e34940b780224279226789b2077b260a9fe3642e8c2187873de1511c18ee1bcec6e978136523520b13686548e4b73929"
        hex"b2ef1aff84f448b4c4f2729d461449b66a53ac62d37d5527f5b450a9b96ddd267fc042656cdebb67318586483632b66801a3e11db211f127f2378059"
        hex"8a11280b5ddffee9105646ec63fb66016ad47ca353fab424204ea793b2ad07c8eaa50b1e9ab78557ab0c3a16fe408dc01f241c1775c73b616825269d"
        hex"37ece72941855220bb33052a9aa201bcac8b702e0580c2e2ba1bd9a2ec062aa1c3b76e30893d9a1653e278fc7d56720af26fe87dd4e1ea9a279a8415"
        hex"0044ba870f688e5fe42dc907832a14c8f9fae5f6cc35b1d3c5d4949da2d0140090d593324e8d5c4cd628eb27b544cb931b571488ca158ec5cbb61dee"
        hex"967c0ed2fcb988619d0007d7260bc31b91450c0adf85f54fe8ebeaee499ac288ff9fb0b4abf3867877c735686c1ac402cfe0495b7b60b3a408405033"
        hex"395cff3b0cc403cf53872dadd359b66f9b2901025d38e6dac8303252b791d971e8dfe09b6a3c587e690e7b52258e3bf4323d502b9b7ddac44646ae62"
        hex"c0c55c7d2746b6dd6a8f52ab0cc7219f0b18875810b5dd0c9ecd85df443d1089d2a07664b5938cc77014b66f14e6f6a0197ce1d99878990286933ca2"
        hex"2a11e7511ba99aa2b9a84a9372b68f3587377b857168ba2378c00c1f6970f535cbd75133c4fd627704a82667f5f76def6fd4547a8eece054d98f7328"
        hex"93c4df5fa2f017f4e397a63010554a4031489150acc6cfd3b75d1d09c48a892fb0e5391c45af4f5f211c2a5e52a652a37a0dfdbc8bfe8b679824e93b"
        hex"9cac2a05cc1924ee2b8008c077f7fda39982e60f050d7c946c1430f41a0bad94d9c20622b4b8e2fe46b696fd28f7725246fb4bf7e70256caa5acc6ec"
        hex"c984ae71ac729913c87f5cf344453eeba6191dad311ccd0103e3fb468748c130593a6e80d21a1a1291b69abd7bbcf257617cab3489fcd83c89803f54"
        hex"65c0d5954d38c96bd3ef99279ed468e9054cda574b847734a9fe318343294463b84191a7df3218b2911c4b13d99c3bb34bf6bb6ce1335afa74ad5383"
        hex"b7812e21be6b2b6dff9a7a28d89a85246ea9b22986f4b7826a76109c8f647c303ce2d604fedecd790be4eb39cfd37329fd9b44e2bfdb5aca4db69e78"
        hex"d97f93112235bea641dbff7e4792f8d23b44c61d5efd2b02816ed18b0070bd8a333229722b553620dc8a689c41ebfba077d9a909d64214448f4f3ac5"
        hex"e541e71bc34095e7f1d40ada2814bbf17fca0a3f0c6f202dadffb50d6b89702f5b38660f2a4a17d6fa231d8f4692564c6c29e0db244c260c50aa7a40"
        hex"abffec3ad055271cc1d796d8c5016dfcb9cb2ab8bebeff109d9e2b0f660d70c4be110fab588e7209fda86b7a6b8fe5d6903db3a63ae2158d8c0bd827"
        hex"c9d48b95fb53ef2a7364821640305e5edb472877e31f658026832adc1d3c330db1960e4c484e68cad85c94c8d90ba5da3f7dcad80b17dca205f6d93f"
        hex"811054135c7afb270fd7c10fa37d7a0156e71ba278f137abd0765840aa723db08693bc2d0f019f1791acb74154d39a7dc85cc71fad04a3692ca39b61"
        hex"55f6dc753ea56816ecc95d0ff44c75dc611d86995b1be5d10902fc9472cb65c3bd3a2538938b731410078cca671bdb441e38cbec56eada90ba1ddd4f"
        hex"c2340a375bf2feb7d7dfe903173d06656f009f57d77e17dd3d311e8d8194e14b443880827a10ecb9c4590c14081cbc9435c57b89adca244374e7f559"
        hex"8e5b42e4572d20898fb3deaa2721bb0976ebd6508b1b84d881f5f659668f9b6a7deb06387a19dfb481157c8fd0f2ca246ac0cd409cb8be745598b4df"
        hex"0c373b3a1b5a57ced6035896526ff8593542581f77732bd79e53e7560f7f830d653afeb8ebd1a3778819b70da689ce5302087304fc3c140ad301e2ca"
        hex"d59ad6f94a879e56b9ceaa4f54490abffd2be6c4dab06914b90f5441d7f6c3ca5398df64f09bf789ddab0c0e36cde8d9d0f4c7a4384e542aeef62fcb"
        hex"d7a4c835c27047159d748785d332c61e092a953cecd0f104d3e63c137781479dc1ca45db1b7305ff146175c494f0e6adfe9a7520022d33757b1c871a"
        hex"7e373f6d86827ca8c1d5d0b4228c2d0052711378125d62c44c3eb84c9648e22ba5a1c01e5b6fbb2f956b13b6dd4c528c3afb68e6882fa08a3e5c1ac2"
        hex"e3ae842d2be33b681b6ac716a57182b280605738d80d71c7c6c4f8dc45e0f8d6ea93831846c83a2d006fd7d842673b8fa8cc45db1eeec40d03fe2a75"
        hex"fb23d901877aff137757846dd1f5ae5d527c8b2e333638a8bb8f409173e9e33ed845722850921c0709fad7b9217798d4165a0d855b9af344414670b9"
        hex"c6eb0b9403b0252c06d91407590450b03bddaf3d698183ba8c589693e1cd55d5239ca66c1125fbdf83d284081ab25032c0bcb1d3a74be2398ef6cfa1"
        hex"a25c63d00c775498303ebb07db74d504e2a0114620a15d80e6d8af26f4eb5da4d240a052176898ed4d69dd302b7b8e03d74c3e8bdda15b1249328ddd"
        hex"0257ba4af2d503bcbaeae8c6bde937fd063c16166a4e7f2f07f6d1b268fbe19bcc6d1b6697bc9ffe3631e1967eb49e15019de62ebfcba75989dc9223"
        hex"944678f41e7bc782ba9219732bb446d4dbf56daca5cd1a1dae9d96eb0f8977425d01daa1c4fec30bba3d5b0eb81061b51596dee0666f371b17e448c0"
        hex"62e75e1a4786324c1db0e5fbc2d93c8bb5b81d11fd021b7728efb3098bba1c15fc7074ac47b21d97862d32b4c3c126f0dfe285fd866aaf1f04868a2e"
        hex"c1ab6d5690b39b165ca77c26f244b09075e5ad0ab3babd8f45b163c8a6d19f14e505dcb268a866a783d6cbbbec780d078376b7f016c35f9575202e39"
        hex"4672a32e1a5d0500d89d23b4934fadb87915a872e9c4d58c03039930bcd4b77d3090952f7ce078c73b61be5efa0bea803b450b679b5db991212c210e"
        hex"ee9fa4e352ced0063305d5772e74adc51399b065c480417396755538d5ed0e3517ae231064ad8f0ca8c4efc451f947eaae850eb44daee599b319c764"
        hex"1b925d66abebed8a655b4f1769ad3ec429a606cac41c2daf85acbc9c43a4a8b5002c9c2180c29cc2731fcb2aed98c8d662810704050d9e2201bd848f"
        hex"771ccf8fc9d588da25a8d147c51ec9049811685a9c3f55c456f24eaac05410f3c9e4766f3eda3d4ea2881658108874253505d5e634b6ef7a45705438"
        hex"d89aa30d082ece48577e06dc00df7dbf5fb91726b2d96f3aa78322719923fe8205cb55cd7c9debbaa9afb79003bab2e84576660cd38124656a84f766"
        hex"167278828a5b25d42ad0a706d78bf0ae37892740325faa0c5b3586e58f4df59c7a7e8abe0b37173501f85e5f12df40de97172c73b8746a09d8a7d376"
        hex"9b1b67f91053a296a9911cc5ae4b69aaf21215fce6f6e5521a545f2c0928f2707a00f29175b608285671a21e0ed4d54cfca2f4e7221b180037de810d"
        hex"3a21af9022697b5622c430435b29204fc94c82eb0eefed5b7ef9acf2d446d50e368a792fa658c9f8736363b061e3b698a069a0c1ce2bddd590feadb0"
        hex"c2bc27084cf026ccc3ba6a0406936958e1b4e754e398a23a09b6bdbb9d7a076492971111207c08e21ea9bc2e9e8024e522fb60a92f12dbf0214cf93f"
        hex"16c46da907f5c7076c8ad09f5933fc2a09dc2b3d919eb12e30fae3259cec067a15a45572dc73b92f7d213060614c11a35ecfb422bbde36e683c441d1"
        hex"864c72a854b1e12ef0a9ee08bd1325faf5889313377b6155925f1a161e908aa9242f3b126658506221373a122d7ed500987bc248bbac42d69f60bf3f"
        hex"5e31066e4490e55a9e35b73730ebdc12d3bd6f9df149e593f4620b25443c5b8639fa18a809c7adab03fac1b7b1a0c81b55b51a6f83b48ed9fb6d3f40"
        hex"f61cdafac8c9b3192b16fc78e1ca20c5559cf31f81b08152236d191b6f949424cfbb34e31939e40a1f704c0633ee4de24873ae04f27e8544ca8ee291"
        hex"11a1a9b70cf6b3660a28c5d8d0e4b25be69d6d71f1fa60056abd58e2bc5997a6b26d6d623b90fcba5c159349e85b48421968ea3910ef5a230deab71d"
        hex"691f2216ca9d2cf95ab0f6d97325e79f403bdbfb51b027ebcdc3741eb1e194098ee7f396ccf446aa59773c3dcc73114084902c0db3b8fc879caaa825"
        hex"94d9a1dd84676762019f760647aa357bfc31eb524bb32e28d8c4d122c41c2d19e0e40daf8b3a8a0fdf104237963dddf704ee57fd28fa56fe645bad44"
        hex"bb9f702914218c93bd0f5fd1093a74f2dac03434bee96344e84dc8ad55db8abd591c8d27f0cec615523078c4580ac107fd4d36d2b45fd7d118b6ad99"
        hex"3ba09aa7147225106d600d1d53e1f0dfda9de8f18a06710e8c481c3b78c222ed337a8116d0b05929aafaa0d2ec1a4bc2ad0ce2cfeb8310830ec66082"
        hex"e7d52e997cfdb85b83f05f1dfad6a191abe2d50559d22f0ae78bd3538f4d25d5315fa5dfbfc6a7b08856680446b7759ce8d468b4aade769f1d577fef"
        hex"564f01bd29a79c31c7115e32142fa926d00eab6428a4df4a4717a7652449d11eb44483447706494cbcb45517b79fb01a5b8950fcc08698883d545ffe"
        hex"bd7e6d8a74119082c0b67f88c359ec43742e5f0c90eab382c98cce7c8eac75ebf38790680ae25866cdecd87f471bb796a2fd2b0153f252c583cc2a5c"
        hex"cc9cc3effcb39abd7bce88943985e6eec86298df48de7f01bef9012df9cdc9b69b281ae1381483494958e14758c9ffd1b5d3ce8e293ad41be5401d1a"
        hex"7e5e11fff6ec136e446e45b851938cd0fdd38b9e8e874ecd653ac02aff72afb60ee1b40aea277961445ee125f1a42a4cfa36b1c3bce7b9a84238b209"
        hex"f3d27bd627cd2340058087533607e5a28d462969f95d4fe582bae7ba5d27a122bb705ac22309c56a610fd9d78152802b8d17a717dab25379acd9f10a"
        hex"df058b1daa98e041045473baa07d6bc8bd5325221970123b8d37cead1c801aed9a3baa162706563f80a1cfd7793c0f3b5b791c9ef719843b0f3d2e88"
        hex"26a8adb4b068db0bb6639c41e9bb2c1b91a8af95178721e511b13195b2a5260381cde00eb53f8624718dee8f989cee91afaf7fb87171b34f52a0ee77"
        hex"917522fa35677707f9929b0dfd0ca8b129b1feab20f722cf389c76a681e5e4c5268bce20486dceebcf0b6a181b0b3edafd717a12792713b7676c9295"
        hex"8b56203c7a3b88087a25fe403d73432b6b350c6fa334e10d5982b56337e081b5fab1ab8ff02d782212d73cf1aa5a2a022b115808a3ea2259b6ad675e"
        hex"b257a4f8cd8b84f4dec7c93d5aa1a49eecdf270e31d7775f1c478dd8dc9980387c566abdfce74716975b8760182a87512e6cc91564904fe9a34b5ba7"
        hex"d03572edc07b7931e3612d3160b044224980a3c21bafae097b41fcc8e3e09eb7b79ce33fbac7bfac8987c91e8634ea5404a20738c97dd624a9335b1e"
        hex"acaf24aecbabce97ba976efa2265303917c57492eeede25e5d970c05cecc04ff2ef0cf4ecb6e1ea0bdbe65aeefef72080767bd5c47eee56481dc9a0d"
        hex"7a5ef94d455e0818d826af108d2790da3ca3681b75e3e68bd1dc2c5051392a04006a44b1ad162b267b0f0b355d334cd2a8c7fb7d97c57f8178f35e75"
        hex"665f0023a0de5080e6e33e9371164eaa58409ff5ae993097a548ea6b9c2d3437c98341008195555fd08fa85b95deff67f95c1962786ffc413e7fdccb"
        hex"95b39c4663961007eddd415e88a5b5d1eeebbad94c7eeb536f2334db3bf727a0bae774b825aad61390d93c1afd2c41b69b00c05ba153cf3f9adc8954"
        hex"2ee2f632514b0b68589cf01a8c05896fbbad6735b59178da70ac4e80d6a87343e08b8c12422a008be3dade16a3ce6123df339f37e913890d8d6156d8"
        hex"86068ad530725695dc4f952bf5ce642c08ab697fd3818df2d6b60d1567718f168908d36be16491c8f7e2236fc59d3c1dd5a88a65cf8ab867f7964e86"
        hex"cb9d6b9aaf88c888b88308c2eb490379ae7e9b1e8005e6624c631310df8ff53aac76b59a38f6a1b4961e7a43fed41bf670ede41176f46c107b8063aa"
        hex"1ae1e168e752fac0063f10319e796e8b9ec82db1afc65f211115337794e1793e70e3fb726787811ecbf0d8c84ae801bda5a24ee9059ef92d31b33841"
        hex"63f4f482c9e06e10082c62b6fd1afd92e98b606665d5a72bcc54c60c751315c1ed7327f1a5268d7b99b965ba9c2f6e7c16f3af94316f4293e6e2602c"
        hex"c19d21a7805a9d2e63553cb5c0eb47c91fcbe2683dd430ca0cfbcf4bfd78c01c";

    // ZK verifying key (VkBlob) for the beacon light-client ROTATE circuit —
    // one aggregation over the committee circuit on the table SHA-256
    // (17 public inputs: 12 KZG accumulator limbs + next_commit, state_root_hi,
    // state_root_lo, attested_slot, gloas_fork_slot). Base v1 (Blake2b
    // transcript). Hermez SRS (wrap k=21, committee circuit k=18). Source:
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
    //      ROTATE_OUT=fixtures/rotate_vkblob cargo run --release --features aggregation \
    //        --example rotate_committee),
    // then run acki-nacki-bridge/scripts/embed_rotate_vk_blob.py <this file> and
    // refresh the tvm-sdk opcode fixtures
    //   (acki-nacki-bridge/scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh).
    // Do NOT hand-edit the hex.
    //   sha256 6fd3b8b47716220b2e47b75f30b3921751e2e03d7af5a831813279a7bdb6ad86
    bytes constant ROTATE_VK_BLOB =
        hex"564b424c4f4200000100000c000000007e0000007b226b223a32312c226e756d5f6164766963655f7065725f7068617365223a5b31305d2c226e756d"
        hex"5f6669786564223a312c226e756d5f6c6f6f6b75705f6164766963655f7065725f7068617365223a5b325d2c226c6f6f6b75705f62697473223a3230"
        hex"2c226e756d5f696e7374616e63655f636f6c756d6e73223a317d8a0600000215000000000c00000078d954a4152ede302e415658146c54f2100aa5cc"
        hex"7cf5347a457200b12e38de28d1331f3ae14f45dc9a79f4b9895f681a9c9daba02b61c0fa0bbf0166ae0c020eb92a84dbc1c622281c309941823e967f"
        hex"54e7b345258512cac9cf79dc276af115be4174b5a060b1c5eebf5fac8f977c187c014cf283add9fc34377bfabe9d5a13b9c3e6a94c6be6b4f8b28a1f"
        hex"91ce55b160b66dd1035543e720574f7b9dd1d119a746ab8b5bf5d093557f129781f1e5f698beda229f61be27a244231c7a31861f22d7f9003d5b423c"
        hex"933b15880d06dd6132ed70e8dff64a937627a4a07e9fb705489f61b018a2fbdd395a890ddaeb9226b6dd65273e4d736405b6f93e2007a12059166d0a"
        hex"8a255d1697467fb3c50179957ff5a895568d77b45001b96782ca4112928f7a096a260967a6b87e39c606186097888fc21e1f0f74c3354238fc54951f"
        hex"eba36c95a678697cda6f9ef8402652f0b85f7550c8ff52239fd6d01481df1c1f033b39ce29cc8bcb36b307334f3daaf82a383e876e886ba5ee14a266"
        hex"40a87516c6268234604487a215436bde1a3ee096e33e1930025b261e7d84da49142f210cce5026ddf3c810f30f16ed130646fd3aa41712685c49cc21"
        hex"f448f2030171ee11b7d72551a25735af87b0bac3c55f5490770b3e9e994f6b6d547e6de5bf45bb0bd6ce8d6887b3e1293e2a3b4e75eed407ebeb0bcf"
        hex"f73d1586ada3897a190eeb22879d3a2f8660423ae46cf76c9642f5b3f70d3388345caed0e3a320c8146013114092cea08348f56bcf2f020b9bb515f2"
        hex"4283034213d0f7bf516e3f172286ba18cc1f24f4423df440c4916fedd3e7658c939654f61079b672a1403664a34eec083a329115a85fe9f262f8ba29"
        hex"4599dc6334bdaf342eea7c54f12b0eeace437f014f2c3526c8281de691cbf64340535c0cf73361cac7db3aa8527f00951431791d51f4af698c7ffac6"
        hex"b6b99e37fd91adbb919fc22a157da86cb6eef5bbd442e018a16f4238fb2b62166d4b71d963b5e3e7c6e299c157c08804830d499b17b2d51f0750af86"
        hex"4b8ecb7711b26f86903f3421c3d4940d93c2c37f7d78c86a6e3fd51cacbdcad594a58cd129919299c148dea2e06c5e10364aa82b1abc25ae178f4530"
        hex"7b909505085bcd80c1a8db6885997c929b9b04795f21527c2ad92bb1e50e611740ea7554e53f1d3913d74282d73ff0644f79e9817260b99e20676aa3"
        hex"5b63ed1c6698262f790ad3b6f0242b8d1d2952778ed43e2c028ec96f491c9e5d454e691fe585c7a25546492faa0a50572c066b7fc109fcee5b8c5986"
        hex"5ebd54c5ce116311295701662ccb91474917ee35b151bf3b245bbbd396fdb1e4c8529cbde22c2c20f27511bbc3598436676bc68d9fc0f8b8f2ad3478"
        hex"825484dbb27dd0ca0bf4a8043ae7c299078a237c9912fd188254a1250d991416d6cb665c1be43a6f2f94a00a655ccd708a4da2b74d953a400b490601"
        hex"9b7458e850ab040092c3c6653953e22ab5cfa3ae189fccb3db179b764ecb4df7741bc0b8fd3e53c68f9086a36f02c91fc194d88d24dfe503a74369bb"
        hex"b9ee1b67c4e85d36a8a8dc6d9b1b8d2deb537b0db2696b17844cec672c3b6f14a974eb6e9cba219acbe072feff2e5ba5c05b281242412b74e99b0124"
        hex"6577fbd758841cc0dbf182d4f7d9f16e0f5e6c5c4e89592e970ce83f3918576b8cab35f218e5cef2488f48244dfb40f85e7da8bf8d6d1c00fb792cce"
        hex"8d0e4a5398a0f5205ff06b6d286c63708968b13072d67fcb282d5527f08b7f064e5a64f54b7c5a139611469db62c3a424880ae050b798ee259134607"
        hex"00ea3d598871de3eae5fb3b2ea262649a51bd48e96e07be1cb860692545ba02f57afec218a7674827dd1b397c089399763a107586a0c63462cb11511"
        hex"ff3deb0765af499c8223bb7e49ea8368435cbf71a2180f88888b012ca54a5b48401bc409786eaf9ec21ad2b80d5f38c7522350e29998241cb7ed5992"
        hex"ef545c090f0dab0d2fb2bd4fcfd544a74ca62bd681324487a4d14b34d7ab6389ac7e2affea3f940ccc49f6023311031767342c94261f2660b7d63040"
        hex"c42d346906ffbbc231d5602d18fee551303b9d8aa171c81ef3dddae8234c33f2e04c19692a12a0d0843b291c745d5b8497f0551ce5623b0eb89b0e15"
        hex"6e3cb2974d2ebfe0d637f044b78a7c22b26db415e78054232b74495d3194f30e08df9d79b233ad0926a358936c857a1a3098ff6bc7de971d5943a3d2"
        hex"43557fa2daf5be6e235b6f016471e73968ed351e0868f98e4a123006008fd9ca0fc06b1fab0d21a77e1da4112405e41e5156a00baab4ddac09d35d21"
        hex"e8248142054bbdb8605edf574f9bcf7d3b669e2e423d7d19";
}
