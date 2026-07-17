"""F8e — multi-proof (0..9) MessagePipeline state machine."""

from __future__ import annotations

import random

import pytest

pytest.importorskip("hypothesis")
from hypothesis import settings
from hypothesis import strategies as st
from hypothesis.stateful import RuleBasedStateMachine, invariant, precondition, rule
from hypothesis.stateful import run_state_machine_as_test

from bridge_helpers import USDC_BRIDGE_ADDR, get_total_bridged_minted, init_bridge_instance, ten_proofs_available
from pipeline_fuzz_helpers import (
    canonical_bridged_minted,
    canonical_max_minted_from_history,
    deliver_random_indices,
    finalize_enqueue,
    sum_all_proof_amounts,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]

_MULTI_SETTINGS = settings(
    max_examples=8,
    stateful_step_count=20,
    deadline=None,
)


class DepositPipelineMultiProofMachine(RuleBasedStateMachine):
    """All ten deposit fixtures — unique deposit_id 0..9."""

    def __init__(self, tb):
        super().__init__()
        self.tb = tb
        self._tag = f"mpl_x10_{id(self) & 0xFFFF}"
        self.bridge_tvc = init_bridge_instance(tb, self._tag)
        self.pipe = MessagePipeline(tb)
        self.pipe.register(USDC_BRIDGE_ADDR, self.bridge_tvc, "USDCBridge")
        self.rng = random.Random(2026)
        self.finalize_history: list[int] = []
        self._last_minted = 0

    def teardown(self):
        self.pipe.cleanup()
        self.tb.cleanup_instance("USDCBridge", self._tag)

    def _check(self) -> None:
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur >= self._last_minted, "AN-MPL-1"
        assert cur <= cap, f"AN-MPL-2: {cur} > {cap}"
        self._last_minted = cur

    @rule(proof_idx=st.integers(min_value=0, max_value=9))
    def enqueue_finalize(self, proof_idx: int):
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check()

    @precondition(lambda self: len(self.pipe.queue) > 0)
    @rule(proof_idx=st.integers(min_value=0, max_value=9))
    def enqueue_while_inflight(self, proof_idx: int):
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self.finalize_history.append(proof_idx)
        self._check()

    @rule(batch=st.integers(min_value=1, max_value=5))
    def deliver_batch(self, batch: int):
        for _ in range(batch):
            if not self.pipe.queue:
                break
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check()

    @rule()
    def shuffle_inflight(self):
        n = len(self.pipe.queue)
        if n < 2 or n > 20:
            return
        perm = list(range(n))
        self.rng.shuffle(perm)
        self.pipe.reorder_inflight(perm)
        self._check()

    @rule()
    def replay_last(self):
        if not self.finalize_history:
            return
        idx = self.finalize_history[-1]
        finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, idx)
        self.finalize_history.append(idx)
        self._check()

    @invariant()
    def mpl_cap(self):
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        assert cur <= canonical_max_minted_from_history(self.finalize_history)


@pytest.mark.skipif(not ten_proofs_available(), reason="need proof_00..proof_09")
def test_multi_proof_pipeline_state_machine(tb):
    """INV AN-MPL-10 — ten-proof pool under random pipeline schedules."""
    run_state_machine_as_test(
        lambda: DepositPipelineMultiProofMachine(tb),
        settings=_MULTI_SETTINGS,
    )


@pytest.mark.skipif(not ten_proofs_available(), reason="need proof_00..proof_09")
def test_all_ten_proofs_fifo_reaches_full_oracle(tb):
    """Deterministic: proofs 0..9 sequential FIFO mint sum of all amounts."""
    indices = list(range(10))
    expected = sum_all_proof_amounts()
    canonical = canonical_bridged_minted(tb, indices, "mpl_x10_fifo")
    assert canonical == expected
    assert expected > 0
