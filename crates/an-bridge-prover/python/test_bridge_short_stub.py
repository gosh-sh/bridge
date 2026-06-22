"""
test_bridge_short_stub.py — STUB / placeholder for the future
production withdrawal flow against a real Ethereum bridge contract.

This script is a thin trim of
`generate_withdrawals_with_live_event_proving.py`. It performs the
**on-chain Acki Nacki side** of a single withdrawal end-to-end and stops
the moment the Circuit 4 ZK proof of the `WithdrawalInitiated` event has
been generated and self-verified.

What this stub does NOT do (intentionally):
  * It does NOT hand the proof to `bridge-verifier-daemon`. The off-chain
    verifier daemon was a modelling tool used to prototype the contract
    side; once the real Ethereum bridge contract is live, this step is
    replaced by an on-chain `proveWithdrawal(...)` transaction.
  * It does NOT submit the proof to the Ethereum bridge contract yet.
    A separate submitter (script or Rust binary, TBD) will be supplied
    later. Until it lands, the proof is dropped in the per-run work
    directory for manual inspection.

╔══════════════════════════════════════════════════════════════════════════╗
║ ⚠️  TODOs (read before extending this stub)                              ║
╠══════════════════════════════════════════════════════════════════════════╣
║                                                                          ║
║  1. After Step 7 (proof produced), the proof JSON must be submitted to   ║
║     the Ethereum bridge contract via a `proveWithdrawal(proof, public)`  ║
║     transaction. A submitter (web3.py script or a Rust binary that       ║
║     wraps ethers-rs) will be added later. Its inputs are:                ║
║         - the 10 public instances from `proof_event_NNN.json`            ║
║         - the raw KZG proof bytes                                        ║
║         - the recipient address (already inside public instances)        ║
║     Until then, copy the file under `WORK_DIR/event_NNNNNN/` somewhere   ║
║     the submitter can read.                                              ║
║                                                                          ║
║  2. `bridge-event-witness-builder` is NOT in its final shape. The        ║
║     current build still produces a witness anchored against              ║
║     `state/verifier_state.json` (the off-chain mirror written by         ║
║     `bridge-verifier-daemon`). It will be rewritten so the anchor is     ║
║     pulled from the **Ethereum bridge contract's `layerWindows`**        ║
║     storage instead — at which point neither `bridge-verifier-daemon`    ║
║     nor its state file is on the production path.                        ║
║                                                                          ║
║  3. `bridge-prover-daemon` today only writes proofs for the modelling    ║
║     verifier daemon to consume. An alternative daemon (or an extra       ║
║     submission mode on the same daemon) will be added that posts the     ║
║     same Circuit 1A + Circuit 2 proofs to the Ethereum bridge contract   ║
║     via `verifyBlock(...)`. That work is independent of this stub.       ║
║                                                                          ║
║  ✅ `bridge-event-private-witness-export` and                            ║
║     `bridge-event-halo2-prover` are already in their final shape —       ║
║     neither needs changes for the on-chain flow.                         ║
║                                                                          ║
╚══════════════════════════════════════════════════════════════════════════╝

Pipeline (Steps 1-7 from the original orchestrator, then STOP):

  1. Deploy a fresh multisig + fund it with ECC[2] via the giver.
  2. Wait for the W·P "fire window" so the event lands in the right
     bundle slot (witness-builder constraint — will go away with the
     contract-side rewrite).
  3. Call `USDCBridge.initiateWithdrawal(dstChainId, recipient)` from
     the multisig. The contract burns ECC and emits
     `WithdrawalInitiated` as an ExtOut message.
  4. Capture event metadata via GraphQL: BOC, block_id, seq_no,
     envelope_hash, dapp + account.
  5. `bridge-event-private-witness-export` → partial.json (READY).
  6. `bridge-event-witness-builder`        → witness.json (STUB — see TODO #2).
  7. `bridge-event-halo2-prover`           → proof_event_NNN.json under
     WORK_DIR. Self-verified by the prover itself (READY).

  STOP HERE. Next step is TODO #1 (submit to Ethereum).

Prerequisites (script does not start any of these):
  * Acki Nacki node reachable at `NETWORK` + `GRAPHQL_URL`.
  * `tvm-cli` on PATH (or via `CLI_NAME`). A bundled binary lives at
    `python/bin/tvm-cli` and is prepended to PATH automatically.
  * Release builds of three event binaries under
    `$PROVER_DIR/target/release/` — see README.md for the build line.
  * For Step 6 to succeed today: `bridge-verifier-daemon` running and
    its `state/verifier_state.json` advanced past the event's bundle.
    This requirement disappears with TODO #2.
  * `$PROVER_DIR/params/event_*.{bin,json}` (auto-generated on first
    prover run; ~3 GB, slow).

Run (shellnet):
  NETWORK=shellnet.ackinacki.org \
      GRAPHQL_URL=https://shellnet.ackinacki.org/graphql \
      python3 python/test_bridge_short_stub.py

Run (local devnet):
  python3 python/test_bridge_short_stub.py

Exit code:
  0 if a Circuit 4 proof was produced and self-verified.
  non-zero on any pipeline failure. Artefacts left under
  `$WORK_DIR/event_NNNNNN/` for forensics.
"""

