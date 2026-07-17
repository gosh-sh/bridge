"""F8 — MessagePipeline message-order fuzz (race / reorder safety).

Explores non-FIFO delivery of internal messages emitted by finalizeDeposit
(voucher deploy + confirmDeposit callback). Safety invariants:

  INV AN-MPL-1: bridged minted counter is monotone non-decreasing while draining.
  INV AN-MPL-2: bridged minted never exceeds the FIFO canonical oracle.
  INV AN-MPL-3: replayed deposit identity never increases minted above first success.
"""

from __future__ import annotations

import random

import pytest

pytest.importorskip("hypothesis")
from hypothesis import given, strategies as st

from bridge_helpers import USDC_BRIDGE_ADDR, fixtures_available, get_total_bridged_minted, init_bridge_instance
from pipeline_fuzz_helpers import (
    canonical_bridged_minted,
    deliver_random_indices,
    drain_fifo,
    finalize_enqueue,
    sum_proof_amounts,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.fuzz, pytest.mark.slow]


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
@given(
    proof_indices=st.lists(
        st.integers(min_value=0, max_value=2),
        min_size=1,
        max_size=3,
    ),
    seed=st.integers(min_value=0, max_value=2**31 - 1),
)
def test_mpl_random_delivery_never_overmints(tb, proof_indices: list[int], seed: int):
    """INV AN-MPL-1/2 — random pop order cannot mint more than FIFO reference."""
    tag = f"mpl_ord_{seed & 0xFFFF}"
    canonical = canonical_bridged_minted(tb, proof_indices, f"{tag}_ref")

    bridge_tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
    rng = random.Random(seed)

    try:
        for idx in proof_indices:
            finalize_enqueue(tb, bridge_tvc, pipe, idx)

        prev = get_total_bridged_minted(tb, bridge_tvc)
        while pipe.queue:
            deliver_random_indices(pipe, rng, max_steps=1)
            cur = get_total_bridged_minted(tb, bridge_tvc)
            assert cur >= prev, "AN-MPL-1 monotone"
            assert cur <= canonical, "AN-MPL-2 over-mint"
            prev = cur

        assert get_total_bridged_minted(tb, bridge_tvc) <= canonical, "AN-MPL-2 final bound"
    finally:
        pipe.cleanup()
        tb.cleanup_instance("USDCBridge", tag)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
@given(seed=st.integers(min_value=0, max_value=2**31 - 1))
def test_mpl_replay_interleave_never_double_mints(tb, seed: int):
    """INV AN-MPL-3 — duplicate proof in batch + random order ≤ single-mint cap."""
    tag = f"mpl_rep_{seed & 0xFFFF}"
    bridge_tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
    rng = random.Random(seed)
    single_cap = sum_proof_amounts([0])

    try:
        finalize_enqueue(tb, bridge_tvc, pipe, 0)
        deliver_random_indices(pipe, rng, max_steps=5)
        minted_mid = get_total_bridged_minted(tb, bridge_tvc)
        assert minted_mid <= single_cap

        finalize_enqueue(tb, bridge_tvc, pipe, 0)
        deliver_random_indices(pipe, rng)
        drain_fifo(pipe)

        assert get_total_bridged_minted(tb, bridge_tvc) <= single_cap, "AN-MPL-3 replay cap"
    finally:
        pipe.cleanup()
        tb.cleanup_instance("USDCBridge", tag)


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_mpl_two_deposit_permutations_match_fifo(tb):
    """Deterministic: both delivery orders for two fresh deposits reach FIFO minted."""
    indices = [0, 1]
    canonical = canonical_bridged_minted(tb, indices, "mpl_perm_ref")

    for swap in (False, True):
        tag = f"mpl_perm_{int(swap)}"
        bridge_tvc = init_bridge_instance(tb, tag)
        pipe = MessagePipeline(tb)
        pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, "USDCBridge")
        try:
            finalize_enqueue(tb, bridge_tvc, pipe, 0)
            finalize_enqueue(tb, bridge_tvc, pipe, 1)
            if swap and len(pipe.queue) >= 2:
                pipe.reorder_inflight([1, 0])
            drain_fifo(pipe)
            assert get_total_bridged_minted(tb, bridge_tvc) == canonical
        finally:
            pipe.cleanup()
            tb.cleanup_instance("USDCBridge", tag)
