#!/usr/bin/env python3
"""Thread-liveness monitor for the running 5-node Acki Nacki devnet.

Samples one node's Docker logs on a fixed cadence and reports, per thread:
  * incoming block-candidate rate       -> is this thread producing?
  * pulse_stall alarm rate              -> producer missing chain-pulse deadlines
  * latest known seq_no per thread      -> extracted from xthread prior_cutoffs

Companion to Michael Vlasov's tests/mt/cli.py --- this is read-only and
does not restart the cluster.  A thread that is registered but frozen
(candidates=0 while stalls>>0) is the classic "born-then-dead" case the
multi-thread cross-thread test occasionally lands in.

Output:
  * concise stdout line per sample
  * JSONL trace to --out for post-run analysis

Usage:
  python3 research/thread_liveness_monitor.py --interval 5 \
      --out research/stats/thread-mon-$(date +%Y%m%d-%H%M).jsonl

Reads only docker logs; does not exec into containers, does not touch
node state, does not modify Michael's test file.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from collections import defaultdict
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path


# ---------------------------------------------------------------------------
# Regex over node logs

# "Incoming block candidate: seq_no: 9197, ... thread: ThreadIdentifier<HEX>"
_RX_CANDIDATE = re.compile(
    r"Incoming block candidate:\s*seq_no:\s*(\d+),.*?thread:\s*ThreadIdentifier<([0-9a-f]+)>"
)

# "pulse_stall ... thread=<T:HEX>"
_RX_STALL = re.compile(r"pulse_stall.*?thread=<T:([0-9a-f]+)>")

# "prior_cutoffs=[("ThreadIdentifier<HEX>", "SEQ"), ...]" -- extract every pair
_RX_CUTOFF = re.compile(r"ThreadIdentifier<([0-9a-f]+)>\"\s*,\s*\"(\d+)\"")


def short(hex_id: str, n: int = 12) -> str:
    return hex_id[:n] if hex_id else "?" * n


# ---------------------------------------------------------------------------

@dataclass
class ThreadSample:
    thread_id: str
    candidates: int = 0
    stalls: int = 0
    max_seq: int = 0

    def status(self, window_s: float) -> str:
        # a thread is "LIVE" if it produced at least one candidate in the window
        if self.candidates > 0:
            return "LIVE"
        # registered but silent + hot stall loop --> STALLED
        if self.stalls > 0:
            return "STALLED"
        return "IDLE"


@dataclass
class Sample:
    ts_utc: str
    window_s: float
    threads: dict[str, ThreadSample] = field(default_factory=dict)

    def to_json(self) -> dict:
        return {
            "ts_utc": self.ts_utc,
            "window_s": self.window_s,
            "threads": {
                tid: {
                    "candidates": t.candidates,
                    "stalls": t.stalls,
                    "max_seq": t.max_seq,
                    "status": t.status(self.window_s),
                }
                for tid, t in self.threads.items()
            },
        }


# ---------------------------------------------------------------------------

def docker_logs_since(container: str, seconds: int) -> str:
    """Return combined stdout+stderr of `docker logs --since Ns container`."""
    proc = subprocess.run(
        ["docker", "logs", "--since", f"{seconds}s", container],
        check=False,
        capture_output=True,
        text=True,
        errors="replace",
    )
    return (proc.stdout or "") + (proc.stderr or "")


def scan_logs(container: str, seconds: int) -> Sample:
    text = docker_logs_since(container, seconds)
    sample = Sample(
        ts_utc=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        window_s=float(seconds),
    )

    def get(tid: str) -> ThreadSample:
        if tid not in sample.threads:
            sample.threads[tid] = ThreadSample(thread_id=tid)
        return sample.threads[tid]

    for m in _RX_CANDIDATE.finditer(text):
        seq = int(m.group(1))
        tid = m.group(2)
        t = get(tid)
        t.candidates += 1
        if seq > t.max_seq:
            t.max_seq = seq

    for m in _RX_STALL.finditer(text):
        get(m.group(1)).stalls += 1

    for m in _RX_CUTOFF.finditer(text):
        tid = m.group(1)
        seq = int(m.group(2))
        t = get(tid)
        if seq > t.max_seq:
            t.max_seq = seq

    return sample


# ---------------------------------------------------------------------------

def render_sample(sample: Sample) -> str:
    lines = [f"[{sample.ts_utc}] window={sample.window_s:.0f}s threads={len(sample.threads)}"]
    # canonical thread first (0x00...) if present
    def sort_key(tid: str) -> tuple:
        return (0 if tid.startswith("0" * 12) else 1, tid)

    for tid in sorted(sample.threads.keys(), key=sort_key):
        t = sample.threads[tid]
        status = t.status(sample.window_s)
        marker = {"LIVE": "OK   ", "STALLED": "STALL", "IDLE": "idle "}[status]
        lines.append(
            f"  {marker}  T:{short(tid)}  "
            f"blocks={t.candidates:>4}  stalls={t.stalls:>4}  "
            f"seq={t.max_seq:>6}"
        )
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--container", default="local_gossip_nodes-node0-1",
                    help="Docker container name to read logs from.")
    ap.add_argument("--interval", type=int, default=5,
                    help="Sampling window in seconds.")
    ap.add_argument("--out", type=Path, default=None,
                    help="Optional JSONL file to append samples to.")
    ap.add_argument("--max-samples", type=int, default=0,
                    help="Stop after N samples (0 = run until Ctrl-C).")
    args = ap.parse_args()

    out_fh = None
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        out_fh = args.out.open("a", buffering=1)
        print(f"[monitor] writing samples to {args.out}", file=sys.stderr)

    n = 0
    try:
        while True:
            sample = scan_logs(args.container, args.interval)
            print(render_sample(sample), flush=True)
            if out_fh:
                out_fh.write(json.dumps(sample.to_json()) + "\n")
            n += 1
            if args.max_samples and n >= args.max_samples:
                break
            time.sleep(args.interval)
    except KeyboardInterrupt:
        print("[monitor] stopped by SIGINT", file=sys.stderr)
    finally:
        if out_fh:
            out_fh.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
