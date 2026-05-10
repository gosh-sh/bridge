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
        hex"2b48b1b65f065da8974022442b567190a8c755b8488d86195e785a0507db39b42c400affa093a2489a1aae5da1299a14f87b7a8836aefd77ee1728d6c0999af32ff42369d7f39f55ce7603fb0f4ca680c67a29c7e904872117c7f6ec7618276f1c5924e793e6bb3e6797e8c8bda07efa346c97f9a020a095bb93899effaa25911963d3ccb788c8c796a29d94206b60f912d208f23a8ab801f5797867226deefc0fd155ee6fa6e129262d7a477f49dfb1fc6016e2cdf21b12de8909c9b8eda1ab0f4f5bf8ab41fdcadae48cb033507c712e03baa161de9febe0cfcea586086a2606b7d0b3bbfa72652ff290f4f3d0e05b3d1b418cb7f05c98f72ffaedc848f214";

    uint256 constant BLOCK_ID =
        6172998380117772144178244320174097278732891977751035358440025816556290812809;
    uint256 constant BK_SET_COMMITMENT =
        8860947598963949231848474266167828716328059409075539559674326459702914164685;
    uint256 constant BLOCK_SEQ_NO = 1;
    uint256 constant LAST_SEEN_BLOCK_SEQNO = 0;

    function setUp() public {
        groth16Verifier = new FallbackGroth16VerifierGenerated();
        verifier = new FallbackVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertTrue(ok, "real fallback proof should verify");
    }

    // ─── Negative: tamper one byte of the proof ──────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_10_SIGNERS;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyFallbackAttestation(
            tampered, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "tampered fallback proof must NOT verify");
    }

    // ─── Negative: wrong block_id rejected ──────────────────────────

    function testVerify_wrongBlockId_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, BLOCK_ID ^ 1, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_id must NOT verify");
    }

    // ─── Negative: wrong BK set commitment rejected ──────────────────────

    function testVerify_wrongBkSetCommitment_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT ^ 1, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong BK set commitment must NOT verify");
    }

    // ─── Negative: wrong block_seq_no rejected ────────────────────────────

    function testVerify_wrongBlockSeqNo_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO + 1, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_seq_no must NOT verify");
    }

    // ─── Negative: wrong last_seen rejected ──────────────────────────────

    function testVerify_wrongLastSeen_fails() public view {
        bool ok = verifier.verifyFallbackAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO + 1
        );
        assertFalse(ok, "wrong last_seen must NOT verify");
    }

    // ─── Negative: wrong proof length rejected without revert ────────────

    function testVerify_wrongProofLength_fails() public view {
        bytes memory tooShort = abi.encodePacked(PROOF_10_SIGNERS, hex"00");
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
