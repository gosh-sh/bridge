"""
The Acki Nacki side of the Arbitrum Sepolia e2e (tests/e2e/local-net/arbitrum/), run on the
local-network host, where the tvm client reaches the nodes. Owner calls on the zerostate
bridge, and the reads the tests assert on. Each command prints KEY=VALUE lines on stdout;
logs go to stderr.

  bridge                    BRIDGE_ACTIVE, BRIDGE_VERSION
  verifier <l2> <parent> <rollup>
                            deployL2Verifier, then wait until getL2Verifier records it:
                            VERIFIER, VERIFIER_PARENT, VERIFIER_L2/L1/ROLLUP (its getConfig)
  trust <evm bridge>        setTrustedL1Bridge(421614, bridge, true); TRUSTED=true|false read back
  msig                      deploy a multisig without USDC (the deposit brings it):
                            MSIG, MSIG_ACCOUNT, MSIG_KEYS (the keys file)
  anchor <chain> <hash>     setAcceptedBlockHash(chain, hash, true): the parent chain's light
                            client, done by the owner; ACCEPTED read back
  accepted <chain> <hash>   ACCEPTED=true|false
  ecc <dapp::account>       ECC3=<base units of USDC on the account>

Env: as test_deploy_and_withdraw_only.py (MODE=local, NETWORK, WORK_DIR, PROVER_DIR,
ACKI_NACKI_ROOT).
"""

import contextlib
import os
import sys
import time

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
VERIFIER_ABI = os.path.join(_HERE, "..", "..", "..", "contracts", "an", "0.81.0_compiled",
                            "exchange", "ArbitrumBlockVerifier.abi.json")
# Account state can lag a call by a block or two.
READ_BACK_SECS = float(os.environ.get("READ_BACK_SECS", "120"))


def setup(tracer):
    common.NETWORK = os.getenv("NETWORK", "http://127.0.0.1:80")
    common.set_config({"async_call": "false"})
    common.setup()
    os.makedirs(WORK_DIR, exist_ok=True)
    materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, USDC_BRIDGE_KEYS)
    validate_usdc_bridge_key(tracer, USDC_BRIDGE_KEYS, overridden=False)


def owner_call(method, params):
    result = common.call_contract(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, USDC_BRIDGE_KEYS, method, params)
    if not common.is_ok(result):
        raise SystemExit(f"{method}: {result}")


def getter(method, params):
    out = common.run_getter(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, method, params)
    if not isinstance(out, dict):
        raise SystemExit(f"{method} answered {out!r}")
    return out


def until(check):
    deadline = time.time() + READ_BACK_SECS
    while True:
        value = check()
        if value or time.time() > deadline:
            return value
        time.sleep(3)


def uint(v):
    return int(str(v or "0"), 0)


def truth(v):
    return v in (True, "true")


def accepted(chain, block_hash):
    return truth(getter("isAcceptedBlockHash", {"chainId": str(chain), "blockHash": block_hash}).get("value0"))


def recorded_verifier(l2, parent):
    got = getter("getL2Verifier", {"l2ChainId": l2})
    return got if uint(got.get("parentChainId")) == int(parent) and got.get("verifier") else None


def verifier_config(target, l2):
    cfg = common.run_getter(target, VERIFIER_ABI, "getConfig", {})
    return cfg if isinstance(cfg, dict) and uint(cfg.get("l2ChainId")) == int(l2) else None


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    cmd, args = sys.argv[1], sys.argv[2:]
    tracer = be.Tracer()
    # Everything the helpers print is a log here: stdout carries only the answer.
    with contextlib.redirect_stdout(sys.stderr):
        setup(tracer)
        if cmd == "bridge" and not args:
            version = getter("getVersion", {})
            answer = [f"BRIDGE_ACTIVE={str(common.is_account_active(USDC_BRIDGE_ADDRESS)).lower()}",
                      f"BRIDGE_VERSION={version.get('value0')}"]
        elif cmd == "verifier" and len(args) == 3:
            l2, parent, rollup = args
            owner_call("deployL2Verifier", {"l2ChainId": l2, "parentChainId": parent, "rollup": rollup})
            got = until(lambda: recorded_verifier(l2, parent)) or {}
            target = common.to_dapp_address(got["verifier"]) if got.get("verifier") else ""
            cfg = (until(lambda: verifier_config(target, l2)) if target else None) or {}
            answer = [f"VERIFIER={got.get('verifier', '')}", f"VERIFIER_PARENT={uint(got.get('parentChainId'))}",
                      f"VERIFIER_L2={uint(cfg.get('l2ChainId'))}", f"VERIFIER_L1={uint(cfg.get('l1ChainId'))}",
                      f"VERIFIER_ROLLUP={uint(cfg.get('rollup')):#042x}"]
        elif cmd == "trust" and len(args) == 1:
            owner_call("setTrustedL1Bridge", {"chainId": str(ARB_SEPOLIA), "l1Bridge": args[0], "allowed": True})
            trusted = until(lambda: truth(getter("isTrustedL1Bridge",
                                                 {"chainId": str(ARB_SEPOLIA), "l1Bridge": args[0]}).get("value0")))
            answer = [f"TRUSTED={str(bool(trusted)).lower()}"]
        elif cmd == "msig" and not args:
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
        elif cmd == "anchor" and len(args) == 2:
            owner_call("setAcceptedBlockHash", {"chainId": args[0], "blockHash": args[1], "accepted": True})
            answer = [f"ACCEPTED={str(bool(until(lambda: accepted(args[0], args[1])))).lower()}"]
        elif cmd == "accepted" and len(args) == 2:
            answer = [f"ACCEPTED={str(accepted(args[0], args[1])).lower()}"]
        elif cmd == "ecc" and len(args) == 1:
            account = common.get_account(args[0]) or {}
            answer = [f"ECC3={int((account.get('ecc_balance') or {}).get('3', 0))}"]
        else:
            raise SystemExit(__doc__)
    print("\n".join(answer))


if __name__ == "__main__":
    main()
