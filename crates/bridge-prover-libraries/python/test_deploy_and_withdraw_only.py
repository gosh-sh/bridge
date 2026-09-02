"""
Isolated contract-only sanity test for the AN→ETH bridge (LOCAL DEVNET + SHELLNET).

No Rust binaries, no halo2 proving. Purely tvm-cli + GraphQL.

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

Env vars: same subset the full orchestrator honors —
  MODE (local|shellnet), PROVER_DIR, NETWORK, GRAPHQL_URL, WORK_DIR,
  USDC_BRIDGE_KEY_PATH, ACKI_NACKI_ROOT.
"""

import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.bridge_e2e import (
    USDC_BRIDGE_ADDRESS, USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    WITHDRAWAL_AMOUNT, USDC_TOKEN_ID,
    DST_CHAIN_ID, RECIPIENT_HEX,
    materialize_usdc_bridge_key_from_node_config,
    validate_usdc_bridge_key,
)
from helper.msig import deploy_multisig

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
        materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, USDC_BRIDGE_KEY_PATH)

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    validate_usdc_bridge_key(tracer, USDC_BRIDGE_KEY_PATH,
                             overridden=USDC_BRIDGE_KEY_PATH_OVERRIDE is not None)

    baseline = gql.fetch_bridge_extouts(limit=500)
    baseline_ids = {n["id"] for n in baseline}
    tracer.log(f"  baseline ExtOut messages from USDCBridge: {len(baseline_ids)}")

    msig_address, msig_abi = deploy_multisig(
        tracer, gql,
        work_dir=WORK_DIR, msig_key_path=MSIG_KEY_PATH,
        usdc_bridge_key_path=USDC_BRIDGE_KEY_PATH,
        is_shellnet=IS_SHELLNET, verbose_faucet=True,
    )

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
