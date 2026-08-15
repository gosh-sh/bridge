"""DEP-AN-11 / DEP-AN-12 — finalizeDeposit → MessagePipeline → confirmDeposit."""

from __future__ import annotations

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    USDC_BRIDGE_ADDR,
    USDC_ECC_ID,
    fixtures_available,
    fr_le,
    init_bridge_instance,
    load_fixture_proof,
    seed_trust_from_pi,
)
from test_base import JsonResult, MessagePipeline

pytestmark = [pytest.mark.integration, pytest.mark.slow]


def voucher_deploy_msg(jr: JsonResult) -> dict:
    deploys = [
        m
        for m in jr.messages
        if m.get("state_init") and m.get("state_init") not in (None, "None")
    ]
    assert len(deploys) == 1, f"expected 1 deploy, got {len(deploys)}"
    return deploys[0]


def voucher_address(pipe: MessagePipeline, deploy_msg: dict) -> str:
    addr = deploy_msg.get("destination") or deploy_msg.get("dst")
    assert addr
    assert addr == pipe.compute_address(deploy_msg["state_init"])
    return addr


def get_minted(tb, bridge_tvc) -> int:
    r = tb.call(
        bridge_tvc,
        BRIDGE_CONTRACT,
        "getTotalBridged",
        {"tokenId": str(USDC_ECC_ID)},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, "getTotalBridged")
    return int(r.response["minted"])


def finalize_and_drain(tb, bridge_tvc, pipe: MessagePipeline, proof: bytes, pi: bytes):
    seed_trust_from_pi(tb, bridge_tvc, pi)
    prev = len(pipe.history)
    r = tb.call(
        bridge_tvc,
        BRIDGE_CONTRACT,
        "finalizeDeposit",
        {"proof": proof.hex(), "publicInputs": pi.hex()},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, "finalizeDeposit")
    pipe.enqueue(r.messages)
    pipe.process(max_steps=20, verbose=False)
    return r, pipe.history[prev:]


def assert_confirm_path(steps, msg: str = "") -> None:
    deploy_ok = any(s.is_deploy and s.vm_success for s in steps)
    bridge_ok = any(
        s.destination == USDC_BRIDGE_ADDR
        and not s.is_deploy
        and s.vm_success
        and s.gas_used > 10_000
        for s in steps
    )
    assert deploy_ok, f"{msg}: no voucher deploy: {steps}"
    assert bridge_ok, f"{msg}: no confirmDeposit on bridge: {steps}"


@pytest.mark.skipif(not fixtures_available(), reason="run scripts/sync_an_contracts.sh")
def test_dep_an_11_pipeline_voucher_confirm_minted(tb):
    """INV DEP-AN-11: voucher deploy → confirmDeposit → minted increases."""
    bridge_tvc = init_bridge_instance(tb, "dep11")
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    proof, pi = load_fixture_proof(0)
    expected = fr_le(pi, 2)
    try:
        minted_before = get_minted(tb, bridge_tvc)
        r, steps = finalize_and_drain(tb, bridge_tvc, pipe, proof, pi)
        vaddr = voucher_address(pipe, voucher_deploy_msg(r))
        pipe.abi_map[vaddr] = "DepositVoucher"
        assert_confirm_path(steps, "DEP-AN-11")
        assert get_minted(tb, bridge_tvc) == minted_before + expected
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "dep11")


@pytest.mark.skipif(not fixtures_available(), reason="run scripts/sync_an_contracts.sh")
def test_dep_an_12_replay_finalize_minted_unchanged(tb):
    """INV DEP-AN-12: replay same proof → minted unchanged."""
    bridge_tvc = init_bridge_instance(tb, "dep12")
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, bridge_tvc, BRIDGE_CONTRACT)
    proof, pi = load_fixture_proof(0)
    try:
        r1, _ = finalize_and_drain(tb, bridge_tvc, pipe, proof, pi)
        pipe.abi_map[voucher_address(pipe, voucher_deploy_msg(r1))] = "DepositVoucher"
        minted_first = get_minted(tb, bridge_tvc)
        assert minted_first == fr_le(pi, 2)

        minted_before = get_minted(tb, bridge_tvc)
        _, steps2 = finalize_and_drain(tb, bridge_tvc, pipe, proof, pi)
        assert get_minted(tb, bridge_tvc) == minted_before

        replay_bridge = [s for s in steps2 if s.destination == USDC_BRIDGE_ADDR and not s.is_deploy]
        assert replay_bridge
        assert all(s.gas_used < 10_000 for s in replay_bridge)
    finally:
        pipe.cleanup()
        tb.cleanup_instance(BRIDGE_CONTRACT, "dep12")
