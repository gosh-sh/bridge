"""F7-A — public-inputs algebra (Hypothesis, no tvm-debugger)."""

from __future__ import annotations

import pytest

pytest.importorskip("hypothesis")
from hypothesis import given, strategies as st

from bridge_helpers import (
    FR_COUNT_ON_CHAIN,
    PI_LEN,
    UINT64_MAX,
    amount_exceeds_uint64,
    build_public_inputs,
    parse_pi_fields,
)

pytestmark = [pytest.mark.unit, pytest.mark.property]


@given(
    deposit_id=st.integers(min_value=0, max_value=2**256 - 1),
    amount=st.integers(min_value=1, max_value=UINT64_MAX),
    dapp_id=st.integers(min_value=0, max_value=2**256 - 1),
)
def test_build_public_inputs_round_trip(deposit_id: int, amount: int, dapp_id: int):
    """INV AN-DEP-9 — encode/decode round-trip for bound fields."""
    pi = build_public_inputs(deposit_id=deposit_id, amount=amount, dapp_id=dapp_id)
    fields = parse_pi_fields(pi)
    assert fields["deposit_id"] == deposit_id
    assert fields["amount"] == amount
    assert fields["dapp_id"] == dapp_id


@given(n_bytes=st.integers(min_value=0, max_value=FR_COUNT_ON_CHAIN * 32 - 1))
def test_truncated_pi_too_short_for_parse(n_bytes: int):
    """INV AN-DEP-2 — truncated PI cannot be parsed as full on-chain layout."""
    pi = build_public_inputs()[:n_bytes]
    with pytest.raises(ValueError, match="PI too short"):
        parse_pi_fields(pi)


def test_full_pi_length_constant():
    assert len(build_public_inputs()) == PI_LEN


@given(amount=st.integers(min_value=UINT64_MAX + 1, max_value=2**256 - 1))
def test_amount_above_uint64_flagged(amount: int):
    """INV AN-DEP-3 — amounts above uint64 are detectable before mint policy."""
    pi = build_public_inputs(amount=amount)
    assert amount_exceeds_uint64(parse_pi_fields(pi)["amount"])


def test_zero_an_account_encodable():
    """INV AN-DEP-8 / QC-AN-10 — zero recipient encodes (ETH would revert)."""
    pi = build_public_inputs(an_account_hi=0, an_account_lo=0)
    fields = parse_pi_fields(pi)
    assert fields["an_account_hi"] == 0
    assert fields["an_account_lo"] == 0


@given(
    deposit_id=st.integers(min_value=0, max_value=2**128 - 1),
    flip_byte=st.integers(min_value=0, max_value=31),
)
def test_dapp_limb_mutation_changes_pi(deposit_id: int, flip_byte: int):
    """BC-AN-01 helper — same depositId, flipped dapp limb → different PI."""
    base = build_public_inputs(deposit_id=deposit_id)
    mutated = bytearray(base)
    idx = 4 * 32 + flip_byte
    mutated[idx] ^= 0x01
    assert parse_pi_fields(bytes(mutated))["deposit_id"] == deposit_id
    assert bytes(mutated) != base
