"""
Isolated contract-only sanity test for the AN→ETH bridge (LOCAL DEVNET + SHELLNET).

Distilled from `generate_withdrawals_with_live_event_proving.py`, but strips
out everything downstream of event emission — no verifier/prover daemons, no
Rust binaries, no halo2 proving. Purely tvm-cli + GraphQL.

Purpose: prove the multisig deploy + USDC mint + initiateWithdrawal + emitted
WithdrawalInitiated event path works on the target cluster BEFORE running
the heavy Circuit 4 pipeline. If this test fails, the full orchestrator
cannot succeed either — this is the cheap fast-fail signal.

Modes (env `MODE`, default `local`):
  local     - local devnet cluster, keys auto-materialized from
              `$ACKI_NACKI_ROOT/config/`
  shellnet  - live https://shellnet.ackinacki.org, no local config
              auto-refresh. Assumes `python/contracts/USDCBridge.shellnet.keys.json`
              has already been refreshed from the partner-posted `config-*/`
              directory after any shellnet redeploy (see TECHNICAL_README
              §"Shellnet key-refresh checklist").

Steps:
  1. Prechecks: USDCBridge active; USDCBridge owner key matches on-chain.
  2. (local only) Materialize BK-set + USDCBridge owner key from sibling
     acki-nacki checkout.
  3. Deploy multisig via GiverV3.sendCurrencyWithFlag. Local: single-shot
     flag=17 (Sehor `tests/exchange/bridge_e2e_self_contained.py`). Shellnet:
     two-shot 17→1 with canonical WALLET_INIT amounts (matches the full
     shellnet orchestrator).
  4. Mint ECC[3] USDC into the multisig via USDCBridge.mintAndSend.
  5. Fire USDCBridge.initiateWithdrawal from the multisig.
  6. Poll USDCBridge ExtOut messages via GQL for a new WithdrawalInitiated
     event (matching `dst == WITHDRAWAL_EVENT_DST` and not in baseline).
  7. PASS iff an event lands within EVENT_INDEXER_TIMEOUT_S; FAIL otherwise.

Env vars: same subset the full orchestrator honors —
  MODE (local|shellnet), PROVER_DIR, NETWORK, GRAPHQL_URL, WORK_DIR,
  USDC_BRIDGE_KEY_PATH, ACKI_NACKI_ROOT.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.bridge_e2e import (
    USDC_BRIDGE_ADDRESS, USDC_BRIDGE_DAPP_ID, USDC_BRIDGE_ACCOUNT_ID,
    GIVER_ADDRESS, USDC_BRIDGE_ABI, USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    GIVER_ABI, GIVER_KEY_PATH, MSIG_ABI, MSIG_TVC_STEM,
    WITHDRAWAL_AMOUNT, ECC_ID_FOR_BURN, USDC_TOKEN_ID,
    DST_CHAIN_ID, RECIPIENT_HEX,
)

# ── Mode-dependent configuration (mirrors full orchestrator) ────────────────
MODE = os.environ.get("MODE", "local").lower()
if MODE not in ("local", "shellnet"):
    raise SystemExit(f"MODE must be 'local' or 'shellnet', got {MODE!r}")
IS_SHELLNET = MODE == "shellnet"

if IS_SHELLNET:
    DEFAULT_NETWORK = "shellnet.ackinacki.org"
    DEFAULT_GRAPHQL = "https://shellnet.ackinacki.org/graphql"
    DEFAULT_WORK    = "work-shellnet"
    _DEFAULT_USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEYS_SHELLNET
    GQL_KWARGS = {"user_agent": "bridge-e2e-orchestrator-shellnet/1.0",
                  "timeout": 30}
    EVENT_INDEXER_TIMEOUT_S = 240
else:
    DEFAULT_NETWORK = "http://127.0.0.1:80"
    DEFAULT_GRAPHQL = "http://localhost/graphql"
    DEFAULT_WORK    = "work-local"
    _DEFAULT_USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEYS
    GQL_KWARGS = {}
    EVENT_INDEXER_TIMEOUT_S = 180

PROVER_DIR  = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR    = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, DEFAULT_WORK))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", DEFAULT_GRAPHQL)
USDC_BRIDGE_KEY_PATH_OVERRIDE = os.environ.get("USDC_BRIDGE_KEY_PATH")
USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEY_PATH_OVERRIDE or _DEFAULT_USDC_BRIDGE_KEY_PATH

MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_withdrawals_e2e.keys.json")

tracer: be.Tracer
gql: be.GqlClient


# ── Deployment ──────────────────────────────────────────────────────────────

def deploy_multisig():
    """Fund + deploy multisig, using the mode-appropriate giver pattern.

    Local devnet: Sehor single-shot flag=17 (`bridge_e2e_self_contained.py`).
    Shellnet: canonical two-shot 17→1 with WALLET_INIT amounts (matches the
    full shellnet orchestrator; single-shot empirically leaves the account
    under-funded for deployx on shellnet).
    """
    tracer.log_phase(f"Deploying multisig ({MODE})")

    work_dir = os.path.join(WORK_DIR, "msig_deploy")
    os.makedirs(work_dir, exist_ok=True)
    msig_tvc_copy = os.path.join(work_dir, "UpdateCustodianMultisigWallet")
    shutil.copy(f"{MSIG_TVC_STEM}.tvc", f"{msig_tvc_copy}.tvc")
    shutil.copy(MSIG_ABI, f"{msig_tvc_copy}.abi.json")
    msig_abi_copy = f"{msig_tvc_copy}.abi.json"

    if os.path.exists(MSIG_KEY_PATH):
        os.remove(MSIG_KEY_PATH)
    raw_msig_address = common.generate_address(msig_tvc_copy, MSIG_KEY_PATH)
    msig_account_id = raw_msig_address.split(":", 1)[1] if ":" in raw_msig_address else raw_msig_address
    msig_dapp_id    = msig_account_id
    msig_address        = f"{msig_dapp_id}::{msig_account_id}"
    msig_address_legacy = f"0:{msig_account_id}"
    pubkey = common.read_public_key(MSIG_KEY_PATH)
    tracer.log(f"  multisig address: {msig_address}")

    total_ecc = WITHDRAWAL_AMOUNT * 4
    if IS_SHELLNET:
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
                True,
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
            True,
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
        f"deployx --abi {msig_abi_copy} --keys {MSIG_KEY_PATH} "
        f"--dst-dapp-id {msig_dapp_id} {msig_tvc_copy}.tvc "
        f"{common.format_params(constructor_params)}",
        True,
    )
    common.wait_account_active(msig_address)
    tracer.log("  multisig deployed and active")

    # Conditional top-up if deploy consumed ECC[2].
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
            True,
        )
        time.sleep(5)

    mint_usdc(msig_address_legacy, WITHDRAWAL_AMOUNT)
    return msig_address, msig_abi_copy


def mint_usdc(msig_address_legacy: str, amount: int):
    """Fund ECC[3] USDC into the multisig via USDCBridge.mintAndSend."""
    real_dapp = gql.fetch_account_dapp_id(USDC_BRIDGE_ACCOUNT_ID, USDC_BRIDGE_DAPP_ID)
    if not real_dapp:
        raise RuntimeError(f"could not resolve USDCBridge dapp_id (acc={USDC_BRIDGE_ACCOUNT_ID})")
    bridge_addr = f"{real_dapp}::{USDC_BRIDGE_ACCOUNT_ID}"

    nonces = common.run_getter(bridge_addr, USDC_BRIDGE_ABI, "getNonces")
    mint_nonce = int(nonces["mintNonce"])
    tracer.log(f"  USDCBridge.mintAndSend → ECC[{USDC_TOKEN_ID}]={amount}, nonce={mint_nonce + 1}")
    common.call_contract(
        bridge_addr, USDC_BRIDGE_ABI, USDC_BRIDGE_KEY_PATH,
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


# ── Local-devnet config materialization (copied from full orchestrator) ─────

def materialize_bk_set_from_node_config():
    """Regenerate bk_set.json from live cluster BLS keys. Not strictly needed
    for this test (no daemons), but keeping it makes the failure surface
    identical to the full orchestrator's prechecks."""
    acki_nacki_root = os.environ.get(
        "ACKI_NACKI_ROOT",
        os.path.abspath(os.path.join(PROVER_DIR, "..", "acki-nacki")),
    )
    config_dir = os.path.join(acki_nacki_root, "config")
    names = subprocess.run(
        ["docker", "ps", "--format", "{{.Names}}"],
        capture_output=True, text=True, check=True,
    ).stdout
    pat = re.compile(r"-node(\d+)-\d+$")
    indices = sorted({
        int(m.group(1))
        for line in names.splitlines()
        for m in [pat.search(line.strip())]
        if m
    })
    if not indices:
        raise RuntimeError("no *-nodeN-* containers detected via docker ps")
    bk_set = {}
    for idx in indices:
        path = os.path.join(config_dir, f"block_keeper{idx}_bls.keys.json")
        if not os.path.isfile(path):
            raise FileNotFoundError(f"BLS key file missing: {path}")
        with open(path) as f:
            data = json.load(f)
        bk_set[str(idx)] = data[0]["public"]
    out_path = os.path.join(PROVER_DIR, "bk_set.json")
    with open(out_path, "w") as f:
        json.dump(bk_set, f, indent=2)
    tracer.log(f"  bk_set.json: {len(bk_set)} signers (indices {indices})")


