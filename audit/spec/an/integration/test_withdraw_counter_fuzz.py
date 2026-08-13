"""F8c — bridged mint/burn counter fuzz after deposit-only minting.

`initiateWithdrawal` has no MessagePipeline async hops (single `tvm.accept` call).
This suite still fuzzes the **accounting** leg: once bridged minted > 0, burned
must stay monotone and (for deposit-only TVM harness) never exceed minted.

Note: real users may hold ECC from owner `mintAndSend` — that path is excluded.
"""

from __future__ import annotations

import pytest

pytest.importorskip("hypothesis")
from hypothesis import given, strategies as st

from bridge_helpers import (
    EVM_RECIPIENT_20B,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    fixtures_available,
    get_bridged_counters,
    init_bridge_instance,
)
from pipeline_fuzz_helpers import drain_fifo, finalize_enqueue, register_voucher
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]

WITHDRAW_SENDER = "0:4444444444444444444444444444444444444444444444444444444444444444"


def seed_bridged_mint(tb, tag: str) -> tuple[object, int]:
    """One successful proof_00 deposit through the pipeline."""
    bridge_tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
    jr = finalize_enqueue(tb, bridge_tvc, pipe, 0)
    drain_fifo(pipe)
    register_voucher(pipe, jr)
    minted, burned = get_bridged_counters(tb, bridge_tvc)
    assert minted > 0 and burned == 0
    return bridge_tvc, minted


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
@given(
    amounts=st.lists(
        st.integers(min_value=1, max_value=500_000),
        min_size=1,
        max_size=6,
    ),
)
def test_withdraw_burn_monotone_and_capped_after_deposit(tb, amounts: list[int]):
    """INV AN-ACC-2 / AN-WD-7 — burned leg monotone, never exceeds bridged minted."""
    tag = "wd_cnt_fuzz"
    bridge_tvc, minted_cap = seed_bridged_mint(tb, tag)
    burned_prev = 0

    try:
        for amount in amounts:
            if burned_prev + amount > minted_cap:
                continue
            r = tb.call_internal(
                bridge_tvc,
                "USDCBridge",
                "initiateWithdrawal",
                {"dstChainId": "11155111", "recipient": EVM_RECIPIENT_20B},
                sender=WITHDRAW_SENDER,
                ecc={USDC_ECC_ID: amount},
                address=USDC_BRIDGE_ADDR,
            )
            if not r.vm_success or r.exit_code != 0:
                continue
            minted, burned = get_bridged_counters(tb, bridge_tvc)
            assert burned >= burned_prev, "burned monotone"
            assert burned <= minted, "AN-ACC-2 minted >= burned (deposit-only)"
            burned_prev = burned
    finally:
        tb.cleanup_instance("USDCBridge", tag)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
@given(seed=st.integers(min_value=0, max_value=2**31 - 1))
def test_deposit_drain_then_withdraw_headroom(tb, seed: int):
    """Deposit pipeline drain then single withdraw within minted headroom."""
    tag = f"wd_head_{seed & 0xFFFF}"
    bridge_tvc, minted_cap = seed_bridged_mint(tb, tag)
    amount = minted_cap // 2 or 1
    try:
        r = tb.call_internal(
            bridge_tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender=WITHDRAW_SENDER,
            ecc={USDC_ECC_ID: amount},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "initiateWithdrawal")
        minted, burned = get_bridged_counters(tb, bridge_tvc)
        assert burned == amount
        assert minted == minted_cap
        assert burned <= minted
    finally:
        tb.cleanup_instance("USDCBridge", tag)
