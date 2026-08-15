"""QC-AN-07 — finalizeDeposit when voucher code is missing / deploy path."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    fixtures_available,
    init_bridge_instance,
    load_fixture_proof,
    seed_trust_from_pi,
)
from test_base import MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.slow]


def get_minted(tb, bridge_tvc) -> int:
    r = tb.call(
        bridge_tvc,
        BRIDGE_CONTRACT,
        "getTotalBridged",
        {"tokenId": str(USDC_ECC_ID)},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r)
    return int(r.response["minted"])


@pytest.mark.skipif(not fixtures_available(), reason="deposit_10proofs not synced")
def test_finalize_deposit_without_voucher_code_no_mint(tb):
    """QC-AN-07 — ZK + deploy msg may run, but mint requires valid voucher code in pipeline."""
    bridge_tvc = init_bridge_instance(tb, "qc07_novc", with_voucher_code=False)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    proof, pi = load_fixture_proof(0)
    seed_trust_from_pi(tb, bridge_tvc, pi)
    try:
        minted_before = get_minted(tb, bridge_tvc)
        r = tb.call(
            bridge_tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": proof.hex(), "publicInputs": pi.hex()},
            address=USDC_BRIDGE_ADDR,
        )
        # Opcode + accept succeed; outbound deploy may still be queued.
        tb.assert_success(r, "finalizeDeposit without voucher code")
        if r.messages:
            pipe.enqueue(r.messages)
            pipe.process(max_steps=20, verbose=False)
        assert get_minted(tb, bridge_tvc) == minted_before
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "qc07_novc")
