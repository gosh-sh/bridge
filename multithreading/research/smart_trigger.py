#!/usr/bin/env python3
"""
Smart trigger — adaptive event firing for the multi-path
collector session.

Fires N WithdrawalInitiated events one at a time.  After each fire, waits
for the event's carrier block to become visible in GQL, reads
`x_thread`, and:

  * if X lands on thread 0  → fire next event after a short pause
    (`--same-thread-pause-s`, default 15 s).  There is nothing to walk;
    no reason to burn the full observation window.
  * else                    → wait the full observation window
    (`--observation-window-s`, default 300 s) so the multi-path
    collector can accumulate alternate paths before we perturb the
    graph with a fresh event.

At the end, waits one final observation window so the last event's
collector record can close, then dumps a per-event digest that combines
what this script observed with what the collector wrote to JSONL.

Usage:
  python research/smart_trigger.py \\
     --count 7 \\
     --collector-out research/stats/dirb-mp-YYYYMMDD-HHMM.jsonl \\
     --observation-window-s 300 \\
     --same-thread-pause-s 15
"""
from __future__ import annotations
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

_HERE       = os.path.dirname(os.path.abspath(__file__))
_VENDORED   = os.path.join(_HERE, "vendored")
_TRIG       = os.path.join(_VENDORED, "test_deploy_and_withdraw_only.py")
USDC_ACC_ID = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
USDC_DAPP   = "0" * 64


def is_thread_0(t: str | None) -> bool:
    if not t or not isinstance(t, str):
        return False
    s = t.lower().removeprefix("0x")
    return bool(s) and all(c == "0" for c in s)


def short_thread(t: str) -> str:
    tn = (t or "").lower().removeprefix("0x")
    if is_thread_0(tn):
        return "t0"
    return "t" + tn[:6]


def now_str() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def log(msg: str) -> None:
    print(f"[{now_str()}] smart-trigger: {msg}", flush=True)


