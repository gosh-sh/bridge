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
///   cargo run -p bridge-prover-orchestrator --bin export-fallback-proof --release
///   cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1b
///   ./circuit-1b setup ../../proofs/fallback/halo2_proof.json
///   ./circuit-1b prove ../../proofs/fallback/halo2_proof.json
///
/// The synthetic Halo2 input is `generate_test_data_fallback_all_sign(10)` —
/// 10-signer BK set, all signing, primary attestation followed by fallback at
/// block_seq_no = 1, last_seen_block_seqno = 0.
contract FallbackVerifierTest is Test {
    FallbackGroth16VerifierGenerated public groth16Verifier;
    FallbackVerifier public verifier;

    // ─── Phase 1.A 10-signer synthetic proof + public inputs ──────────────
    bytes constant PROOF_10_SIGNERS =
        hex"1cb89d6fa285c4f67c2290d7449ddfa84fb33deeeecec28db43264e88afdac49075d4c00e39326ffafc68f4c76f00201c838c2b00c99a22ae2478f89cddf668d21fabb2601bf26caf2b5f71ce45d2ba6ee90f7881441117ac1e542f41e5e56dd1771f7c5713dbfdcccc384884120905a105ec4dc7371c786e740b009975840fa28767f00cb90b1ba0aab04192b0b3cb2427609f21cc3e697ab206a5d375de05e085cc291503760eb7f3b77ed60091a1a1398edabfb7bdec26eb723fb5f8fa579145547ee74bdab6132f87d76033e616c8c8a51657b5c836c68b3c9efd0bd82ca13dc83ebb0498c6eca909b4d80c867b5d57120ce918ae90f5f98d9474d9d100a";

    uint256 constant ENVELOPE_HASH =
        20202806359575240131428837967779228818217543621557080492235371780190089760303;
    uint256 constant BK_SET_COMMITMENT =
        14809724215823772589831973348579836204559371810832509842002213641396869426146;
    uint256 constant BLOCK_SEQ_NO = 1;
    uint256 constant LAST_SEEN_BLOCK_SEQNO = 0;

    function setUp() public {
        groth16Verifier = new FallbackGroth16VerifierGenerated();
        verifier = new FallbackVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, ENVELOPE_HASH, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertTrue(ok, "real fallback proof should verify");
    }

    // ─── Negative: tamper one byte of the proof ──────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_10_SIGNERS;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyFallbackAttestation(
            tampered, ENVELOPE_HASH, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "tampered fallback proof must NOT verify");
    }

    // ─── Negative: wrong envelope hash rejected ──────────────────────────

    function testVerify_wrongEnvelopeHash_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS,
            ENVELOPE_HASH ^ 1,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong envelope hash must NOT verify");
    }

    // ─── Negative: wrong BK set commitment rejected ──────────────────────

    function testVerify_wrongBkSetCommitment_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT ^ 1,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong BK set commitment must NOT verify");
    }

    // ─── Negative: wrong block_seq_no rejected ────────────────────────────

    function testVerify_wrongBlockSeqNo_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO + 1,
            LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_seq_no must NOT verify");
    }

    // ─── Negative: wrong last_seen rejected ──────────────────────────────

    function testVerify_wrongLastSeen_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO + 1
        );
        assertFalse(ok, "wrong last_seen must NOT verify");
    }

    // ─── Negative: wrong proof length rejected without revert ────────────

    function testVerify_wrongProofLength_fails() public view {
        bytes memory tooShort = abi.encodePacked(PROOF_10_SIGNERS, hex"00");
        bool ok = verifier.verifyFallbackAttestation(
            tooShort, ENVELOPE_HASH, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "non-256-byte proofs must NOT verify");
    }

    // ─── Constructor: zero verifier address rejected ─────────────────────

    function testCtor_zeroVerifier_reverts() public {
        vm.expectRevert(FallbackVerifier.InvalidVerifierAddress.selector);
        new FallbackVerifier(address(0));
    }
}
