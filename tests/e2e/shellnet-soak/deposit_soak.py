#!/usr/bin/env python3
"""Keeps sending small USDC deposits and checks that the relayers credit them.

The test is a depositor and nothing more. The `ackinacki-bridge` CLI sends
`approve` and `deposit` and is stopped the moment it starts waiting for the
block anchor, so it never builds a proof or sends `finalizeDeposit`: that is
the work of the bridge's relayers, and this is what the test exercises. A
deposit counts as finalized when its voucher is deployed on Acki Nacki, and as
credited once the recipient's balance has grown by the finalized total.

The run ends at a limit (the bridge's TVL cap, the wallet's USDC or gas), after
repeated failures that spend gas, on a deposit only a person can settle, or
when the recipient stays short of the finalized total: sending more would only
leave more USDC behind. It survives restarts: the CLI's operation records say
what is already on chain. SOAK_TO must not be spent from during a run.

The settings come from the environment. The wallet key stays in the CLI's own
`BRIDGE_DEPOSIT_PRIVATE_KEY`, next to `BRIDGE_DEPOSIT_FROM`; this script does
not read it.

  SOAK_TO              Acki Nacki recipient, `dapp_id::account_id` (required)
  SOAK_DIR             holds ledger.json, results.jsonl, status.json (required)
  SOAK_CLI_STATE_DIR   the CLI's --state-dir, as this script sees it (required)
  BRIDGE_GQL_ENDPOINT  Acki Nacki GraphQL URL (required)
  SOAK_CLI             command that runs the CLI (`ackinacki-bridge`)
  SOAK_NETWORK         EVM network (`arbitrum-sepolia`)
  SOAK_AMOUNT          USDC per deposit (`0.001`)
  SOAK_INTERVAL_S      from the start of one deposit to the next (600); it does
                       not wait for earlier deposits to be credited
  SOAK_POLL_S          time between credit checks (60)
  SOAK_RETRY_S         wait after a failed send (300)
  SOAK_MAX_FAILURES    failures in a row before stopping (10)
  SOAK_SHORTFALL_S     how long the balance may lag the vouchers (1800)
  SOAK_RPC_URL         EVM RPC for the wallet's gas check (optional)
  SOAK_MIN_GAS_WEI     stop below this much gas in BRIDGE_DEPOSIT_FROM (10^14)

Exit codes: 0 on SIGTERM, SIGINT or a credited `--once`; 10 when it stopped at
a limit or for a person. A supervisor must not restart it after 10.
"""

import argparse
import calendar
import collections
import http.client
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

EXIT_STOPPED = 10
USDC_ECC_ID = 3

# The CLI's stages of an operation (`OpStage`).
UNRESOLVED = {"requested", "signed"}
# `credited` is not here: the test never takes a deposit that far itself.
ON_CHAIN = {"confirmed", "anchored", "proved", "finalizing"}
CLOSED = {"failed", "abandoned"}

# Refusals that mean a limit was reached, not that something broke.
LIMIT_MARKERS = (
    "would exceed its tvlCap",
    "would exceed the AN token maxTvl",
    "TvlCapReached",
    "the deposit needs",
    "insufficient funds",
)
# The `approve` or the `deposit` failed on chain or on the way there: these
# are the failures that can cost gas.
GAS_FAILURES = {21, 22}
# The deposit is in the EVM bridge and no retry will finalize it.
NEEDS_PERSON = {33, 35, 37}

# The CLI proves only after this long with the block anchored and no voucher.
# It is stopped before that; this keeps the proof out of reach regardless.
RELAYER_GRACE_S = 365 * 24 * 3600
ANCHOR_TIMEOUT_S = 5
# A send that takes longer is interrupted and looked at again.
SEND_TIMEOUT_S = 3600
# After the SIGTERM of a timeout, how long the CLI gets before SIGKILL.
KILL_AFTER_S = 180
GQL_TIMEOUT_S = 30
GQL_BATCH = 50
READ_ERRORS = (OSError, ValueError, KeyError, TypeError, AttributeError, http.client.HTTPException)
HEX32 = re.compile(r"^[0-9a-f]{64}$")


