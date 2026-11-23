#!/usr/bin/env python3
"""Keeps burning small USDC withdrawals and checks that the relayer pays them.

The test is a withdrawing wallet and nothing more. It calls the multisig's
`sendTransaction` with the bridge's `initiateWithdrawal` and stops there: the
Circuit 4 proof and `withdrawByProof` are the work of the bridge's withdrawal
relayer, and this is what the test exercises. A withdrawal counts as paid when
`WithdrawalByProofExecuted` for the recipient and amount appears on the EVM
bridge. It does not wait for one payout before the next burn.

A burn counts only once the chain shows it: the multisig's message to the
bridge, run by the bridge and not refunded. While a send's outcome is open no
other burn goes out, and an external message that has expired cannot land, so
a burn is never repeated: a send is written off only once the chain has
finalized past its expiry without it. The run survives restarts and ends when the bridge
keeps refusing burns, which is how it meets a withdrawal limit.

`spent.json` in SOAK_DIR holds what left the sender, for the deposit test's
balance check (its SOAK_SPENT_FILE).

The settings come from the environment. The script passes the path of the
multisig's key file to tvm-cli and does not read it.

  SOAK_FROM            Acki Nacki multisig, `dapp_id::account_id` (required)
  SOAK_FROM_KEYS       its key file (required)
  SOAK_TO              EVM recipient, 0x... (required)
  SOAK_DIR             holds ledger.json, results.jsonl, status.json (required)
  SOAK_RPC_URL         EVM RPC (required)
  BRIDGE_ADDRESS       the EVM bridge (required)
  BRIDGE_GQL_ENDPOINT  Acki Nacki GraphQL URL (required)
  SOAK_AN_URL          Acki Nacki endpoint for tvm-cli (required)
  SOAK_TVM_CLI         tvm-cli (`tvm-cli`)
  SOAK_ABI_DIR         holds USDCBridge.abi.json and the multisig's ABI
                       (crates/ackinacki-bridge/abi)
  SOAK_AN_BRIDGE       the Acki Nacki bridge (`0:1a1a...1a1a`)
  SOAK_CHAIN_ID        EVM chain id (421614)
  SOAK_AMOUNT          USDC per withdrawal (`0.0005`)
  SOAK_INTERVAL_S      from the start of one burn to the next (600)
  SOAK_POLL_S          time between checks (60)
  SOAK_RETRY_S         wait after a burn that did not go out (300)
  SOAK_MAX_REFUSED     refused burns in a row before stopping (10)
  SOAK_LOG_SPAN        blocks per eth_getLogs (5000)

Exit codes: 0 on SIGTERM, SIGINT or a paid `--once`; 10 when it stopped. A
supervisor must not restart it after 10.
"""

import argparse
import http.client
import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Optional

EXIT_STOPPED = 10
USDC_ECC_ID = 3

# keccak256("WithdrawalByProofExecuted(uint256,address,uint256,uint256,address)")
PAID_TOPIC = "0x88738ab1295650557339cebd9882c9db35ae232be3a8fcb180054213fe0dad92"

# How long one tvm-cli call may take.
CALL_TIMEOUT_S = 150
# After this, a message that is not on chain has expired and cannot land:
# the call's timeout plus tvm-cli's 60 s message lifetime, with a margin.
SEND_WINDOW_S = 300
# Value the multisig attaches for the bridge's fees, in nano.
SEND_VALUE = "1000000000"
# A message may be stamped a little before this host's clock.
CLOCK_SKEW_S = 60
RPC_TIMEOUT_S = 30
KEPT_BURN_IDS = 200
# Each payout check reads this many blocks again, in case a node behind a load
# balancer answered for blocks it did not have yet.
LOG_OVERLAP = 2400
KEPT_PAID_LOGS = 500
READ_ERRORS = (OSError, ValueError, KeyError, TypeError, AttributeError, IndexError,
               http.client.HTTPException, subprocess.SubprocessError)


