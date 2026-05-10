// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/LayerHashesMovementVerifier.sol";
import "../src/LayerHashesGroth16VerifierGenerated.sol";

/// @title LayerHashesMovementVerifierTest
/// @notice End-to-end test for Phase 3.3: Circuit 2 (Layer Hashes Movement)
///         Halo2 SHPLONK proof → gnark Groth16 wrap → on-chain Solidity
///         verification.
///
/// The proof + public inputs below were produced by:
///   cargo run -p bridge-prover-orchestrator --bin export-layer-hashes-proof --release
///       -- --num-layers 1 --num-chain-steps 1
///   cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-2
///   ./circuit-2 setup ../../proofs/layer-hashes/halo2_proof.json
///   ./circuit-2 prove ../../proofs/layer-hashes/halo2_proof.json
///
/// The synthetic Halo2 input is `build_synthetic_layer_hashes_input(1, 1)` —
/// 1 active layer, 1 active chain step, 10 padded layer slots (zeros), 11
/// padded chain links (10 inactive). The pass-through `bk_set_poseidon` is
/// the sentinel `0xDEADBEEF` (`= 3735928559`) since Circuit 2 does not
/// recompute it.
contract LayerHashesMovementVerifierTest is Test {
    LayerHashesGroth16VerifierGenerated public groth16Verifier;
    LayerHashesMovementVerifier public verifier;

    // ─── Synthetic Circuit 2 proof + public inputs ─────────────────────────
    bytes constant PROOF_LH_1_1 =
        hex"173e51f5d58128e91202ec7b3e6a48cb7c0100e3ea4897a86c90b3017e4603ce2bd6fb05e2438e04999646b40955ac974f068e77ff0c83d61fe3a3ac1aa5cfbf051cbb4d26a0bb3c4f55959e3644412ac176269b797058f83ad9027e81032f220d184fc2128587bea04717319518d0f850f1b2ecc72de79086c525fa11e89d2512302f7c796fd0c38d89393ee082b78e19045c81c42f2e32b1bbc21a1ba3113400cab4fbb64b2cc6f523bc4d24e9a954f6fd704f574bf132e03afb3fcf25fe151a629789802ba1aaab22d31418cbd9016ee86cd4a6cec2604177281e1dd91bc2251736d4732826be41ce07feab487471e7af48c942414a5d57b59f7f17006857";

    uint256 constant BLOCK_ID =
        4632658081093189414249586021269033046596116847351275129986961861250909326240;
    uint256 constant BK_SET_COMMITMENT = 3735928559; // 0xDEADBEEF (synthetic sentinel)
    uint256 constant NUM_LAYERS = 1;
    uint256 constant LAYER_HASH_0 =
        16175756571004841845076918654963776760534355234316594695443775841165724319600;
    uint256 constant PREV_HASH =
        547193593808027162361820057086019574459897834898992323377401094686459936997;

    function _layerHashes() internal pure returns (uint256[10] memory arr) {
        arr[0] = LAYER_HASH_0;
        // arr[1..10] left zero (inactive layers).
    }

    function setUp() public {
        groth16Verifier = new LayerHashesGroth16VerifierGenerated();
        verifier = new LayerHashesMovementVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, _layerHashes(), PREV_HASH
        );
        assertTrue(ok, "real layer-hashes proof should verify");
    }

    // ─── Negative: tampered proof rejected ────────────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_LH_1_1;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyLayerHashesMovement(
            tampered, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, _layerHashes(), PREV_HASH
        );
        assertFalse(ok, "tampered layer-hashes proof must NOT verify");
    }

    // ─── Negative: wrong block_id rejected ────────────────────────────────

    function testVerify_wrongBlockId_fails() public view {
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID ^ 1, BK_SET_COMMITMENT, NUM_LAYERS, _layerHashes(), PREV_HASH
        );
        assertFalse(ok, "wrong block_id must NOT verify");
    }

    // ─── Negative: wrong BK set commitment rejected ───────────────────────

    function testVerify_wrongBkSetCommitment_fails() public view {
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT ^ 1, NUM_LAYERS, _layerHashes(), PREV_HASH
        );
        assertFalse(ok, "wrong BK set commitment must NOT verify");
    }

    // ─── Negative: wrong num_layers rejected ──────────────────────────────

    function testVerify_wrongNumLayers_fails() public view {
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS + 1, _layerHashes(), PREV_HASH
        );
        assertFalse(ok, "wrong num_layers must NOT verify");
    }

    // ─── Negative: wrong layer hash rejected ──────────────────────────────

    function testVerify_wrongLayerHash_fails() public view {
        uint256[10] memory hashes = _layerHashes();
        hashes[0] ^= 1;
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, hashes, PREV_HASH
        );
        assertFalse(ok, "wrong layer hash must NOT verify");
    }

    // ─── Negative: wrong prev_max_level_layer_hash rejected ───────────────

    function testVerify_wrongPrevHash_fails() public view {
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, _layerHashes(), PREV_HASH ^ 1
        );
        assertFalse(ok, "wrong prev_max_level_layer_hash must NOT verify");
    }

    // ─── Negative: padding-slot tampering rejected ────────────────────────
    /// @dev Inactive layer slots ([1..10] for num_layers=1) must remain zero;
    ///      flipping one rejects, ensuring the circuit verifies all 10 slots.

    function testVerify_paddingSlotTampered_fails() public view {
        uint256[10] memory hashes = _layerHashes();
        hashes[5] = uint256(keccak256("phantom-layer"));
        bool ok = verifier.verifyLayerHashesMovement(
            PROOF_LH_1_1, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, hashes, PREV_HASH
        );
        assertFalse(ok, "tampered padding slot must NOT verify");
    }

    // ─── Negative: wrong proof length rejected without revert ─────────────

    function testVerify_wrongProofLength_fails() public view {
        bytes memory tooShort = abi.encodePacked(PROOF_LH_1_1, hex"00");
        bool ok = verifier.verifyLayerHashesMovement(
            tooShort, BLOCK_ID, BK_SET_COMMITMENT, NUM_LAYERS, _layerHashes(), PREV_HASH
        );
        assertFalse(ok, "non-256-byte proofs must NOT verify");
    }

    // ─── Constructor: zero verifier address rejected ──────────────────────

    function testCtor_zeroVerifier_reverts() public {
        vm.expectRevert(LayerHashesMovementVerifier.InvalidVerifierAddress.selector);
        new LayerHashesMovementVerifier(address(0));
    }
}
