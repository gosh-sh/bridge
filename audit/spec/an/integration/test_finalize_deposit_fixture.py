"""USDCBridge.finalizeDeposit with real deposit_10proofs fixture (integration)."""

from __future__ import annotations

import pytest

from bridge_helpers import USDC_BRIDGE_ADDR, fixtures_available, init_bridge_instance, load_fixture_proof

pytestmark = pytest.mark.integration


@pytest.mark.skipif(not fixtures_available(), reason="run scripts/sync_an_contracts.sh for fixtures")
def test_finalize_deposit_proof_00(tb):
  # INV: DEP-AN-10 — real SHPLONK proof verifies in tvm-debugger (VK embedded in contract).
  tvc = init_bridge_instance(tb, "fd_real00")
  try:
    proof, pi = load_fixture_proof(0)
    r = tb.call(
      tvc,
      "USDCBridge",
      "finalizeDeposit",
      {"proof": proof.hex(), "publicInputs": pi.hex()},
      address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, "finalizeDeposit proof_00")
    assert len(r.messages) >= 1, "expected DepositVoucher deploy internal message"
  finally:
    tb.cleanup_instance("USDCBridge", "fd_real00")
