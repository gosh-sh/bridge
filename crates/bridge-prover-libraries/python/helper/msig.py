"""
Multisig lifecycle helpers for the AN→ETH bridge Circuit 4 (event) E2E path.

Both `test_deploy_and_withdraw_only.py` and
`generate_withdrawals_with_live_event_proving.py` need to:
  1. genaddr an UpdateCustodianMultisigWallet
  2. fund it (mode-dependent: shellnet two-shot vs local single-shot)
  3. deployx it
  4. top up ECC[2] if the deploy consumed it
  5. mint ECC[3] USDC into it via USDCBridge.mintAndSend

This module owns all of that. `bridge_e2e.py` keeps the E2E scaffolding
(Tracer, GqlClient, pipeline stages) and USDCBridge owner-key concerns.
"""

import os
import shutil
import time

from helper import common
from helper.bridge_e2e import (
    ECC_ID_FOR_BURN,
    GIVER_ABI, GIVER_ADDRESS, GIVER_KEY_PATH,
    MSIG_ABI, MSIG_TVC_STEM,
    USDC_BRIDGE_ABI, USDC_BRIDGE_ACCOUNT_ID, USDC_BRIDGE_DAPP_ID,
    USDC_TOKEN_ID, WITHDRAWAL_AMOUNT,
)


def deploy_multisig(tracer, gql, *, work_dir: str, msig_key_path: str,
                    usdc_bridge_key_path: str, is_shellnet: bool,
                    verbose_faucet: bool = False):
    """Genaddr → fund → deployx → top-up → mint. Returns `(msig_address, msig_abi_copy)`.

    `verbose_faucet=True` prints tvm-cli output for the giver
    `sendCurrencyWithFlag` calls (useful when diagnosing under-funded
    deploys; noisy for happy-path runs).
    """
    mode = "shellnet" if is_shellnet else "local"
    tracer.log_phase(f"Deploying multisig ({mode})")

    deploy_dir = os.path.join(work_dir, "msig_deploy")
    os.makedirs(deploy_dir, exist_ok=True)
    msig_tvc_copy = os.path.join(deploy_dir, "UpdateCustodianMultisigWallet")
    shutil.copy(f"{MSIG_TVC_STEM}.tvc", f"{msig_tvc_copy}.tvc")
    shutil.copy(MSIG_ABI, f"{msig_tvc_copy}.abi.json")
    msig_abi_copy = f"{msig_tvc_copy}.abi.json"

    if os.path.exists(msig_key_path):
        os.remove(msig_key_path)
    raw_msig_address = common.generate_address(msig_tvc_copy, msig_key_path)
    msig_account_id = raw_msig_address.split(":", 1)[1] if ":" in raw_msig_address else raw_msig_address
    msig_dapp_id = msig_account_id
    msig_address        = f"{msig_dapp_id}::{msig_account_id}"      # CLI / query form
    msig_address_legacy = f"0:{msig_account_id}"                     # ABI payload form
    pubkey = common.read_public_key(msig_key_path)
    tracer.log(f"  multisig address: {msig_address}")

    # Fund ECC[2] (gas + vmshell). Shellnet needs a two-shot pattern with the
    # canonical WALLET_INIT amounts — single-shot empirically leaves the
    # account under-funded for deployx. Local devnet works with one shot.
    total_ecc = WITHDRAWAL_AMOUNT * 4
    if is_shellnet:
        value = 10_000_000_000_000     # canonical WALLET_INIT_BALANCE
        ecc2  = 100_000_000_000_000    # canonical WALLET_INIT_CC
        for shot, flag in enumerate(("17", "1"), start=1):
            tracer.log(f"  faucet shot {shot}/2 (flag={flag}): value={value}, ecc[2]={ecc2}")
            common.call_contract(
                GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
                "sendCurrencyWithFlag",
                {"dest": msig_address_legacy, "value": str(value),
                 "ecc": {str(ECC_ID_FOR_BURN): str(ecc2)},
                 "flag": flag, "bounce": False},
                verbose_faucet,
            )
            time.sleep(3)
    else:
        fund_ecc    = max(total_ecc, 100_000_000_000_000)
        fund_native = 200_000_000_000_000
        tracer.log(f"  funding via giver single-shot (flag=17), "
                   f"native={fund_native}, ecc[{ECC_ID_FOR_BURN}]={fund_ecc}")
        common.call_contract(
            GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
            "sendCurrencyWithFlag",
            {"dest": msig_address_legacy, "value": str(fund_native),
             "ecc": {str(ECC_ID_FOR_BURN): str(fund_ecc)},
             "flag": "17", "bounce": False},
            verbose_faucet,
        )
    time.sleep(8)
    for _ in range(60):
        account = common.get_account(msig_address)
        if 'acc_type' in account:
            tracer.log(f"  account appeared: {account['acc_type']} "
                       f"ecc={account.get('ecc_balance')}")
            break
        time.sleep(1)
    else:
        raise RuntimeError("multisig account never materialized after giver funding")

    constructor_params = {
        "owners_pubkey":   [f"0x{pubkey}"],
        "owners_address":  [],
        "reqConfirms":     1,
        "reqConfirmsData": 1,
        "value":           100_000_000,
    }
    common.execute_cli_cmd(
        f"deployx --abi {msig_abi_copy} --keys {msig_key_path} "
        f"--dst-dapp-id {msig_dapp_id} {msig_tvc_copy}.tvc "
        f"{common.format_params(constructor_params)}",
        True,
    )
    common.wait_account_active(msig_address)
    tracer.log("  multisig deployed and active")

    # Top up ECC[2] if deploy ate into it.
    account = common.get_account(msig_address)
    ecc = account.get("ecc_balance", {}) or {}
    have = int(ecc.get(str(ECC_ID_FOR_BURN), 0))
    if have < total_ecc:
        tracer.log(f"  topping up ECC[{ECC_ID_FOR_BURN}] (have={have}, need={total_ecc})")
        common.call_contract(
            GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
            "sendCurrencyWithFlag",
            {"dest": msig_address_legacy, "value": "2000000000",
             "ecc": {str(ECC_ID_FOR_BURN): str(total_ecc)}, "flag": "1"},
            verbose_faucet,
        )
        time.sleep(5)

    mint_usdc(tracer, gql, msig_address_legacy, WITHDRAWAL_AMOUNT,
              usdc_bridge_key_path=usdc_bridge_key_path)
    return msig_address, msig_abi_copy


