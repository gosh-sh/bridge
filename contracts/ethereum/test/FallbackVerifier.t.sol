// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/FallbackVerifier.sol";
import "../src/FallbackGroth16VerifierGenerated.sol";

/// @title FallbackVerifierTest
/// @notice End-to-end test for the Phase 3 first slice: Circuit 1B (Fallback
///         attestation) Halo2 SHPLONK proof → gnark Groth16 wrap → on-chain
///         Solidity verification.
///
/// The proof + public inputs below were produced by:
///   export-bound-block-proofs (bound scenario) → proofs/bound/fallback/halo2_proof.json
///   ./scripts/install_fallback_groth16_verifier.sh
///
/// Bound block: block_seq_no = 1, last_seen_block_seqno = 0, 10-signer BK set.
contract FallbackVerifierTest is Test {
    FallbackGroth16VerifierGenerated public groth16Verifier;
    FallbackVerifier public verifier;

    bytes constant PROOF_BOUND =
        hex"212f982de268d14bfb91ccc69ebc6c21d6ae2ae87b2881cc720a5592b31870b31fb381c2d182984116ad9b9341aa8e0820970e85fdd551defa426df81020584b2866279b1f4ec9b72992b730af5b8e5e61dd2b74751bac97e4517c64992ec0ed26e00df3e5b93e91b615b94eb668728c4ade5e6f86b1cba5d14857501cf58b2b2af996d1e5dffc86ce1a43495c44fb43771df6d7862d028de7c57b8e777ad81818a51b734a9f615495596dc7d52b905f59b50bee535f0dee3e8efb66855eb1fb2c7ecdccf9f7cba2bc3a3800bd88e74098817410014953887f9c075a1bb3d92d194092e7697e6e0e3ae28c21640909d93e9547a6f15998db594a210f18b6a709";

    uint256 constant BLOCK_ID =
        2366447193515816262113949878948219819843072667754353638138148121387707202273;
    uint256 constant BK_SET_COMMITMENT =
        2747213842646881738625689668394971873270726869184199131100305895318145776650;
    uint256 constant BLOCK_SEQ_NO = 1;
    uint256 constant LAST_SEEN_BLOCK_SEQNO = 0;

    function setUp() public {
        groth16Verifier = new FallbackGroth16VerifierGenerated();
        verifier = new FallbackVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_BOUND, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertTrue(ok, "real fallback proof should verify");
    }

    // ─── Negative: tamper one byte of the proof ──────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_BOUND;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyFallbackAttestation(
            tampered, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "tampered fallback proof must NOT verify");
    }

    // ─── Negative: wrong block_id rejected ──────────────────────────

    function testVerify_wrongBlockId_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_BOUND, BLOCK_ID ^ 1, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_id must NOT verify");
    }

    // ─── Negative: wrong BK set commitment rejected ──────────────────────

    function testVerify_wrongBkSetCommitment_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_BOUND, BLOCK_ID, BK_SET_COMMITMENT ^ 1, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong BK set commitment must NOT verify");
    }

    // ─── Negative: wrong block_seq_no rejected ────────────────────────────

    function testVerify_wrongBlockSeqNo_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_BOUND, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO + 1, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_seq_no must NOT verify");
    }

    // ─── Negative: wrong last_seen rejected ──────────────────────────────

    function testVerify_wrongLastSeen_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_BOUND, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO + 1
        );
        assertFalse(ok, "wrong last_seen must NOT verify");
    }

    // ─── Negative: wrong proof length rejected without revert ────────────

    function testVerify_wrongProofLength_fails() public view {
        bytes memory tooShort = abi.encodePacked(PROOF_BOUND, hex"00");
        bool ok = verifier.verifyFallbackAttestation(
            tooShort, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "non-256-byte proofs must NOT verify");
    }

    // ─── Constructor: zero verifier address rejected ─────────────────────

    function testCtor_zeroVerifier_reverts() public {
        vm.expectRevert(FallbackVerifier.InvalidVerifierAddress.selector);
        new FallbackVerifier(address(0));
    }
}
