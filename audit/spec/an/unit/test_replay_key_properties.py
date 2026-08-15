"""F7-B — replay anchor preimage properties (Hypothesis)."""

from __future__ import annotations

import pytest
from hypothesis import given, strategies as st

from bridge_helpers import DEFAULT_CHAIN_ID, encode_replay_anchor

pytestmark = [pytest.mark.unit, pytest.mark.property]

pytest.importorskip("hypothesis")


@given(
    deposit_id=st.integers(min_value=0, max_value=2**256 - 1),
    contract_addr=st.integers(min_value=0, max_value=2**256 - 1),
    dapp_a=st.integers(min_value=0, max_value=2**256 - 1),
    dapp_b=st.integers(min_value=0, max_value=2**256 - 1),
    chain_id=st.integers(min_value=0, max_value=2**256 - 1),
)
def test_different_dapp_id_changes_anchor(
    deposit_id: int, contract_addr: int, dapp_a: int, dapp_b: int, chain_id: int
):
    """INV AN-VCH-1 — replay slot splits on dappId (on-chain pins dappId=0)."""
    if dapp_a == dapp_b:
        return
    a = encode_replay_anchor(deposit_id, contract_addr, dapp_a, chain_id)
    b = encode_replay_anchor(deposit_id, contract_addr, dapp_b, chain_id)
    assert a != b


@given(
    deposit_id=st.integers(min_value=0, max_value=2**256 - 1),
    contract_addr=st.integers(min_value=0, max_value=2**256 - 1),
    dapp_id=st.integers(min_value=0, max_value=2**256 - 1),
    chain_id=st.integers(min_value=0, max_value=2**256 - 1),
)
def test_replay_anchor_stable(
    deposit_id: int, contract_addr: int, dapp_id: int, chain_id: int
):
    k1 = encode_replay_anchor(deposit_id, contract_addr, dapp_id, chain_id)
    k2 = encode_replay_anchor(deposit_id, contract_addr, dapp_id, chain_id)
    assert k1 == k2
    assert len(k1) == 128


def test_replay_anchor_collision_smoke():
    """Sample 500 random quads — abi.encode preimages are unique."""
    seen: set[bytes] = set()
    for i in range(500):
        key = encode_replay_anchor(i, i << 64, i ^ 0xDEAD, DEFAULT_CHAIN_ID + i)
        assert key not in seen
        seen.add(key)
