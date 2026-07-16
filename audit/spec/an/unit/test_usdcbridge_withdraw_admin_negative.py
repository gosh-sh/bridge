"""USDCBridge — initiateWithdrawal + owner mint negatives (WD-AN / ADM-AN)."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    ERR_INVALID_NONCE,
    ERR_MULTIPLE_ECC,
    ERR_NO_ECC,
    ERR_RECIPIENT_TOO_LONG,
    ERR_UNSUPPORTED_TOKEN,
    ERR_ZERO_AMOUNT,
    EVM_RECIPIENT_20B,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    init_bridge_instance,
)

pytestmark = pytest.mark.unit


def test_initiate_withdrawal_no_ecc(tb):
    tvc = init_bridge_instance(tb, "wd_no_ecc")
    try:
        r = tb.call(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_NO_ECC)
    finally:
        tb.cleanup_instance("USDCBridge", "wd_no_ecc")


def test_initiate_withdrawal_unsupported_token(tb):
    tvc = init_bridge_instance(tb, "wd_badtok")
    try:
        r = tb.call_internal(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender="0:1111111111111111111111111111111111111111111111111111111111111111",
            ecc={2: 1_000_000},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_UNSUPPORTED_TOKEN)
    finally:
        tb.cleanup_instance("USDCBridge", "wd_badtok")


def test_initiate_withdrawal_zero_amount(tb):
    tvc = init_bridge_instance(tb, "wd_zero")
    try:
        r = tb.call_internal(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender="0:1111111111111111111111111111111111111111111111111111111111111111",
            ecc={USDC_ECC_ID: 0},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_ZERO_AMOUNT)
    finally:
        tb.cleanup_instance("USDCBridge", "wd_zero")


def test_initiate_withdrawal_recipient_too_long(tb):
    tvc = init_bridge_instance(tb, "wd_long")
    try:
        r = tb.call(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": "aa" * 65},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_RECIPIENT_TOO_LONG)
    finally:
        tb.cleanup_instance("USDCBridge", "wd_long")


def test_initiate_withdrawal_multiple_ecc(tb):
    tvc = init_bridge_instance(tb, "wd_multi")
    try:
        r = tb.call_internal(
            tvc,
            "USDCBridge",
            "initiateWithdrawal",
            {"dstChainId": "1", "recipient": EVM_RECIPIENT_20B},
            sender="0:1111111111111111111111111111111111111111111111111111111111111111",
            ecc={USDC_ECC_ID: 1_000_000, 2: 1},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_MULTIPLE_ECC)
    finally:
        tb.cleanup_instance("USDCBridge", "wd_multi")


def test_mint_and_send_wrong_nonce(tb):
    tvc = init_bridge_instance(tb, "adm_nonce")
    try:
        r = tb.call(
            tvc,
            "USDCBridge",
            "mintAndSend",
            {
                "recipient": "0:2222222222222222222222222222222222222222222222222222222222222222",
                "value": "1000000",
                "nonce": "0",
            },
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_INVALID_NONCE)
    finally:
        tb.cleanup_instance("USDCBridge", "adm_nonce")