def materialize_usdc_bridge_key_from_node_config():
    acki_nacki_root = os.environ.get(
        "ACKI_NACKI_ROOT",
        os.path.abspath(os.path.join(PROVER_DIR, "..", "acki-nacki")),
    )
    src = os.path.join(acki_nacki_root, "config", "USDCBridge.keys.json")
    if not os.path.isfile(src):
        raise FileNotFoundError(f"USDCBridge owner key missing at {src}")
    if os.path.realpath(src) == os.path.realpath(USDC_BRIDGE_KEY_PATH):
        tracer.log(f"  USDCBridge key already points at {src} — skip copy")
        return
    shutil.copyfile(src, USDC_BRIDGE_KEY_PATH)
    tracer.log(f"  copied {src} → {USDC_BRIDGE_KEY_PATH}")


def validate_usdc_bridge_key():
    with open(USDC_BRIDGE_KEY_PATH) as f:
        local_pub_hex = json.load(f)["public"].lower()
    raw = common.run_getter(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, "getOwnerPubkey")
    on_chain_pub_int = int(str(raw["value0"]), 0)
    on_chain_pub_hex = f"{on_chain_pub_int:064x}"
    if on_chain_pub_hex != local_pub_hex:
        raise RuntimeError(
            f"USDCBridge owner key mismatch — local {local_pub_hex} vs "
            f"on-chain {on_chain_pub_hex}"
        )
    tracer.log(f"  USDCBridge owner key OK ({local_pub_hex[:16]}…)")


