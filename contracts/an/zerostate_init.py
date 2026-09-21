#!/usr/bin/env python3
"""Bring a premined eccUSDCBridge account to the state a fresh chain needs.

acki-nacki's zerostate generator hands this module the image of the premine
stub, the tools to work on it and the handful of values only it knows (the
bridge's address, its owner key, the USDC wallet it was given). Everything the
bridge itself requires — the data cell, the code upgrade, the owner-side setup
calls, and the write into the zerostate — is decided here, so a contract change
lands in this repository alone.

The module is placed into acki-nacki by `contracts/scripts/bridge_contracts.py`
together with the contracts, pinned to one commit and checked against a sha256.
"""

from __future__ import annotations

import json
import os
import shutil
import tempfile

CONTEXT_VERSION = 1

# The Ethereum bridge a fresh chain trusts, and the chain it lives on. The
# allowlist starts empty and fail-closed, so a network generated without this
# refuses every deposit until its owner seeds one by hand.
L1_CHAIN_ID = os.environ.get("BRIDGE_ZS_L1_CHAIN_ID", "11155111")
L1_BRIDGE = os.environ.get("BRIDGE_ZS_L1_BRIDGE", "0xCdFd6Cef70F68d0849310cD970F8ef8F8E4b4fdb")

BRIDGE_TVC = "contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.tvc"
BRIDGE_ABI = "contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json"
VOUCHER_TVC = "contracts/an/0.81.0_compiled/exchange/DepositVoucher.tvc"
LIGHT_CLIENT_TVC = "contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.tvc"
ENCODER_TVC = "contracts/an/zerostate/BridgeZerostateData.tvc"
ENCODER_ABI = "contracts/an/zerostate/BridgeZerostateData.abi.json"

# The encoder is never deployed; the debugger runs it at an arbitrary address.
ENCODER_ADDRESS = "0:" + "22" * 32

# The 18-key contract the caller (acki-nacki's zerostate generator) hands in.
# Two of these carry an invariant the caller must honour — getting either
# wrong does not raise, it silently corrupts the zerostate:
#
#   * `format_params` and `abi_header` are used exactly as given: both are
#     interpolated raw into a shell command, so whatever they produce/hold
#     must already be shell-quoted.
#   * `account_tvc` is rewritten in place by every debugger call below (that
#     mutation is how the upgrade and the two owner calls reach the account
#     this module then writes into the zerostate) — the caller must hand
#     over a throwaway copy of the premine stub image, and remove it once
#     this module returns.
#
# version            -- context schema version; must equal CONTEXT_VERSION
# tvm_debugger       -- path to the tvm-debugger binary
# zerostate_helper   -- path to the zerostate-helper binary
# zerostate_path     -- zerostate directory zerostate_helper writes into
# account_tvc        -- premine stub image; see the in-place invariant above
# update_zero_abi    -- ABI of the account's current (pre-upgrade) code, for `updateCode`
# root_keys          -- keys `updateCode` is signed with (the chain's root/BlockKeeper keys)
# address            -- the bridge account's address in the zerostate
# keys               -- the bridge owner's keys; sign the two setter calls after the upgrade
# usdc_wallet        -- USDC wallet address baked into the encoded data cell
# balance            -- nanotoken balance zerostate_helper gives the account
# dapp_id            -- dapp id zerostate_helper assigns to the account
# abi_header         -- see the shell-quoting invariant above
# run                -- callable: shell command string -> its combined output
# format_params      -- see the shell-quoting invariant above (dict -> `-p` argument)
# get_code           -- callable: .tvc path -> TVM code cell for that image
# read_public_key    -- callable: keys-file path -> hex public key (no 0x)
# artefact           -- callable: repo-relative artefact path -> its placed, absolute path
REQUIRED_KEYS = (
    "version", "tvm_debugger", "zerostate_helper", "zerostate_path", "account_tvc",
    "update_zero_abi", "root_keys", "address", "keys", "usdc_wallet", "balance",
    "dapp_id", "abi_header", "run", "format_params", "get_code", "read_public_key",
    "artefact",
)

TVM_OK = "TVM terminated with exit code 0"