def mint_usdc(tracer, gql, msig_address_legacy: str, amount: int, *,
              usdc_bridge_key_path: str):
    """Fund ECC[3] USDC into the multisig via USDCBridge.mintAndSend.

    Resolves the bridge's real on-chain `dapp_id` via GQL (required for
    `callx` routing on shellnet; harmless on local), then polls until
    the credit lands on the multisig (fast on local, ~tens of seconds
    on shellnet).
    """
    real_dapp = gql.fetch_account_dapp_id(USDC_BRIDGE_ACCOUNT_ID, USDC_BRIDGE_DAPP_ID)
    if not real_dapp:
        raise RuntimeError(f"could not resolve USDCBridge dapp_id (acc={USDC_BRIDGE_ACCOUNT_ID})")
    bridge_addr = f"{real_dapp}::{USDC_BRIDGE_ACCOUNT_ID}"

    nonces = common.run_getter(bridge_addr, USDC_BRIDGE_ABI, "getNonces")
    mint_nonce = int(nonces["mintNonce"])
    tracer.log(f"  USDCBridge.mintAndSend → ECC[{USDC_TOKEN_ID}]={amount}, nonce={mint_nonce + 1}")
    common.call_contract(
        bridge_addr, USDC_BRIDGE_ABI, usdc_bridge_key_path,
        "mintAndSend",
        {"recipient": msig_address_legacy,
         "value":     str(amount),
         "nonce":     str(mint_nonce + 1)},
        True,
    )

    msig_account_id = msig_address_legacy.split(":", 1)[1]
    msig_self_dapp = f"{msig_account_id}::{msig_account_id}"
    deadline = time.time() + 120
    while time.time() < deadline:
        account = common.get_account(msig_self_dapp)
        ecc = account.get("ecc_balance", {}) or {}
        have = int(ecc.get(str(USDC_TOKEN_ID), 0))
        if have >= amount:
            tracer.log(f"  ECC[{USDC_TOKEN_ID}] credited: {have}")
            return
        time.sleep(2)
    raise RuntimeError(
        f"USDCBridge.mintAndSend did not credit ECC[{USDC_TOKEN_ID}]={amount} "
        f"to {msig_address_legacy} within 120s"
    )
