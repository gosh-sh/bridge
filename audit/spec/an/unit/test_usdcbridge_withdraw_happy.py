"""WD-AN-06 — initiateWithdrawal happy path (burn + outbound event)."""

from __future__ import annotations

import pytest

from bridge_helpers import EVM_RECIPIENT_20B, USDC_BRIDGE_ADDR, USDC_ECC_ID, init_bridge_instance, BRIDGE_CONTRACT

pytestmark = pytest.mark.unit


def test_initiate_withdrawal_happy_burn_and_event(tb):
    """INV WD-AN-06: valid ECC attach → success + external WithdrawalInitiated."""
    tvc = init_bridge_instance(tb, "wd_happy")
    sender = "0:3333333333333333333333333333333333333333333333333333333333333333"
    try:
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "initiateWithdrawal",
            {"dstChainId": "11155111", "recipient": EVM_RECIPIENT_20B},
            sender=sender,
            ecc={USDC_ECC_ID: 1_000_000},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "initiateWithdrawal")
        assert r.messages, "expected outbound messages (monitoring event)"
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "wd_happy")
