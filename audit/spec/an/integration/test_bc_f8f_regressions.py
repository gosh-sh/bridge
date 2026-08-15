"""F8-F — BC-AN-01/02 regression gate (eccUSDCBridge @ contracts/bridge).

BC-AN-01: on-chain `f.dappId = 0` (dapp limbs in PI ignored).
BC-AN-02: `_trustedL1Bridge` SET allowlist per chainId (mitigation on contracts/bridge).

Linked: audit/findings/BC-AN-01/, BC-AN-02/; closeout-an.md § F8-F.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from bridge_helpers import (
    BRIDGE_CONTRACT,
    ERR_UNSUPPORTED_SRC_CHAIN,
    USDC_BRIDGE_ADDR,
    build_public_inputs,
    init_bridge_instance,
)

pytestmark = [pytest.mark.integration, pytest.mark.bc_regression]

_CONTRACTS_ROOT = Path(__file__).resolve().parents[2] / "an-contracts"
_BRIDGE_SOL = _CONTRACTS_ROOT / "exchange" / "eccUSDCBridge.sol"


def _bridge_source() -> str:
    if not _BRIDGE_SOL.is_file():
        pytest.skip(f"sync contracts first: {_BRIDGE_SOL}")
    return _BRIDGE_SOL.read_text(encoding="utf-8")


def test_f8f_bc_an_01_replay_hash_includes_chain_id():
    """BC-AN-01 — replay slot includes chainId + pinned dappId."""
    src = _bridge_source()
    assert "abi.encode(f.depositId, f.contractAddr, f.dappId, f.chainId)" in src or re.search(
        r"abi\.encode\([^)]*depositId[^)]*contractAddr[^)]*dappId[^)]*chainId",
        src,
    )


def test_f8f_bc_an_01_dapp_id_pinned_to_zero():
    """BC-AN-01 mitigation — dapp limbs in PI are ignored; deposits always dapp 0."""
    src = _bridge_source()
    assert "f.dappId       = 0" in src or "f.dappId = 0" in src


def test_f8f_bc_an_02_trusted_l1_bridge_allowlist_present():
    """BC-AN-02 mitigation — owner-managed trusted L1 bridge SET on contracts/bridge."""
    src = _bridge_source()
    assert "_trustedL1Bridge" in src
    assert "setTrustedL1Bridge" in src


@pytest.mark.integration
def test_f8f_bc_an_02_untrusted_contract_addr_rejected_pre_zk(tb):
    """Untrusted (chainId, contractAddr) rejected before ZK (ERR_UNSUPPORTED_SRC_CHAIN)."""
    tvc = init_bridge_instance(tb, "f8f_bc02")
    try:
        pi = build_public_inputs(contract_addr=0xDEADBEEF).hex()
        r = tb.call(
            tvc,
            BRIDGE_CONTRACT,
            "finalizeDeposit",
            {"proof": "00", "publicInputs": pi},
            address=USDC_BRIDGE_ADDR,
        )
        tb.assert_failure(r, ERR_UNSUPPORTED_SRC_CHAIN)
    finally:
        tb.cleanup_instance(BRIDGE_CONTRACT, "f8f_bc02")