def gql(url: str, query: str, timeout: int = 10) -> dict:
    req = urllib.request.Request(
        url,
        data=json.dumps({"query": query}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def fetch_bridge_msg_ids(url: str, limit: int = 200) -> set[str]:
    q = f'''{{
      blockchain {{
        account(account_id: "{USDC_ACC_ID}", dapp_id: "{USDC_DAPP}") {{
          messages(msg_type: [ExtOut], last: {limit}) {{
            edges {{ node {{ id }} }}
          }}
        }}
      }}
    }}'''
    try:
        data = gql(url, q)
    except (urllib.error.URLError, TimeoutError, OSError):
        return set()
    edges = (((data.get("data") or {}).get("blockchain") or {})
             .get("account") or {}).get("messages", {}).get("edges") or []
    return {e["node"]["id"] for e in edges if e.get("node", {}).get("id")}


def fetch_msg_with_block(url: str, msg_id: str) -> dict | None:
    q = f'''{{
      blockchain {{
        message(hash: "{msg_id}") {{
          id block_id
          src_transaction {{ block_id }}
        }}
      }}
    }}'''
    try:
        data = gql(url, q)
    except (urllib.error.URLError, TimeoutError, OSError):
        return None
    return (((data.get("data") or {}).get("blockchain") or {})
            .get("message"))


def fetch_bridge_msgs_full(url: str, limit: int = 200) -> list[dict]:
    q = f'''{{
      blockchain {{
        account(account_id: "{USDC_ACC_ID}", dapp_id: "{USDC_DAPP}") {{
          messages(msg_type: [ExtOut], last: {limit}) {{
            edges {{ node {{ id block_id src_transaction {{ block_id }} }} }}
          }}
        }}
      }}
    }}'''
    try:
        data = gql(url, q)
    except (urllib.error.URLError, TimeoutError, OSError):
        return []
    edges = (((data.get("data") or {}).get("blockchain") or {})
             .get("account") or {}).get("messages", {}).get("edges") or []
    return [e["node"] for e in edges]


def fetch_block_thread(url: str, block_id: str) -> tuple[str, int] | None:
    for key in ("hash", "block_id"):
        q = f'''{{
          blockchain {{
            block({key}: "{block_id}") {{
              seq_no thread_id
            }}
          }}
        }}'''
        try:
            data = gql(url, q)
        except (urllib.error.URLError, TimeoutError, OSError):
            return None
        b = (((data.get("data") or {}).get("blockchain") or {}).get("block"))
        if b:
            return b.get("thread_id"), int(b.get("seq_no") or -1)
    return None


def fire_one(python_bin: str) -> tuple[int, float]:
    """Fire one WithdrawalInitiated event; return (rc, elapsed_s)."""
    env = os.environ.copy()
    env.setdefault("MODE", "local")
    env.setdefault("PROVER_DIR", _VENDORED)
    t0 = time.time()
    rc = subprocess.call([python_bin, _TRIG], env=env, cwd=_VENDORED)
    return rc, time.time() - t0


def wait_for_new_msg(url: str, baseline: set[str],
                     max_wait_s: float, poll_s: float = 3.0) -> str | None:
    """Poll until a new bridge ExtOut appears; return its msg_id."""
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        current = fetch_bridge_msg_ids(url)
        new = current - baseline
        if new:
            # Newest by insertion (GQL `last:N` returns oldest→newest);
            # pick any — usually there's exactly one.
            return sorted(new)[-1]
        time.sleep(poll_s)
    return None


def resolve_msg_block(url: str, msg_id: str,
                      max_wait_s: float, poll_s: float = 2.0) -> tuple[str, str, int] | None:
    """After a new msg appears, wait for its carrier block + thread info.
    Returns (block_id, thread_id, seq_no)."""
    deadline = time.time() + max_wait_s
    while time.time() < deadline:
        # Prefer batch — some GQL versions don't accept single-message lookup.
        for m in fetch_bridge_msgs_full(url):
            if m.get("id") != msg_id:
                continue
            blk = m.get("block_id") or (m.get("src_transaction") or {}).get("block_id")
            if blk:
                thr = fetch_block_thread(url, blk)
                if thr:
                    return blk, thr[0], thr[1]
        time.sleep(poll_s)
    return None


def load_collector_records(path: str) -> list[dict]:
    if not path or not os.path.isfile(path):
        return []
    out = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def digest(events: list[dict], collector_records: list[dict]) -> None:
    print()
    print("═" * 72)
    print(f"SMART TRIGGER DIGEST — {len(events)} events fired")
    print("═" * 72)
    by_mid = {r.get("event_msg_id"): r for r in collector_records}
    for i, ev in enumerate(events, 1):
        print()
        print(f"── event #{i} ── msg_id={ev.get('msg_id')}")
        print(f"    fire_rc            = {ev.get('fire_rc')}")
        print(f"    fire_elapsed_s     = {ev.get('fire_elapsed_s'):.1f}")
        print(f"    x_thread           = {ev.get('x_thread')}   ({short_thread(ev.get('x_thread', ''))})")
        print(f"    x_seq              = {ev.get('x_seq')}")
        print(f"    x_block_id         = {ev.get('x_block_id')}")
        print(f"    smart_wait_s       = {ev.get('smart_wait_s'):.1f}  reason={ev.get('wait_reason')}")
        rec = by_mid.get(ev.get("msg_id"))
        if rec is None:
            print("    collector_record   = <not yet emitted>")
            continue
        outcome = rec.get("outcome")
        print(f"    outcome            = {outcome}")
        print(f"    num_paths          = {rec.get('num_paths')}")
        print(f"    shortest_length    = {rec.get('shortest_length')}")
        sp = rec.get("shortest_path") or {}
        if sp:
            print(f"    shortest_path.signature = {sp.get('signature')}")
            print(f"    shortest_path.anchor    = seq={sp.get('anchor',{}).get('seq')} thread={short_thread(sp.get('anchor',{}).get('thread',''))}")
            print(f"    shortest_path.hops:")
            for h in sp.get("hops") or []:
                print(f"      {short_thread(h.get('from_thread',''))}#{h.get('from_seq'):<6d} "
                      f"--slot{h.get('slot')}-→ "
                      f"{short_thread(h.get('to_thread',''))}#{h.get('to_seq'):<6d}")
        print(f"    length_histogram   = {rec.get('length_histogram')}")
        print(f"    T_first_path_wall_s= {rec.get('T_first_path_wall_s')}")
        print(f"    T_best_path_wall_s = {rec.get('T_best_path_wall_s')}")
        # List up to 5 alternative paths (beyond shortest).
        alt = rec.get("all_paths") or []
        if len(alt) > 1:
            print(f"    alternative_paths ({len(alt) - 1}):")
            for p in alt[1:6]:
                print(f"      len={p.get('length')} sig={p.get('signature')}")
            if len(alt) > 6:
                print(f"      ... (+{len(alt) - 6} more)")

    # Aggregate stats.
    print()
    print("── aggregate ─────────────────────────────────────────────────")
    on_t0     = sum(1 for e in events if is_thread_0(e.get("x_thread")))
    off_t0    = len(events) - on_t0
    with_rec  = sum(1 for e in events if by_mid.get(e.get("msg_id")))
    print(f"  events fired      : {len(events)}")
    print(f"  X on thread 0     : {on_t0}")
    print(f"  X off thread 0    : {off_t0}")
    print(f"  collector records : {with_rec}")
    lens = [by_mid[e['msg_id']].get('shortest_length') for e in events
            if by_mid.get(e['msg_id'])
            and by_mid[e['msg_id']].get('shortest_length') is not None]
    if lens:
        print(f"  shortest_length   : min={min(lens)} max={max(lens)} "
              f"mean={sum(lens)/len(lens):.2f}")
    paths = [by_mid[e['msg_id']].get('num_paths', 0) for e in events
             if by_mid.get(e['msg_id'])]
    if paths:
        print(f"  num_paths         : min={min(paths)} max={max(paths)} "
              f"mean={sum(paths)/len(paths):.2f}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--count", type=int, default=5,
                    help="Number of events to fire (default: 5).")
    ap.add_argument("--graphql", default="http://localhost/graphql")
    ap.add_argument("--observation-window-s", type=float, default=300.0,
                    help="Wait between events if X landed off thread 0.")
    ap.add_argument("--same-thread-pause-s", type=float, default=15.0,
                    help="Wait between events if X landed on thread 0.")
    ap.add_argument("--msg-wait-s", type=float, default=180.0,
                    help="How long to wait for a fresh msg to appear on GQL.")
    ap.add_argument("--block-wait-s", type=float, default=120.0,
                    help="How long to wait for the msg's carrier block "
                         "to become resolvable.")
    ap.add_argument("--python", default=sys.executable)
    ap.add_argument("--collector-out",
                    help="Path to the multipath collector's JSONL output. "
                         "If given, digest merges collector records at end.")
    ap.add_argument("--events-out",
                    help="Optional JSON summary of what this script observed.")
    args = ap.parse_args()

    if not os.path.isfile(_TRIG):
        log(f"ERROR: vendored trigger not found: {_TRIG}")
        return 2

    log(f"config: count={args.count} obs_window={args.observation_window_s}s "
        f"same_thread_pause={args.same_thread_pause_s}s "
        f"graphql={args.graphql}")

    events: list[dict] = []
    baseline = fetch_bridge_msg_ids(args.graphql)
    log(f"baseline: {len(baseline)} existing ExtOut msgs on USDCBridge")

    for i in range(1, args.count + 1):
        log(f"── firing event {i}/{args.count} ──")
        rc, elapsed = fire_one(args.python)
        log(f"  fire rc={rc} elapsed={elapsed:.1f}s")

        msg_id = wait_for_new_msg(args.graphql, baseline,
                                  max_wait_s=args.msg_wait_s)
        if not msg_id:
            log(f"  no new bridge msg within {args.msg_wait_s}s — skipping wait")
            events.append({
                "msg_id": None, "fire_rc": rc, "fire_elapsed_s": elapsed,
                "x_thread": None, "x_seq": None, "x_block_id": None,
                "smart_wait_s": 0.0, "wait_reason": "no_msg_seen",
            })
            continue
        log(f"  new msg_id={msg_id}")
        baseline.add(msg_id)

        info = resolve_msg_block(args.graphql, msg_id,
                                 max_wait_s=args.block_wait_s)
        if info is None:
            log(f"  msg's carrier block not resolvable within "
                f"{args.block_wait_s}s")
            wait_reason = "block_unresolved"
            wait_s = args.same_thread_pause_s
            events.append({
                "msg_id": msg_id, "fire_rc": rc, "fire_elapsed_s": elapsed,
                "x_thread": None, "x_seq": None, "x_block_id": None,
                "smart_wait_s": wait_s, "wait_reason": wait_reason,
            })
        else:
            blk, thr, seq = info
            log(f"  X on {short_thread(thr)}  seq={seq}  block={blk[:16]}…")
            if is_thread_0(thr):
                wait_s = args.same_thread_pause_s
                wait_reason = "x_thread_0_short_pause"
            else:
                wait_s = args.observation_window_s
                wait_reason = "x_off_thread_0_full_window"
            events.append({
                "msg_id": msg_id, "fire_rc": rc, "fire_elapsed_s": elapsed,
                "x_thread": thr, "x_seq": seq, "x_block_id": blk,
                "smart_wait_s": wait_s, "wait_reason": wait_reason,
            })

        if i < args.count:
            log(f"  waiting {events[-1]['smart_wait_s']:.1f}s "
                f"({events[-1]['wait_reason']}) before next fire...")
            time.sleep(events[-1]["smart_wait_s"])

    # Final observation window so the last event's collector record closes.
    log(f"── all {args.count} events fired; waiting {args.observation_window_s}s "
        f"for collector to close last event ──")
    time.sleep(args.observation_window_s)

    # Emit events log + digest.
    if args.events_out:
        with open(args.events_out, "w") as f:
            json.dump(events, f, indent=2, default=str)
        log(f"wrote events summary → {args.events_out}")

    collector_records = load_collector_records(args.collector_out or "")
    log(f"loaded {len(collector_records)} collector records from "
        f"{args.collector_out}")
    digest(events, collector_records)
    return 0


if __name__ == "__main__":
    sys.exit(main())
