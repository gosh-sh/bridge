"""BC-AN-01 — dappId replay-key split (contract + optional dual-proof PoC)."""

from __future__ import annotations

from pathlib import Path

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_INVALID_ZKPROOF,
    ERR_UNSUPPORTED_SRC_CHAIN,
    USDC_BRIDGE_ADDR,
    build_public_inputs,
    fixtures_available,
    fr_le,
    init_bridge_instance,
    load_fixture_proof,
    PI_LEN,
    parse_pi_fields,
    seed_trust_from_pi,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.slow]

BC_AN_01_ROOT = Path(__file__).resolve().parent.parent / "fixtures" / "bc_an_01"


def bc_an_01_fixtures_ready() -> bool:
    """Dual-proof PoC fixtures must be 12-PI (384 B) — regenerate after contracts/bridge."""
    for sub in ("dapp_a", "dapp_b"):
        d = BC_AN_01_ROOT / sub
        pi = d / "public_inputs.bin"
        if not (d / "proof.bin").is_file() or not pi.is_file():
            return False
        if pi.stat().st_size != PI_LEN:
            return False
    return True


def load_bc_proof(tag: str) -> tuple[bytes, bytes]:
    d = BC_AN_01_ROOT / tag
    return (d / "proof.bin").read_bytes(), (d / "public_inputs.bin").read_bytes()


def get_minted(tb, bridge_tvc) -> int:
    from bridge_helpers import USDC_ECC_ID

    r = tb.call(
        bridge_tvc,
        BRIDGE_CONTRACT,
        "getTotalBridged",
        {"tokenId": str(USDC_ECC_ID)},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r)
    return int(r.response["minted"])


def finalize_and_drain(tb, bridge_tvc, pipe: MessagePipeline, proof: bytes, pi: bytes):
    seed_trust_from_pi(tb, bridge_tvc, pi)
    r = tb.call(
        bridge_tvc,
        BRIDGE_CONTRACT,
        "finalizeDeposit",
        {"proof": proof.hex(), "publicInputs": pi.hex()},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, "finalizeDeposit")
    pipe.enqueue(r.messages)
    pipe.process(max_steps=20, verbose=False)
    return r


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_bc_an_01_tampered_dapp_id_rejects_same_proof(tb):
    """Same proof bytes cannot be replayed with a different dappId in PI (ZK bind)."""
    tvc = init_bridge_instance(tb, "bc01_tamper")
    proof, pi = load_fixture_proof(0)
    mutated = bytearray(pi)
    mutated[6 * 32] ^= 0x01  # dappId low limb (fr[6])
    try:
        seed_trust_from_pi(tb, tvc, bytes(mutated))
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": proof.hex(), "publicInputs": bytes(mutated).hex()},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_INVALID_ZKPROOF)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "bc01_tamper")


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_bc_an_01_replay_key_differs_by_dapp_id(tb):
    """PI blobs differ when circuit dapp limbs differ (on-chain pins dappId=0)."""
    _, pi_a = load_fixture_proof(0)
    base = parse_pi_fields(pi_a)
    pi_b = build_public_inputs(
        deposit_id=base["deposit_id"],
        sender=base["sender"],
        amount=base["amount"],
        contract_addr=base["contract_addr"],
        chain_id=base["chain_id"],
        dapp_id=base["dapp_id"],
        an_account_hi=base["an_account_hi"],
        an_account_lo=base["an_account_lo"],
    )
    pi_c = build_public_inputs(
        deposit_id=base["deposit_id"],
        sender=base["sender"],
        amount=base["amount"],
        contract_addr=base["contract_addr"],
        chain_id=base["chain_id"],
        dapp_id=base["dapp_id"] ^ 1,
        an_account_hi=base["an_account_hi"],
        an_account_lo=base["an_account_lo"],
    )
    assert pi_b != pi_c
    assert fr_le(pi_b, 0) == fr_le(pi_c, 0)
    assert fr_le(pi_b, 3) == fr_le(pi_c, 3)
    assert (fr_le(pi_b, 5), fr_le(pi_b, 6)) != (fr_le(pi_c, 5), fr_le(pi_c, 6))


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_bc_an_02_untrusted_l1_bridge_rejects_pre_zk(tb):
    """BC-AN-02 — untrusted (chainId, contractAddr) rejected before ZK."""
    tvc = init_bridge_instance(tb, "bc02_allow")
    try:
        pi = build_public_inputs(contract_addr=0xDEADBEEF).hex()
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": "00", "publicInputs": pi},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_UNSUPPORTED_SRC_CHAIN)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "bc02_allow")


@pytest.mark.skipif(not bc_an_01_fixtures_ready(), reason="regenerate 12-PI fixtures: scripts/audit/generate_bc_an_01_dual_proofs.sh")
def test_bc_an_01_double_mint_same_deposit_two_dapp_ids(tb):
    """BC-AN-01 regression — contracts/bridge pins on-chain dappId=0.

    Two valid proofs with different dapp limbs must NOT double-mint: replay
    anchor uses f.dappId (always 0), not PI fr[5]/fr[6].
    """
    bridge_tvc = init_bridge_instance(tb, "bc01_poc")
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    proof_a, pi_a = load_bc_proof("dapp_a")
    proof_b, pi_b = load_bc_proof("dapp_b")
    try:
        dep_a, dep_b = fr_le(pi_a, 0), fr_le(pi_b, 0)
        dapp_a = (fr_le(pi_a, 5) << 128) | fr_le(pi_a, 6)
        dapp_b = (fr_le(pi_b, 5) << 128) | fr_le(pi_b, 6)
        assert dep_a == dep_b, "PoC requires same depositId"
        assert dapp_a != dapp_b, "PoC requires different dappId in PI"

        minted0 = get_minted(tb, bridge_tvc)
        finalize_and_drain(tb, bridge_tvc, pipe, proof_a, pi_a)
        minted1 = get_minted(tb, bridge_tvc)
        assert minted1 == minted0 + fr_le(pi_a, 2)

        finalize_and_drain(tb, bridge_tvc, pipe, proof_b, pi_b)
        minted2 = get_minted(tb, bridge_tvc)
        assert minted2 == minted1, "second finalize must not mint again (dappId pinned to 0)"
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "bc01_poc")
