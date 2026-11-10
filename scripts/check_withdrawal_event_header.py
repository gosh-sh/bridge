#!/usr/bin/env python3
"""Check that the tracked eccBridge emits WithdrawalInitiated with the header
the withdrawal proof (Circuit 4) is built for.

Circuit 4 proves the bridge's `WithdrawalInitiated` external outbound message,
whose header it reads as `ext_out_msg_info$11 src:MsgAddressInt
dest:MsgAddressExt created_lt created_at`. `sold` 0.81.x emits events with
another header, tag `$110110` and one more field at its end: the user's tokens
burn, and no proof of the withdrawal can be built. Which one a
bridge emits is decided by the compiler alone, so this runs the tracked
`eccBridge.tvc` itself through a withdrawal in tvm-debugger and reads the
header of the event it emits.

    TVM_DEBUGGER=... TVM_CLI=... scripts/check_withdrawal_event_header.py [eccBridge.tvc]

The default `.tvc` is the one `contracts/an/exchange/Makefile` says eccBridge is
built into. Exits non-zero if the bridge does not emit exactly one
`WithdrawalInitiated` with the `$11` header.
"""

import base64
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[1]
DEBUGGER = os.environ.get("TVM_DEBUGGER", "tvm-debugger")
TVM_CLI = os.environ.get("TVM_CLI", "tvm-cli")
BRIDGE = "0:" + "1a" * 32
SENDER = "0:" + "ab" * 32
EVM_BRIDGE = str(0x3FB082A905A895151224F6D93BD074E816338638)


def tracked_bridge() -> pathlib.Path:
    makefile = (REPO / "contracts/an/exchange/Makefile").read_text()
    version = re.search(r"^SOLD_VERSION_eccBridge := (\S+)$", makefile, re.M)[1]
    return REPO / f"contracts/an/{version}_compiled/exchange/eccBridge.tvc"


def root_bits(boc_b64: str) -> str:
    """The data bits of a bag of cells' first root cell."""
    b = base64.b64decode(boc_b64)
    if b[:4] != bytes.fromhex("b5ee9c72"):
        raise ValueError("not a bag of cells")
    flags, off = b[4], b[5]
    size = flags & 7
    i = 6
    cells = int.from_bytes(b[i:i + size], "big")
    roots = int.from_bytes(b[i + size:i + 2 * size], "big")
    i += 3 * size + off
    root = int.from_bytes(b[i:i + size], "big")
    i += size * roots
    if flags & 0x80:
        i += off * cells
    for n in range(cells):
        d1, d2 = b[i], b[i + 1]
        data = b[i + 2:i + 2 + (d2 + 1) // 2]
        i += 2 + (d2 + 1) // 2 + (d1 & 7) * size
        if n == root:
            bits = "".join(f"{x:08b}" for x in data)
            if d2 % 2:  # the last byte is padded with 1 and zeros
                bits = bits.rstrip("0")[:-1]
            return bits
    raise ValueError("no root cell")


def main() -> int:
    tvc = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else tracked_bridge()
    abi = tvc.with_name("eccBridge.abi.json")
    keys_rs = (REPO / "crates/ackinacki-bridge/src/test_keys.rs").read_text()
    keys = {n: re.search(rf'PAIR_{n.upper()}: &str = "([0-9a-f]+)"', keys_rs)[1]
            for n in ("public", "secret")}
    with tempfile.TemporaryDirectory(prefix="withdrawal-event-") as tmp:
        state = pathlib.Path(tmp) / "bridge.tvc"
        shutil.copyfile(tvc, state)
        key_file = pathlib.Path(tmp) / "keys.json"
        key_file.write_text(json.dumps(keys))

        def run(function, args, *extra):
            out = subprocess.run(
                [DEBUGGER, "run", "-i", str(state), "-a", str(abi), "--address", BRIDGE, "--json",
                 "-m", function, "-p", json.dumps(args), *extra],
                capture_output=True, text=True, check=True)
            result = json.loads(out.stdout)
            if result["exit_code"] != 0:
                sys.exit(f"{function}: exit code {result['exit_code']}")
            return result

        header = ["--abi-header", json.dumps({"expire": int(time.time()) + 240})]
        owner = header + ["--sign", str(key_file)]
        run("constructor", {"pubkey": "0x" + keys["public"], "usdcWallet": "0:" + "00" * 32}, *header)
        run("setEvmBridge", {"dstChainId": "1", "evmBridge": EVM_BRIDGE}, *owner)
        run("setBridgeToken", {"chainId": "1", "evmToken": "68", "tokenId": 3,
                               "maxTransfer": "50000000", "maxTvl": "10000000000",
                               "depositsEnabled": True}, *owner)
        run("mintAndSendBridged", {"recipient": SENDER, "value": "1000000", "nonce": "1",
                                   "chainId": "1"}, *owner)
        result = run("initiateWithdrawal", {"dstChainId": "1", "recipient": "11" * 20},
                     "--internal", "--message-source", SENDER, "--message-value", "1000000000",
                     "--message-ecc", json.dumps({"3": "1000000"}))

    events = [m for m in result.get("messages", []) if m.get("type") == "external"]
    if len(events) != 1:
        print(f"{tvc}: a withdrawal emitted {len(events)} external messages, expected 1",
              file=sys.stderr)
        return 1
    boc = events[0]["boc"]
    decoded = json.loads(subprocess.run([TVM_CLI, "-j", "decode", "msg", boc, "--abi", str(abi)],
                                        capture_output=True, text=True, check=True).stdout)
    if "WithdrawalInitiated" not in decoded.get("BodyCall", {}):
        print(f"{tvc}: the withdrawal's event is not WithdrawalInitiated: {decoded}", file=sys.stderr)
        return 1
    bits = root_bits(boc)
    # $11 then the source, addr_std$10. The 0.81 header is $11 then the extension tag 01.
    if not bits.startswith("1110"):
        kind = "the 0.81 header ($110110)" if bits.startswith("110110") else f"header bits {bits[:8]}"
        print(f"{tvc}: WithdrawalInitiated has {kind}; Circuit 4 proves only ext_out_msg_info$11. "
              "Build eccBridge with the compiler contracts/an/exchange/Makefile pins.", file=sys.stderr)
        return 1
    print(f"ok  {tvc.relative_to(REPO) if tvc.is_relative_to(REPO) else tvc}: WithdrawalInitiated "
          "has ext_out_msg_info$11")
    return 0


if __name__ == "__main__":
    sys.exit(main())
