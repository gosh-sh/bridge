#!/usr/bin/env python3
"""
Post-hoc analysis of `collector.py` JSONL output.

Prints a human-readable digest to stdout and (with --json) a machine-readable
summary. Consumes one or more JSONL files.

Sections in the digest:
  1. Total events + outcome breakdown.
  2. Thread distribution of X (where events landed).
  3. For outcome=ok: hop-count, same-thread-hops, T_anchor_wall_s histograms.
  4. For outcome=ok: distribution of (b_seq - x_seq) — anchor delay in
     finalized blocks of X's thread. This is the metric the ~50 protocol
     ceiling caps in the non-terminated case.
  5. Orphan / failure counts, with sampled msg_ids for follow-up.

Usage:
    python analyzer.py research/stats/dirb-*.jsonl
    python analyzer.py research/stats/dirb-*.jsonl --json > summary.json
"""
from __future__ import annotations
import argparse
import glob
import json
import statistics
import sys
from collections import Counter, defaultdict


def load(paths: list[str]) -> list[dict]:
    records = []
    for pat in paths:
        for p in glob.glob(pat):
            with open(p) as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        records.append(json.loads(line))
                    except json.JSONDecodeError as ex:
                        print(f"[warn] skipping malformed line in {p}: {ex}",
                              file=sys.stderr)
    return records


def histogram(values: list, buckets: list) -> list[tuple[str, int]]:
    """Bucket boundaries: e.g. [0, 1, 3, 10, 30, 100]. Right-open, last bucket
    is `>= last`."""
    if not values:
        return []
    counts = [0] * (len(buckets) + 1)
    labels = []
    for i, b in enumerate(buckets):
        if i == 0:
            labels.append(f"< {b}")
        else:
            labels.append(f"[{buckets[i-1]}, {b})")
    labels.append(f">= {buckets[-1]}")
    for v in values:
        placed = False
        for i, b in enumerate(buckets):
            if v < b:
                counts[i] += 1
                placed = True
                break
        if not placed:
            counts[-1] += 1
    return list(zip(labels, counts))


def print_section(title: str) -> None:
    print()
    print(f"── {title} " + "─" * max(1, 68 - len(title)))


def summarize(records: list[dict]) -> dict:
    total = len(records)
    outcome_counts = Counter(r.get("outcome") for r in records)
    thread_counts  = Counter(r.get("x_thread") for r in records)

    ok = [r for r in records if r.get("outcome") == "ok"]
    hop_counts        = [r["hop_count"]        for r in ok if r.get("hop_count") is not None]
    same_thread_hops  = [r["same_thread_hops"] for r in ok if r.get("same_thread_hops") is not None]
    cross_thread_hops = [r["cross_thread_hops"] for r in ok if r.get("cross_thread_hops") is not None]
    t_anchor_wall     = [r["T_anchor_wall_s"]  for r in ok if r.get("T_anchor_wall_s") is not None]
    t_anchor_blocks   = [r["T_anchor_blocks_thread_t"]
                         for r in ok if r.get("T_anchor_blocks_thread_t") is not None]

    def stats(xs: list) -> dict:
        if not xs:
            return {"n": 0}
        try:
            median = statistics.median(xs)
        except statistics.StatisticsError:
            median = None
        return {
            "n":      len(xs),
            "min":    min(xs),
            "max":    max(xs),
            "mean":   sum(xs) / len(xs),
            "median": median,
            "p90":    sorted(xs)[int(0.9 * (len(xs) - 1))] if len(xs) > 1 else xs[0],
            "p99":    sorted(xs)[int(0.99 * (len(xs) - 1))] if len(xs) > 1 else xs[0],
        }

    # Bucketed histograms.
    hop_hist        = histogram(hop_counts,        [1, 2, 5, 10, 25, 50, 100])
    same_hop_hist   = histogram(same_thread_hops,  [1, 2, 5, 10, 25, 50, 100])
    t_wall_hist     = histogram(t_anchor_wall,     [1, 5, 15, 30, 60, 180, 600])
    t_blocks_hist   = histogram(t_anchor_blocks,   [1, 2, 5, 10, 25, 50])

    # Orphan / failure samples (up to 5 msg_ids each).
    fail_samples = defaultdict(list)
    for r in records:
        o = r.get("outcome")
        if o != "ok" and o != "same_thread_trivial":
            fail_samples[o].append(r.get("event_msg_id"))
    fail_samples = {k: v[:5] for k, v in fail_samples.items()}

    return {
        "total":              total,
        "outcome_counts":     dict(outcome_counts),
        "thread_counts":      dict(thread_counts),
        "hop_count_stats":    stats(hop_counts),
        "same_thread_stats":  stats(same_thread_hops),
        "cross_thread_stats": stats(cross_thread_hops),
        "t_anchor_wall_s":    stats(t_anchor_wall),
        "t_anchor_blocks":    stats(t_anchor_blocks),
        "histograms": {
            "hop_count":        hop_hist,
            "same_thread_hops": same_hop_hist,
            "t_anchor_wall_s":  t_wall_hist,
            "t_anchor_blocks":  t_blocks_hist,
        },
        "failure_samples":    fail_samples,
    }