@dataclass
class Config:
    to: str
    directory: Path
    cli_state: Path
    gql: str
    cli: list
    network: str
    amount: str
    interval_s: float
    poll_s: float
    retry_s: float
    max_failures: int
    shortfall_s: float
    rpc_url: Optional[str]
    wallet: Optional[str]
    min_gas_wei: int

    @classmethod
    def from_env(cls) -> "Config":
        required = ("SOAK_TO", "SOAK_DIR", "SOAK_CLI_STATE_DIR", "BRIDGE_GQL_ENDPOINT")
        missing = [k for k in required if not os.environ.get(k)]
        if missing:
            sys.exit(f"set {', '.join(missing)}")
        env = os.environ.get
        return cls(
            to=os.environ["SOAK_TO"],
            directory=Path(os.environ["SOAK_DIR"]),
            cli_state=Path(os.environ["SOAK_CLI_STATE_DIR"]),
            gql=os.environ["BRIDGE_GQL_ENDPOINT"],
            cli=shlex.split(env("SOAK_CLI", "ackinacki-bridge")),
            network=env("SOAK_NETWORK", "arbitrum-sepolia"),
            amount=env("SOAK_AMOUNT", "0.001"),
            interval_s=float(env("SOAK_INTERVAL_S", "600")),
            poll_s=float(env("SOAK_POLL_S", "60")),
            retry_s=float(env("SOAK_RETRY_S", "300")),
            max_failures=int(env("SOAK_MAX_FAILURES", "10")),
            shortfall_s=float(env("SOAK_SHORTFALL_S", "1800")),
            rpc_url=env("SOAK_RPC_URL"),
            wallet=env("BRIDGE_DEPOSIT_FROM"),
            min_gas_wei=int(env("SOAK_MIN_GAS_WEI", str(10**14))),
        )

    def command(self, resume: Optional[str]) -> list:
        cmd = self.cli + [
            "deposit",
            "--network", self.network,
            "--amount", self.amount,
            "--to", self.to,
            "--qr-mode", "local-key",
            "--anchor-timeout-s", str(ANCHOR_TIMEOUT_S),
            "--relayer-grace-s", str(RELAYER_GRACE_S),
            "--yes", "--non-interactive", "--json",
        ]
        return cmd + ["--resume", resume] if resume else cmd


@dataclass
class Outcome:
    exit_code: int
    op_id: Optional[str] = None
    message: str = ""
    events: dict = field(default_factory=dict)


