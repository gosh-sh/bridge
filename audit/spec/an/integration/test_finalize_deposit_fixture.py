"""USDCBridge.finalizeDeposit with real deposit_10proofs fixtures (integration)."""

from __future__ import annotations

import pytest

from bridge_helpers import USDC_BRIDGE_ADDR, fixtures_available, init_bridge_instance, load_fixture_proof

pytestmark = pytest.mark.integration


@pytest.mark.skipif(not fixtures_available(), reason="run scripts/sync_an_contracts.sh for fixtures")
@pytest.mark.parametrize("proof_idx", [0, 1])
def test_finalize_deposit_real_fixture(tb, proof_idx: int):
    """INV: DEP-AN-10/13 — real SHPLONK proofs verify (VK embedded in contract)."""
    tvc = init_bridge_instance(tb, f"fd_real{proof_idx:02d}")
    try:
        proof, pi = load_fixture_proof(proof_idx)
        r = tb.call(
            tvc,
            "USDCBridge",
            "finalizeDeposit",
            {"proof": proof.hex(), "publicInputs": pi.hex()},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_success(r, f"finalizeDeposit proof_{proof_idx:02d}")
        assert len(r.messages) >= 1, "expected DepositVoucher deploy internal message"
    finally:
        tb.cleanup_instance("USDCBridge", f"fd_real{proof_idx:02d}")