@dataclass
class Config:
    sender: str
    keys: str
    to: str
    directory: Path
    rpc_url: str
    bridge: str
    gql_url: str
    an_url: str
    tvm_cli: str
    abi_dir: Path
    an_bridge: str
    chain_id: int
    amount: str
    units: int
    interval_s: float
    poll_s: float
    retry_s: float
    max_refused: int
    log_span: int

    @classmethod
    def from_env(cls) -> "Config":
        required = ("SOAK_FROM", "SOAK_FROM_KEYS", "SOAK_TO", "SOAK_DIR", "SOAK_RPC_URL",
                    "BRIDGE_ADDRESS", "BRIDGE_GQL_ENDPOINT", "SOAK_AN_URL")
        missing = [k for k in required if not os.environ.get(k)]
        if missing:
            sys.exit(f"set {', '.join(missing)}")
        env = os.environ.get
        amount = env("SOAK_AMOUNT", "0.0005")
        default_abi = Path(__file__).resolve().parents[3] / "crates" / "ackinacki-bridge" / "abi"
        return cls(
            sender=os.environ["SOAK_FROM"],
            keys=os.environ["SOAK_FROM_KEYS"],
            to=os.environ["SOAK_TO"],
            directory=Path(os.environ["SOAK_DIR"]),
            rpc_url=os.environ["SOAK_RPC_URL"],
            bridge=os.environ["BRIDGE_ADDRESS"],
            gql_url=os.environ["BRIDGE_GQL_ENDPOINT"],
            an_url=os.environ["SOAK_AN_URL"],
            tvm_cli=env("SOAK_TVM_CLI", "tvm-cli"),
            abi_dir=Path(env("SOAK_ABI_DIR", str(default_abi))),
            an_bridge=env("SOAK_AN_BRIDGE", "0:" + "1a" * 32),
            chain_id=int(env("SOAK_CHAIN_ID", "421614")),
            amount=amount,
            units=int(Decimal(amount) * 1_000_000),
            interval_s=float(env("SOAK_INTERVAL_S", "600")),
            poll_s=float(env("SOAK_POLL_S", "60")),
            retry_s=float(env("SOAK_RETRY_S", "300")),
            max_refused=int(env("SOAK_MAX_REFUSED", "10")),
            log_span=int(env("SOAK_LOG_SPAN", "5000")),
        )


def holds_usdc(message: dict, units: int) -> bool:
    return any(int(float(c["currency"])) == USDC_ECC_ID and int(c["value"]) == units
               for c in message.get("value_other") or [])


