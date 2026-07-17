"""Shared helpers for USDCBridge audit spec tests."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, Optional

from test_base import TestBase

# Fixed premine address from modifiers.sol (workchain 0).
USDC_BRIDGE_ADDR = (
    "0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
)
USDC_ECC_ID = 3
PI_LEN = 352  # 11 × 32-byte LE Fr

ERR_ZERO_AMOUNT = 204
ERR_INVALID_SENDER = 207
ERR_NOT_OWNER = 209
ERR_NOT_WHOLE_USDC = 213
ERR_INVALID_NONCE = 215
ERR_NO_ECC = 216
ERR_MULTIPLE_ECC = 217
ERR_RECIPIENT_TOO_LONG = 218
ERR_HASH_MISMATCH = 219
ERR_INVALID_ZKPROOF = 220
ERR_UNSUPPORTED_TOKEN = 221
ERR_OVERFLOW = 214

EVM_RECIPIENT_20B = "742d35cc6634c05329225a8b3683a49a3ca7d65c"

FIXTURES_ROOT = Path(__file__).resolve().parent / "fixtures" / "deposit_10proofs"


def fr_le(pi: bytes, index: int) -> int:
    start = index * 32
    return int.from_bytes(pi[start : start + 32], "little")


def build_public_inputs(
    *,
    deposit_id: int = 1,
    sender: int = 0,
    amount: int = 1_000_000,
    contract_addr: int = 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82,
    dapp_id: int = 0x1A1A1A1A1A,
    an_account_hi: int = 0,
    an_account_lo: int = 0x742d35Cc6f573FaDEE3BdC23f9D14CfA5Fd4596c,
    pad_tail: bool = True,
) -> bytes:
    """Assemble 11×32 B LE public-inputs blob (first 8 Fr used on-chain)."""
    buf = bytearray(PI_LEN)
    fields = [
        deposit_id,
        sender,
        amount,
        contract_addr,
        (dapp_id >> 128) & ((1 << 128) - 1),
        dapp_id & ((1 << 128) - 1),
        an_account_hi,
        an_account_lo,
    ]
    for i, val in enumerate(fields):
        buf[i * 32 : (i + 1) * 32] = val.to_bytes(32, "little")
    return bytes(buf)


def deposit_voucher_code_boc(tb: TestBase) -> str:
    """Code BOC (base64) for DepositVoucher — patches into _depositVoucherCode."""
    return tb.extract_code_boc("DepositVoucher")


def init_bridge_instance(
    tb: TestBase,
    tag: str,
    *,
    with_voucher_code: bool = True,
    usdc_wallet: str = "0:1111111111111111111111111111111111111111111111111111111111111111",
) -> Path:
    """Fresh USDCBridge TVC: constructor + optional voucher code patch."""
    tvc = tb.create_instance("USDCBridge", tag)
    pubkey = "0x" + tb.admin_pubkey
    r = tb.call(
        tvc,
        "USDCBridge",
        "constructor",
        {"pubkey": pubkey, "usdcWallet": usdc_wallet},
        address=USDC_BRIDGE_ADDR,
    )
    tb.assert_success(r, f"USDCBridge constructor ({tag})")
    if with_voucher_code:
        tb.patch_state(
            tvc,
            "USDCBridge",
            {"_depositVoucherCode": deposit_voucher_code_boc(tb)},
        )
    return tvc


def load_fixture_proof(n: int = 0) -> tuple[bytes, bytes]:
    d = FIXTURES_ROOT / f"proof_{n:02d}"
    proof = (d / "proof.bin").read_bytes()
    pi = (d / "public_inputs.bin").read_bytes()
    assert len(pi) == PI_LEN, f"{d}: expected {PI_LEN} B public_inputs, got {len(pi)}"
    return proof, pi


def fixtures_available() -> bool:
    return (FIXTURES_ROOT / "proof_00" / "proof.bin").is_file()
