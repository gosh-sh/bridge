"""F8d — cross-counter fuzz: deposit pipeline vs owner mint vs withdraw (QC-AN-05).

Documents that `_totalMinted` and `_totalMintedBridgeByToken` are intentionally
decoupled: owner `mintAndSend` lets users burn ECC without increasing the
bridged minted leg, so `burned > minted` on `getTotalBridged` is observable.
"""

from __future__ import annotations

import random

import pytest

pytest.importorskip("hypothesis")
from hypothesis import settings
from hypothesis import strategies as st
from hypothesis.stateful import RuleBasedStateMachine, invariant, precondition, rule
from hypothesis.stateful import run_state_machine_as_test

from bridge_helpers import (
    EVM_RECIPIENT_20B,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    fixtures_available,
    get_bridged_counters,
    get_total_bridged_minted,
    get_total_minted,
    init_bridge_instance,
)
from pipeline_fuzz_helpers import (
    WHOLE_USDC,
    canonical_max_minted_from_history,
    deliver_random_indices,
    finalize_enqueue,
    owner_mint_and_send,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]

WITHDRAW_SENDER = "0:5555555555555555555555555555555555555555555555555555555555555555"

_CC_SETTINGS = settings(
    max_examples=12,
    stateful_step_count=18,
    deadline=None,
)


class CrossCounterMachine(RuleBasedStateMachine):
    """Interleave bridged deposit pipeline, owner mint, and withdraw burns."""

    def __init__(self, tb):
        super().__init__()
        self.tb = tb
        self._tag = f"cc_sm_{id(self) & 0xFFFF}"
        self.bridge_tvc = init_bridge_instance(tb, self._tag)
        self.pipe = MessagePipeline(tb)
        self.pipe.register(USDC_BRIDGE_ADDR, self.bridge_tvc, "USDCBridge")
        self.rng = random.Random(7)
        self.finalize_history: list[int] = []
        self.mint_nonce = 0
        self._last_bridged_minted = 0
        self._last_bridged_burned = 0
        self._last_total_minted = 0

    def teardown(self):
        self.pipe.cleanup()
        self.tb.cleanup_instance("USDCBridge", self._tag)

    def _sync_counters(self) -> tuple[int, int, int]:
        bridged_m, bridged_b = get_bridged_counters(self.tb, self.bridge_tvc)
        total_m = get_total_minted(self.tb, self.bridge_tvc)
        return bridged_m, bridged_b, total_m

    def _check_after_step(self) -> None:
        bridged_m, bridged_b, total_m = self._sync_counters()
        cap = canonical_max_minted_from_history(self.finalize_history)

        assert bridged_m >= self._last_bridged_minted, "bridged minted monotone"
        assert bridged_b >= self._last_bridged_burned, "bridged burned monotone"
        assert total_m >= self._last_total_minted, "_totalMinted monotone"
        assert bridged_m <= cap, "deposit path cannot over-mint bridged leg"

        self._last_bridged_minted = bridged_m
        self._last_bridged_burned = bridged_b
        self._last_total_minted = total_m

    @rule(proof_idx=st.integers(min_value=0, max_value=4))
    def enqueue_finalize(self, proof_idx: int):
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check_after_step()

    @precondition(lambda self: len(self.pipe.queue) > 0)
    @rule(proof_idx=st.integers(min_value=0, max_value=4))
    def enqueue_while_inflight(self, proof_idx: int):
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check_after_step()

    @rule(batch=st.integers(min_value=1, max_value=3))
    def deliver_batch(self, batch: int):
        for _ in range(batch):
            if not self.pipe.queue:
                break
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check_after_step()

    @rule(whole_units=st.integers(min_value=1, max_value=3))
    def owner_mint(self, whole_units: int):
        self.mint_nonce += 1
        value = whole_units * WHOLE_USDC
        bridged_before = get_total_bridged_minted(self.tb, self.bridge_tvc)
        total_before = get_total_minted(self.tb, self.bridge_tvc)
        owner_mint_and_send(
            self.tb, self.bridge_tvc, value=value, nonce=self.mint_nonce
        )
        assert get_total_bridged_minted(self.tb, self.bridge_tvc) == bridged_before
        assert get_total_minted(self.tb, self.bridge_tvc) == total_before + value
        self._check_after_step()

    @rule(amount=st.integers(min_value=1, max_value=WHOLE_USDC))
    def withdraw_burn(self, amount: int):
        r = self.tb.call_internal(
            self.bridge_tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender=WITHDRAW_SENDER,
            ecc={USDC_ECC_ID: amount},
            address=USDC_BRIDGE_ADDR,
        )
        if r.vm_success and r.exit_code == 0:
            self._check_after_step()

    @invariant()
    def bridged_mint_cap(self):
        bridged_m, _, _ = self._sync_counters()
        assert bridged_m <= canonical_max_minted_from_history(self.finalize_history)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_cross_counter_state_machine(tb):
    """INV AN-ADM-3 + AN-MPL-2 under owner mint / withdraw interleave."""
    run_state_machine_as_test(
        lambda: CrossCounterMachine(tb),
        settings=_CC_SETTINGS,
    )


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_qc_an_05_owner_mint_allows_burned_above_bridged_minted(tb):
    """QC-AN-05 — owner ECC mint does not bump bridged minted; withdraw can desync counters."""
    tvc = init_bridge_instance(tb, "qc_an_05")
    try:
        owner_mint_and_send(tb, tvc, value=WHOLE_USDC, nonce=1)
        bridged_m, bridged_b = get_bridged_counters(tb, tvc)
        assert bridged_m == 0
        assert get_total_minted(tb, tvc) == WHOLE_USDC

        r = tb.call_internal(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender=WITHDRAW_SENDER,
            ecc={USDC_ECC_ID: WHOLE_USDC // 2},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "initiateWithdrawal")
        bridged_m2, bridged_b2 = get_bridged_counters(tb, tvc)
        assert bridged_m2 == 0
        assert bridged_b2 == WHOLE_USDC // 2
        assert bridged_b2 > bridged_m2, "QC-AN-05: bridged burned can exceed bridged minted"
    finally:
        tb.cleanup_instance("USDCBridge", "qc_an_05")