class Soak:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.stop = threading.Event()
        cfg.directory.mkdir(parents=True, exist_ok=True)
        self.results = cfg.directory / "results.jsonl"
        self.status = cfg.directory / "status.json"
        self.spent = cfg.directory / "spent.json"
        self.ledger_path = cfg.directory / "ledger.json"
        self.ledger = self.load_ledger()
        self.last_error = ""
        self.retry_at = 0.0
        self.halt: Optional[str] = None
        self.short_of_balance = False

    # --- files ---------------------------------------------------------

    def load_ledger(self) -> dict:
        try:
            return json.loads(self.ledger_path.read_text())
        except FileNotFoundError:
            return {
                "next_round": 1,
                "current": None,
                "burned": 0,
                "burned_units": 0,
                "in_flight_units": 0,
                "refused": 0,
                "paid": 0,
                "paid_units": 0,
                "unpaid": [],
                "burn_ids": [],
                "log_cursor": None,
                "last_send_at": None,
            }

    @staticmethod
    def write_atomic(path: Path, text: str):
        tmp = path.with_suffix(path.suffix + ".tmp")
        with tmp.open("w") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)

    def save_ledger(self):
        # The spent total first: a reader may count a burn early, never late.
        self.write_atomic(self.spent, json.dumps({
            "burned_units": self.ledger["burned_units"],
            "in_flight_units": self.ledger["in_flight_units"],
        }))
        self.write_atomic(self.ledger_path, json.dumps(self.ledger))

    def record(self, entry: dict):
        entry = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **entry}
        line = json.dumps(entry, sort_keys=True)
        with self.results.open("a") as f:
            f.write(line + "\n")
        print(line, flush=True)

    def write_status(self):
        ledger = self.ledger
        oldest = min((u["at"] for u in ledger["unpaid"]), default=None)
        self.write_atomic(self.status, json.dumps({
            "updated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "burned": ledger["burned"],
            "burned_units": ledger["burned_units"],
            "refused": ledger["refused"],
            "paid": ledger["paid"],
            "paid_units": ledger["paid_units"],
            "pending": len(ledger["unpaid"]),
            "oldest_pending_s": round(time.time() - oldest) if oldest else 0,
            "send_open": bool(ledger["current"]),
            "last_error": self.last_error[:300],
        }, sort_keys=True))

    # --- Acki Nacki ----------------------------------------------------

    def gql(self, query: str) -> dict:
        request = urllib.request.Request(
            self.cfg.gql_url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": "withdraw-soak/1"},
        )
        with urllib.request.urlopen(request, timeout=RPC_TIMEOUT_S) as response:
            body = json.load(response)
        if body.get("errors") or not body.get("data"):
            raise ValueError(str(body.get("errors"))[:200])
        return body["data"]["blockchain"]

    def account_args(self) -> str:
        dapp, account = self.cfg.sender.split("::")
        return f'account_id:"{account}",dapp_id:"{dapp}"'

    def sender_units(self) -> int:
        data = self.gql(f"query{{blockchain{{account({self.account_args()})"
                        "{info{balance_other{currency value(format:DEC)}}}}}")["account"]
        for item in (data.get("info") or {}).get("balance_other") or []:
            if int(float(item["currency"])) == USDC_ECC_ID:
                return int(item["value"])
        return 0

    def find_burn(self, since: float) -> Optional[dict]:
        """The multisig's message to the bridge sent since `since`."""
        data = self.gql(
            f"query{{blockchain{{account({self.account_args()}){{"
            "messages(last:25,msg_type:[IntOut]){edges{node{id created_at dst "
            "value_other{currency value(format:DEC)} dst_transaction{id aborted out_msgs}}}}}}}"
        )["account"]
        bridge = self.cfg.an_bridge.split(":")[-1]
        for edge in data["messages"]["edges"]:
            message = edge["node"]
            if message["created_at"] < since - CLOCK_SKEW_S \
                    or message["id"] in self.ledger["burn_ids"]:
                continue
            if (message.get("dst") or "").endswith(bridge) and holds_usdc(message, self.cfg.units):
                return message
        return None

    def refused_by_bridge(self, message: dict) -> Optional[bool]:
        """The bridge refunds a refused burn: the same USDC back to the sender.
        None while its outgoing messages are not indexed yet."""
        run = message["dst_transaction"]
        if run.get("aborted"):
            return True
        ids = run.get("out_msgs") or []
        if not ids:
            return False
        fields = " ".join(f'm{i}:message(hash:"{h}"){{dst value_other{{currency value(format:DEC)}}}}'
                          for i, h in enumerate(ids))
        data = self.gql(f"query{{blockchain{{{fields}}}}}")
        out = [data.get(f"m{i}") for i in range(len(ids))]
        if not all(out):
            return None
        sender = self.cfg.sender.split("::")[1]
        return any((m.get("dst") or "").endswith(sender) and holds_usdc(m, self.cfg.units)
                   for m in out)

    def indexed_through(self) -> int:
        """The time the index has certainly reached: the finalized time, never
        past the newest block it holds, so a stalled index writes nothing off."""
        data = self.gql("query{blockchain{finalizedTimestamp "
                        "blocks(last:1){edges{node{gen_utime}}}}}")
        edges = data["blocks"]["edges"]
        return min(data["finalizedTimestamp"], edges[0]["node"]["gen_utime"]) if edges else 0

    def resolve_current(self):
        """Settles the open send from what the chain shows."""
        ledger = self.ledger
        current = ledger["current"]
        if not current:
            return
        try:
            message = self.find_burn(current["started"])
            run_done = bool(message and message.get("dst_transaction"))
            refused = self.refused_by_bridge(message) if run_done else False
            indexed = 0 if message else self.indexed_through()
        except READ_ERRORS as e:
            self.last_error = f"burn check: {type(e).__name__}"
            return
        if message is None:
            # Only an index past the message's expiry shows it never landed.
            if indexed > current["started"] + SEND_WINDOW_S:
                ledger["current"] = None
                ledger["in_flight_units"] = 0
                self.save_ledger()
                self.retry_at = time.time() + self.cfg.retry_s
                self.record({"status": "not_sent", "round": current["round"],
                             "message": self.last_error[:300]})
            return
        if refused is None:
            return
        if not run_done:
            return  # the bridge has not run it yet
        tx_id = message["id"]
        ledger["burn_ids"] = (ledger["burn_ids"] + [tx_id])[-KEPT_BURN_IDS:]
        ledger["current"] = None
        ledger["in_flight_units"] = 0
        ledger["next_round"] = current["round"] + 1
        ledger["last_send_at"] = current["started"]
        if refused:
            ledger["refused"] += 1
            ledger["refused_in_row"] = ledger.get("refused_in_row", 0) + 1
            self.save_ledger()
            self.record({"status": "refused", "round": current["round"], "tx": tx_id})
            if ledger["refused_in_row"] >= self.cfg.max_refused:
                self.last_error = f"the bridge refused {ledger['refused_in_row']} burns in a row"
                self.halt = "refused"
            return
        ledger["refused_in_row"] = 0
        self.last_error = ""
        ledger["burned"] += 1
        ledger["burned_units"] += self.cfg.units
        ledger["unpaid"].append({"at": current["started"], "round": current["round"]})
        self.save_ledger()
        self.record({"status": "burned", "round": current["round"], "tx": tx_id})

    # --- sending -------------------------------------------------------

    def tvm(self, *args, timeout: float) -> subprocess.CompletedProcess:
        return subprocess.run([self.cfg.tvm_cli, "-j", *args], cwd=self.cfg.directory,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              timeout=timeout)

    def send(self) -> bool:
        """Sends one burn. False when nothing went out."""
        cfg = self.cfg
        try:
            balance = self.sender_units()
            if balance < cfg.units:
                if not self.short_of_balance:
                    self.record({"status": "no_balance", "units": balance})
                self.short_of_balance = True
                return False
            self.short_of_balance = False
            body = self.tvm("body", "--abi", str(cfg.abi_dir / "USDCBridge.abi.json"),
                            "initiateWithdrawal",
                            json.dumps({"dstChainId": str(cfg.chain_id),
                                        "recipient": cfg.to[2:].lower()}), timeout=30)
            if body.returncode != 0:
                raise ValueError("tvm-cli body failed")
            payload = json.loads(body.stdout)["Message"]
        except (*READ_ERRORS, subprocess.TimeoutExpired) as e:
            self.last_error = f"before the burn: {type(e).__name__}: {str(e)[:120]}"
            return False
        round_number = self.ledger["next_round"]
        self.ledger["current"] = {"round": round_number, "started": time.time()}
        self.ledger["in_flight_units"] = cfg.units
        self.save_ledger()
        params = {"dest": cfg.an_bridge, "value": SEND_VALUE,
                  "cc": {str(USDC_ECC_ID): str(cfg.units)},
                  "bounce": True, "flags": 1, "payload": payload}
        try:
            call = self.tvm("-u", cfg.an_url, "callx",
                            "--abi", str(cfg.abi_dir / "UpdateCustodianMultisigWallet.abi.json"),
                            "--addr", cfg.sender, "--keys", cfg.keys,
                            "-m", "sendTransaction", json.dumps(params), timeout=CALL_TIMEOUT_S)
            if call.returncode != 0:
                self.last_error = f"tvm-cli call: {(call.stderr or call.stdout)[-200:]}"
        except subprocess.TimeoutExpired:
            self.last_error = "tvm-cli call timed out"
        except OSError as e:
            self.last_error = f"tvm-cli call: {type(e).__name__}"
        return True

    # --- payouts -------------------------------------------------------

    def rpc(self, method: str, params: list):
        request = urllib.request.Request(
            self.cfg.rpc_url,
            data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": "withdraw-soak/1"},
        )
        with urllib.request.urlopen(request, timeout=RPC_TIMEOUT_S) as response:
            body = json.load(response)
        if "error" in body:
            raise ValueError(f"{method}: {str(body['error'])[:160]}")
        return body["result"]

    def check_payouts(self):
        ledger = self.ledger
        try:
            head = int(self.rpc("eth_blockNumber", []), 16)
            if ledger["log_cursor"] is None:
                ledger["log_cursor"] = head + 1
                self.save_ledger()
                return
            recipient = "0x" + self.cfg.to[2:].lower().rjust(64, "0")
            first = ledger.setdefault("log_start", ledger["log_cursor"])
            start = max(first, ledger["log_cursor"] - LOG_OVERLAP)
            while start <= head:
                end = min(head, start + self.cfg.log_span - 1)
                logs = self.rpc("eth_getLogs", [{
                    "address": self.cfg.bridge,
                    "topics": [PAID_TOPIC, None, recipient],
                    "fromBlock": hex(start),
                    "toBlock": hex(end),
                }])
                for log in logs:
                    self.take_payout(log)
                start = end + 1
                ledger["log_cursor"] = max(ledger["log_cursor"], start)
                self.save_ledger()
        except READ_ERRORS as e:
            self.last_error = f"payout check: {type(e).__name__}"

    def take_payout(self, log: dict):
        amount = int(log["data"][2:66], 16)
        ledger = self.ledger
        key = f"{log.get('transactionHash')}:{log.get('logIndex')}"
        if amount != self.cfg.units or key in ledger.setdefault("paid_logs", []):
            return
        ledger["paid_logs"] = (ledger["paid_logs"] + [key])[-KEPT_PAID_LOGS:]
        ledger["paid"] += 1
        ledger["paid_units"] += amount
        burn = ledger["unpaid"].pop(0) if ledger["unpaid"] else None
        self.record({
            "status": "paid",
            "round": burn["round"] if burn else None,
            "seconds": round(time.time() - burn["at"]) if burn else None,
            "tx_hash": log.get("transactionHash"),
            "submitter": "0x" + log["data"][-40:],
        })

    def run(self, once: bool) -> int:
        burned_before = self.ledger["burned"]
        sent_here = False
        while not self.stop.is_set():
            self.resolve_current()
            self.check_payouts()
            self.write_status()
            if self.halt:
                self.record({"status": "stopped", "reason": self.halt,
                             "message": self.last_error[:500]})
                self.write_status()
                return EXIT_STOPPED
            burned_here = self.ledger["burned"] > burned_before
            if once and burned_here and not self.ledger["unpaid"]:
                return 0
            last = self.ledger.get("last_send_at") or 0
            due = max(self.retry_at, 0 if once else last + self.cfg.interval_s)
            open_send = bool(self.ledger["current"])
            if not open_send and not (once and burned_here) and time.time() >= due:
                sent_here = self.send()
                if not sent_here:
                    self.retry_at = time.time() + self.cfg.retry_s
                continue
            wait = self.cfg.poll_s if once or open_send else min(self.cfg.poll_s, due - time.time())
            self.stop.wait(max(0.0, wait))
        return 0

    def on_signal(self, _signum, _frame):
        # A tvm-cli call in progress is left to finish: its burn gets counted.
        self.stop.set()


def main() -> int:
    parser = argparse.ArgumentParser(description="Continuous USDC withdrawal test")
    parser.add_argument("--once", action="store_true",
                        help="burn one withdrawal, wait for its payout and exit")
    args = parser.parse_args()
    soak = Soak(Config.from_env())
    signal.signal(signal.SIGTERM, soak.on_signal)
    signal.signal(signal.SIGINT, soak.on_signal)
    return soak.run(args.once)


if __name__ == "__main__":
    sys.exit(main())