import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)

# Bundled `tvm-cli` lives under `python/bin/` — prepend to PATH so
# `helper.common`'s `shutil.which("tvm-cli")` resolves it. Override with
# `CLI_NAME` env var to point at a different binary.
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common

# ── Addresses ─────────────────────────────────────────────────────────────────
# tvm-cli v3 requires `<dapp_id>::<account_id>` for CLI args / `account` queries.
# ABI payload `address` fields (e.g. `dest`) still take legacy `0:<acc_id>`.
USDC_BRIDGE_ADDRESS_LEGACY = "0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
GIVER_ADDRESS_LEGACY       = "0:1111111111111111111111111111111111111111111111111111111111111111"

USDC_BRIDGE_ADDRESS = common.to_dapp_address(USDC_BRIDGE_ADDRESS_LEGACY)
GIVER_ADDRESS       = common.to_dapp_address(GIVER_ADDRESS_LEGACY)

# Split for v3 GQL `blockchain.account(account_id:, dapp_id:)`.
USDC_BRIDGE_DAPP_ID, USDC_BRIDGE_ACCOUNT_ID = USDC_BRIDGE_ADDRESS.split("::", 1)

# ── ABIs / TVCs (bundled under python/contracts/) ─────────────────────────────
_CONTRACTS = os.path.join(_HERE, "contracts")
USDC_BRIDGE_ABI  = os.path.join(_CONTRACTS, "USDCBridge.abi.json")
GIVER_ABI        = os.path.join(_CONTRACTS, "GiverV3.abi.json")
GIVER_KEY_PATH   = os.path.join(_CONTRACTS, "GiverV3.keys.json")
MSIG_ABI         = os.path.join(_CONTRACTS, "UpdateCustodianMultisigWallet.abi.json")
MSIG_TVC_STEM    = os.path.join(_CONTRACTS, "UpdateCustodianMultisigWallet")

# ── Withdrawal parameters ─────────────────────────────────────────────────────
WITHDRAWAL_AMOUNT  = 1_000_000
ECC_ID_FOR_BURN    = 2   # Shell (giver has it at genesis)
DST_CHAIN_ID       = 1
RECIPIENT_HEX      = "742d35cc6634c0532925a3b844bc454e4438f44e"

# External-address dst for the WithdrawalInitiated event:
#   makeAddrExtern(WithdrawalInitiatedEmit=618=0x26a, bitCntAddress=256)
WITHDRAWAL_EVENT_DST = ":000000000000000000000000000000000000000000000000000000000000026a"

# ── Bridge / verifier paths ───────────────────────────────────────────────────
PROVER_DIR  = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR    = os.environ.get("WORK_DIR", "/tmp/bridge-e2e-stub")
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", "http://localhost/graphql")

MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_withdrawals_stub.keys.json")

# ── History-window math constants (still W·P-aligned today — see TODO #2) ─────
W           = 128
P           = 4
MAX_LAYERS  = 10

