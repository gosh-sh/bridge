"""USDCBridge — getter smoke + constructor wiring (F3 unit)."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    BRIDGE_VERSION_NAME,
    USDC_BRIDGE_ADDR,
    init_bridge_instance,
)

pytestmark = pytest.mark.unit


def test_get_version(tb):
  # INV: F-BR-01 — compiled artifact is callable after constructor.
  tvc = init_bridge_instance(tb, "ver", with_voucher_code=False)
  try:
    r = tb.call(tvc, BRIDGE_CONTRACT, "getVersion", {}, address=USDC_BRIDGE_ADDR)
    tb.assert_success(r)
    assert r.response["value0"] == "1.3.0"
    assert r.response["value1"] == BRIDGE_VERSION_NAME
  finally:
    tb.cleanup_instance(BRIDGE_CONTRACT, "ver")


def test_get_deposit_voucher_code_hash_set_after_patch(tb):
  tvc = init_bridge_instance(tb, "vhash")
  try:
    r = tb.call(tvc, BRIDGE_CONTRACT, "getDepositVoucherCodeHash", {}, address=USDC_BRIDGE_ADDR)
    tb.assert_success(r)
    assert int(r.response["value0"], 0) != 0
  finally:
    tb.cleanup_instance(BRIDGE_CONTRACT, "vhash")


def test_get_total_bridged_initial_zero(tb):
  tvc = init_bridge_instance(tb, "bridged0")
  try:
    r = tb.call(
      tvc,
      BRIDGE_CONTRACT,
      "getTotalBridged",
      {"tokenId": "3"},
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r)
    assert r.response["minted"] == "0"
    assert r.response["burned"] == "0"
  finally:
    tb.cleanup_instance(BRIDGE_CONTRACT, "bridged0")