def parse_time(text: Optional[str]) -> float:
    try:
        return float(calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ")))
    except (TypeError, ValueError):
        return time.time()


class Soak:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.stop = threading.Event()
        self.child: Optional[subprocess.Popen] = None
        cfg.directory.mkdir(parents=True, exist_ok=True)
        self.results = cfg.directory / "results.jsonl"
        self.status = cfg.directory / "status.json"
        self.ledger_path = cfg.directory / "ledger.json"
        self.ledger = self.load_ledger()
        self.unresolved: Optional[str] = None
        self.gas_failures = 0
        self.resume_failures = 0
        self.last_error = ""
        self.recipient_delta: Optional[int] = None
        # (time, finalized units): what the recipient must have by now.
        self.finalized_history = collections.deque()
        self.halt: Optional[str] = None

    # --- files ---------------------------------------------------------

    def load_ledger(self) -> dict:
        try:
            return json.loads(self.ledger_path.read_text())
        except FileNotFoundError:
            return {
                "pending": {},
                "done": [],
                "sent": 0,
                "finalized": 0,
                "finalized_units": 0,
                "failed": 0,
                "baseline_units": None,
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
        self.write_atomic(self.ledger_path, json.dumps(self.ledger))

    def record(self, entry: dict):
        entry = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), **entry}
        line = json.dumps(entry, sort_keys=True)
        with self.results.open("a") as f:
            f.write(line + "\n")
        print(line, flush=True)

    def write_status(self):
        pending = self.ledger["pending"]
        oldest = min((p["sent_at"] for p in pending.values()), default=None)
        expected = self.ledger["finalized_units"]
        self.write_atomic(self.status, json.dumps({
            "updated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "sent": self.ledger["sent"],
            "finalized": self.ledger["finalized"],
            "failed": self.ledger["failed"],
            "pending": len(pending),
            "oldest_pending_s": round(time.time() - oldest) if oldest else 0,
            "finalized_units": expected,
            "recipient_delta_units": self.recipient_delta,
            "balance_matches": None if self.recipient_delta is None
            else self.recipient_delta >= expected,
            "last_error": self.last_error[:300],
        }, sort_keys=True))

    # --- what the CLI's records say is on chain ------------------------

    def sync(self):
        """Takes new operations from the CLI's state directory."""
        ledger = self.ledger
        done = set(ledger["done"])
        self.unresolved = None
        changed = False
        for path in sorted(self.cfg.cli_state.glob("*.json")):
            op = path.stem
            if op in done or op in ledger["pending"]:
                continue
            try:
                rec = json.loads(path.read_text())
            except (OSError, ValueError) as e:
                self.last_error = f"operation {op}: {e}"
                continue
            stage = rec.get("stage")
            deposit = rec.get("deposit")
            if (rec.get("params") or {}).get("to") != self.cfg.to or stage == "credited":
                # Not a deposit of this run: the directory may have older ones.
                ledger["done"].append(op)
                done.add(op)
                changed = True
            elif stage in UNRESOLVED:
                self.unresolved = op
            elif stage in ON_CHAIN and deposit:
                ledger["pending"][op] = {
                    "deposit_id": deposit.get("deposit_id"),
                    "tx_hash": (rec.get("tx") or {}).get("tx_hash"),
                    "voucher": deposit.get("voucher_account"),
                    "units": (rec.get("params") or {}).get("amount_units", 0),
                    "sent_at": parse_time(rec.get("updated_at")),
                }
                ledger["sent"] += 1
                ledger["last_send_at"] = max(ledger.get("last_send_at") or 0,
                                             parse_time(rec.get("created_at")))
                changed = True
                self.record({"status": "sent", "op_id": op,
                             "deposit_id": deposit.get("deposit_id"),
                             "tx_hash": ledger["pending"][op]["tx_hash"]})
            elif stage in CLOSED:
                ledger["done"].append(op)
                done.add(op)
                changed = True
                if deposit:
                    ledger["failed"] += 1
                    self.record({"status": "failed", "op_id": op,
                                 "deposit_id": deposit.get("deposit_id"),
                                 "failure": rec.get("failure")})
        if changed:
            self.save_ledger()

    # --- Acki Nacki ----------------------------------------------------

    def gql(self, query: str) -> dict:
        request = urllib.request.Request(
            self.cfg.gql,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": "deposit-soak/1"},
        )
        with urllib.request.urlopen(request, timeout=GQL_TIMEOUT_S) as response:
            body = json.load(response)
        if body.get("errors") or not body.get("data"):
            raise ValueError(str(body.get("errors"))[:200])
        return body["data"]["blockchain"]

    def recipient_units(self) -> int:
        dapp, account = self.cfg.to.split("::")
        data = self.gql(
            f'query{{blockchain{{account(account_id:"{account}",dapp_id:"{dapp}")'
            "{info{balance_other{currency value(format:DEC)}}}}}"
        )
        info = (data.get("account") or {}).get("info") or {}
        for item in info.get("balance_other") or []:
            if int(float(item["currency"])) == USDC_ECC_ID:
                return int(item["value"])
        return 0

    def check_credits(self):
        ledger = self.ledger
        try:
            if ledger["baseline_units"] is None:
                ledger["baseline_units"] = self.recipient_units()
                self.save_ledger()
            checkable = [(op, p) for op, p in ledger["pending"].items()
                         if HEX32.match(p.get("voucher") or "")]
            for start in range(0, len(checkable), GQL_BATCH):
                self.check_vouchers(checkable[start:start + GQL_BATCH])
            self.recipient_delta = self.recipient_units() - ledger["baseline_units"]
            self.check_balance()
        except READ_ERRORS as e:
            self.last_error = f"credit check: {type(e).__name__}"

    def check_vouchers(self, batch: list):
        ledger = self.ledger
        fields = " ".join(
            # A voucher is deployed by an external message: its own dapp.
            f'a{i}:account(account_id:"{p["voucher"]}",dapp_id:"{p["voucher"]}")'
            "{info{acc_type_name}}"
            for i, (_, p) in enumerate(batch)
        )
        data = self.gql(f"query{{blockchain{{{fields}}}}}")
        for i, (op, p) in enumerate(batch):
            info = (data.get(f"a{i}") or {}).get("info") or {}
            if info.get("acc_type_name") != "Active":
                continue
            del ledger["pending"][op]
            ledger["done"].append(op)
            ledger["finalized"] += 1
            ledger["finalized_units"] += int(p["units"])
            self.save_ledger()
            self.record({"status": "finalized", "op_id": op, "deposit_id": p["deposit_id"],
                         "seconds": round(time.time() - p["sent_at"])})

    def check_balance(self):
        """A voucher does not prove the mint: the balance has to follow it."""
        now = time.monotonic()
        history = self.finalized_history
        history.append((now, self.ledger["finalized_units"]))
        # What was finalized a full SOAK_SHORTFALL_S ago: newer deposits may
        # still be on their way, and with a steady stream some always are.
        due = None
        while history and now - history[0][0] >= self.cfg.shortfall_s:
            due = history.popleft()[1]
        if due is None:
            return
        history.appendleft((now - self.cfg.shortfall_s, due))
        missing = due - self.recipient_delta
        if missing > 0:
            self.last_error = f"recipient is short of {missing} units"
            self.record({"status": "credit_shortfall", "missing_units": missing})
            self.halt = "credit_shortfall"

    def gas_is_low(self) -> bool:
        """False too when the balance cannot be read: the send finds out."""
        if not (self.cfg.rpc_url and self.cfg.wallet):
            return False
        body = {"jsonrpc": "2.0", "id": 1, "method": "eth_getBalance",
                "params": [self.cfg.wallet, "latest"]}
        request = urllib.request.Request(
            self.cfg.rpc_url,
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json", "User-Agent": "deposit-soak/1"},
        )
        try:
            with urllib.request.urlopen(request, timeout=GQL_TIMEOUT_S) as response:
                return int(json.load(response)["result"], 16) < self.cfg.min_gas_wei
        except READ_ERRORS:
            return False

    # --- sending -------------------------------------------------------

    def run_cli(self, resume: Optional[str]) -> Outcome:
        outcome = Outcome(exit_code=-1, op_id=resume)
        stderr_tail = collections.deque(maxlen=20)
        child = subprocess.Popen(
            self.cfg.command(resume),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.child = child
        if self.stop.is_set():
            child.send_signal(signal.SIGTERM)
        drain = threading.Thread(target=lambda: stderr_tail.extend(child.stderr), daemon=True)
        drain.start()
        timer = threading.Timer(SEND_TIMEOUT_S, self.interrupt, args=(child,))
        killer = threading.Timer(SEND_TIMEOUT_S + KILL_AFTER_S, child.kill)
        for t in (timer, killer):
            t.daemon = True
            t.start()
        interrupted = False
        try:
            for line in child.stdout:
                try:
                    item = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(item, dict):
                    continue
                if item.get("op_id"):
                    outcome.op_id = item["op_id"]
                if item.get("event") == "step":
                    # From here on the deposit is the relayers' to finish.
                    at_anchor = str(item.get("step")).lower() == "anchor"
                    later = str(item.get("step")).lower() in ("prove", "finalize")
                    running = str(item.get("state")).lower() == "running"
                    if not interrupted and ((at_anchor and running) or later):
                        interrupted = True
                        self.interrupt(child)
                elif "event" not in item and isinstance(item.get("error"), dict):
                    error = item["error"]
                    outcome.op_id = error.get("op_id") or outcome.op_id
                    causes = [str(c) for c in error.get("causes") or []]
                    outcome.message = " | ".join([str(error.get("message", ""))] + causes)
            outcome.exit_code = child.wait()
        finally:
            timer.cancel()
            killer.cancel()
            self.child = None
        drain.join(timeout=5)
        if outcome.exit_code != 0 and not outcome.message and not drain.is_alive():
            outcome.message = "".join(stderr_tail)[-500:]
        return outcome

    @staticmethod
    def interrupt(child: subprocess.Popen):
        if child.poll() is None:
            child.send_signal(signal.SIGTERM)

    def record_readable(self, op: str) -> bool:
        try:
            json.loads((self.cfg.cli_state / f"{op}.json").read_text())
            return True
        except (OSError, ValueError):
            return False

    def send(self) -> tuple:
        """One send. Returns (delivered, reason to stop or None)."""
        resume = self.unresolved
        if not resume and self.gas_is_low():
            self.last_error = "the wallet is below SOAK_MIN_GAS_WEI"
            return False, "limit"
        before = self.ledger["sent"]
        out = self.run_cli(resume)
        if out.op_id and not self.record_readable(out.op_id):
            # Deposits would go out and never be counted.
            self.last_error = f"operation {out.op_id} cannot be read in SOAK_CLI_STATE_DIR"
            return False, "wrong_state_dir"
        self.sync()
        if self.ledger["sent"] > before:
            self.gas_failures = 0
            self.resume_failures = 0
            self.last_error = ""
            return True, None
        if self.stop.is_set():
            return False, None
        self.last_error = out.message
        self.record({"status": "send_error", "exit_code": out.exit_code, "op_id": out.op_id,
                     "resumed": bool(resume), "message": out.message[:500]})
        if any(marker in out.message for marker in LIMIT_MARKERS):
            return False, "limit"
        if out.exit_code in NEEDS_PERSON:
            return False, "needs_person"
        if out.exit_code in GAS_FAILURES:
            self.gas_failures += 1
            if self.gas_failures >= self.cfg.max_failures:
                return False, "gas_failures"
        if self.unresolved:
            self.resume_failures += 1
            if self.resume_failures >= self.cfg.max_failures:
                return False, "unresolved_deposit"
        return False, None

    def stopped(self, reason: str) -> int:
        self.record({"status": "stopped", "reason": reason, "message": self.last_error[:500]})
        self.write_status()
        return EXIT_STOPPED

    def run(self, once: bool) -> int:
        retry_at = 0.0
        sent_here = False
        while not self.stop.is_set():
            self.sync()
            self.check_credits()
            self.write_status()
            if self.halt:
                return self.stopped(self.halt)
            settled = not self.ledger["pending"] and self.recipient_delta is not None \
                and self.recipient_delta >= self.ledger["finalized_units"]
            if once and sent_here and settled:
                return 0
            # By the wall clock and from the CLI's records: a restart, even
            # after a hard kill, does not add a deposit.
            last = self.ledger.get("last_send_at") or 0
            due = max(retry_at, 0 if once else last + self.cfg.interval_s)
            if (not once or not sent_here) and time.time() >= due:
                delivered, reason = self.send()
                if reason:
                    return self.stopped(reason)
                if delivered:
                    sent_here = True
                else:
                    retry_at = time.time() + self.cfg.retry_s
                continue
            wait = self.cfg.poll_s if once else min(self.cfg.poll_s, due - time.time())
            self.stop.wait(max(0.0, wait))
        return 0

    def on_signal(self, signum, _frame):
        self.stop.set()
        child = self.child
        if child and child.poll() is None:
            child.send_signal(signum)


def main() -> int:
    parser = argparse.ArgumentParser(description="Continuous USDC deposit test")
    parser.add_argument("--once", action="store_true",
                        help="send one deposit, wait for its credit and exit")
    args = parser.parse_args()
    soak = Soak(Config.from_env())
    signal.signal(signal.SIGTERM, soak.on_signal)
    signal.signal(signal.SIGINT, soak.on_signal)
    return soak.run(args.once)


if __name__ == "__main__":
    sys.exit(main())
