"""
Phase 1 isolation test: deploy a multisig on an MT (multi-thread) cluster and
verify that cross-DApp funding via the upgraded GiverV3 works.

What this test exercises (and ONLY this):
  1. prechecks (NETWORK, cluster reachable, USDCBridge active)
  2. genaddr an UpdateCustodianMultisigWallet with a fresh key
     (self-rooted DApp: msig_dapp_id == msig_account_id)
  3. fund the msig from GIVER_ADDRESS via `sendCurrencyWithFlag(..., dapp_id)`
     — this is the cross-DApp routing bit that was broken before the giver
     upgrade (msig lives in a different DApp than the giver)
  4. deployx msig
  5. verify active + ECC[2] non-zero

What this test deliberately does NOT exercise (Phase 2):
  - USDCBridge.mintAndSend (needs cross-DApp upgrade on eccUSDCBridge.sol)
  - multisig.sendTransaction → USDCBridge.initiateWithdrawal
    (needs multisig v2 cross-DApp upgrade)

Modes (env MODE, default `local`): same as test_deploy_and_withdraw_only.py.

Env vars honored:
  MODE (local|shellnet), PROVER_DIR, NETWORK, GRAPHQL_URL, WORK_DIR,
  ACKI_NACKI_ROOT (local, for USDCBridge liveness check).
"""

import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.bridge_e2e import USDC_BRIDGE_ADDRESS
from helper.msig import deploy_and_fund_multisig_only

MODE = os.environ.get("MODE", "local").lower()
if MODE not in ("local", "shellnet"):
    raise SystemExit(f"MODE must be 'local' or 'shellnet', got {MODE!r}")
IS_SHELLNET = MODE == "shellnet"

if IS_SHELLNET:
    DEFAULT_NETWORK = "shellnet.ackinacki.org"
    DEFAULT_GRAPHQL = "https://shellnet.ackinacki.org/graphql"
    DEFAULT_WORK    = "work-shellnet-phase1"
else:
    DEFAULT_NETWORK = "http://127.0.0.1:80"
    DEFAULT_GRAPHQL = "http://localhost/graphql"
    DEFAULT_WORK    = "work-local-phase1"

PROVER_DIR  = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR    = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, DEFAULT_WORK))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", DEFAULT_GRAPHQL)
MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_phase1.keys.json")


def main():
    tracer = be.Tracer()
    os.makedirs(WORK_DIR, exist_ok=True)

    network = os.getenv("NETWORK", DEFAULT_NETWORK)
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    tracer.log_phase(f"Phase 1 prechecks ({MODE})")
    tracer.log(f"  NETWORK:     {network}")
    tracer.log(f"  PROVER_DIR:  {PROVER_DIR}")
    tracer.log(f"  GRAPHQL_URL: {GRAPHQL_URL}")
    tracer.log(f"  WORK_DIR:    {WORK_DIR}")
    assert common.is_account_active(USDC_BRIDGE_ADDRESS), \
        f"USDCBridge not active at {USDC_BRIDGE_ADDRESS} — cluster not ready"
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    msig_address, _ = deploy_and_fund_multisig_only(
        tracer,
        work_dir=WORK_DIR, msig_key_path=MSIG_KEY_PATH,
        is_shellnet=IS_SHELLNET, verbose_faucet=True,
    )

    tracer.log_phase("PASS — multisig deployed and funded on MT cluster")
    tracer.log(f"  msig address: {msig_address}")


if __name__ == "__main__":
    main()