# ── Event polling (subset of bridge_e2e.capture_event_metadata) ─────────────

def wait_for_withdrawal_event(baseline_ids: set, timeout_s: int) -> dict | None:
    """Poll USDCBridge ExtOut messages until a new WithdrawalInitiated event
    surfaces. Returns the raw GQL node dict on success, None on timeout."""
    tracer.log_phase("Waiting for emitted WithdrawalInitiated event")
    deadline = time.time() + timeout_s
    last_count = -1
    while time.time() < deadline:
        nodes = gql.fetch_bridge_extouts(limit=500)
        matched = [
            n for n in nodes
            if n.get("dst") == be.WITHDRAWAL_EVENT_DST
            and n.get("id") not in baseline_ids
        ]
        if len(matched) != last_count:
            tracer.log(f"  matched={len(matched)} new event(s)")
            last_count = len(matched)
        if matched:
            matched.sort(key=lambda n: n.get("created_at") or 0)
            return matched[-1]
        time.sleep(2)
    return None


# ── Driver ──────────────────────────────────────────────────────────────────

def main():
    global tracer, gql
    tracer = be.Tracer()
    gql = be.GqlClient(GRAPHQL_URL, **GQL_KWARGS)
    os.makedirs(WORK_DIR, exist_ok=True)

    network = os.getenv("NETWORK", DEFAULT_NETWORK)
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    tracer.log_phase(f"Prechecks ({MODE})")
    tracer.log(f"  NETWORK:     {network}")
    tracer.log(f"  PROVER_DIR:  {PROVER_DIR}")
    tracer.log(f"  GRAPHQL_URL: {GRAPHQL_URL}")
    tracer.log(f"  WORK_DIR:    {WORK_DIR}")
    tracer.log(f"  USDC_BRIDGE_KEY_PATH: {USDC_BRIDGE_KEY_PATH}")
    assert common.is_account_active(USDC_BRIDGE_ADDRESS), \
        f"USDCBridge not active at {USDC_BRIDGE_ADDRESS}"
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    # bk_set: local devnet fetches from GraphQL AND falls back to bk_set.json.
    # The daemons handle this themselves — the isolated test does not run
    # daemons, so we skip materialization here regardless of MODE.
    if not IS_SHELLNET and USDC_BRIDGE_KEY_PATH_OVERRIDE is None:
        tracer.log_phase("Materializing USDCBridge.keys.json from local cluster config")
        materialize_usdc_bridge_key_from_node_config()

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    validate_usdc_bridge_key()

    baseline = gql.fetch_bridge_extouts(limit=500)
    baseline_ids = {n["id"] for n in baseline}
    tracer.log(f"  baseline ExtOut messages from USDCBridge: {len(baseline_ids)}")

    msig_address, msig_abi = deploy_multisig()

    tracer.log_phase("Dispatching initiateWithdrawal")
    tracer.log(f"  dstChainId={DST_CHAIN_ID}, recipient=0x{RECIPIENT_HEX}, "
               f"amount={WITHDRAWAL_AMOUNT}, tokenId={USDC_TOKEN_ID}")
    call_result = be.call_initiate_withdrawal(
        msig_address, msig_abi, MSIG_KEY_PATH, DST_CHAIN_ID, RECIPIENT_HEX
    )
    if not common.is_ok(call_result):
        tracer.log(f"  call result (may still emit asynchronously): {call_result}")

    event = wait_for_withdrawal_event(baseline_ids, EVENT_INDEXER_TIMEOUT_S)
    if event is None:
        tracer.log_phase("FAIL")
        tracer.log("  No WithdrawalInitiated event observed within "
                   f"{EVENT_INDEXER_TIMEOUT_S}s")
        sys.exit(1)

    tracer.log_phase("PASS — WithdrawalInitiated event observed")
    tracer.log(f"  message id:              {event['id']}")
    tracer.log(f"  src_transaction.block_id (== Block.hash): {event['block_id']}")
    tracer.log(f"  src_dapp_id:             {event.get('src_dapp_id')}")
    tracer.log(f"  dst:                     {event['dst']}")


if __name__ == "__main__":
    main()
