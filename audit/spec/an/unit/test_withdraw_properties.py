"""F7-E — initiateWithdrawal property tests (Hypothesis + debugger)."""

from __future__ import annotations

import pytest
from hypothesis import given, strategies as st

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_MULTIPLE_ECC,
    ERR_NO_ECC,
    ERR_RECIPIENT_TOO_LONG,
    ERR_UNSUPPORTED_TOKEN,
    EVM_RECIPIENT_20B,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    init_bridge_instance,
)
pytestmark = [pytest.mark.unit, pytest.mark.property]

pytest.importorskip("hypothesis")

SENDER = "0:1111111111111111111111111111111111111111111111111111111111111111"


@given(length=st.integers(min_value=0, max_value=128))
def test_recipient_length_exit_code(tb, length: int):
    """INV AN-WD-5 — recipient longer than 64 bytes reverts 218."""
    tvc = init_bridge_instance(tb, f"wd_len_{length}")
    recipient = ("aa" * length) if length else ""
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": recipient},
            address=USDC_BRIDGE_ADDR,
        )
        if length > 64:
            tb.assert_failure(r, ERR_RECIPIENT_TOO_LONG)
        else:
            tb.assert_failure(r, ERR_NO_ECC)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, f"wd_len_{length}")


@pytest.mark.parametrize(
    "ecc,expected",
    [
        ({}, ERR_NO_ECC),
        ({USDC_ECC_ID: 1_000_000}, None),
        ({2: 1}, ERR_UNSUPPORTED_TOKEN),
    ],
)
def test_ecc_attachment_table(tb, ecc: dict, expected: int | None):
    """INV AN-WD-1/3 — ECC attachment rules."""
    tvc = init_bridge_instance(tb, "wd_ecc_tbl")
    try:
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender=SENDER,
            ecc=ecc,
            address=USDC_BRIDGE_ADDR,
        )
        if expected is None:
            tb.assert_success(r)
        else:
            tb.assert_failure(r, expected)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "wd_ecc_tbl")


def test_multiple_ecc_reverts(tb):
    """INV AN-WD-2 — exactly one ECC currency required."""
    tvc = init_bridge_instance(tb, "wd_multi")
    try:
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender=SENDER,
            ecc={USDC_ECC_ID: 1, 2: 1},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_MULTIPLE_ECC)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "wd_multi")
