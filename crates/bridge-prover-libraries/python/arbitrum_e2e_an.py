"""
The Acki Nacki side of the Arbitrum Sepolia e2e (tests/e2e/local-net/arbitrum-e2e.sh),
run on the local-network host, where the tvm client reaches the nodes. Owner calls on
the zerostate bridge and the multisig the deposit and the withdrawal move USDC through.
Each command prints KEY=VALUE lines on stdout; logs go to stderr.

  msig                      deploy a multisig, without USDC: the deposit brings it; prints
                            MSIG, MSIG_ACCOUNT, MSIG_KEYS (the keys file)
  trust <evm bridge>        setTrustedL1Bridge(421614, bridge, true)
  anchor <chain> <hash>     setAcceptedBlockHash(chain, hash, true): the parent chain's light
                            client, done by the owner
  ecc <dapp::account>       the account's ECC[3] (USDC) balance: ECC3=<base units>

Env: as test_deploy_and_withdraw_only.py (MODE=local, NETWORK, WORK_DIR, PROVER_DIR,
ACKI_NACKI_ROOT).
"""

import contextlib
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common  # noqa: E402
from helper import bridge_e2e as be  # noqa: E402
from helper.bridge_e2e import (  # noqa: E402
    USDC_BRIDGE_ABI, USDC_BRIDGE_ADDRESS, USDC_BRIDGE_KEYS,
    materialize_usdc_bridge_key_from_node_config, validate_usdc_bridge_key,
)
from helper.msig import deploy_multisig  # noqa: E402

ARB_SEPOLIA = 421614
PROVER_DIR = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, "work-local"))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", "http://localhost/graphql")


def setup(tracer):
    network = os.getenv("NETWORK", "http://127.0.0.1:80")
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    os.makedirs(WORK_DIR, exist_ok=True)
    materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, USDC_BRIDGE_KEYS)
    validate_usdc_bridge_key(tracer, USDC_BRIDGE_KEYS, overridden=False)


def owner_call(method, params):
    result = common.call_contract(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, USDC_BRIDGE_KEYS, method, params)
    if not common.is_ok(result):
        raise SystemExit(f"{method}: {result}")


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    cmd, args = sys.argv[1], sys.argv[2:]
    tracer = be.Tracer()
    # Everything the helpers print is a log here: stdout carries only the answer.
    with contextlib.redirect_stdout(sys.stderr):
        setup(tracer)
        if cmd == "msig":
            gql = be.GqlClient(GRAPHQL_URL)
            keys = os.path.join(WORK_DIR, "msig_arbitrum_e2e.keys.json")
            address, _abi = deploy_multisig(
                tracer, gql, work_dir=WORK_DIR, msig_key_path=keys,
                usdc_bridge_key_path=USDC_BRIDGE_KEYS, is_shellnet=False,
                verbose_faucet=False, mint=False,
            )
            # A multisig lives in its own dapp: keep the address the helper returns.
            ext = address if "::" in address else common.to_dapp_address(address)
            answer = [f"MSIG={ext}", f"MSIG_ACCOUNT={ext.split('::')[1]}", f"MSIG_KEYS={keys}"]
        elif cmd == "trust" and len(args) == 1:
            owner_call("setTrustedL1Bridge",
                       {"chainId": str(ARB_SEPOLIA), "l1Bridge": args[0], "allowed": True})
            answer = [f"TRUSTED={args[0]}"]
        elif cmd == "anchor" and len(args) == 2:
            owner_call("setAcceptedBlockHash", {"chainId": args[0], "blockHash": args[1], "accepted": True})
            answer = [f"ANCHORED={args[0]}:{args[1]}"]
        elif cmd == "ecc" and len(args) == 1:
            account = common.get_account(args[0]) or {}
            answer = [f"ECC3={int((account.get('ecc_balance') or {}).get('3', 0))}"]
        else:
            raise SystemExit(__doc__)
    print("\n".join(answer))


if __name__ == "__main__":
    main()
