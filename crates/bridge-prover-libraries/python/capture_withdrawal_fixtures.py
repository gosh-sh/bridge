"""
Capture the `WithdrawalInitiated` events Circuit 4 is keyed and tested on.

Writes `crates/bridge-circuits/bridge-event-prove-circuit/withdrawals.txt` (the
7-line records `event_data_helper.rs` reads) from a local network whose
zerostate carries this tree's `eccUSDCBridge` — `tests/e2e/local-net/`
brings one up. The events come from the deployed contract, not from a
builder: the circuit fixes the body length and the field offsets at keygen,
so the fixture has to be what the contract emits.

The records cover three destination chains, each bound to its own EVM bridge
(`setEvmBridge`), so `dstChainId` and `dstBridge` both vary across the file:
11155111 keeps the bridge the zerostate bound (`BRIDGE_ZS_L1_BRIDGE`), the
other two are bound here. The owner mints the inventory each chain needs with
`mintAndSendBridged`; the withdrawals are sent one at a time so each event is
matched to the record that caused it.

Env: NETWORK, GRAPHQL_URL, WORK_DIR, ACKI_NACKI_ROOT as for
`test_deploy_and_withdraw_only.py` (local mode only); OUT overrides the
output path.
"""

import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.msig import deploy_multisig

PROVER_DIR = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, "work-fixtures"))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", "http://localhost/graphql")
OUT = os.environ.get("OUT", os.path.join(
    PROVER_DIR, "..", "bridge-circuits", "bridge-event-prove-circuit", "withdrawals.txt"))
MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_fixtures.keys.json")
EVENT_TIMEOUT_S = 180

# Bound here; 11155111 keeps the zerostate's binding.
EXTRA_EVM_BRIDGES = {
    421614: 0x9C2e1A6b5F3d07E84aB2c1d4E6f8A0b3C5d7E9f1,
    1: 0x5aD1c7E3b9F2048a6C1e3D5f7A9b0C2d4E6f8A01,
}
CHAINS = [11155111, 421614, 1]
RECIPIENTS_HEX = [
    "742d35cc6634c0532925a3b844bc454e4438f44e",
    "5fb33d710f3a3d11f3c14edaa78d2b58a8b3d2c1",
    "3a0f8b9a8e7c6d5b4a392817160504030201f0ee",
    "f0eedccbaa99887766554433221100ffeeddccbb",
    "0123456789abcdef0123456789abcdef01234567",
    "fedcba9876543210fedcba9876543210fedcba98",
    "1111222233334444555566667777888899990000",
    "aaaabbbbccccddddeeeeffff0000111122223333",
    "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "cafebabecafebabecafebabecafebabecafebabe",
]
RECORDS = [(CHAINS[i % len(CHAINS)], r, 1_000_000 + i) for i, r in enumerate(RECIPIENTS_HEX)]


def owner_call(key_path: str, method: str, params: dict) -> None:
    result = common.call_contract(be.USDC_BRIDGE_ADDRESS, be.USDC_BRIDGE_ABI, key_path,
                                  method, params, True)
    if not common.is_ok(result):
        raise RuntimeError(f"USDCBridge.{method}({params}) failed: {result}")


def bind_evm_bridges(tracer, key_path: str) -> dict:
    for chain, bridge in EXTRA_EVM_BRIDGES.items():
        owner_call(key_path, "setEvmBridge",
                   {"dstChainId": str(chain), "evmBridge": f"0x{bridge:040x}"})
    bound = {}
    deadline = time.time() + 120
    for chain in CHAINS:
        while True:
            try:
                bound[chain] = be.require_evm_bridge(chain)
                break
            except RuntimeError:
                if time.time() > deadline:
                    raise
                time.sleep(2)
        tracer.log(f"  chain {chain} -> EVM bridge 0x{bound[chain]:040x}")
    for chain, bridge in EXTRA_EVM_BRIDGES.items():
        if bound[chain] != bridge:
            raise RuntimeError(f"getEvmBridge({chain}) is 0x{bound[chain]:040x}, set 0x{bridge:040x}")
    return bound


def mint_nonce() -> int:
    return int(common.run_getter(be.USDC_BRIDGE_ADDRESS, be.USDC_BRIDGE_ABI,
                                 "getNonces")["mintNonce"])