def _check_context(ctx: dict) -> None:
    missing = [key for key in REQUIRED_KEYS if key not in ctx]
    if missing:
        raise SystemExit(
            "bridge zerostate: the context is missing " + ", ".join(missing)
            + f"; this module speaks context version {CONTEXT_VERSION}"
        )
    if ctx["version"] != CONTEXT_VERSION:
        raise SystemExit(
            f"bridge zerostate: context version {ctx['version']} is not supported; "
            f"this module speaks version {CONTEXT_VERSION}"
        )


def _debugger(ctx: dict, abi: str, address: str, method: str, params: dict,
              image: str, keys: str | None = None, decode_out: bool = False) -> str:
    cmd = (f"{ctx['tvm_debugger']} run -a {abi} --address {address} "
           f"-m {method} -p {ctx['format_params'](params)} -i {image}")
    if keys is not None:
        cmd += f" --sign {keys}"
    cmd += f" --abi-header {ctx['abi_header']}"
    if decode_out:
        cmd += " --decode-out-messages"
    output = ctx["run"](cmd)
    if TVM_OK not in output:
        raise SystemExit(f"bridge zerostate: {method} failed:\n{output}")
    return output


def _data_cell(ctx: dict, voucher_code: str) -> str:
    """The cell `updateCode` writes into the premined bridge.

    `tvm-debugger run -i` rewrites the persistent-data cell of whatever image
    it is given, and the placed encoder .tvc is a tracked artefact checked
    against a sha256 manifest — running the debugger on it in place would
    dirty that file on every zerostate generation. Run it on a throwaway
    copy instead; `account_tvc` below is the opposite case, where mutating
    the image in place is the whole point.
    """
    encoder_tvc = ctx["artefact"](ENCODER_TVC)
    with tempfile.TemporaryDirectory() as tmp_dir:
        image = os.path.join(tmp_dir, os.path.basename(ENCODER_TVC))
        shutil.copyfile(encoder_tvc, image)
        output = _debugger(
            ctx,
            abi=ctx["artefact"](ENCODER_ABI),
            address=ENCODER_ADDRESS,
            method="getBridgeData",
            params={
                "pubkey": f"0x{ctx['read_public_key'](ctx['keys'])}",
                "usdcWallet": ctx["usdc_wallet"],
                "totalMinted": "0",
                "mintNonce": "0",
                "mintAccumulatorNonce": "0",
                "mintedKeys": [],
                "mintedValues": [],
                "burnedKeys": [],
                "burnedValues": [],
                "depositVoucherCode": voucher_code,
            },
            image=image,
            decode_out=True,
        )
    for line in output.split("\n"):
        if '"value0"' in line:
            return json.loads(line)["value0"]
    raise SystemExit(f"bridge zerostate: the encoder returned no cell:\n{output}")


def initialize(ctx: dict) -> None:
    _check_context(ctx)

    bridge_code = ctx["get_code"](ctx["artefact"](BRIDGE_TVC))
    voucher_code = ctx["get_code"](ctx["artefact"](VOUCHER_TVC))

    _debugger(ctx, abi=ctx["update_zero_abi"], address=ctx["address"], method="updateCode",
              params={"newcode": bridge_code, "cell": _data_cell(ctx, voucher_code)},
              image=ctx["account_tvc"], keys=ctx["root_keys"])

    # Seed the L1 allowlist INTO the zerostate: the upgrade cell deliberately
    # carries no trusted set, so a fresh deploy would reject every deposit.
    _debugger(ctx, abi=ctx["artefact"](BRIDGE_ABI), address=ctx["address"],
              method="setTrustedL1Bridge",
              params={"chainId": L1_CHAIN_ID, "l1Bridge": L1_BRIDGE, "allowed": "true"},
              image=ctx["account_tvc"], keys=ctx["keys"])

    # The upgrade cell does not carry the light-client code either, and the
    # bridge derives the light-client address from it, so without this a fresh
    # chain has no anchor writer to deploy or trust.
    _debugger(ctx, abi=ctx["artefact"](BRIDGE_ABI), address=ctx["address"],
              method="setLightClientCode",
              params={"code": ctx["get_code"](ctx["artefact"](LIGHT_CLIENT_TVC))},
              image=ctx["account_tvc"], keys=ctx["keys"])

    ctx["run"](f"{ctx['zerostate_helper']} -d -p {ctx['zerostate_path']} add contract "
               f"{ctx['account_tvc']} {ctx['balance']} {ctx['address']} {ctx['dapp_id']}")
