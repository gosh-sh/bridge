#!/usr/bin/env python3
"""
Post-hoc analysis of `multipath_collector.py` output.

Each JSONL record carries a *set* of paths per event, so the digest
answers questions the direct-hit analyzer can't:

  * How much does the shortest path beat the first-found path?
  * What is the length distribution of alternative paths?
  * Which thread signatures dominate the shortest-path set?
  * How often does an indirect prefix (Y → t' → t) win?

Sections:
  1. Total & outcome breakdown.
  2. Thread distribution of X.
  3. Shortest-path length: stats + histogram.
  4. Path-set size per event: how many alternatives did we see?
  5. First-found vs shortest: gap size and time-to-improvement.
  6. Thread-signature census on the shortest path (rank by frequency).
  7. Direct vs indirect prefix: shortest-path first hop stays in
     x_thread (direct) or jumps through another thread (indirect).

Usage:
    python multipath_analyzer.py research/stats/dirb-mp-*.jsonl
    python multipath_analyzer.py research/stats/dirb-mp-*.jsonl --json > s.json
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
                        print(f"[warn] skip malformed line in {p}: {ex}",
                              file=sys.stderr)
    return records


def is_thread_0(t) -> bool:
    if t is None or not isinstance(t, str):
        return False
    s = t.lower().removeprefix("0x")
    return bool(s) and all(c == "0" for c in s)


def histogram(values: list, buckets: list) -> list[tuple[str, int]]:
    if not values:
        return []
    labels = []
    for i, b in enumerate(buckets):
        labels.append(f"< {b}" if i == 0 else f"[{buckets[i-1]}, {b})")
    labels.append(f">= {buckets[-1]}")
    counts = [0] * (len(buckets) + 1)
    for v in values:
        for i, b in enumerate(buckets):
            if v < b:
                counts[i] += 1
                break
        else:
            counts[-1] += 1
    return list(zip(labels, counts))


def stats(xs: list) -> dict:
    if not xs:
        return {"n": 0}
    xs_sorted = sorted(xs)
    def q(p):
        if len(xs_sorted) == 1:
            return xs_sorted[0]
        return xs_sorted[int(p * (len(xs_sorted) - 1))]
    return {
        "n":      len(xs),
        "min":    xs_sorted[0],
        "max":    xs_sorted[-1],
        "mean":   sum(xs) / len(xs),
        "median": statistics.median(xs),
        "p90":    q(0.90),
        "p99":    q(0.99),
    }


def summarize(records: list[dict]) -> dict:
    total = len(records)
    outcome_counts = Counter(r.get("outcome") for r in records)
    thread_counts  = Counter(r.get("x_thread") for r in records)

    ok = [r for r in records if r.get("outcome") == "ok"]
    shortest_lengths = [r["shortest_length"] for r in ok
                        if r.get("shortest_length") is not None]
    num_paths        = [r.get("num_paths", 0) for r in ok]
    t_first          = [r["T_first_path_wall_s"] for r in ok
                        if r.get("T_first_path_wall_s") is not None]
    t_best           = [r["T_best_path_wall_s"] for r in ok
                        if r.get("T_best_path_wall_s") is not None]
    # Time between first-found and best-found — how long we had to wait
    # for the shorter alternative to appear.
    improvement_wait = []
    for r in ok:
        if (r.get("T_best_path_wall_s") is not None
            and r.get("T_first_path_wall_s") is not None):
            improvement_wait.append(
                r["T_best_path_wall_s"] - r["T_first_path_wall_s"])

    # First-vs-best gap in hops.
    first_vs_best_gap = []
    for r in ok:
        paths = r.get("all_paths") or []
        if len(paths) < 1:
            continue
        # Paths are already sorted shortest-first in the collector,
        # so "first found" isn't captured explicitly.  Best is
        # paths[0]["length"]; longest recorded is paths[-1]["length"].
        first_vs_best_gap.append(paths[-1]["length"] - paths[0]["length"])

    # Signature census on the shortest path.
    sig_counts = Counter()
    for r in ok:
        sp = r.get("shortest_path") or {}
        sig = sp.get("signature")
        if sig:
            sig_counts[sig] += 1

    # Direct vs indirect prefix classifier.
    direct = 0
    indirect = 0
    for r in ok:
        sp = r.get("shortest_path") or {}
        hops = sp.get("hops") or []
        if not hops:
            continue
        first_target_thread = hops[0].get("to_thread")
        x_thread = r.get("x_thread")
        if first_target_thread == x_thread:
            direct += 1
        else:
            indirect += 1

    return {
        "total":             total,
        "outcome_counts":    dict(outcome_counts),
        "thread_counts":     dict(thread_counts),
        "shortest_length_stats":   stats(shortest_lengths),
        "num_paths_stats":         stats(num_paths),
        "T_first_path_wall_s":     stats(t_first),
        "T_best_path_wall_s":      stats(t_best),
        "improvement_wait_s":      stats(improvement_wait),
        "first_vs_best_gap_hops":  stats(first_vs_best_gap),
        "histograms": {
            "shortest_length":    histogram(shortest_lengths, [1, 2, 3, 5, 8, 13, 21, 34]),
            "num_paths":          histogram(num_paths,        [1, 2, 5, 10, 20]),
            "T_first_path_wall":  histogram(t_first,          [1, 5, 15, 30, 60, 180, 600]),
            "improvement_wait_s": histogram(improvement_wait, [0.001, 1, 5, 15, 60, 180]),
            "first_vs_best_gap":  histogram(first_vs_best_gap,[1, 2, 5, 10]),
        },
        "shortest_signatures":     sig_counts.most_common(20),
        "prefix_classifier":       {"direct": direct, "indirect": indirect},
    }


def print_section(title: str) -> None:
    print()
    print(f"── {title} " + "─" * max(1, 68 - len(title)))


def print_stats_line(label: str, s: dict, fmt: str = "d") -> None:
    if not s.get("n"):
        print(f"  {label:22s} n=0")
        return
    if fmt == "f":
        print(f"  {label:22s} n={s['n']} min={s['min']:.2f} "
              f"median={s['median']:.2f} mean={s['mean']:.2f} "
              f"p90={s['p90']:.2f} p99={s['p99']:.2f} max={s['max']:.2f}")
    else:
        print(f"  {label:22s} n={s['n']} min={s['min']} "
              f"median={s['median']} mean={s['mean']:.2f} "
              f"p90={s['p90']} p99={s['p99']} max={s['max']}")


def print_hist(name: str, entries: list[tuple[str, int]]) -> None:
    if not entries:
        return
    print(f"  {name}:")
    for label, count in entries:
        bar = "#" * min(50, count)
        print(f"    {label:>16s} {count:5d} {bar}")


def print_digest(s: dict) -> None:
    print_section("1. Total & outcome")
    print(f"total events: {s['total']}")
    for o, c in sorted(s["outcome_counts"].items(), key=lambda kv: -kv[1]):
        print(f"  {o:26s} {c}")

    print_section("2. Where X landed (thread distribution)")
    for t, c in sorted(s["thread_counts"].items(), key=lambda kv: -kv[1]):
        marker = "  [thread 0]" if is_thread_0(t) else ""
        print(f"  {str(t)[:20]:22s} {c}{marker}")

    print_section("3. Shortest-path length (outcome=ok)")
    print_stats_line("shortest_length", s["shortest_length_stats"], "d")
    print_hist("shortest_length histogram (~8 SHA-256 per hop in circuit)",
               s["histograms"]["shortest_length"])

    print_section("4. Number of alternative paths per event")
    print_stats_line("num_paths", s["num_paths_stats"], "d")
    print_hist("num_paths histogram", s["histograms"]["num_paths"])
    print_stats_line("first_vs_best gap (hops)",
                     s["first_vs_best_gap_hops"], "d")
    print_hist("first-vs-best gap histogram",
               s["histograms"]["first_vs_best_gap"])

    print_section("5. Latency: first path vs best path (seconds)")
    print_stats_line("T_first_path_wall_s", s["T_first_path_wall_s"], "f")
    print_stats_line("T_best_path_wall_s",  s["T_best_path_wall_s"],  "f")
    print_stats_line("improvement_wait_s",  s["improvement_wait_s"],  "f")
    print_hist("T_first_path_wall_s histogram",
               s["histograms"]["T_first_path_wall"])
    print_hist("improvement_wait_s histogram",
               s["histograms"]["improvement_wait_s"])

    print_section("6. Shortest-path thread signatures (top 20)")
    if not s["shortest_signatures"]:
        print("  (no ok records)")
    for sig, c in s["shortest_signatures"]:
        print(f"  {c:5d}  {sig}")

    print_section("7. Direct vs indirect prefix on the shortest path")
    pc = s["prefix_classifier"]
    total_class = pc["direct"] + pc["indirect"]
    if total_class:
        d_pct = 100.0 * pc["direct"]   / total_class
        i_pct = 100.0 * pc["indirect"] / total_class
        print(f"  direct    (Y → x_thread → … → X):    {pc['direct']:5d}  {d_pct:5.1f}%")
        print(f"  indirect  (Y → other_thread → … → X): {pc['indirect']:5d}  {i_pct:5.1f}%")
        if pc["indirect"]:
            print()
            print("  Indirect-prefix wins mean a same-thread parent-chain walk"
                  " would have been strictly longer.")
            print("  If this fraction is high, the 3+-thread BFS in the "
                  "collector is doing real work.")
    else:
        print("  (no ok records)")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--json", action="store_true")
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
