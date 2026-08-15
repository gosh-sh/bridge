"""USDCBridge deposit edge cases — QC-AN-08/10, DepositVoucher hash guard."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_HASH_MISMATCH,
    ERR_INVALID_ZKPROOF,
    USDC_BRIDGE_ADDR,
    build_public_inputs,
    init_bridge_instance,
)
pytestmark = pytest.mark.unit


def test_finalize_deposit_public_inputs_too_short(tb):
    """QC-AN-08 — truncated PI must not reach mint path."""
    tvc = init_bridge_instance(tb, "pi_short")
    try:
        pi = build_public_inputs()[:64]  # 2 Fr, need 8 for parse
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": "00", "publicInputs": pi.hex()},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, message="truncated publicInputs")
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "pi_short")


def test_finalize_deposit_zero_an_account_not_rejected_pre_zk(tb):
    """QC-AN-10 — anAccount==0 is not rejected before ZK (unlike ETH deposit guard)."""
    tvc = init_bridge_instance(tb, "acct_zero")
    try:
        pi = build_public_inputs(an_account_hi=0, an_account_lo=0).hex()
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": "00", "publicInputs": pi},
            address=USDC_BRIDGE_ADDR,
        )
        # Parse + amount check pass; fails at ZK, not at recipient validation.
        tb.assert_failure(r, ERR_INVALID_ZKPROOF)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "acct_zero")


def test_deposit_voucher_hash_mismatch(tb):
    """DepositVoucher — constructor rejects identity/hash mismatch (219)."""
    tvc = tb.create_instance("DepositVoucher", "vh_mis")
    try:
        tb.patch_state(tvc, "DepositVoucher", {"_depositHash": "1"})
        r = tb.call_internal(
            tvc,
            "DepositVoucher",
            "constructor",
            {
                "chainId": "11155111",
                "depositId": "1",
                "contractAddr": "2",
                "dappId": "3",
                "amount": "1000000",
                "anAccount": "0x742d35Cc6f573FaDEE3BdC23f9D14CfA5Fd4596c",
            },
            sender=USDC_BRIDGE_ADDR,
            address="0:2222222222222222222222222222222222222222222222222222222222222222",
        )
        tb.assert_failure(r, ERR_HASH_MISMATCH)
    finally:
        tb.cleanup_instance("DepositVoucher", "vh_mis")
