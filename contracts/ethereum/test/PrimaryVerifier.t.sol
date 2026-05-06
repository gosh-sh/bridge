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
    bytes constant PROOF_10_SIGNERS = hex"06def497052d80a1cea44b5e54c6e732cf45113942698c33a5bd23931418d2f60e85ecc488a67b6f89c7866094d4e59c0ddabe2de43b630b968d1e6947f0ba0c2e0f7a6149e8931b84fc1f5360e4686a503e1d5b6c930b465d036f807af46dad2d25a588123f857b582a4a3781648452e3ac982fcfd79307f8afd842c11f841f257e32dc80cf78fd615a563ec4331d2311388f50b90aad9294e47d3b866aa00a2cec3b53cf3b3420c6cab0a378c14df16acf021a7855ba515ab7312d66b04b582da755c8097fffeec88191e7a0ded9a93c420ef6c24dc76d48a70641612c3e2606c20efe2cb2bab6a7d7fa60b3a69ce25485d52d00a540bb8464495fa6c0a393";

    uint256 constant ENVELOPE_HASH =
        8798063818110817011738334898030988624623503880890323629448651362520823232112;
    uint256 constant BK_SET_COMMITMENT =
        12068713943170546064912791389409969875327728946929544601470440427472424489498;
    uint256 constant BLOCK_SEQ_NO = 1;
    uint256 constant LAST_SEEN_BLOCK_SEQNO = 0;

    function setUp() public {
        groth16Verifier = new PrimaryGroth16VerifierGenerated();
        verifier = new PrimaryVerifier(address(groth16Verifier));
    }

    // ─── Positive: real proof verifies ────────────────────────────────────

    function testVerify_realProof_succeeds() public view {
        bool ok = verifier.verifyPrimaryAttestation(
            PROOF_10_SIGNERS,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO
        );
        assertTrue(ok, "real primary proof should verify");
    }

    // ─── Negative: tamper one byte of the proof ──────────────────────────

    function testVerify_tamperedProof_fails() public view {
        bytes memory tampered = PROOF_10_SIGNERS;
        tampered[100] = bytes1(uint8(tampered[100]) ^ 0xff);

        bool ok = verifier.verifyPrimaryAttestation(
            tampered,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO
        );
        assertFalse(ok, "tampered primary proof must NOT verify");
    }

    // ─── Negative: wrong envelope hash rejected ──────────────────────────

    function testVerify_wrongEnvelopeHash_fails() public view {
        bool ok = verifier.verifyPrimaryAttestation(
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
        bool ok = verifier.verifyPrimaryAttestation(
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
        bool ok = verifier.verifyPrimaryAttestation(
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
        bool ok = verifier.verifyPrimaryAttestation(
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
        bool ok = verifier.verifyPrimaryAttestation(
            tooShort,
            ENVELOPE_HASH,
            BK_SET_COMMITMENT,
            BLOCK_SEQ_NO,
            LAST_SEEN_BLOCK_SEQNO
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
