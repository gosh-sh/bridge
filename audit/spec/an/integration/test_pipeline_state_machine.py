"""F8b — Hypothesis state machine for deposit MessagePipeline schedules.

Interleaves finalize enqueue, FIFO/random delivery, and queue reordering.
Withdraw path is intentionally out of scope here — `initiateWithdrawal` is a
single atomic internal call with no async MessagePipeline hops.
"""

from __future__ import annotations

import random

import pytest

pytest.importorskip("hypothesis")
from hypothesis import settings
from hypothesis.stateful import RuleBasedStateMachine, invariant, precondition, rule
from hypothesis import strategies as st
from hypothesis.stateful import run_state_machine_as_test

from bridge_helpers import USDC_BRIDGE_ADDR, fixtures_available, get_total_bridged_minted, init_bridge_instance
from pipeline_fuzz_helpers import (
    canonical_max_minted_from_history,
    deliver_random_indices,
    finalize_enqueue,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]

_SM_SETTINGS = settings(
    max_examples=20,
    stateful_step_count=16,
    deadline=None,
)

_DEEP_SM_SETTINGS = settings(
    max_examples=15,
    stateful_step_count=24,
    deadline=None,
)


class DepositPipelineMachine(RuleBasedStateMachine):
    """Explore deposit finalize + async delivery schedules."""

    def __init__(self, tb):
        super().__init__()
        self.tb = tb
        self._tag = f"mpl_sm_{id(self) & 0xFFFF}"
        self.bridge_tvc = init_bridge_instance(tb, self._tag)
        self.pipe = MessagePipeline(tb)
        self.pipe.register(USDC_BRIDGE_ADDR, self.bridge_tvc, "USDCBridge")
        self.rng = random.Random(42)
        self.finalize_history: list[int] = []
        self._last_minted = 0

    def teardown(self):
        self.pipe.cleanup()
        self.tb.cleanup_instance("USDCBridge", self._tag)

    def _refresh_minted(self) -> int:
        return get_total_bridged_minted(self.tb, self.bridge_tvc)

    def _check_step_invariants(self) -> None:
        cur = self._refresh_minted()
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur >= self._last_minted, "AN-MPL-1 monotone"
        assert cur <= cap, f"AN-MPL-2 over-mint: {cur} > {cap}"
        self._last_minted = cur

    @rule(proof_idx=st.integers(min_value=0, max_value=2))
    def enqueue_finalize(self, proof_idx: int):
        """External finalizeDeposit → enqueue deploy message(s)."""
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check_step_invariants()

    @rule()
    def deliver_one_random(self):
        if self.pipe.queue:
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check_step_invariants()

    @rule()
    def deliver_one_fifo(self):
        if self.pipe.queue:
            self.pipe.process_one_at(0, verbose=False)
        self._check_step_invariants()

    @rule()
    def shuffle_inflight(self):
        n = len(self.pipe.queue)
        if n < 2 or n > 6:
            return
        perm = list(range(n))
        self.rng.shuffle(perm)
        self.pipe.reorder_inflight(perm)
        self._check_step_invariants()

    @rule()
    def replay_last_finalize(self):
        if not self.finalize_history:
            return
        idx = self.finalize_history[-1]
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, idx)
        self.finalize_history.append(idx)
        self._check_step_invariants()

    @invariant()
    def mpl_caps_hold(self):
        cur = self._refresh_minted()
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur <= cap, "AN-MPL-2 invariant"
        assert cur >= self._last_minted, "AN-MPL-1 invariant"


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_deposit_pipeline_state_machine(tb):
    """INV AN-MPL-1/2/3 — stateful interleaving of finalize + delivery."""
    run_state_machine_as_test(
        lambda: DepositPipelineMachine(tb),
        settings=_SM_SETTINGS,
    )


class DepositPipelineDeepMachine(RuleBasedStateMachine):
    """Deeper schedules: enqueue while inflight, batch delivery, larger queues."""

    def __init__(self, tb):
        super().__init__()
        self.tb = tb
        self._tag = f"mpl_deep_{id(self) & 0xFFFF}"
        self.bridge_tvc = init_bridge_instance(tb, self._tag)
        self.pipe = MessagePipeline(tb)
        self.pipe.register(USDC_BRIDGE_ADDR, self.bridge_tvc, "USDCBridge")
        self.rng = random.Random(99)
        self.finalize_history: list[int] = []
        self._last_minted = 0

    def teardown(self):
        self.pipe.cleanup()
        self.tb.cleanup_instance("USDCBridge", self._tag)

    def _check_step_invariants(self) -> None:
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur >= self._last_minted, "AN-MPL-1 monotone"
        assert cur <= cap, f"AN-MPL-2 over-mint: {cur} > {cap}"
        self._last_minted = cur

    @rule(proof_idx=st.integers(min_value=0, max_value=4))
    def enqueue_finalize(self, proof_idx: int):
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check_step_invariants()

    @precondition(lambda self: len(self.pipe.queue) > 0)
    @rule(proof_idx=st.integers(min_value=0, max_value=4))
    def enqueue_while_inflight(self, proof_idx: int):
        """New finalize while prior deploy/confirm messages still queued."""
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check_step_invariants()

    @rule(batch=st.integers(min_value=1, max_value=4))
    def deliver_batch(self, batch: int):
        for _ in range(batch):
            if not self.pipe.queue:
                break
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check_step_invariants()

    @rule()
    def shuffle_inflight(self):
        n = len(self.pipe.queue)
        if n < 2 or n > 12:
            return
        perm = list(range(n))
        self.rng.shuffle(perm)
        self.pipe.reorder_inflight(perm)
        self._check_step_invariants()

    @rule()
    def replay_last_finalize(self):
        if not self.finalize_history:
            return
        idx = self.finalize_history[-1]
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, idx)
        self.finalize_history.append(idx)
        self._check_step_invariants()

    @invariant()
    def mpl_caps_hold(self):
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur <= cap, "AN-MPL-2 invariant"
        assert cur >= self._last_minted, "AN-MPL-1 invariant"


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_deposit_pipeline_deep_state_machine(tb):
    """INV AN-MPL-5 — enqueue during inflight + batch delivery (race-prone)."""
    run_state_machine_as_test(
        lambda: DepositPipelineDeepMachine(tb),
        settings=_DEEP_SM_SETTINGS,
    )
