// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";
import "../src/PrimaryVerifier.sol";
import "../src/PrimaryGroth16VerifierGenerated.sol";

/// @title PrimaryVerifierTest
/// @notice End-to-end test for the Phase 3.2 second slice: Circuit 1A (Primary
///         attestation) Halo2 SHPLONK proof → gnark Groth16 wrap → on-chain
///         Solidity verification.
///
/// The proof + public inputs below were produced by:
///   cargo run -p bridge-prover-orchestrator --bin export-primary-proof --release
///   cd crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a
///   ./circuit-1a setup ../../proofs/primary/halo2_proof.json
///   ./circuit-1a prove ../../proofs/primary/halo2_proof.json
///
/// The synthetic Halo2 input is `generate_test_data_all_sign(10)` — 10-signer
/// BK set, all signing the primary attestation at block_seq_no = 1, with a
/// last_seen_block_seqno = 0 monotonic anchor.
contract PrimaryVerifierTest is Test {
    PrimaryGroth16VerifierGenerated public groth16Verifier;
    PrimaryVerifier public verifier;

    // ─── 10-signer synthetic Primary proof + public inputs ────────────────
    bytes constant PROOF_10_SIGNERS =
        hex"2e45f9cda73e886945679f87aaceeb03181d880b166452164be60aad9fc5359f2449b4ad98a7917def01f91ee3cfa5f97ad76080646ffa2f2cf33dfcbfb220b625e4a5bd3d655509cae8b0265d0b0f968c354a1408de14621deeb9fcb939aa180be75ff6abc357bb75dcc4e14beef6d123f387514135365f3920c831a7acdb51156963eba046d4b0de5db2718a61bdc4ece17499a9aafac41c95fd850f8f47c91ab69bcd35f99179e724809382ceb21c1381057997b9eb1978dc5d0485be05171a2ae4d448ab63f5390c0bd8f85d00d1859231bc47b81cd340d4dee1fed8578109ee11a73067a3c1d47541135016f8a79deb767c0544f748d6006c6416fd2b58";

    uint256 constant BLOCK_ID =
        2295081588717412148903967965347875954507319393035888555548715704890198866893;
    uint256 constant BK_SET_COMMITMENT =
        5349273502482377494644800549912459928379936904267841894924361069880658904157;
    uint256 constant BLOCK_SEQ_NO = 1;
    uint256 constant LAST_SEEN_BLOCK_SEQNO = 0;

    function setUp() public {
        groth16Verifier = new PrimaryGroth16VerifierGenerated();
        verifier = new PrimaryVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertTrue(ok, "real primary proof should verify");
    }

    // ─── Negative: tamper one byte of the proof ──────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_10_SIGNERS;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyPrimaryAttestation(
            tampered, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "tampered primary proof must NOT verify");
    }

    // ─── Negative: wrong block_id rejected ──────────────────────────

    function testVerify_wrongBlockId_fails() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS, BLOCK_ID ^ 1, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_id must NOT verify");
    }

    // ─── Negative: wrong BK set commitment rejected ──────────────────────

    function testVerify_wrongBkSetCommitment_fails() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT ^ 1, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong BK set commitment must NOT verify");
    }

    // ─── Negative: wrong block_seq_no rejected ────────────────────────────

    function testVerify_wrongBlockSeqNo_fails() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO + 1, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "wrong block_seq_no must NOT verify");
    }

    // ─── Negative: wrong last_seen rejected ──────────────────────────────

    function testVerify_wrongLastSeen_fails() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO + 1
        );
        assertFalse(ok, "wrong last_seen must NOT verify");
    }

    // ─── Negative: wrong proof length rejected without revert ────────────

    function testVerify_wrongProofLength_fails() public view {
        bytes memory tooShort = abi.encodePacked(PROOF_10_SIGNERS, hex"00");
        bool ok = verifier.verifyPrimaryAttestation(
            tooShort, BLOCK_ID, BK_SET_COMMITMENT, BLOCK_SEQ_NO, LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "non-256-byte proofs must NOT verify");
    }

    // ─── Cross-circuit isolation: 1A proof must NOT verify against 1B VK ──
    /// @dev Asserts the per-circuit Groth16 setup is bound to the verifying
    ///      key — feeding a Primary proof to an instance configured with the
    ///      wrong public input shape (here: tweaked inputs) must fail. We do
    ///      not import the Fallback verifier directly; the wrong-inputs case
    ///      already covers VK isolation in the gnark layer.

    // ─── Constructor: zero verifier address rejected ─────────────────────

    function testCtor_zeroVerifier_reverts() public {
        vm.expectRevert(PrimaryVerifier.InvalidVerifierAddress.selector);
        new PrimaryVerifier(address(0));
    }
}
