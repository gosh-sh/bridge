"""
End-to-end driver for the Acki Nacki → Ethereum bridge Circuit 4
(`WithdrawalInitiated`) proving pipeline. Supports local devnet and
shellnet — select with `MODE` env var (`local` (default) | `shellnet`).

Pipeline (single event):
  deploy + fund multisig → fire `initiateWithdrawal` → capture event
  via GQL → wait for verifier daemon to reach the W-aligned key block
  → run the three Rust bins (private-witness-export, witness-builder,
  halo2-prover) → assert daemon verdict (`verified && anchor_matched`).

Env vars (all optional, defaults are MODE-dependent):
  MODE                  local | shellnet  (default: local)
  PROVER_DIR            default: parent of this script
  NETWORK, GRAPHQL_URL, WORK_DIR
  USDC_BRIDGE_KEY_PATH  override path to the keypair file used to sign
                        `USDCBridge.mintAndSend`. When unset and MODE=local,
                        the bundled `python/contracts/USDCBridge.keys.json`
                        is auto-overwritten from the sibling
                        `acki-nacki/config/USDCBridge.keys.json` (escape
                        hatch for out-of-tree deployments). Either way the
                        orchestrator validates the chosen key against the
                        on-chain `USDCBridge.getOwnerPubkey()` before
                        proceeding, so a stale/wrong key fails fast instead
                        of bouncing later with TVM exit_code 209.
  ACKI_NACKI_ROOT       override sibling acki-nacki checkout (used by both
                        bk_set and USDCBridge key materialization in local
                        mode). Default: `<PROVER_DIR>/../acki-nacki`.

Prereqs (the script does not start these):
  - Live cluster reachable at GRAPHQL_URL.
  - `bridge-verifier-daemon` running in $PROVER_DIR (Track D4a build).
  - $PROVER_DIR/params/ has primary + layer + event VK/PK.
  - Release builds of bridge-event-private-witness-export,
    bridge-event-witness-builder, bridge-event-halo2-prover.

Exit 0 iff daemon accepts the Circuit 4 proof; artefacts kept under
$WORK_DIR on failure.
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
    GIVER_ADDRESS, USDC_BRIDGE_ABI,
    USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    GIVER_ABI, GIVER_KEY_PATH, MSIG_ABI, MSIG_TVC_STEM,
    WITHDRAWAL_AMOUNT, ECC_ID_FOR_BURN, USDC_TOKEN_ID,
    DST_CHAIN_ID, RECIPIENT_HEX,
    W, P, MAX_LAYERS,
)

# ── Mode-dependent configuration ─────────────────────────────────────────────
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
    EVENT_INDEXER_TIMEOUT_S    = 240
    VERIFIER_STATE_TIMEOUT_S   = 2400
    FIRE_WINDOW_WAIT_TIMEOUT_S = 900
else:
    DEFAULT_NETWORK = "http://127.0.0.1:80"
    DEFAULT_GRAPHQL = "http://localhost/graphql"
    DEFAULT_WORK    = "work-local"
    _DEFAULT_USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEYS
    GQL_KWARGS = {}
    EVENT_INDEXER_TIMEOUT_S    = 120
    VERIFIER_STATE_TIMEOUT_S   = 1800
    FIRE_WINDOW_WAIT_TIMEOUT_S = 600

# An explicit override pins the key path (no auto-materialize). Otherwise we
# fall back to the MODE-picked bundled file and — in local mode — refresh it
# from the sibling acki-nacki checkout before use.
USDC_BRIDGE_KEY_PATH_OVERRIDE = os.environ.get("USDC_BRIDGE_KEY_PATH")
USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEY_PATH_OVERRIDE or _DEFAULT_USDC_BRIDGE_KEY_PATH

DAEMON_RESULT_TIMEOUT_S = 600
RUST_BIN_TIMEOUT_S      = 600

PROVER_DIR  = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR    = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, DEFAULT_WORK))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", DEFAULT_GRAPHQL)

MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_withdrawals_e2e.keys.json")

tracer: be.Tracer
gql: be.GqlClient


# ── Deployment ────────────────────────────────────────────────────────────────

def deploy_multisig():
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
    msig_address        = f"{msig_dapp_id}::{msig_account_id}"      # CLI / query form
    msig_address_legacy = f"0:{msig_account_id}"                     # ABI payload form
    pubkey = common.read_public_key(MSIG_KEY_PATH)
    tracer.log(f"  multisig address: {msig_address}")

    # Fund ECC[2] (gas + vmshell). Shellnet needs a two-shot pattern with the
    # canonical WALLET_INIT amounts — single-shot empirically leaves the
    # account under-funded for deployx. Local devnet works with one shot.
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
            )
            time.sleep(3)
    else:
        fund_value = max(total_ecc, 100_000_000_000_000)
        tracer.log(f"  funding via giver ecc[{ECC_ID_FOR_BURN}]={fund_value}")
        common.call_contract(
            GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
            "sendCurrencyWithFlag",
            {"dest": msig_address_legacy, "value": "200000000000000",
             "ecc": {str(ECC_ID_FOR_BURN): str(fund_value)},
             "flag": "17", "bounce": False},
        )
    time.sleep(8)
    for _ in range(60):
        account = common.get_account(msig_address)
        if 'acc_type' in account:
            tracer.log(f"  account appeared: {account['acc_type']} "
                       f"ecc={account.get('ecc_balance')}")
            break
        time.sleep(1)

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
             "ecc": {str(ECC_ID_FOR_BURN): str(total_ecc)}, "flag": "1"}
        )
        time.sleep(5)

    mint_usdc(msig_address_legacy, WITHDRAWAL_AMOUNT)
    return msig_address, msig_abi_copy


def mint_usdc(msig_address_legacy: str, amount: int):
    """Fund ECC[3] USDC into the multisig via USDCBridge.mintAndSend.

    Resolves the bridge's real on-chain `dapp_id` via GQL (required for
    `callx` routing on shellnet; harmless on local), then polls until
    the credit lands on the multisig (fast on local, ~tens of seconds
    on shellnet). The signing key is mode-selected at module load.
    """
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


# ── Local-devnet bk_set materialization ───────────────────────────────────────

def materialize_bk_set_from_node_config():
    """Regenerate `<PROVER_DIR>/bk_set.json` from the running local cluster's
    BLS key files. Daemon-side `bridge_prover_lib::bk_set_fetcher` tries the
    GraphQL `bkSetUpdates` stream first; on a fresh devnet (zero validator
    churn since genesis) that stream returns empty and the daemon falls back
    to `./bk_set.json` (relative to its cwd, which is `PROVER_DIR`).

    For ad-hoc local runs we cannot rely on a committed `bk_set.json` snapshot
    — node images may regenerate BLS keys when rebuilt from scratch. Source
    the set from the same `config/block_keeperN_bls.keys.json` files the node
    containers boot with, enumerated by the running `*-nodeN-*` containers so
    we don't hard-code the topology size.

    Shellnet path uses GraphQL only — never call this when `MODE=shellnet`.

    `ACKI_NACKI_ROOT` env var overrides the default sibling path.
    """
    acki_nacki_root = os.environ.get(
        "ACKI_NACKI_ROOT",
        os.path.abspath(os.path.join(PROVER_DIR, "..", "acki-nacki")),
    )
    config_dir = os.path.join(acki_nacki_root, "config")
    try:
        names = subprocess.run(
            ["docker", "ps", "--format", "{{.Names}}"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        raise RuntimeError(
            f"failed to enumerate docker containers: {e}. "
            "the local devnet cluster must be running before this test"
        ) from e

    pat = re.compile(r"-node(\d+)-\d+$")
    indices = sorted({
        int(m.group(1))
        for line in names.splitlines()
        for m in [pat.search(line.strip())]
        if m
    })
    if not indices:
        raise RuntimeError(
            "no `*-nodeN-*` containers detected via `docker ps`; "
            "start the cluster (e.g. `make run`) before this test"
        )

    bk_set = {}
    for idx in indices:
        path = os.path.join(config_dir, f"block_keeper{idx}_bls.keys.json")
        if not os.path.isfile(path):
            raise FileNotFoundError(
                f"BLS key file missing: {path} (needed for node{idx})"
            )
        with open(path) as f:
            data = json.load(f)
        try:
            pub = data[0]["public"]
        except (IndexError, KeyError, TypeError) as e:
            raise ValueError(f"unexpected format in {path}: {e}") from e
        bk_set[str(idx)] = pub

    out_path = os.path.join(PROVER_DIR, "bk_set.json")
    with open(out_path, "w") as f:
        json.dump(bk_set, f, indent=2)
    tracer.log(f"  bk_set.json: {len(bk_set)} signers (indices {indices}) "
               f"sourced from {config_dir}")
    tracer.log(f"  wrote {out_path}")


# ── Local-devnet USDCBridge owner key materialization & validation ────────────

def materialize_usdc_bridge_key_from_node_config():
    """Overwrite the bundled `python/contracts/USDCBridge.keys.json` with the
    in-tree `acki-nacki/config/USDCBridge.keys.json` that the local devnet
    actually deployed the contract with.

    The bundled key is shipped only so `python/` can run drop-in without a
    hard dep on a sibling checkout. On a freshly-rebuilt devnet the in-tree
    key can drift from the bundled one — when that happens
    `USDCBridge.mintAndSend` silently bounces (TVM exit_code 209) and the
    orchestrator deadlocks waiting for the ECC[3] credit.

    Only called when MODE=local and the user did not pin a path via
    `USDC_BRIDGE_KEY_PATH`. Honors `ACKI_NACKI_ROOT` for sibling location.
    """
    acki_nacki_root = os.environ.get(
        "ACKI_NACKI_ROOT",
        os.path.abspath(os.path.join(PROVER_DIR, "..", "acki-nacki")),
    )
    src = os.path.join(acki_nacki_root, "config", "USDCBridge.keys.json")
    if not os.path.isfile(src):
        raise FileNotFoundError(
            f"USDCBridge owner key missing at {src}; the local cluster's "
            f"acki-nacki checkout was expected at {acki_nacki_root} "
            "(override with ACKI_NACKI_ROOT, or pin USDC_BRIDGE_KEY_PATH)"
        )
    if os.path.realpath(src) == os.path.realpath(USDC_BRIDGE_KEY_PATH):
        tracer.log(f"  USDCBridge key already points at {src} — skip copy")
        return
    shutil.copyfile(src, USDC_BRIDGE_KEY_PATH)
    tracer.log(f"  copied {src}")
    tracer.log(f"       → {USDC_BRIDGE_KEY_PATH}")


def validate_usdc_bridge_key():
    """Cross-check the local USDCBridge keypair against the on-chain owner
    pubkey via `USDCBridge.getOwnerPubkey()`. Failure here would otherwise
    surface much later as an opaque 120s timeout waiting for the ECC[3]
    credit, with `exit_code 209` only visible in node logs.
    """
    try:
        with open(USDC_BRIDGE_KEY_PATH) as f:
            local_pub_hex = json.load(f)["public"].lower()
    except (OSError, KeyError, json.JSONDecodeError) as e:
        raise RuntimeError(
            f"cannot read local USDCBridge key at {USDC_BRIDGE_KEY_PATH}: {e}"
        ) from e

    raw = common.run_getter(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, "getOwnerPubkey")
    if not isinstance(raw, dict) or "value0" not in raw:
        raise RuntimeError(
            f"USDCBridge.getOwnerPubkey returned unexpected payload: {raw!r}"
        )
    on_chain_pub_int = int(str(raw["value0"]), 0)
    on_chain_pub_hex = f"{on_chain_pub_int:064x}"

    if on_chain_pub_hex != local_pub_hex:
        hint = (
            "set USDC_BRIDGE_KEY_PATH to the keypair the contract was "
            "deployed with"
            if USDC_BRIDGE_KEY_PATH_OVERRIDE is not None
            else "rerun against a fresh local devnet, or unset "
                 "USDC_BRIDGE_KEY_PATH to let the orchestrator auto-sync"
        )
        raise RuntimeError(
            "USDCBridge owner key mismatch — mintAndSend would bounce with "
            f"TVM exit_code 209.\n"
            f"  local  ({USDC_BRIDGE_KEY_PATH}): {local_pub_hex}\n"
            f"  on-chain (getOwnerPubkey):       {on_chain_pub_hex}\n"
            f"  hint: {hint}"
        )
    tracer.log(f"  USDCBridge owner key OK ({local_pub_hex[:16]}…)")


# ── Driver ────────────────────────────────────────────────────────────────────

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
    assert os.path.isdir(PROVER_DIR), f"PROVER_DIR not found: {PROVER_DIR}"
    tracer.log(f"  NETWORK:     {network}")
    tracer.log(f"  PROVER_DIR:  {PROVER_DIR}")
    tracer.log(f"  GRAPHQL_URL: {GRAPHQL_URL}")
    tracer.log(f"  WORK_DIR:    {WORK_DIR}")
    tracer.log(f"  W = {W}, P = {P} (bundle = {W*P} blocks), MAX_LAYERS = {MAX_LAYERS}")
    assert common.is_account_active(USDC_BRIDGE_ADDRESS), \
        f"USDCBridge not active at {USDC_BRIDGE_ADDRESS}"
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    if not IS_SHELLNET:
        tracer.log_phase("Materializing bk_set.json from local cluster config")
        materialize_bk_set_from_node_config()
        if USDC_BRIDGE_KEY_PATH_OVERRIDE is None:
            tracer.log_phase("Materializing USDCBridge.keys.json from local cluster config")
            materialize_usdc_bridge_key_from_node_config()

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    tracer.log(f"  USDC_BRIDGE_KEY_PATH: {USDC_BRIDGE_KEY_PATH}"
               + (" (user-overridden)" if USDC_BRIDGE_KEY_PATH_OVERRIDE else ""))
    validate_usdc_bridge_key()

    baseline = gql.fetch_bridge_extouts(limit=500)
    baseline_ids = {n["id"] for n in baseline}
    tracer.log(f"  baseline ExtOut messages from USDCBridge: {len(baseline_ids)}")

    msig_address, msig_abi = deploy_multisig()

    be.wait_for_fire_window(tracer, gql, FIRE_WINDOW_WAIT_TIMEOUT_S)

    tracer.log_phase("Dispatching initiateWithdrawal")
    tracer.log(f"  dstChainId={DST_CHAIN_ID}, recipient=0x{RECIPIENT_HEX}, "
               f"amount={WITHDRAWAL_AMOUNT}, tokenId={USDC_TOKEN_ID}")
    call_result = be.call_initiate_withdrawal(
        msig_address, msig_abi, MSIG_KEY_PATH, DST_CHAIN_ID, RECIPIENT_HEX
    )
    if not common.is_ok(call_result):
        tracer.log(f"  call result (may still emit asynchronously): {call_result}")

    meta = be.capture_event_metadata(tracer, gql, baseline_ids, EVENT_INDEXER_TIMEOUT_S)
    tracer.log("  captured metadata:")
    for k, v in meta.items():
        tracer.log(f"    {k}: {v}")

    event_seq = meta["block_seq_no"]
    key_block_seq, thinned_kb_seq, target_seq = be.compute_target_seq(event_seq)
    tracer.log_phase("Boundary math")
    tracer.log(f"  event_seq        = {event_seq}")
    tracer.log(f"  key_block_seq    = {key_block_seq}  (W-aligned L1 tree the event lives in)")
    tracer.log(f"  thinned_kb_seq   = {thinned_kb_seq}  (verifier-stored L1 root anchor)")
    tracer.log(f"  target_seq       = {target_seq}    (verifier must reach this seq_no)")
    if key_block_seq != thinned_kb_seq:
        raise RuntimeError(
            f"event landed in wrong W-window: key_block_seq={key_block_seq} != "
            f"thinned_kb_seq={thinned_kb_seq}. Re-run the test."
        )

    be.wait_for_verifier_state(tracer, gql, PROVER_DIR, target_seq, VERIFIER_STATE_TIMEOUT_S)

    pipeline = be.run_event_proving_steps(
        tracer, PROVER_DIR, WORK_DIR, GRAPHQL_URL, meta,
        rust_bin_timeout_s=RUST_BIN_TIMEOUT_S,
        daemon_result_timeout_s=DAEMON_RESULT_TIMEOUT_S,
        seq_no=0,
    )
    result = pipeline["result"]
    seq_no = pipeline["seq_no"]
    work_event_dir = pipeline["work_event_dir"]
    proofs_dir = pipeline["proofs_dir"]

    tracer.log_phase("Daemon verdict")
    tracer.log(json.dumps(result, indent=2))

    verified       = result.get("verified") is True
    anchor_matched = result.get("anchor_matched") is True
    if not anchor_matched:
        tracer.log(f"  ANCHOR MISMATCH: {result.get('error')}")
    if not verified:
        tracer.log(f"  VERIFICATION FAILED: {result.get('error')}")

    if verified and anchor_matched:
        tracer.log_phase("END-TO-END SUCCESS")
        tracer.log(f"  daemon verified Circuit 4 proof for event in block "
                   f"seq_no={event_seq} at verifier height "
                   f"{result.get('verified_at_block_height')}")
        tracer.log(f"  artefacts: {work_event_dir} + {proofs_dir}/proof_event_{seq_no:06d}.json")
        sys.exit(0)
    else:
        tracer.log_phase("END-TO-END FAILURE")
        tracer.log(f"  artefacts kept for diagnosis under {work_event_dir}")
        sys.exit(1)

if __name__ == "__main__":
    main()
