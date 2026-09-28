#!/usr/bin/env python3
"""
P3 — periodic WithdrawalInitiated trigger.

Wraps `vendored/test_deploy_and_withdraw_only.py` in a sleep loop so the reachability
collector (P4) has a steady stream of new events to walk. Every iteration is an
independent subprocess with a fresh multisig — self-funds ECC[3] via
`USDCBridge.mintAndSend` inside `deploy_multisig`, so no per-event pre-mint step.

Usage:
    python research/trigger_loop.py --interval 60             # forever, one event / 60s
    python research/trigger_loop.py --interval 30 --count 5   # exit after 5 events (pilot)

Env vars honored (passed through to the subprocess):
    MODE                    default "local"
    NETWORK                 default "http://127.0.0.1:80"
    GRAPHQL_URL             default "http://localhost/graphql"
    USDC_BRIDGE_KEY_PATH    optional override
    ACKI_NACKI_ROOT         needed by helper/common (path to acki-nacki checkout)
    PROVER_DIR              default = parent of vendored script
    WORK_DIR                default "work-local" under PROVER_DIR

The subprocess `stdout`+`stderr` is passed through so pane-C shows the tvm-cli
progress live.
"""
from __future__ import annotations
import argparse
import os
import subprocess
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_VENDORED = os.path.join(_HERE, "vendored")
_SCRIPT = os.path.join(_VENDORED, "test_deploy_and_withdraw_only.py")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--interval", type=float, default=60.0,
                    help="Seconds between event triggers (default: 60).")
    ap.add_argument("--count", type=int, default=0,
                    help="Number of events to fire before exiting (0 = forever).")
    ap.add_argument("--python", default=sys.executable,
                    help="Python interpreter to use for the subprocess.")
    args = ap.parse_args()

    if not os.path.isfile(_SCRIPT):
        print(f"[trigger_loop] vendored script not found: {_SCRIPT}", file=sys.stderr)
        return 2

    env = os.environ.copy()
    env.setdefault("MODE", "local")

    # PROVER_DIR must point at the *vendored* directory so the script resolves
    # `WORK_DIR = PROVER_DIR/work-local` relative to us, not the upstream tree.
    env.setdefault("PROVER_DIR", _VENDORED)

    fired = 0
    try:
        while True:
            wall = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
            print(f"[trigger_loop] {wall} firing event #{fired + 1}", flush=True)

            rc = subprocess.call(
                [args.python, _SCRIPT],
                env=env, cwd=_VENDORED,
            )
            fired += 1
            print(f"[trigger_loop] event #{fired} subprocess exited rc={rc}", flush=True)

            if args.count and fired >= args.count:
                print(f"[trigger_loop] count target reached ({args.count}); done.",
                      flush=True)
                return 0

            time.sleep(args.interval)
    except KeyboardInterrupt:
        print(f"[trigger_loop] interrupted after {fired} event(s).", file=sys.stderr)
        return 0


if __name__ == "__main__":
    sys.exit(main())
