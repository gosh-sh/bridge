"""F7-D — negative finalize storm (Hypothesis + debugger, slow)."""

from __future__ import annotations

import pytest

pytest.importorskip("hypothesis")
from hypothesis import HealthCheck, given, settings, strategies as st

from bridge_helpers import (
    ERR_INVALID_ZKPROOF,
    USDC_BRIDGE_ADDR,
    build_public_inputs,
    get_total_bridged_minted,
    init_bridge_instance,
)

pytestmark = [pytest.mark.integration, pytest.mark.property, pytest.mark.slow]


@pytest.fixture
def f7_d_bridge(tb):
    """One bridge instance per Hypothesis test item (reused across examples)."""
    tvc = init_bridge_instance(tb, "f7_d_garbage")
    yield tvc
    tb.cleanup_instance("USDCBridge", "f7_d_garbage")


@settings(suppress_health_check=[HealthCheck.function_scoped_fixture])
@given(proof=st.binary(min_size=0, max_size=128))
def test_random_proof_with_valid_shaped_pi_no_mint(tb, f7_d_bridge, proof: bytes):
    """INV AN-DEP-4 — garbage proof does not increase bridged minted counter."""
    before = get_total_bridged_minted(tb, f7_d_bridge)
    pi = build_public_inputs().hex()
    r = tb.call(
        f7_d_bridge,
        "USDCBridge",
        "finalizeDeposit",
        {"proof": proof.hex(), "publicInputs": pi},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_failure(r, ERR_INVALID_ZKPROOF)
    assert get_total_bridged_minted(tb, f7_d_bridge) == before