# ── Timeouts (seconds) ────────────────────────────────────────────────────────
EVENT_INDEXER_TIMEOUT_S        = 120
VERIFIER_STATE_TIMEOUT_S       = 1800
RUST_BIN_TIMEOUT_S             = 600
FIRE_WINDOW_WAIT_TIMEOUT_S     = 600

# ── Tracing ───────────────────────────────────────────────────────────────────
_T0 = time.time()


def t_prefix():
    elapsed = int(time.time() - _T0)
    m, s = divmod(elapsed, 60)
    return f"[T+{m:02d}:{s:02d}]"


def log_phase(msg):
    print(f"\n{t_prefix()} === {msg} ===", flush=True)


def log(msg):
    print(f"{t_prefix()} {msg}", flush=True)


# ── GraphQL helpers ───────────────────────────────────────────────────────────

def _gql(query: str):
    req = urllib.request.Request(
        GRAPHQL_URL,
        data=json.dumps({"query": query}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode())


def fetch_bridge_extouts(limit: int = 500):
    # v3 GQL schema: `blockchain.account(account_id:, dapp_id:)`.
    q = f'''{{
      blockchain {{
        account(account_id: "{USDC_BRIDGE_ACCOUNT_ID}", dapp_id: "{USDC_BRIDGE_DAPP_ID}") {{
          messages(msg_type: [ExtOut], last: {limit}) {{
            edges {{ node {{
              id boc body src dst created_at
              block_id src_dapp_id
              src_transaction {{ id block_id }}
            }} }}
          }}
        }}
      }}
    }}'''
    data = _gql(q)
    edges = (data.get("data") or {}).get("blockchain", {}).get("account", {}) \
        .get("messages", {}).get("edges", []) or []
    nodes = []
    for e in edges:
        n = e["node"]
        if not n.get("block_id"):
            tx = n.get("src_transaction") or {}
            tx_block_id = tx.get("block_id") if isinstance(tx, dict) else None
            if tx_block_id:
                n["block_id"] = tx_block_id
        nodes.append(n)
    return nodes


def fetch_account_dapp_id(account_id: str, dapp_id: str) -> str:
    # v3 GQL takes split account_id / dapp_id.
    q = f'''{{ blockchain {{
        account(account_id: "{account_id}", dapp_id: "{dapp_id}") {{
            info {{ dapp_id }}
        }}
    }} }}'''
    data = _gql(q)
    info = (data.get("data") or {}).get("blockchain", {}).get("account", {}).get("info") or {}
    return info.get("dapp_id") or ""


def find_block_by_hash(block_hash: str):
    q = f'''{{
      blockchain {{
        block(hash: "{block_hash}") {{
          hash block_id seq_no height envelope_hash key_block
        }}
      }}
    }}'''
    data = _gql(q)
    return (data.get("data") or {}).get("blockchain", {}).get("block")


def fetch_latest_block_seq_no() -> int:
    q = '{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }'
    try:
        data = _gql(q)
        edges = data["data"]["blockchain"]["blocks"]["edges"]
        return int(edges[-1]["node"]["seq_no"])
    except Exception:
        return -1


# ── tvm-cli body encoding ─────────────────────────────────────────────────────

def encode_initiate_withdrawal_body(dst_chain_id: int, recipient_hex: str) -> str:
    params = json.dumps({
        "dstChainId": str(dst_chain_id),
        "recipient":  recipient_hex,
    })
    cmd = (f"{common.TVM_CLI} -j body --abi {USDC_BRIDGE_ABI} "
           f"initiateWithdrawal '{params}'")
    out = subprocess.check_output(cmd, shell=True, stderr=subprocess.STDOUT).decode().strip()
    return json.loads(out)["Message"]


# ── Deployment ────────────────────────────────────────────────────────────────

def deploy_multisig():
    log_phase("Deploying multisig")

    work_dir = "/tmp/msig_withdrawals_stub"
    os.makedirs(work_dir, exist_ok=True)
    msig_tvc_copy = os.path.join(work_dir, "UpdateCustodianMultisigWallet")
    shutil.copy(f"{MSIG_TVC_STEM}.tvc", f"{msig_tvc_copy}.tvc")
    shutil.copy(MSIG_ABI, f"{msig_tvc_copy}.abi.json")
    msig_abi_copy = f"{msig_tvc_copy}.abi.json"

    if os.path.exists(MSIG_KEY_PATH):
        os.remove(MSIG_KEY_PATH)
    raw_msig_address = common.generate_address(msig_tvc_copy, MSIG_KEY_PATH)
    # Self-rooted deploy: dapp_id == account_id (tvm-cli v3 requires
    # --dst-dapp-id on every deployx).
    msig_account_id = raw_msig_address.split(":", 1)[1] if ":" in raw_msig_address else raw_msig_address
    msig_dapp_id    = msig_account_id
    msig_address        = f"{msig_dapp_id}::{msig_account_id}"      # CLI / query form
    msig_address_legacy = f"0:{msig_account_id}"                     # ABI payload form
    pubkey = common.read_public_key(MSIG_KEY_PATH)
    log(f"  multisig address: {msig_address}")

    total_ecc = WITHDRAWAL_AMOUNT * 4
    fund_value = max(total_ecc, 100_000_000_000_000)
    log(f"  funding via giver call_contract ecc[{ECC_ID_FOR_BURN}]={fund_value}")
    common.call_contract(
        GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
        "sendCurrencyWithFlag",
        {
            "dest":   msig_address_legacy,   # ABI `address` field — legacy form
            "value":  "200000000000000",
            "ecc":    {str(ECC_ID_FOR_BURN): str(fund_value)},
            "flag":   "17",
            "bounce": False,
        },
    )
    time.sleep(8)
    for _ in range(60):
        account = common.get_account(msig_address)
        if 'acc_type' in account:
            log(f"  account appeared: {account['acc_type']} "
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
    log("  multisig deployed and active")

    account = common.get_account(msig_address)
    ecc = account.get("ecc_balance", {}) or {}
    have = int(ecc.get(str(ECC_ID_FOR_BURN), 0))
    if have < total_ecc:
        log(f"  topping up ECC[{ECC_ID_FOR_BURN}] (have={have}, need={total_ecc})")
        common.call_contract(
            GIVER_ADDRESS, GIVER_ABI, GIVER_KEY_PATH,
            "sendCurrencyWithFlag",
            {"dest": msig_address_legacy, "value": "2000000000",
             "ecc": {str(ECC_ID_FOR_BURN): str(total_ecc)}, "flag": "1"}
        )
        time.sleep(5)

    return msig_address, msig_abi_copy


def call_initiate_withdrawal(msig_address, msig_abi, dst_chain_id, recipient_hex):
    payload = encode_initiate_withdrawal_body(dst_chain_id, recipient_hex)
    params = {
        "dest":    USDC_BRIDGE_ADDRESS_LEGACY,   # ABI `address` field — legacy form
        "value":   "1000000000",
        "cc":      {str(ECC_ID_FOR_BURN): str(WITHDRAWAL_AMOUNT)},
        "bounce":  False,
        "flags":   1,
        "payload": payload,
    }
    return common.call_contract(
        msig_address, msig_abi, MSIG_KEY_PATH,
        "sendTransaction", params, True,
    )


# ── Pipeline stages ───────────────────────────────────────────────────────────

def capture_event_metadata(baseline_msg_ids: set):
    log_phase("Waiting for emitted WithdrawalInitiated event")
    deadline = time.time() + EVENT_INDEXER_TIMEOUT_S
    target = None
    last_log = -1
    while time.time() < deadline:
        nodes = fetch_bridge_extouts(limit=500)
        matched = [
            n for n in nodes
            if n.get("dst") == WITHDRAWAL_EVENT_DST
            and n.get("id") not in baseline_msg_ids
        ]
        if len(matched) != last_log:
            log(f"  matched={len(matched)} new event(s)")
            last_log = len(matched)
        if matched:
            matched.sort(key=lambda n: n.get("created_at") or 0)
            target = matched[-1]
            break
        time.sleep(2)

    if target is None:
        raise RuntimeError("did not observe a WithdrawalInitiated event within "
                           f"{EVENT_INDEXER_TIMEOUT_S}s")

    log(f"  event message id:    {target['id']}")
    log(f"  src_transaction.block_id (== Block.hash): {target['block_id']}")
    log(f"  src_dapp_id:         {target.get('src_dapp_id')}")

    block = find_block_by_hash(target["block_id"])
    if block is None:
        raise RuntimeError(f"block(hash:{target['block_id']}) returned null")
    log(f"  block seq_no={block['seq_no']} height={block['height']} "
        f"key_block={block['key_block']}")

    account_dapp_id_hex = fetch_account_dapp_id(USDC_BRIDGE_ACCOUNT_ID, USDC_BRIDGE_DAPP_ID)
    if not account_dapp_id_hex:
        account_dapp_id_hex = "0" * 64
    log(f"  account_dapp_id:  {account_dapp_id_hex}")

    account_id_hex = USDC_BRIDGE_ACCOUNT_ID
    assert len(account_id_hex) == 64

    return {
        "event_boc_b64":   target["boc"],
        "block_id":        block["block_id"],
        "block_hash":      block["hash"],
        "block_seq_no":    int(block["seq_no"]),
        "block_height":    int(block["height"]),
        "envelope_hash":   block["envelope_hash"],
        "account_dapp_id": account_dapp_id_hex,
        "account_id":      account_id_hex,
        "message_id":      target["id"],
    }


def wait_for_verifier_state(min_seq_no: int):
    # TODO #2: replace with a query against the Ethereum bridge contract's
    # `layerWindows` storage once the on-chain side is live. Today this
    # still reads the off-chain modelling verifier daemon's state file.
    state_path = os.path.join(PROVER_DIR, "state", "verifier_state.json")
    log_phase(f"Waiting for verifier state to reach seq_no >= {min_seq_no}")
    log(f"  state file: {state_path}  (TODO: switch to ETH contract read)")
    deadline = time.time() + VERIFIER_STATE_TIMEOUT_S
    last_log_seq = -1
    while time.time() < deadline:
        try:
            with open(state_path) as f:
                state = json.load(f)
            current = int(state.get("stored_last_seen_block_seq_no", 0))
            if current != last_log_seq:
                latest = fetch_latest_block_seq_no()
                log(f"  verifier at seq_no={current}, chain at seq_no={latest}")
                last_log_seq = current
            if current >= min_seq_no:
                log(f"  verifier reached seq_no={current} (>= {min_seq_no})")
                return current
        except FileNotFoundError:
            log(f"  state file not present yet — daemon may still be starting")
        except json.JSONDecodeError:
            pass
        time.sleep(5)
    raise TimeoutError(
        f"verifier state did not reach seq_no {min_seq_no} within "
        f"{VERIFIER_STATE_TIMEOUT_S}s — is bridge-verifier-daemon running?"
    )


def run_rust_bin(name: str, args: list, parse_last_json: bool = True):
    bin_path = os.path.join(PROVER_DIR, "target", "release", name)
    if not os.path.isfile(bin_path):
        raise FileNotFoundError(
            f"{bin_path} not found — did you `cargo build --release -p {name}` "
            f"in {PROVER_DIR}?"
        )
    cmd = [bin_path] + args
    log(f"  $ {' '.join(cmd)}")
    proc = subprocess.run(
        cmd, capture_output=True, text=True, timeout=RUST_BIN_TIMEOUT_S,
        cwd=PROVER_DIR,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        raise RuntimeError(f"{name} exited with code {proc.returncode}")

    if not parse_last_json:
        return proc.stdout

    last_non_empty = next(
        (ln for ln in reversed(proc.stdout.splitlines()) if ln.strip()),
        None,
    )
    if not last_non_empty:
        raise RuntimeError(f"{name} produced no stdout")
    try:
        return json.loads(last_non_empty)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"{name} last stdout line is not valid JSON: {last_non_empty[:200]!r} "
            f"({e})"
        )


# ── Driver ────────────────────────────────────────────────────────────────────

def main():
    os.makedirs(WORK_DIR, exist_ok=True)

    network = os.getenv("NETWORK", "http://127.0.0.1:80")
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    log_phase("Prechecks")
    assert os.path.isdir(PROVER_DIR), f"PROVER_DIR not found: {PROVER_DIR}"
    log(f"  NETWORK:     {network}")
    log(f"  PROVER_DIR:  {PROVER_DIR}")
    log(f"  GRAPHQL_URL: {GRAPHQL_URL}")
    log(f"  W = {W}, P = {P} (bundle = {W*P} blocks), MAX_LAYERS = {MAX_LAYERS}")
    assert common.is_account_active(USDC_BRIDGE_ADDRESS), \
        f"USDCBridge not active at {USDC_BRIDGE_ADDRESS}"
    log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    baseline = fetch_bridge_extouts(limit=500)
    baseline_ids = {n["id"] for n in baseline}
    log(f"  baseline ExtOut messages from USDCBridge: {len(baseline_ids)}")

    msig_address, msig_abi = deploy_multisig()

    # ─── Wait for the fire window ─────────────────────────────────────────
    # TODO #2: the fire-window dance is a consequence of how the current
    # witness-builder snaps onto the off-chain verifier's W·P-aligned
    # bundle layout. Should simplify once the witness is anchored against
    # the ETH bridge contract.
    log_phase("Waiting for fire window")
    wp = W * P
    fire_deadline = time.time() + FIRE_WINDOW_WAIT_TIMEOUT_S
    last_logged = -1
    while time.time() < fire_deadline:
        latest = fetch_latest_block_seq_no()
        if latest < 0:
            time.sleep(2)
            continue
        next_thinned = ((latest // wp) + 1) * wp
        fire_start = next_thinned - W
        if latest != last_logged:
            log(f"  latest_seq={latest}, next_thinned_kb={next_thinned}, "
                f"fire_start={fire_start}")
            last_logged = latest
        if latest >= fire_start:
            log(f"  ENTERED fire window: latest={latest} ∈ [{fire_start}..{next_thinned})")
            break
        time.sleep(2)
    else:
        raise TimeoutError(
            f"chain never entered fire window within {FIRE_WINDOW_WAIT_TIMEOUT_S}s"
        )

    # ─── Trigger one event ────────────────────────────────────────────────
    log_phase("Dispatching initiateWithdrawal")
    log(f"  dstChainId={DST_CHAIN_ID}, recipient=0x{RECIPIENT_HEX}, "
        f"amount={WITHDRAWAL_AMOUNT}, tokenId={ECC_ID_FOR_BURN}")
    call_result = call_initiate_withdrawal(
        msig_address, msig_abi, DST_CHAIN_ID, RECIPIENT_HEX
    )
    if not common.is_ok(call_result):
        log(f"  call result (may still emit asynchronously): {call_result}")

    # ─── Capture metadata ─────────────────────────────────────────────────
    meta = capture_event_metadata(baseline_ids)
    log("  captured metadata:")
    for k, v in meta.items():
        log(f"    {k}: {v}")

    # ─── Boundary math ────────────────────────────────────────────────────
    event_seq = meta["block_seq_no"]
    key_block_seq    = ((event_seq // W) + 1) * W
    thinned_kb_seq   = ((event_seq // (W*P)) + 1) * (W*P)
    target_seq       = thinned_kb_seq
    log_phase("Boundary math")
    log(f"  event_seq        = {event_seq}")
    log(f"  key_block_seq    = {key_block_seq}  (W-aligned L1 tree the event lives in)")
    log(f"  thinned_kb_seq   = {thinned_kb_seq}  (verifier-stored L1 root anchor)")
    log(f"  target_seq       = {target_seq}    (verifier must reach this seq_no)")
    if key_block_seq != thinned_kb_seq:
        raise RuntimeError(
            f"event landed in wrong W-window: key_block_seq={key_block_seq} != "
            f"thinned_kb_seq={thinned_kb_seq}. The fire-window timing missed; "
            f"chain probably advanced past the boundary before the event landed. "
            f"Re-run the test."
        )

    wait_for_verifier_state(target_seq)

    # ─── Run the three Rust binaries ─────────────────────────────────────
    seq_no = 0
    work_event_dir = os.path.join(WORK_DIR, f"event_{seq_no:06d}")
    os.makedirs(work_event_dir, exist_ok=True)
    partial_path = os.path.join(work_event_dir, "partial.json")
    witness_path = os.path.join(work_event_dir, "witness.json")
    # Proof output: STUB drops it in WORK_DIR rather than $PROVER_DIR/proofs/
    # so the off-chain `bridge-verifier-daemon` (if running) doesn't pick it
    # up. The next step is to submit this file to the Ethereum bridge
    # contract — see TODO #1 at the top of the script.
    proofs_dir   = work_event_dir
    log(f"  work dir:    {work_event_dir}")
    log(f"  proofs dir:  {proofs_dir}  (STUB: not sent to verifier-daemon)")

    log_phase("Step 5: bridge-event-private-witness-export  [READY]")
    run_rust_bin(
        "bridge-event-private-witness-export",
        [
            "--event-boc-b64",   meta["event_boc_b64"],
            "--block-id",        meta["block_id"],
            "--block-seq-no",    str(meta["block_seq_no"]),
            "--account-dapp-id", meta["account_dapp_id"],
            "--account-id",      meta["account_id"],
            "--envelope-hash",   meta["envelope_hash"],
            "--out",             partial_path,
        ],
        parse_last_json=False,
    )
    assert os.path.isfile(partial_path), f"exporter did not produce {partial_path}"
    log(f"  wrote {partial_path}")

    # TODO #2: today this binary reads `state/verifier_state.json`. It
    # will be rewritten to read the Ethereum bridge contract's
    # `layerWindows` storage instead — at which point the
    # `bridge-verifier-daemon` (and its state file) is no longer needed.
    log_phase("Step 6: bridge-event-witness-builder  [STUB — will be rewritten]")
    wb_summary = run_rust_bin(
        "bridge-event-witness-builder",
        [
            "--partial-witness", partial_path,
            "--out",             witness_path,
            "--verifier-state",  os.path.join(PROVER_DIR, "state/verifier_state.json"),
            "--gql-endpoint",    GRAPHQL_URL,
            "--layer-idx",       "0",
        ],
    )
    log(f"  summary: {json.dumps(wb_summary, indent=2)}")
    assert os.path.isfile(witness_path), f"witness-builder did not produce {witness_path}"

    log_phase(f"Step 7: bridge-event-halo2-prover (seq_no={seq_no})  [READY]")
    ep_summary = run_rust_bin(
        "bridge-event-halo2-prover",
        [
            "--fixture",  witness_path,
            "--out-dir",  proofs_dir,
            "--seq-no",   str(seq_no),
        ],
    )
    log(f"  summary: schema={ep_summary.get('schema_version')}, "
        f"self_verified={ep_summary.get('self_verified')}, "
        f"proof_file={ep_summary.get('proof_file')}")
    if not ep_summary.get("self_verified"):
        log("  WARNING: prover-side self-verify FAILED")
        log_phase("STUB END — proof produced but self-verify failed")
        sys.exit(1)

    proof_file = ep_summary.get("proof_file")
    log_phase("STUB END — proof produced and self-verified")
    log(f"  proof:     {proof_file}")
    log(f"  artefacts: {work_event_dir}")
    log("")
    log("  ┌─ NEXT STEP (TODO #1) ─────────────────────────────────────────┐")
    log("  │ Submit this proof to the Ethereum bridge contract via         │")
    log("  │ `proveWithdrawal(...)`. A submitter (web3.py script or Rust   │")
    log("  │ binary wrapping ethers-rs) will be supplied later — until     │")
    log("  │ then, hand `proof_event_NNN.json` to whichever tool you have. │")
    log("  └───────────────────────────────────────────────────────────────┘")
    sys.exit(0)


if __name__ == "__main__":
    main()
