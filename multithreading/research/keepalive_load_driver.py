#!/usr/bin/env python3
"""Custom cross-DApp keep-alive load driver.

Purpose
-------
Once Michael's ``tests/mt/cli.py test-multithread-cross-thread`` has
deployed the ``MessageArchiveTest`` contracts and triggered the initial
thread split, use this driver to *keep the child thread alive* with a
much smaller, sustained cross-DApp load than the MT test's hold cycles.

The MT test's hold uses 5000-message bursts, which on a 12--14 GiB
Docker VM eventually pushes Aerospike past its ``stop-writes`` limit.
This driver defaults to 200-message bursts every 15 s, from **both**
senders (DApp 0x00... -> 0x80... and 0x80... -> 0x00...), so both
threads keep receiving inbound messages and their producers keep
finalizing blocks. Total steady rate ~27 msg/s per sender; scale up
via ``--burst-total`` or down via ``--interval`` as needed.

Reads addresses from ``tests/mt/MessageArchiveTest.accounts.json``,
signs each external call with the sender's own key file.  Does not
touch Michael's cli.py.  Reuses the already-deployed contracts on the
running cluster --- must be started **after** Michael's test has
already registered the split.

Usage
-----
    python3 research/keepalive_load_driver.py \\
        --acki-nacki-root /Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki \\
        --tvm-cli /Users/alinat/HALO2_TVM_EXPERIMENTS/tvm-sdk/target/release/tvm-cli \\
        --interval 15 \\
        --burst-total 200 \\
        --batch-size 100 \\
        --log research/stats/keepalive-$(date +%Y%m%d-%H%M).jsonl
"""
from __future__ import annotations

import argparse
import json
import re
import signal
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path


# ---------------------------------------------------------------------------
# tvm-cli helpers

class CliError(RuntimeError):
    pass


