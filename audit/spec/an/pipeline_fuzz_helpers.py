"""MessagePipeline fuzz helpers — delivery schedules and reference oracles."""

from __future__ import annotations

import random
from typing import Iterable, List, Sequence

from bridge_helpers import (
    FAKE_MISSING_DEST,
    MAX_PROOF_INDEX,
    USDC_BRIDGE_ADDR,
    fixtures_available,
    fr_le,
    get_total_bridged_minted,
    init_bridge_instance,
    load_fixture_proof,
    max_available_proof_index,
    ten_proofs_available,
)
from test_base import JsonResult, MessagePipeline, TestBase


def voucher_deploy_msg(jr: JsonResult) -> dict:
    deploys = [
        m
        for m in jr.messages
        if m.get("state_init") and m.get("state_init") not in (None, "None")
    ]
    if len(deploys) != 1:
        raise ValueError(f"expected 1 deploy message, got {len(deploys)}")
    return deploys[0]


def voucher_address(pipe: MessagePipeline, deploy_msg: dict) -> str:
    addr = deploy_msg.get("destination") or deploy_msg.get("dst")
    assert addr
    assert addr == pipe.compute_address(deploy_msg["state_init"])
    return addr


def finalize_enqueue(
    tb: TestBase, bridge_tvc, pipe: MessagePipeline, proof_idx: int
) -> JsonResult:
    proof, pi = load_fixture_proof(proof_idx)
    r = tb.call(
        bridge_tvc,
        "USDCBridge",
        "finalizeDeposit",
        {"proof": proof.hex(), "publicInputs": pi.hex()},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, f"finalizeDeposit proof_{proof_idx:02d}")
    pipe.enqueue(r.messages)
    return r


def register_voucher(pipe: MessagePipeline, jr: JsonResult) -> str:
    deploy = voucher_deploy_msg(jr)
    vaddr = voucher_address(pipe, deploy)
    pipe.abi_map[vaddr] = "DepositVoucher"
    return vaddr


def drain_fifo(pipe: MessagePipeline, *, max_steps: int = 50) -> None:
    steps = 0
    while pipe.queue and steps < max_steps:
        pipe.process_one_at(0, verbose=False)
        steps += 1


def deliver_random_indices(
    pipe: MessagePipeline, rng: random.Random, *, max_steps: int = 80
) -> int:
    """Pop random queue slots until empty or step budget exhausted."""
    steps = 0
    while pipe.queue and steps < max_steps:
        idx = rng.randrange(len(pipe.queue))
        pipe.process_one_at(idx, verbose=False)
        steps += 1
    return steps


def canonical_bridged_minted(
    tb: TestBase, proof_indices: Sequence[int], tag: str
) -> int:
    """FIFO reference: sequential finalize + drain for each proof index."""
    tvc = init_bridge_instance(tb, tag)
    pipe = MessagePipeline(tb)
    pipe.register(USDC_BRIDGE_ADDR, tvc, "USDCBridge")
    try:
        for idx in proof_indices:
            jr = finalize_enqueue(tb, tvc, pipe, idx)
            drain_fifo(pipe)
            register_voucher(pipe, jr)
        return get_total_bridged_minted(tb, tvc)
    finally:
        pipe.cleanup()
        tb.cleanup_instance("USDCBridge", tag)


def sum_proof_amounts(indices: Iterable[int]) -> int:
    total = 0
    for i in indices:
        _, pi = load_fixture_proof(i)
        total += fr_le(pi, 2)
    return total


def canonical_max_minted_from_history(proof_indices: Sequence[int]) -> int:
    """Upper bound on bridge minted: first successful finalize per deposit_id only."""
    seen_deposit_ids: set[int] = set()
    total = 0
    for idx in proof_indices:
        _, pi = load_fixture_proof(idx)
        deposit_id = fr_le(pi, 0)
        if deposit_id in seen_deposit_ids:
            continue
        seen_deposit_ids.add(deposit_id)
        total += fr_le(pi, 2)
    return total


def assert_fixtures_ready() -> None:
    if not fixtures_available():
        raise RuntimeError("deposit_10proofs fixtures missing")


WHOLE_USDC = 1_000_000
OWNER_RECIPIENT = "0:3333333333333333333333333333333333333333333333333333333333333333"


def owner_mint_and_send(tb: TestBase, tvc, *, value: int, nonce: int) -> None:
    """Owner path — bumps `_totalMinted`, not bridged counter."""
    r = tb.call(
        tvc,
        "USDCBridge",
        "mintAndSend",
        {"recipient": OWNER_RECIPIENT, "value": str(value), "nonce": str(nonce)},
        sign_keys=tb.admin_keys,
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, "mintAndSend")


def proof_index_strategy():
    """Hypothesis strategy for available proof indices."""
    import hypothesis.strategies as st

    hi = max_available_proof_index()
    if hi < 0:
        return st.nothing()
    return st.integers(min_value=0, max_value=hi)


def bounce_probe_from_message(msg: dict, *, dest: str = FAKE_MISSING_DEST) -> dict:
    """Clone an inflight message targeting an unregistered address with bounce."""
    probe = dict(msg)
    probe["destination"] = dest
    probe["bounce"] = True
    return probe


def inject_bounce_probe(pipe: MessagePipeline, rng: random.Random) -> bool:
    """Enqueue a bounce probe cloned from a random inflight message."""
    inflight = pipe.peek_inflight()
    if not inflight:
        return False
    candidates = [m for m in inflight if m.get("boc")]
    if not candidates:
        return False
    msg = rng.choice(candidates)
    pipe.enqueue([bounce_probe_from_message(msg)])
    return True


def retry_deploy_message(jr: JsonResult) -> dict:
    """Deploy leg from a successful finalize (relayer retry semantics)."""
    return voucher_deploy_msg(jr)


def sum_all_proof_amounts(max_index: int = MAX_PROOF_INDEX) -> int:
    return sum_proof_amounts(range(max_index + 1))
