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
  lightclient               deployLightClient(owner pubkey, 11155111, 0, 0): the Sepolia light
                            client, its committee unset (lc-bootstrap sets it once); LC (as
                            dapp::account), LC_L1, LC_OWNER and OWNER_PUBKEY read back
  lc <dapp::account>        the light client's state: LC_FINALIZED_SLOT, LC_UPDATES, LC_COMMITTEE,
                            LC_PERIOD, LC_OWNER_ROTATION, LC_GLOAS_SLOT, LC_GLOAS_SET
  disable-owner-anchors <chain>
                            disableOwnerAnchorsOn(chain); OWNER_ANCHORS read back
  owner-anchors <chain>     OWNER_ANCHORS=true|false (isOwnerAnchorsEnabled)
  msig                      deploy a multisig without USDC (the deposit brings it):
                            MSIG, MSIG_ACCOUNT, MSIG_KEYS (the keys file)
  accepted <chain> <hash>   ACCEPTED=true|false
  ecc <dapp::account>       ECC3=<base units of USDC on the account>

Env: as test_deploy_and_withdraw_only.py (MODE=local, NETWORK, WORK_DIR, PROVER_DIR,
ACKI_NACKI_ROOT).
"""

import contextlib
import json
import os
import re
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
SEPOLIA = 11155111
PROVER_DIR = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, "work-local"))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", "http://localhost/graphql")
_COMPILED = os.path.join(_HERE, "..", "..", "..", "contracts", "an", "0.81.0_compiled", "exchange")
VERIFIER_ABI = os.path.join(_COMPILED, "ArbitrumBlockVerifier.abi.json")
LC_ABI = os.path.join(_COMPILED, "EthBeaconLightClient.abi.json")
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


def deployed(address):
    """`0:<account>` when an address getter names an account; None for addr_none or zero."""
    a = str(address or "")
    return a if re.fullmatch(r"0:[0-9a-f]{64}", a) and int(a[2:], 16) else None


def owner_anchors(chain):
    return truth(getter("isOwnerAnchorsEnabled", {"chainId": str(chain)}).get("value0"))


def lc_getter(target, method):
    out = common.run_getter(target, LC_ABI, method, {})
    return out if isinstance(out, dict) else {}


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
        elif cmd == "lightclient" and not args:
            with open(USDC_BRIDGE_KEYS) as f:
                pubkey = json.load(f)["public"]
            owner_call("deployLightClient", {"pubkey": "0x" + pubkey, "l1ChainId": str(SEPOLIA),
                                             "bootstrapCommittee": "0", "bootstrapPeriod": "0"})
            got = until(lambda: deployed(getter("getLightClient", {"l1ChainId": str(SEPOLIA)}).get("value0"))) or ""
            target = common.to_dapp_address(got) if got else ""
            cfg = (until(lambda: lc_getter(target, "getConfig")) if target else None) or {}
            answer = [f"LC={target}", f"LC_L1={uint(cfg.get('l1ChainId'))}",
                      f"LC_OWNER={uint(cfg.get('ownerPubkey')):064x}", f"OWNER_PUBKEY={pubkey}"]
        elif cmd == "lc" and len(args) == 1:
            head, committee = lc_getter(args[0], "getHead"), lc_getter(args[0], "getCommitteeState")
            gloas = lc_getter(args[0], "getGloasForkSlot")
            answer = [f"LC_FINALIZED_SLOT={uint(head.get('finalizedSlot'))}",
                      f"LC_UPDATES={uint(head.get('updatesApplied'))}",
                      f"LC_COMMITTEE={uint(committee.get('currentCommittee')):#066x}",
                      f"LC_PERIOD={uint(committee.get('committeePeriod'))}",
                      f"LC_OWNER_ROTATION={str(truth(committee.get('ownerRotationEnabled'))).lower()}",
                      f"LC_GLOAS_SLOT={uint(gloas.get('gloasForkSlot'))}",
                      f"LC_GLOAS_SET={str(truth(gloas.get('isSet'))).lower()}"]
        elif cmd == "disable-owner-anchors" and len(args) == 1:
            owner_call("disableOwnerAnchorsOn", {"chainId": args[0]})
            answer = [f"OWNER_ANCHORS={str(not until(lambda: not owner_anchors(args[0]))).lower()}"]
        elif cmd == "owner-anchors" and len(args) == 1:
            answer = [f"OWNER_ANCHORS={str(owner_anchors(args[0])).lower()}"]
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
