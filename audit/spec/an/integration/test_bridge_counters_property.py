"""F7-C — bridge counter monotonicity (seeded integration)."""

from __future__ import annotations

import pytest

pytest.importorskip("hypothesis")
from hypothesis import given, strategies as st

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_INVALID_ZKPROOF,
    EVM_RECIPIENT_20B,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    build_public_inputs,
    fixtures_available,
    fr_le,
    get_total_bridged_minted,
    get_total_minted,
    init_bridge_instance,
    load_fixture_proof,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.property]

RECIPIENT = "0:3333333333333333333333333333333333333333333333333333333333333333"
SENDER = "0:1111111111111111111111111111111111111111111111111111111111111111"


def finalize_and_drain(tb, bridge_tvc, pipe: MessagePipeline, proof: bytes, pi: bytes):
    from bridge_helpers import seed_trust_from_pi

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
def test_f7_c1_finalize_increases_bridged_minted(tb):
    """INV AN-DEP-9 — successful cross-chain finalize increases bridge minted counter."""
    bridge_tvc = init_bridge_instance(tb, "f7_c1")
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    proof, pi = load_fixture_proof(0)
    try:
        before = get_total_bridged_minted(tb, bridge_tvc)
        finalize_and_drain(tb, bridge_tvc, pipe, proof, pi)
        after = get_total_bridged_minted(tb, bridge_tvc)
        assert after == before + fr_le(pi, 2)
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "f7_c1")


def test_f7_c2_owner_mint_does_not_touch_bridged_counter(tb):
    """INV AN-ADM-3 — mintAndSend bumps _totalMinted only."""
    tvc = init_bridge_instance(tb, "f7_c2")
    try:
        minted_before = get_total_minted(tb, tvc)
        bridged_before = get_total_bridged_minted(tb, tvc)
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": "1000000", "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "mintAndSend")
        assert get_total_minted(tb, tvc) == minted_before + 1_000_000
        assert get_total_bridged_minted(tb, tvc) == bridged_before
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "f7_c2")


def test_f7_c3_withdraw_increases_burned_only(tb):
    """INV AN-WD-7 — initiateWithdrawal increases burned leg of bridge counter."""
    tvc = init_bridge_instance(tb, "f7_c3")
    try:
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "initiateWithdrawal",
            {"dstChainId": "11155111", "recipient": EVM_RECIPIENT_20B},
            sender=SENDER,
            ecc={USDC_ECC_ID: 500_000},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "initiateWithdrawal")
        br = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "getTotalBridged",
            {"tokenId": str(USDC_ECC_ID)},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(br)
        assert int(br.response["minted"]) == 0
        assert int(br.response["burned"]) == 500_000
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "f7_c3")


@given(st.lists(st.booleans(), min_size=1, max_size=5))
@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_f7_c4_bridged_minted_never_decreases(tb, actions: list[bool]):
    """INV AN-ACC-2 — interleaved invalid/valid finalize keeps bridged minted non-decreasing."""
    bridge_tvc = init_bridge_instance(tb, "f7_c4")
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    valid_idx = 0
    try:
        minted = get_total_bridged_minted(tb, bridge_tvc)
        for is_valid in actions:
            if is_valid:
                proof, pi = load_fixture_proof(valid_idx)
                valid_idx += 1
                before = get_total_bridged_minted(tb, bridge_tvc)
                finalize_and_drain(tb, bridge_tvc, pipe, proof, pi)
                after = get_total_bridged_minted(tb, bridge_tvc)
                assert after >= before
                if after > before:
                    assert after == before + fr_le(pi, 2)
                minted = after
            else:
                before = minted
                r = tb.call(
                    bridge_tvc,
                    BRIDGE_CONTRACT,
                    "finalizeDeposit",
                    {
                        "proof": b"\xff\xee".hex(),
                        "publicInputs": build_public_inputs(deposit_id=9000 + valid_idx).hex(),
                    },
                    address=USDC_BRIDGE_ADDR,
                )
                tb.assert_failure(r, ERR_INVALID_ZKPROOF)
                assert get_total_bridged_minted(tb, bridge_tvc) == before
            assert get_total_bridged_minted(tb, bridge_tvc) >= minted
            minted = get_total_bridged_minted(tb, bridge_tvc)
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "f7_c4")
