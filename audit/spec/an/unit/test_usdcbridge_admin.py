"""USDCBridge — TIP-3 callback + owner admin paths (F2 unit)."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_INVALID_NONCE,
    ERR_INVALID_SENDER,
    ERR_NOT_OWNER,
    ERR_NOT_WHOLE_USDC,
    ERR_OVERFLOW,
    ERR_ZERO_AMOUNT,
    USDC_BRIDGE_ADDR,
    init_bridge_instance,
)
pytestmark = pytest.mark.unit

USDC_WALLET = "0:1111111111111111111111111111111111111111111111111111111111111111"
DEPOSITOR = "0:2222222222222222222222222222222222222222222222222222222222222222"
RECIPIENT = "0:3333333333333333333333333333333333333333333333333333333333333333"
ALT_PUBKEY = "0x" + "b" * 64


def get_total_minted(tb, tvc) -> int:
    r = tb.call(tvc, BRIDGE_CONTRACT, "getTotalMinted", {}, address=USDC_BRIDGE_ADDR)
    tb.assert_success(r)
    return int(r.response["value0"])


def test_on_transfer_received_wrong_sender(tb):
    """TIP-AN-01 — only _usdcWallet may call onTransferReceived."""
    tvc = init_bridge_instance(tb, "tip_sender", usdc_wallet=USDC_WALLET)
    try:
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "onTransferReceived",
            {
                "from": DEPOSITOR,
                "value1": USDC_BRIDGE_ADDR,
                "value": "1000000",
                "value3": "0",
            },
            sender=DEPOSITOR,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_INVALID_SENDER)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "tip_sender")


def test_on_transfer_received_happy_mints(tb):
    """TIP-AN-02 — wallet callback mints ECC and bumps _totalMinted."""
    tvc = init_bridge_instance(tb, "tip_ok", usdc_wallet=USDC_WALLET)
    amount = 2_000_000
    try:
        before = get_total_minted(tb, tvc)
        r = tb.call_internal(
            tvc,
            BRIDGE_CONTRACT,
            "onTransferReceived",
            {
                "from": DEPOSITOR,
                "value1": USDC_BRIDGE_ADDR,
                "value": str(amount),
                "value3": "0",
            },
            sender=USDC_WALLET,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "onTransferReceived")
        assert get_total_minted(tb, tvc) == before + amount
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "tip_ok")


def test_mint_and_send_not_owner(tb):
    """ADM-AN-02 — mintAndSend requires owner signature."""
    tvc = init_bridge_instance(tb, "adm_noown")
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": "1000000", "nonce": "1"},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_NOT_OWNER)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_noown")


def test_mint_and_send_happy(tb):
    """ADM-AN-03 — owner mint increments nonce and _totalMinted."""
    tvc = init_bridge_instance(tb, "adm_mint")
    try:
        before = get_total_minted(tb, tvc)
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": "1000000", "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "mintAndSend")
        assert get_total_minted(tb, tvc) == before + 1_000_000
        nr = tb.call(tvc, BRIDGE_CONTRACT, "getNonces", {}, address=USDC_BRIDGE_ADDR)
        tb.assert_success(nr)
        assert int(nr.response["mintNonce"]) == 1
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_mint")


def test_mint_and_send_zero_amount(tb):
    """ADM-AN-04 — zero mint rejected."""
    tvc = init_bridge_instance(tb, "adm_zero")
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": "0", "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_ZERO_AMOUNT)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_zero")


def test_mint_and_send_accumulator_not_whole_usdc(tb):
    """ADM-AN-05 — accumulator path requires whole micro-USDC."""
    tvc = init_bridge_instance(tb, "adm_whole")
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSendAccumulator",
            {"buyer": RECIPIENT, "value": "1000001", "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_NOT_WHOLE_USDC)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_whole")


def test_mint_and_send_amount_overflow(tb):
    """ADM-AN-06 — uint64 cap on owner mint."""
    tvc = init_bridge_instance(tb, "adm_ovf")
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": str(1 << 64), "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_OVERFLOW)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_ovf")


def test_set_pubkey_rotates_owner(tb):
    """ADM-AN-07 — owner can rotate pubkey; old key no longer mints."""
    tvc = init_bridge_instance(tb, "adm_spk")
    try:
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "setPubkey",
            {"pubkey": ALT_PUBKEY},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, "setPubkey")
        gr = tb.call(tvc, BRIDGE_CONTRACT, "getOwnerPubkey", {}, address=USDC_BRIDGE_ADDR)
        tb.assert_success(gr)
        assert gr.response["value0"].lower().replace("0x", "") == ALT_PUBKEY[2:]

        r2 = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "mintAndSend",
            {"recipient": RECIPIENT, "value": "1000000", "nonce": "1"},
            sign_keys=tb.admin_keys,
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r2, ERR_NOT_OWNER)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "adm_spk")