def run_cli(tvm_cli: Path, args: list[str], timeout: float = 60.0,
             url: str | None = None) -> str:
    """Run tvm-cli with -j (JSON output) and return stdout.

    tvm-cli's persisted config file may point at shellnet (default installs
    often do); passing `-u <url>` at the top level overrides it for this call.
    """
    cmd: list[str] = [str(tvm_cli), "-j"]
    if url:
        cmd += ["-u", url]
    cmd += list(args)
    try:
        proc = subprocess.run(
            cmd, check=False, capture_output=True, text=True,
            errors="replace", timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise CliError(f"tvm-cli timed out: {' '.join(cmd)}") from exc
    if proc.returncode != 0:
        raise CliError(
            f"tvm-cli exit={proc.returncode}: {proc.stderr.strip() or proc.stdout.strip()}"
        )
    return proc.stdout


# ---------------------------------------------------------------------------

@dataclass
class Account:
    address: str      # "<dapp_id>:<account_id>"
    keys_path: Path

    @property
    def dapp_id(self) -> str:
        return self.address.split(":", 1)[0]

    @property
    def short(self) -> str:
        dapp, aid = self.address.split(":", 1)
        return f"{dapp[:4]}../{aid[:6]}"

    @property
    def cli_addr(self) -> str:
        """tvm-cli v3 wants dapp_id::account_id (double colon) for CLI args.

        accounts.json stores addresses as `<dapp_id>:<account_id>` (single
        colon); anything passed on the CLI (--addr, positional targets) must be
        rewritten to double-colon.  See tests/helper/common.py:to_dapp_address.
        """
        dapp, aid = self.address.split(":", 1)
        return f"{dapp}::{aid}"

    @property
    def payload_addr(self) -> str:
        """ABI payload `address` fields must stay in legacy '0:<account_id>'
        form even under tvm-cli v3 — passing a `<dapp>:<acc>` string here
        overflows tvm-cli's workchain-id integer parser.  The destination DApp
        must ride alongside as its own `uint256 destination_dapp_id` parameter.
        See tests/helper/common.py:to_legacy_address.
        """
        _, aid = self.address.split(":", 1)
        return f"0:{aid}"


@dataclass
class Pair:
    """Cross-DApp fan pair: sender fans to receiver on opposite DApp."""
    sender: Account
    receiver: Account


def load_accounts(accounts_json: Path, keys_dir: Path) -> list[Account]:
    data = json.loads(accounts_json.read_text())
    out = []
    for entry in data:
        out.append(Account(
            address=entry["address"],
            keys_path=(keys_dir / entry["keys"]).resolve(),
        ))
    return out


def build_pairs(accounts: list[Account]) -> list[Pair]:
    """Michael's specs (cli.py:2791-2835): for each DApp, index 0 is
    receiver and index 1 is sender.  With --threads 2 the account list
    is [recv_0, send_0, recv_1, send_1].  Cross pairs go send_0 ->
    recv_1 (thread 0 -> child thread partition) and send_1 -> recv_0
    (child thread -> thread 0 partition).
    """
    if len(accounts) < 4:
        raise SystemExit(f"expected >= 4 accounts, got {len(accounts)}")
    recv_0, send_0 = accounts[0], accounts[1]
    recv_1, send_1 = accounts[2], accounts[3]
    return [Pair(sender=send_0, receiver=recv_1), Pair(sender=send_1, receiver=recv_0)]


# ---------------------------------------------------------------------------

def decode_account_field(tvm_cli: Path, address: str, abi: Path,
                          fields: tuple[str, ...],
                          url: str | None = None) -> dict[str, int]:
    """Return integer values for the requested contract public fields."""
    out = run_cli(tvm_cli, ["decode", "account", "data",
                             "--addr", address, "--abi", str(abi)],
                   url=url)
    # tvm-cli -j prints a JSON object.  Some builds embed it in a
    # human-readable prelude; strip anything before the first "{".
    lbrace = out.find("{")
    if lbrace < 0:
        raise CliError(f"tvm-cli decode returned no JSON: {out[:200]!r}")
    obj = json.loads(out[lbrace:])
    result: dict[str, int] = {}
    for f in fields:
        raw = obj.get(f, "0")
        try:
            result[f] = int(str(raw), 0) if isinstance(raw, str) and raw.startswith("0x") \
                        else int(raw)
        except ValueError:
            raise CliError(f"decode: field {f}={raw!r} not integer")
    return result


def fire_cross_dapp_burst(tvm_cli: Path, abi: Path, pair: Pair,
                            fan_seq: int, start_sequence: int,
                            total: int, batch_size: int,
                            url: str | None = None) -> None:
    dapp_id = pair.receiver.dapp_id
    # tvm-cli passes uint256 as decimal string OR 0x-hex; use 0x-hex for readability.
    params = {
        "fan_seq": str(fan_seq),
        "destination": pair.receiver.payload_addr,
        "destination_dapp_id": "0x" + dapp_id,
        "start_sequence": str(start_sequence),
        "total": str(total),
        "batch_size": str(batch_size),
    }
    run_cli(tvm_cli, [
        "call", pair.sender.cli_addr, "enqueueCrossDappMessages",
        json.dumps(params, separators=(",", ":")),
        "--abi", str(abi),
        "--sign", str(pair.sender.keys_path),
    ], url=url)


# ---------------------------------------------------------------------------

@dataclass
class DriverStats:
    started_utc: str
    cycles: int = 0
    fired: dict[str, int] = field(default_factory=dict)  # sender_short -> total msgs fired
    errors: int = 0
    last_receiver_counts: dict[str, int] = field(default_factory=dict)

    def to_json(self, ts_utc: str) -> dict:
        return {
            "ts_utc": ts_utc,
            "started_utc": self.started_utc,
            "cycles": self.cycles,
            "fired": dict(self.fired),
            "errors": self.errors,
            "last_receiver_counts": dict(self.last_receiver_counts),
        }


# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--acki-nacki-root", type=Path,
                    default=Path("/Users/alinat/HALO2_TVM_EXPERIMENTS/acki-nacki"),
                    help="Path to acki-nacki checkout (holds tests/mt/).")
    ap.add_argument("--tvm-cli", type=Path,
                    default=Path("/Users/alinat/HALO2_TVM_EXPERIMENTS/tvm-sdk/target/release/tvm-cli"),
                    help="Mac-native tvm-cli binary.")
    ap.add_argument("--interval", type=float, default=15.0,
                    help="Seconds between fan bursts (default 15).")
    ap.add_argument("--burst-total", type=int, default=200,
                    help="Messages per sender per burst (default 200).")
    ap.add_argument("--batch-size", type=int, default=100,
                    help="Contract-side batch size (default 100).")
    ap.add_argument("--log", type=Path, default=None,
                    help="Optional JSONL trace file.")
    ap.add_argument("--max-cycles", type=int, default=0,
                    help="Stop after N cycles (0 = run until SIGINT).")
    ap.add_argument("--network", default="http://127.0.0.1:80",
                    help="tvm-cli -u endpoint (default matches tests/helper/common.py).")
    args = ap.parse_args()

    mt_dir = args.acki_nacki_root / "tests" / "mt"
    accounts_file = mt_dir / "MessageArchiveTest.accounts.json"
    abi_file = mt_dir / "MessageArchiveTest.abi.json"
    if not accounts_file.exists():
        raise SystemExit(f"missing {accounts_file} - has Michael's test been run yet?")
    if not abi_file.exists():
        raise SystemExit(f"missing {abi_file}")

    accounts = load_accounts(accounts_file, mt_dir)
    pairs = build_pairs(accounts)
    print(f"[driver] loaded {len(accounts)} accounts, {len(pairs)} cross-DApp pairs",
          file=sys.stderr)
    for p in pairs:
        print(f"  send {p.sender.short}  --->  recv {p.receiver.short}  "
              f"(dst_dapp={p.receiver.dapp_id[:4]}..)", file=sys.stderr)
    print(f"[driver] burst_total={args.burst_total} batch_size={args.batch_size} "
          f"interval={args.interval}s", file=sys.stderr)

    stats = DriverStats(started_utc=datetime.now(timezone.utc).isoformat(timespec="seconds"))
    log_fh = None
    if args.log:
        args.log.parent.mkdir(parents=True, exist_ok=True)
        log_fh = args.log.open("a", buffering=1)

    stop_flag = {"stop": False}
    def _sigint(_signo, _frame):
        stop_flag["stop"] = True
    signal.signal(signal.SIGINT, _sigint)

    try:
        while not stop_flag["stop"]:
            cycle_start = time.monotonic()
            ts = datetime.now(timezone.utc).isoformat(timespec="seconds")
            print(f"[{ts}] cycle {stats.cycles + 1}", flush=True)

            for pair in pairs:
                try:
                    sender_state = decode_account_field(
                        args.tvm_cli, pair.sender.cli_addr, abi_file,
                        ("last_fan_seq",),
                        url=args.network,
                    )
                    receiver_state = decode_account_field(
                        args.tvm_cli, pair.receiver.cli_addr, abi_file,
                        ("received_cross_messages",),
                        url=args.network,
                    )
                except CliError as exc:
                    print(f"  [!] state read failed for {pair.sender.short}: {exc}",
                          flush=True)
                    stats.errors += 1
                    continue

                next_fan = sender_state["last_fan_seq"] + 1
                start_seq = receiver_state["received_cross_messages"]
                stats.last_receiver_counts[pair.receiver.short] = start_seq

                try:
                    fire_cross_dapp_burst(
                        args.tvm_cli, abi_file, pair,
                        fan_seq=next_fan, start_sequence=start_seq,
                        total=args.burst_total, batch_size=args.batch_size,
                        url=args.network,
                    )
                    stats.fired[pair.sender.short] = (
                        stats.fired.get(pair.sender.short, 0) + args.burst_total
                    )
                    print(f"  fired {args.burst_total:>4} msgs  "
                          f"{pair.sender.short} -> {pair.receiver.short}  "
                          f"(fan_seq={next_fan}, start={start_seq})",
                          flush=True)
                except CliError as exc:
                    print(f"  [!] fire failed {pair.sender.short}: {exc}",
                          flush=True)
                    stats.errors += 1

            stats.cycles += 1
            if log_fh:
                log_fh.write(json.dumps(stats.to_json(ts)) + "\n")

            if args.max_cycles and stats.cycles >= args.max_cycles:
                break

            elapsed = time.monotonic() - cycle_start
            sleep_s = max(0.0, args.interval - elapsed)
            if sleep_s > 0 and not stop_flag["stop"]:
                time.sleep(sleep_s)
    finally:
        print(f"[driver] stopping. cycles={stats.cycles} errors={stats.errors} "
              f"fired={stats.fired}", file=sys.stderr)
        if log_fh:
            log_fh.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
