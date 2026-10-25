"""
Arbitrum block proof on a live Acki Nacki network (local devnet or shellnet).

No Ethereum or Arbitrum node: the vector is
`scripts/l2-block-proof-bench/fixtures/synthetic_421614.json`, a parent-chain
block whose receipt trie holds the rollup's `AssertionCreated`, and the
Arbitrum block M that assertion names with block N's hash in M's state. What
this runs on the network is everything after the data: the bridge deploying
the verifier from the code the zerostate installed, `KECCAK256` in the node's
VM, the verifier's message to the bridge and the bridge's checks on it.

  1. the owner deploys the verifier: `deployL2Verifier(421614, 11155111, rollup)`;
  2. `proveBlock` before the parent block is accepted: the bridge refuses it
     (224, bounced), N stays out of the anchor set;
  3. the owner admits the vector's parent block for 11155111
     (`setAcceptedBlockHash`), standing in for the Sepolia light client;
  4. `proveBlock` again, from nobody in particular: N is in the anchor set
     for 421614, and only there.

Env vars: MODE (local|shellnet), NETWORK, PROVER_DIR, WORK_DIR,
USDC_BRIDGE_KEY_PATH, ACKI_NACKI_ROOT, BRIDGE_READY_TIMEOUT_SECS,
ANCHOR_TIMEOUT_SECS, L2_VECTOR (another vector in the same format).
"""

import json
import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.bridge_e2e import (
    USDC_BRIDGE_ABI, USDC_BRIDGE_ADDRESS, USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    materialize_usdc_bridge_key_from_node_config, validate_usdc_bridge_key,
)

REPO = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))
VERIFIER_ABI = os.path.join(REPO, "contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.abi.json")
VECTOR = os.environ.get("L2_VECTOR",
                        os.path.join(REPO, "scripts/l2-block-proof-bench/fixtures/synthetic_421614.json"))

MODE = os.environ.get("MODE", "local").lower()
if MODE not in ("local", "shellnet"):
    raise SystemExit(f"MODE must be 'local' or 'shellnet', got {MODE!r}")
IS_SHELLNET = MODE == "shellnet"
DEFAULT_NETWORK = "shellnet.ackinacki.org" if IS_SHELLNET else "http://127.0.0.1:80"
PROVER_DIR = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, "work-shellnet" if IS_SHELLNET else "work-local"))
USDC_BRIDGE_KEY_PATH_OVERRIDE = os.environ.get("USDC_BRIDGE_KEY_PATH")
USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEY_PATH_OVERRIDE or (USDC_BRIDGE_KEYS_SHELLNET if IS_SHELLNET else USDC_BRIDGE_KEYS)
BRIDGE_READY_TIMEOUT_SECS = float(os.environ.get("BRIDGE_READY_TIMEOUT_SECS", "180"))
# A deploy, a proof and the bridge's answer each wait for a block to finalize.
ANCHOR_TIMEOUT_SECS = float(os.environ.get("ANCHOR_TIMEOUT_SECS", "180"))

tracer: be.Tracer


def wait_for(what: str, check, timeout: float):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = check()
        if value:
            return value
        time.sleep(3)
    tracer.log_phase("FAIL")
    tracer.log(f"  {what}: not within {timeout:.0f} s")
    sys.exit(1)


def bridge_getter(method: str, params: dict) -> dict:
    out = common.run_getter(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, method, params)
    if not isinstance(out, dict):
        raise RuntimeError(f"{method} answered {out!r}")
    return out


def accepted(chain_id: int, block_hash: str) -> bool:
    out = bridge_getter("isAcceptedBlockHash", {"chainId": str(chain_id), "blockHash": block_hash})
    return out.get("value0") in (True, "true")


def owner_call(method: str, params: dict):
    result = common.call_contract(USDC_BRIDGE_ADDRESS, USDC_BRIDGE_ABI, USDC_BRIDGE_KEY_PATH, method, params)
    if not common.is_ok(result):
        tracer.log_phase("FAIL")
        tracer.log(f"  {method}: {result}")
        sys.exit(1)


