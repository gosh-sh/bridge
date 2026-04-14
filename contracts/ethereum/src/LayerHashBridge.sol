// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "./ILayerHashVerifier.sol";

/// @title LayerHashBridge
/// @notice Stores Acki Nacki layer hashes on Ethereum, updated via ZK proofs.
/// @dev Each update verifies a Groth16-wrapped Halo2 proof that a BLS-attested
///      Acki Nacki block contains the claimed layer hashes. The proof anchors to
///      the previous top-level layer hash, creating a chain of verified state.
contract LayerHashBridge {
    uint256 public constant MAX_LAYERS = 10;

    ILayerHashVerifier public verifier;
    address public owner;

    uint256 public currentBkSetCommitment;
    uint256[10] public currentLayerHashes;
    uint256 public currentNumLayers;
    uint256 public updateCount;

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

    event VerifierUpdated(address oldVerifier, address newVerifier);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error Unauthorized();
    error InvalidVerifier();
    error PrevHashMismatch(uint256 expected, uint256 got);
    error InvalidProof();
    error InvalidNumLayers();
    error NotInitialized();

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

    /// @notice Update the BK set commitment (owner-only for now).
    /// @dev In production, this should be replaced with a ZK-proven BK rotation.
    function setBkSetCommitment(uint256 newCommitment) external onlyOwner {
        uint256 old = currentBkSetCommitment;
        currentBkSetCommitment = newCommitment;
        emit BkSetCommitmentUpdated(old, newCommitment);
    }

    /// @notice Update the verifier contract (owner-only, for upgrades).
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

    /// @notice Read a specific layer hash.
    function getLayerHash(uint256 index) external view returns (uint256) {
        return currentLayerHashes[index];
    }

    /// @notice Read all current layer hashes.
    function getAllLayerHashes() external view returns (uint256[10] memory) {
        return currentLayerHashes;
    }
}