def mint_inventory(tracer, gql, key_path: str, msig_legacy: str) -> None:
    for chain in CHAINS:
        amount = sum(a for c, _, a in RECORDS if c == chain)
        nonce = mint_nonce() + 1
        tracer.log(f"  mintAndSendBridged {amount} for chain {chain}, nonce {nonce}")
        owner_call(key_path, "mintAndSendBridged",
                   {"recipient": msig_legacy, "value": str(amount),
                    "nonce": str(nonce), "chainId": str(chain)})
        # The next mint reads its nonce from the bridge; the node answers
        # getters from state that can lag the mint just sent.
        deadline = time.time() + 120
        while mint_nonce() < nonce:
            if time.time() > deadline:
                raise RuntimeError(f"mint nonce {nonce} never reached the bridge state")
            time.sleep(2)
    total = sum(a for _, _, a in RECORDS)
    msig_id = msig_legacy.split(":", 1)[1]
    deadline = time.time() + 120
    while time.time() < deadline:
        ecc = common.get_account(f"{msig_id}::{msig_id}").get("ecc_balance", {}) or {}
        if int(ecc.get(str(be.USDC_TOKEN_ID), 0)) >= total:
            return
        time.sleep(2)
    raise RuntimeError(f"ECC[{be.USDC_TOKEN_ID}]={total} not credited to {msig_legacy}")


def withdraw_and_capture(tracer, gql, msig, msig_abi, chain, recipient, amount) -> dict:
    be.require_bridged_headroom(amount, chain)
    be.require_evm_bridge(chain)
    baseline = {n["id"] for n in gql.fetch_bridge_extouts(limit=500)}
    common.call_contract(msig, msig_abi, MSIG_KEY_PATH, "sendTransaction", {
        "dest": be.USDC_BRIDGE_ADDRESS_LEGACY,
        "value": "1000000000",
        "cc": {str(be.USDC_TOKEN_ID): str(amount)},
        "bounce": True,
        "flags": 1,
        "payload": be.encode_initiate_withdrawal_body(chain, recipient),
    }, True)
    deadline = time.time() + EVENT_TIMEOUT_S
    while time.time() < deadline:
        new = [n for n in gql.fetch_bridge_extouts(limit=500)
               if n.get("dst") == be.WITHDRAWAL_EVENT_DST and n["id"] not in baseline]
        if len(new) > 1:
            raise RuntimeError(f"{len(new)} new events for one withdrawal: {[n['id'] for n in new]}")
        if new and new[0].get("block_id"):
            return new[0]
        time.sleep(2)
    raise RuntimeError(f"no WithdrawalInitiated for chain {chain} recipient {recipient}")


def main():
    tracer = be.Tracer()
    gql = be.GqlClient(GRAPHQL_URL)
    os.makedirs(WORK_DIR, exist_ok=True)
    common.NETWORK = os.environ.setdefault("NETWORK", "http://127.0.0.1:80")
    common.set_config({"async_call": "false"})
    common.setup()

    if not common.wait_account_status(be.USDC_BRIDGE_ADDRESS, "Active", timeout=180):
        raise SystemExit(f"USDCBridge not active at {be.USDC_BRIDGE_ADDRESS}")
    key_path = be.USDC_BRIDGE_KEYS
    be.materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, key_path)
    be.validate_usdc_bridge_key(tracer, key_path, overridden=False)

    tracer.log_phase("Binding EVM bridges")
    bound = bind_evm_bridges(tracer, key_path)

    msig, msig_abi = deploy_multisig(
        tracer, gql, work_dir=WORK_DIR, msig_key_path=MSIG_KEY_PATH,
        usdc_bridge_key_path=key_path, is_shellnet=False, mint=False,
    )
    msig_legacy = f"0:{msig.split('::', 1)[1]}"
    tracer.log_phase("Minting inventory")
    mint_inventory(tracer, gql, key_path, msig_legacy)

    lines = []
    for chain, recipient, amount in RECORDS:
        tracer.log_phase(f"initiateWithdrawal chain={chain} recipient={recipient} amount={amount}")
        event = withdraw_and_capture(tracer, gql, msig, msig_abi, chain, recipient, amount)
        tracer.log(f"  event {event['id']} in block {event['block_id']}, "
                   f"dstBridge 0x{bound[chain]:040x}")
        lines += [str(chain), recipient, str(amount), str(be.USDC_TOKEN_ID), msig_legacy,
                  event["boc"], event["block_id"], ""]

    with open(OUT, "w") as f:
        f.write("\n".join(lines) + "\n")
    tracer.log_phase(f"Wrote {len(RECORDS)} records to {os.path.abspath(OUT)}")


if __name__ == "__main__":
    main()