def main():
    global tracer
    tracer = be.Tracer()
    os.makedirs(WORK_DIR, exist_ok=True)
    network = os.getenv("NETWORK", DEFAULT_NETWORK)
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()

    with open(VECTOR) as f:
        v = json.load(f)
    l2, l1, rollup = v["l2ChainId"], v["l1ChainId"], v["rollup"]
    parent, block = v["expect"]["parentBlockHash"], v["expect"]["blockHash"]

    tracer.log_phase(f"Prechecks ({MODE})")
    tracer.log(f"  NETWORK: {network}")
    tracer.log(f"  vector:  {VECTOR} (chain {l2} on {l1}, N = {v['proveBlock']['blockNumber']})")
    wait_for(f"USDCBridge active at {USDC_BRIDGE_ADDRESS}",
             lambda: common.is_account_active(USDC_BRIDGE_ADDRESS), BRIDGE_READY_TIMEOUT_SECS)
    version = bridge_getter("getVersion", {})
    tracer.log(f"  USDCBridge {version}")
    if not IS_SHELLNET and USDC_BRIDGE_KEY_PATH_OVERRIDE is None:
        materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, USDC_BRIDGE_KEY_PATH)
    validate_usdc_bridge_key(tracer, USDC_BRIDGE_KEY_PATH, overridden=USDC_BRIDGE_KEY_PATH_OVERRIDE is not None)
    if accepted(l2, block):
        raise SystemExit(f"N {block} is already accepted for {l2}: this network has run the vector before")

    tracer.log_phase(f"deployL2Verifier({l2}, {l1}, {rollup})")
    owner_call("deployL2Verifier", {"l2ChainId": str(l2), "parentChainId": str(l1), "rollup": rollup})

    # The API answers from account state that can predate the call by a block.
    def recorded():
        got = bridge_getter("getL2Verifier", {"l2ChainId": str(l2)})
        return got if int(str(got.get("parentChainId") or "0"), 0) == l1 and got.get("verifier") else None
    got = wait_for(f"getL2Verifier({l2}) to record the verifier", recorded, ANCHOR_TIMEOUT_SECS)
    verifier = got["verifier"]
    tracer.log(f"  recorded verifier {verifier}, parent chain {got['parentChainId']}")
    target = common.to_dapp_address(verifier)
    wait_for("the verifier account active", lambda: common.is_account_active(target), ANCHOR_TIMEOUT_SECS)

    def config():
        out = common.run_getter(target, VERIFIER_ABI, "getConfig", {})
        return out if isinstance(out, dict) and int(str(out.get("l2ChainId", "0")), 0) == l2 else None
    cfg = wait_for("the verifier initialized", config, ANCHOR_TIMEOUT_SECS)
    tracer.log(f"  verifier config {cfg}")
    if int(str(cfg["l1ChainId"]), 0) != l1 or int(str(cfg["rollup"]), 0) != int(rollup, 16):
        raise SystemExit(f"verifier config {cfg} is not ({l2}, {l1}, {rollup})")

    prove = v["proveBlock"]
    path = os.path.join(WORK_DIR, "proveBlock.params.json")
    with open(path, "w") as f:
        json.dump(prove, f)

    def prove_block():
        # Unsigned: proveBlock is permissionless. The parameters go in a file;
        # they are some 10 KB of hex, too long for a command line on some hosts.
        return common.execute_cli_cmd(f"callx --abi {VERIFIER_ABI} --addr {target} -m proveBlock {path}")

    tracer.log_phase("proveBlock before the parent block is accepted")
    if accepted(l1, parent):
        raise SystemExit(f"the vector's parent block {parent} is already accepted for {l1}")
    result = prove_block()
    tracer.log(f"  verifier: {'ok' if common.is_ok(result) else result}")
    time.sleep(20)
    if accepted(l2, block):
        tracer.log_phase("FAIL")
        tracer.log(f"  N admitted for {l2} although its parent block is not accepted for {l1}")
        sys.exit(1)
    tracer.log(f"  N not admitted, as it must not be")

    tracer.log_phase(f"setAcceptedBlockHash({l1}, parent block) — the light client's job, done by the owner")
    owner_call("setAcceptedBlockHash", {"chainId": str(l1), "blockHash": parent, "accepted": True})
    wait_for("the parent block accepted", lambda: accepted(l1, parent), ANCHOR_TIMEOUT_SECS)

    tracer.log_phase("proveBlock")
    result = prove_block()
    if not common.is_ok(result):
        tracer.log_phase("FAIL")
        tracer.log(f"  proveBlock: {result}")
        sys.exit(1)
    tracer.log(f"  transaction {result.get('transaction_id') or result}")
    wait_for(f"N {block} accepted for {l2}", lambda: accepted(l2, block), ANCHOR_TIMEOUT_SECS)
    if accepted(l1, block):
        raise SystemExit(f"N is accepted for {l1} too; it may only be for {l2}")

    tracer.log_phase(f"PASS — the bridge accepts N for {l2} from the verifier's proof")
    tracer.log(f"  verifier: {verifier}")
    tracer.log(f"  N:        {block}")


if __name__ == "__main__":
    main()
