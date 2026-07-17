"""F8f — bounce probes and deploy retry in MessagePipeline.

Uses `MessagePipeline._try_bounce_for_missing_dest` for bounce:true messages to
unregistered addresses, plus explicit deploy-message retry (relayer semantics).
"""

from __future__ import annotations

import random

import pytest

pytest.importorskip("hypothesis")
from hypothesis import settings
from hypothesis import strategies as st
from hypothesis.stateful import RuleBasedStateMachine, invariant, precondition, rule
from hypothesis.stateful import run_state_machine_as_test

from bridge_helpers import USDC_BRIDGE_ADDR, fixtures_available, get_total_bridged_minted, init_bridge_instance
from pipeline_fuzz_helpers import (
    canonical_max_minted_from_history,
    deliver_random_indices,
    drain_fifo,
    finalize_enqueue,
    inject_bounce_probe,
    retry_deploy_message,
)
from test_base import JsonResult, MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]

_BOUNCE_SETTINGS = settings(
    max_examples=10,
    stateful_step_count=18,
    deadline=None,
)


class BounceRetryPipelineMachine(RuleBasedStateMachine):
    def __init__(self, tb):
        super().__init__()
        self.tb = tb
        self._tag = f"mpl_bnc_{id(self) & 0xFFFF}"
        self.bridge_tvc = init_bridge_instance(tb, self._tag)
        self.pipe = MessagePipeline(tb)
        self.pipe.register(USDC_BRIDGE_ADDR, self.bridge_tvc, "USDCBridge")
        self.rng = random.Random(808)
        self.finalize_history: list[int] = []
        self._last_jr: JsonResult | None = None
        self._last_minted = 0
        self._bounce_injected = 0

    def teardown(self):
        self.pipe.cleanup()
        self.tb.cleanup_instance("USDCBridge", self._tag)

    def _check(self) -> None:
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        cap = canonical_max_minted_from_history(self.finalize_history)
        assert cur >= self._last_minted, "AN-MPL-1"
        assert cur <= cap, f"AN-MPL-2: {cur} > {cap}"
        self._last_minted = cur

    @rule(proof_idx=st.integers(min_value=0, max_value=4))
    def enqueue_finalize(self, proof_idx: int):
        jr = finalize_enqueue(self.tb, self.bridge_tvc, self.pipe, proof_idx)
        self._last_jr = jr
        self.finalize_history.append(proof_idx)
        self._check()

    @precondition(lambda self: len(self.pipe.queue) > 0)
    @rule()
    def inject_bounce(self):
        if inject_bounce_probe(self.pipe, self.rng):
            self._bounce_injected += 1
        self._check()

    @precondition(lambda self: self._last_jr is not None)
    @rule()
    def retry_deploy(self):
        assert self._last_jr is not None
        self.pipe.enqueue([retry_deploy_message(self._last_jr)])
        self._check()

    @rule()
    def deliver_one(self):
        if self.pipe.queue:
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check()

    @rule(batch=st.integers(min_value=1, max_value=3))
    def deliver_batch(self, batch: int):
        for _ in range(batch):
            if not self.pipe.queue:
                break
            deliver_random_indices(self.pipe, self.rng, max_steps=1)
        self._check()

    @invariant()
    def mpl_cap(self):
        cur = get_total_bridged_minted(self.tb, self.bridge_tvc)
        assert cur <= canonical_max_minted_from_history(self.finalize_history)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_bounce_retry_pipeline_state_machine(tb):
    """INV AN-MPL-8/9 — bounce noise + deploy retry never over-mint."""
    run_state_machine_as_test(
        lambda: BounceRetryPipelineMachine(tb),
        settings=_BOUNCE_SETTINGS,
    )


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_bounce_probe_alone_does_not_mint(tb):
    """Bounce to missing dest processes without increasing bridged minted."""
    tag = "bounce_only"
    bridge_tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
    rng = random.Random(1)
    try:
        jr = finalize_enqueue(tb, bridge_tvc, pipe, 0)
        assert pipe.queue
        inject_bounce_probe(pipe, rng)
        before = len(pipe.history)
        # Deliver until bounce probe consumed (queue may grow with bounce msgs)
        steps = 0
        while pipe.queue and steps < 30:
            deliver_random_indices(pipe, rng, max_steps=1)
            steps += 1
        assert get_total_bridged_minted(tb, bridge_tvc) <= canonical_max_minted_from_history([0])
        assert len(pipe.history) >= before
    finally:
        pipe.cleanup()
        tb.cleanup_instance("USDCBridge", tag)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_deploy_retry_after_success_is_idempotent(tb):
    """Retry deploy message after full drain must not increase minted."""
    tag = "retry_idem"
    bridge_tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
    try:
        jr = finalize_enqueue(tb, bridge_tvc, pipe, 0)
        drain_fifo(pipe)
        minted = get_total_bridged_minted(tb, bridge_tvc)
        assert minted > 0
        pipe.enqueue([retry_deploy_message(jr)])
        drain_fifo(pipe)
        assert get_total_bridged_minted(tb, bridge_tvc) == minted
    finally:
        pipe.cleanup()
        tb.cleanup_instance("USDCBridge", tag)
