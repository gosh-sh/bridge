"""USDCBridge.finalizeDeposit — negative paths (pre-accept + ZK reject)."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    ERR_INVALID_ZKPROOF,
    ERR_OVERFLOW,
    ERR_ZERO_AMOUNT,
    USDC_BRIDGE_ADDR,
    build_public_inputs,
    init_bridge_instance,
)

pytestmark = pytest.mark.unit


def test_finalize_deposit_zero_amount(tb):
  # INV: DEP-AN-01 — amount must be > 0 (parsed before accept).
  tvc = init_bridge_instance(tb, "fd_zero")
  try:
    pi = build_public_inputs(amount=0).hex()
    r = tb.call(
      tvc,
      "USDCBridge",
      "finalizeDeposit",
      {"proof": "00", "publicInputs": pi},
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_failure(r, ERR_ZERO_AMOUNT)
  finally:
    tb.cleanup_instance("USDCBridge", "fd_zero")


def test_finalize_deposit_amount_overflow(tb):
  # QC-AN-01 — amount Fr must fit uint64 before mint path.
  tvc = init_bridge_instance(tb, "fd_ovf")
  try:
    pi = build_public_inputs(amount=(1 << 64)).hex()
    r = tb.call(
      tvc,
      "USDCBridge",
      "finalizeDeposit",
      {"proof": "00", "publicInputs": pi},
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_failure(r, ERR_OVERFLOW)
  finally:
    tb.cleanup_instance("USDCBridge", "fd_ovf")


def test_finalize_deposit_invalid_zk_proof(tb):
  # INV: DEP-AN-02 — garbage proof rejected with ERR_INVALID_ZKPROOF (220).
  tvc = init_bridge_instance(tb, "fd_badzk")
  try:
    pi = build_public_inputs(amount=1).hex()
    r = tb.call(
      tvc,
      "USDCBridge",
      "finalizeDeposit",
      {"proof": "00", "publicInputs": pi},
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_failure(r, ERR_INVALID_ZKPROOF)
  finally:
    tb.cleanup_instance("USDCBridge", "fd_badzk")


def test_confirm_deposit_wrong_sender(tb):
  # INV: DEP-AN-03 — only deterministic DepositVoucher may call confirmDeposit.
  from bridge_helpers import ERR_INVALID_SENDER

  tvc = init_bridge_instance(tb, "cd_sender")
  try:
    r = tb.call(
      tvc,
      "USDCBridge",
      "confirmDeposit",
      {
        "depositId": "1",
        "contractAddr": "0",
        "dappId": "0",
        "amount": "1000000",
        "anAccount": "0x742d35Cc6f573FaDEE3BdC23f9D14CfA5Fd4596c",
      },
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_failure(r, ERR_INVALID_SENDER)
  finally:
    tb.cleanup_instance("USDCBridge", "cd_sender")
