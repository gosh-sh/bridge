// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./ILayerHashVerifier.sol";
import "./IBkSetRotationVerifier.sol";

/// @title LayerHashBridge
/// @notice Stores Acki Nacki layer hashes on Ethereum, updated via ZK proofs.
/// @dev Each update verifies a Groth16-wrapped Halo2 proof that a BLS-attested
///      Acki Nacki block contains the claimed layer hashes. The proof anchors to
///      the previous top-level layer hash, creating a chain of verified state.
///
///      BK set commitment can be updated via two paths:
///      1. ZK-proven rotation: anyone submits a proof that the current BK set
///         attested to the new BK set (trustless, preferred).
///      2. Timelocked owner proposal: owner proposes a new commitment, which
///         takes effect after COMMITMENT_TIMELOCK delay (emergency fallback).
contract LayerHashBridge {
    uint256 public constant MAX_LAYERS = 10;
    uint256 public constant COMMITMENT_TIMELOCK = 7 days;

    ILayerHashVerifier public verifier;
    IBkSetRotationVerifier public bkRotationVerifier;
    address public owner;

    uint256 public currentBkSetCommitment;
    uint256[10] public currentLayerHashes;
    uint256 public currentNumLayers;
    uint256 public updateCount;

    uint256 public pendingBkSetCommitment;
    uint256 public commitmentActivationTime;

    event LayerHashesUpdated(
        uint256 indexed updateIndex,
        uint256 numLayers,
        uint256 prevHash,
        uint256 timestamp
    );

    event BkSetCommitmentUpdated(
        uint256 oldCommitment,
        uint256 newCommitment
    );

    event BkSetCommitmentProposed(
        uint256 indexed currentCommitment,
        uint256 indexed proposedCommitment,
        uint256 activationTime
    );

    event BkSetCommitmentProposalCancelled(uint256 indexed cancelledCommitment);

    event BkSetRotationVerifierUpdated(address oldVerifier, address newVerifier);
    event VerifierUpdated(address oldVerifier, address newVerifier);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error Unauthorized();
    error InvalidVerifier();
    error PrevHashMismatch(uint256 expected, uint256 got);
    error InvalidProof();
    error InvalidNumLayers();
    error NotInitialized();
    error NoPendingCommitment();
    error TimelockNotExpired(uint256 activationTime, uint256 currentTime);

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    constructor(address _verifier, uint256 _initialBkSetCommitment) {
        if (_verifier == address(0)) revert InvalidVerifier();
        verifier = ILayerHashVerifier(_verifier);
        currentBkSetCommitment = _initialBkSetCommitment;
        owner = msg.sender;
    }

    // ─── Layer hash updates ───────────────────────────────────────────

    /// @notice Submit a layer hash update with a ZK proof.
    /// @param proof 256-byte Groth16 proof
    /// @param numLayers Number of active layers in this update
    /// @param newLayerHashes Array of 10 layer hash values (zero-padded for inactive)
    /// @param prevMaxLevelLayerHash Previous top-level hash (must match stored state)
    function updateLayerHashes(
        bytes calldata proof,
        uint256 numLayers,
        uint256[10] calldata newLayerHashes,
        uint256 prevMaxLevelLayerHash
    ) external {
        if (numLayers == 0 || numLayers > MAX_LAYERS) {
            revert InvalidNumLayers();
        }

        // Chain anchor: verify the previous hash matches our stored state.
        // For the first update (currentNumLayers == 0), any prevHash is accepted.
        if (currentNumLayers > 0) {
            uint256 storedPrevHash = currentLayerHashes[currentNumLayers - 1];
            if (prevMaxLevelLayerHash != storedPrevHash) {
                revert PrevHashMismatch(storedPrevHash, prevMaxLevelLayerHash);
            }
        }

        bool valid = verifier.verifyLayerHashUpdate(
            proof,
            currentBkSetCommitment,
            numLayers,
            newLayerHashes,
            prevMaxLevelLayerHash
        );
        if (!valid) revert InvalidProof();

        currentNumLayers = numLayers;
        for (uint256 i = 0; i < MAX_LAYERS; i++) {
            currentLayerHashes[i] = newLayerHashes[i];
        }

        updateCount++;
        emit LayerHashesUpdated(updateCount, numLayers, prevMaxLevelLayerHash, block.timestamp);
    }

    // ─── ZK-proven BK set rotation (trustless) ───────────────────────

    /// @notice Rotate the BK set commitment using a ZK proof that the current
    ///         committee attested to the new committee.
    /// @param proof 256-byte Groth16 proof of BK set rotation
    /// @param newCommitment Poseidon commitment to the new BK set
    function rotateBkSet(bytes calldata proof, uint256 newCommitment) external {
        if (address(bkRotationVerifier) == address(0)) revert InvalidVerifier();

        bool valid = bkRotationVerifier.verifyRotation(
            proof, currentBkSetCommitment, newCommitment
        );
        if (!valid) revert InvalidProof();

        uint256 old = currentBkSetCommitment;
        currentBkSetCommitment = newCommitment;
        emit BkSetCommitmentUpdated(old, newCommitment);
    }

    // ─── Timelocked BK set commitment (emergency fallback) ───────────

    /// @notice Propose a new BK set commitment. Takes effect after COMMITMENT_TIMELOCK.
    /// @dev Emergency fallback — in normal operation, use rotateBkSet() with a ZK proof.
    function proposeBkSetCommitment(uint256 newCommitment) external onlyOwner {
        pendingBkSetCommitment = newCommitment;
        commitmentActivationTime = block.timestamp + COMMITMENT_TIMELOCK;
        emit BkSetCommitmentProposed(
            currentBkSetCommitment, newCommitment, commitmentActivationTime
        );
    }

    /// @notice Execute a pending BK set commitment after the timelock expires.
    ///         Callable by anyone once the timelock has passed.
    function executeBkSetCommitment() external {
        if (commitmentActivationTime == 0) revert NoPendingCommitment();
        if (block.timestamp < commitmentActivationTime) {
            revert TimelockNotExpired(commitmentActivationTime, block.timestamp);
        }

        uint256 old = currentBkSetCommitment;
        currentBkSetCommitment = pendingBkSetCommitment;

        pendingBkSetCommitment = 0;
        commitmentActivationTime = 0;

        emit BkSetCommitmentUpdated(old, currentBkSetCommitment);
    }

    /// @notice Cancel a pending BK set commitment proposal.
    function cancelBkSetCommitment() external onlyOwner {
        if (commitmentActivationTime == 0) revert NoPendingCommitment();

        uint256 cancelled = pendingBkSetCommitment;
        pendingBkSetCommitment = 0;
        commitmentActivationTime = 0;

        emit BkSetCommitmentProposalCancelled(cancelled);
    }

    // ─── Admin ────────────────────────────────────────────────────────

    /// @notice Set the BK set rotation verifier contract.
    function setBkRotationVerifier(address newVerifier) external onlyOwner {
        address old = address(bkRotationVerifier);
        bkRotationVerifier = IBkSetRotationVerifier(newVerifier);
        emit BkSetRotationVerifierUpdated(old, newVerifier);
    }

    /// @notice Update the layer hash verifier contract (owner-only, for upgrades).
    function setVerifier(address newVerifier) external onlyOwner {
        if (newVerifier == address(0)) revert InvalidVerifier();
        address old = address(verifier);
        verifier = ILayerHashVerifier(newVerifier);
        emit VerifierUpdated(old, newVerifier);
    }

    /// @notice Transfer ownership.
    function transferOwnership(address newOwner) external onlyOwner {
        emit OwnershipTransferred(owner, newOwner);
        owner = newOwner;
    }

    // ─── Views ────────────────────────────────────────────────────────

    /// @notice Read a specific layer hash.
    function getLayerHash(uint256 index) external view returns (uint256) {
        return currentLayerHashes[index];
    }

    /// @notice Read all current layer hashes.
    function getAllLayerHashes() external view returns (uint256[10] memory) {
        return currentLayerHashes;
    }
}
