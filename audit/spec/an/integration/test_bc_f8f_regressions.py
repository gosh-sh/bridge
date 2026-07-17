"""F8-F — BC-AN-01/02 regression gate.

Stays green while BC candidates remain open (documents absent guards / vulnerable behaviour).
After author fix: update tests (OK) or flip expected behaviour — do not silently delete.

Linked: audit/findings/BC-AN-01/, BC-AN-02/; closeout-an.md § F8-F.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from bridge_helpers import ERR_INVALID_ZKPROOF, USDC_BRIDGE_ADDR, build_public_inputs, init_bridge_instance

pytestmark = [pytest.mark.integration, pytest.mark.bc_regression]

_CONTRACTS_ROOT = Path(__file__).resolve().parents[2] / "an-contracts"
_USDC_BRIDGE_SOL = _CONTRACTS_ROOT / "exchange" / "USDCBridge.sol"


def _usdc_bridge_source() -> str:
    if not _USDC_BRIDGE_SOL.is_file():
        pytest.skip(f"sync contracts first: {_USDC_BRIDGE_SOL}")
    return _USDC_BRIDGE_SOL.read_text(encoding="utf-8")


def test_f8f_bc_an_01_replay_hash_includes_dapp_id():
    """BC-AN-01 — replay slot splits on dappId (not depositId alone)."""
    src = _usdc_bridge_source()
    assert "tvm.hash(abi.encode(f.depositId, f.contractAddr, f.dappId))" in src or re.search(
        r"abi\.encode\([^)]*depositId[^)]*contractAddr[^)]*dappId", src
    ), "expected depositId+contractAddr+dappId replay preimage"


def test_f8f_bc_an_01_no_on_chain_expected_dapp_id():
    """BC-AN-01 — no immutable EXPECTED_DAPP_ID guard in current USDCBridge."""
    src = _usdc_bridge_source()
    assert "EXPECTED_DAPP_ID" not in src
    assert "expectedDappId" not in src
    # dappId taken from PI only — not compared to deployment constant.
    assert "f.dappId" in src


def test_f8f_bc_an_02_no_on_chain_l1_bridge_allowlist():
    """BC-AN-02 — no immutable EXPECTED_L1_BRIDGE / contractAddr pin."""
    src = _usdc_bridge_source()
    assert "EXPECTED_L1_BRIDGE" not in src
    assert "expectedL1Bridge" not in src.lower()
    assert "f.contractAddr = fr[3]" in src


@pytest.mark.integration
def test_f8f_bc_an_02_alien_contract_addr_not_rejected_pre_zk(tb):
    """BC-AN-02 — arbitrary contractAddr reaches ZK path (fails on bad proof, not allowlist)."""
    tvc = init_bridge_instance(tb, "f8f_bc02")
    try:
        pi = build_public_inputs(contract_addr=0xDEADBEEF).hex()
        r = tb.call(
            tvc,
            "USDCBridge",
            "finalizeDeposit",
            {"proof": "00", "publicInputs": pi},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_INVALID_ZKPROOF)
    finally:
        tb.cleanup_instance("USDCBridge", "f8f_bc02")