def print_digest(s: dict) -> None:
    print_section("1. Total & outcome")
    print(f"total events: {s['total']}")
    for o, c in sorted(s["outcome_counts"].items(), key=lambda kv: -kv[1]):
        print(f"  {o:26s} {c}")

    print_section("2. Where X landed (thread distribution)")
    for t, c in sorted(s["thread_counts"].items(), key=lambda kv: -kv[1]):
        marker = "  [thread 0]" if t and all(ch == "0" for ch in str(t).lower().removeprefix("0x")) else ""
        print(f"  {str(t)[:20]:22s} {c}{marker}")

    print_section("3. Hop counts (outcome=ok only)")
    print(f"total ok:        {s['hop_count_stats'].get('n', 0)}")
    if s["hop_count_stats"].get("n"):
        st = s["hop_count_stats"]
        print(f"  hop_count       min={st['min']} median={st['median']} "
              f"mean={st['mean']:.2f} p90={st['p90']} p99={st['p99']} max={st['max']}")
        st = s["same_thread_stats"]
        print(f"  same_thread_hops min={st['min']} median={st['median']} "
              f"mean={st['mean']:.2f} p90={st['p90']} p99={st['p99']} max={st['max']}")
        st = s["cross_thread_stats"]
        print(f"  cross_thread    min={st['min']} median={st['median']} mean={st['mean']:.2f}")
        print("  hop_count histogram:")
        for label, count in s["histograms"]["hop_count"]:
            bar = "#" * min(50, count)
            print(f"    {label:>14s} {count:5d} {bar}")

    print_section("4. Anchor delay (outcome=ok only)")
    if s["t_anchor_wall_s"].get("n"):
        st = s["t_anchor_wall_s"]
        print(f"  T_anchor_wall_s  min={st['min']:.1f} median={st['median']:.1f} "
              f"mean={st['mean']:.1f} p90={st['p90']:.1f} max={st['max']:.1f}")
        st = s["t_anchor_blocks"]
        print(f"  T_anchor_blocks  min={st['min']} median={st['median']} "
              f"mean={st['mean']:.2f} p90={st['p90']} max={st['max']}")
        print("  T_anchor_blocks (b_seq - x_seq) histogram (soft ~50 cap per protocol):")
        for label, count in s["histograms"]["t_anchor_blocks"]:
            bar = "#" * min(50, count)
            print(f"    {label:>14s} {count:5d} {bar}")

    print_section("5. Failures / orphans")
    if not s["failure_samples"]:
        print("  none")
    else:
        for outcome, samples in s["failure_samples"].items():
            print(f"  outcome={outcome}  n={s['outcome_counts'].get(outcome, 0)}")
            for m in samples:
                print(f"    example msg_id: {m}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("paths", nargs="+", help="One or more JSONL files (globs ok).")
    ap.add_argument("--json", action="store_true",
                    help="Emit machine-readable summary on stdout instead of a digest.")
    args = ap.parse_args()

    records = load(args.paths)
    s = summarize(records)
    if args.json:
        json.dump(s, sys.stdout, indent=2, default=str)
        print()
    else:
        print_digest(s)
    return 0


if __name__ == "__main__":
    sys.exit(main())
