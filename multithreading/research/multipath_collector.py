#!/usr/bin/env python3
"""
multi-path collector.

For each WithdrawalInitiated event X, enumerate every plausible walk

    Y (thread 0, on-chain-anchored)  →  …  →  X (event block)

that follows `proof_block_refs` edges, and rank them by hop count.

Motivation
----------
Each hop in the aggregated bridge SNARK costs **~8 SHA-256 gadgets**.
Fifty hops is a huge circuit load, so shortest-path characterisation is
a first-class research target — extracting a single walk per event is
not enough; we need the full set of paths that materialise within the
observation window so the shortest can be identified with confidence.

Approach
--------
1. Build a **global block DAG** in memory from every finalized block we
   observe.  Edges are the `proof_block_refs` entries, so each edge
   points to an *older* block.
2. For every pending event X, run **BFS on the reverse graph** rooted
   at X: the frontier expands from X toward newer blocks that reference
   X (directly or transitively).  Each time BFS reaches a thread-0
   block, that block is a candidate anchor Y — record the shortest
   path Y ◀── … ◀── X (`A ◀── C` = C is a leaf of A's L7).
3. Keep watching for an **observation window** (default 300 s) after
   the first path appears.  New blocks arrive during the window; each
   new block may unlock a shorter or structurally different path.
4. When the window elapses (or a hard orphan timeout hits), emit a
   JSONL record: shortest path, up to K distinct paths, thread
   signatures, first-path/best-path latencies.

Why BFS on the reverse graph and not the simple `thread0_view[t]`
scan?  The simple scan finds the closest direct thread-0 → x_thread
ref and walks parents — perfect for 2-thread runs where that IS the
only path.  With 4+ threads a hop through an intermediate thread can
be strictly shorter (Y → t' → t → … → X with, say, 3 total hops
vs. 40+ same-thread parents).  BFS handles both cases uniformly.

Fetch-on-demand: when a block references an unknown target, we
`fetch_block` it (bounded per poll).  That fills in the transitive
parent chain even when the intermediate thread isn't in the polling
window.

Output (one JSONL row per event, emitted when the observation window
expires or the orphan timeout fires):

{
  "schema":               "direction_b_multipath.v1",
  "event_msg_id":         "0:…",
  "event_first_seen_wall": 1698765432.1,
  "x_block_id":           "b19f…",
  "x_thread":             "0000…",
  "x_seq":                12345,
  "x_is_thread0":         false,
  "outcome":              "ok" | "same_thread_trivial" |
                          "orphan_timeout" | "event_block_unresolved",
  "observation_window_s": 300.0,
  "num_paths":            7,
  "shortest_length":      3,
  "shortest_path": {
    "length":    3,
    "signature": "t0 -> t1a2b -> t2c3d -> X",
    "anchor":    {"block_id": "…", "thread": "0000…", "seq": 12400},
    "hops": [
      {"from_block": "…", "from_thread": "0000…", "from_seq": 12400,
       "slot": 2,
       "to_block": "…",   "to_thread": "1a2b…",   "to_seq": 12350},
      ...
    ]
  },
  "all_paths": [ { "length": …, "signature": …, "anchor": …, "hops": [] }, … ],
  "length_histogram": {"3": 1, "4": 2, "5": 3, ...},
  "T_first_path_wall_s": 4.2,
  "T_best_path_wall_s":  12.5,
  "graph_size_at_close": {"blocks": 812, "edges": 3244}
}

Every path in `all_paths` traverses `proof_block_refs` edges only —
each is a valid Direction (b) walk the aggregator could prove.  A
shorter path means fewer BridgeMultiHopProof invocations and cheaper
aggregation.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import signal
import sys
import time
import urllib.error
import urllib.request
import uuid

# ── Constants ────────────────────────────────────────────────────────────────
USDC_BRIDGE_ACCOUNT_ID = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
USDC_BRIDGE_DAPP_ID    = "0" * 64
DEFAULT_THREAD_ID_HEX  = "0" * 68


# ── Helpers ──────────────────────────────────────────────────────────────────
def is_thread_0(t) -> bool:
    if t is None:
        return False
    if isinstance(t, int):
        return t == 0
    if isinstance(t, str):
        s = t.lower().removeprefix("0x")
        if not s:
            return False
        return all(c == "0" for c in s)
    return False


def normalize_thread_id(t) -> str:
    if t is None:
        return "<none>"
    if isinstance(t, int):
        return f"int:{t}"
    if isinstance(t, str):
        return t.lower().removeprefix("0x")
    return f"raw:{t!r}"


def short_thread(t: str) -> str:
    tn = normalize_thread_id(t)
    if is_thread_0(tn):
        return "t0"
    return "t" + tn[:6]


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


# ── Minimal GraphQL client (stdlib only) ─────────────────────────────────────
class GqlClient:
    def __init__(self, url: str, timeout: int = 15,
                 user_agent: str = "direction-b-multipath/1.0"):
        self.url        = url
        self.timeout    = timeout
        self.user_agent = user_agent
        self.gql_calls  = 0

    def _post(self, query: str) -> dict:
        self.gql_calls += 1
        req = urllib.request.Request(
            self.url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json",
                     "User-Agent":  self.user_agent},
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            return json.loads(resp.read().decode())

    def fetch_bridge_extouts(self, limit: int = 500) -> list[dict]:
        q = f'''{{
          blockchain {{
            account(account_id: "{USDC_BRIDGE_ACCOUNT_ID}", dapp_id: "{USDC_BRIDGE_DAPP_ID}") {{
              messages(msg_type: [ExtOut], last: {limit}) {{
                edges {{ node {{
                  id dst src created_at
                  block_id src_dapp_id
                  src_transaction {{ id block_id }}
                }} }}
              }}
            }}
          }}
        }}'''
        try:
            data = self._post(q)
        except (urllib.error.URLError, TimeoutError):
            return []
        edges = (((data.get("data") or {}).get("blockchain") or {})
                 .get("account") or {}).get("messages", {}).get("edges") or []
        out = []
        for e in edges:
            n = e["node"]
            if not n.get("block_id"):
                tx = n.get("src_transaction") or {}
                n["block_id"] = tx.get("block_id") if isinstance(tx, dict) else None
            out.append(n)
        return out

    def fetch_block(self, block_hash_or_id: str) -> dict | None:
        # Endpoint's BlockchainQuery.block only takes `hash`; block_id lookup
        # goes through blockByHeight elsewhere.
        q = f'''{{
          blockchain {{
            block(hash: "{block_hash_or_id}") {{
              hash block_id seq_no thread_id proof_block_refs
            }}
          }}
        }}'''
        try:
            data = self._post(q)
        except (urllib.error.URLError, TimeoutError):
            return None
        return (((data.get("data") or {}).get("blockchain") or {}).get("block"))

    def fetch_latest_blocks(self, limit: int = 50) -> list[dict]:
        # `last: N` returns the newest N blocks (ascending seq order).
        q = f'''{{
          blockchain {{
            blocks(last: {limit}) {{
              edges {{ node {{
                hash block_id seq_no thread_id proof_block_refs
              }} }}
            }}
          }}
        }}'''
        try:
            data = self._post(q)
        except (urllib.error.URLError, TimeoutError):
            return []
        edges = (((data.get("data") or {}).get("blockchain") or {})
                 .get("blocks") or {}).get("edges") or []
        return [e["node"] for e in edges if isinstance(e, dict) and "node" in e]


# ── Global block DAG state ───────────────────────────────────────────────────
class Graph:
    """
    In-memory DAG of every block we've seen.

    Every block's `refs[i]` is a directed edge to an *older* block.
    We also store the reverse map so that BFS from X (older) can
    walk newer-ward without scanning the whole store on every step.
    """
    def __init__(self) -> None:
        # canonical block key = hash (falling back to block_id when the
        # producer only exposes block_id).
        self.blocks:     dict[str, dict] = {}    # key → normalised block record
        self.parents_of: dict[str, set[str]] = collections.defaultdict(set)
        # thread_hex → set(block key)
        self.by_thread:  dict[str, set[str]] = collections.defaultdict(set)
        # per-thread max seq_no we've seen (for orphan diagnostics)
        self.thread_tip: dict[str, int] = collections.defaultdict(int)
        # counters
        self.total_edges = 0

    @staticmethod
    def key_of(raw: dict) -> str | None:
        """Prefer `hash` when present; fall back to `block_id`."""
        for k in ("hash", "block_id"):
            v = raw.get(k)
            if v:
                return v
        return None

    def ingest(self, raw: dict) -> bool:
        """Return True if this is a first-time ingest."""
        key = self.key_of(raw)
        if not key or key in self.blocks:
            return False
        thread = normalize_thread_id(raw.get("thread_id"))
        try:
            seq = int(raw.get("seq_no") or -1)
        except (TypeError, ValueError):
            seq = -1
        refs = list(raw.get("proof_block_refs") or [])
        # Normalise & filter empties.
        norm_refs = [r for r in refs if isinstance(r, str) and r]
        rec = {
            "key":    key,
            "hash":   raw.get("hash"),
            "block_id": raw.get("block_id"),
            "thread": thread,
            "seq":    seq,
            "refs":   norm_refs,
        }
        self.blocks[key] = rec
        self.by_thread[thread].add(key)
        if seq > self.thread_tip[thread]:
            self.thread_tip[thread] = seq
        for target in norm_refs:
            self.parents_of[target].add(key)
            self.total_edges += 1
        return True

    def has(self, key: str) -> bool:
        return key in self.blocks

    def num_blocks(self) -> int:
        return len(self.blocks)


# ── BFS: enumerate paths Y → … → X on the reverse graph ─────────────────────
def enumerate_paths(graph: Graph,
                    x_key: str,
                    max_hop_budget: int,
                    max_paths: int) -> list[dict]:
    """
    BFS from X on the reverse graph.  Each time the BFS reaches a
    thread-0 block Y we record the shortest Y → … → X path via
    predecessor reconstruction.  A thread-0 node is not extended past
    itself: it *is* the anchor.

    Returns a list of dicts, sorted by hop length ascending, capped
    at `max_paths`.  A dict looks like:

        {
          "length": 3,
          "signature": "t0 -> t1a2b -> t2c3d -> X",
          "anchor": {"block_id": "…", "thread": "0000…", "seq": 12400},
          "hops": [{...}, ...],   # ordered Y-hop first, X-hop last
        }
    """
    if x_key not in graph.blocks:
        return []
    x_rec = graph.blocks[x_key]

    # BFS state
    distance:    dict[str, int] = {x_key: 0}
    predecessor: dict[str, tuple[str, int]] = {}   # newer_key → (older_key, slot)
    queue: collections.deque[str] = collections.deque([x_key])
    anchors: list[tuple[int, str]] = []            # (distance, y_key)

    while queue and len(anchors) < max_paths:
        cur = queue.popleft()
        d = distance[cur]
        if d >= max_hop_budget:
            continue
        cur_rec = graph.blocks.get(cur)
        if cur_rec is not None and is_thread_0(cur_rec["thread"]) and d > 0:
            anchors.append((d, cur))
            # Don't extend past a thread-0 anchor — Y IS the anchor.
            continue
        for parent_key in graph.parents_of.get(cur, ()):
            if parent_key in distance:
                continue
            parent_rec = graph.blocks.get(parent_key)
            if parent_rec is None:
                continue
            # Which slot in parent.refs points at cur?
            try:
                slot = parent_rec["refs"].index(cur)
            except ValueError:
                slot = -1
            distance[parent_key] = d + 1
            predecessor[parent_key] = (cur, slot)
            queue.append(parent_key)

    # Reconstruct paths for each anchor.
    paths: list[dict] = []
    for _dist, y_key in anchors:
        y_rec = graph.blocks[y_key]
        hops: list[dict] = []
        cur = y_key
        # Walk predecessor chain: at each step, `cur` is the newer
        # block; predecessor[cur] gives the older (target-ward) hop.
        while cur in predecessor:
            older_key, slot = predecessor[cur]
            cur_rec = graph.blocks[cur]
            old_rec = graph.blocks[older_key]
            hops.append({
                "from_block":  cur_rec.get("hash") or cur_rec.get("block_id") or cur,
                "from_thread": cur_rec["thread"],
                "from_seq":    cur_rec["seq"],
                "slot":        slot,
                "to_block":    old_rec.get("hash") or old_rec.get("block_id") or older_key,
                "to_thread":   old_rec["thread"],
                "to_seq":      old_rec["seq"],
            })
            cur = older_key
        signature = " -> ".join(
            [short_thread(y_rec["thread"])]
            + [short_thread(h["to_thread"]) for h in hops]
        )
        paths.append({
            "length":    len(hops),
            "signature": signature,
            "anchor": {
                "block_id": y_rec.get("hash") or y_rec.get("block_id") or y_key,
                "thread":   y_rec["thread"],
                "seq":      y_rec["seq"],
            },
            "hops": hops,
        })
    paths.sort(key=lambda p: p["length"])
    return paths


# ── Pending events ───────────────────────────────────────────────────────────
class PendingEvent:
    __slots__ = ("msg_id", "x_key", "x_thread", "x_seq",
                 "first_seen_wall", "deadline_wall",
                 "paths", "path_keys",
                 "shortest_length", "shortest_first_wall",
                 "first_path_wall", "best_path_wall",
                 "unresolved_x")

    def __init__(self, msg_id: str, x_key: str,
                 x_thread: str, x_seq: int,
                 first_seen_wall: float, deadline_wall: float):
        self.msg_id            = msg_id
        self.x_key             = x_key
        self.x_thread          = x_thread
        self.x_seq             = x_seq
        self.first_seen_wall   = first_seen_wall
        self.deadline_wall     = deadline_wall
        self.paths:      list[dict]   = []
        self.path_keys:  set[tuple]   = set()
        self.shortest_length:    int | None   = None
        self.shortest_first_wall: float | None = None
        self.first_path_wall:    float | None = None
        self.best_path_wall:     float | None = None
        self.unresolved_x        = False


def path_key(p: dict) -> tuple:
    return tuple((h["from_block"], h["to_block"]) for h in p["hops"])


# ── Main loop ────────────────────────────────────────────────────────────────
def log(msg: str, verbose: bool = True) -> None:
    if verbose:
        print(f"[{now_iso()}] {msg}", file=sys.stderr, flush=True)


def poll_and_ingest(gql: GqlClient, graph: Graph,
                    scan_window: int,
                    fetch_budget: int,
                    verbose: bool) -> int:
    """Pull latest N blocks, ingest new ones, fetch referenced blocks
    we haven't seen (bounded by `fetch_budget` per call).

    Returns the number of newly-ingested blocks (including on-demand
    fetches)."""
    fresh = 0
    latest = gql.fetch_latest_blocks(limit=scan_window)
    to_expand: list[str] = []
    for node in latest:
        if graph.ingest(node):
            fresh += 1
            for r in node.get("proof_block_refs") or []:
                if r and r not in graph.blocks:
                    to_expand.append(r)

    # Bounded on-demand expansion of missing refs.  Follow the chain
    # for a limited hop count so the parent-chain fills in.
    seen_this_call: set[str] = set()
    fetched = 0
    frontier = collections.deque(to_expand)
    while frontier and fetched < fetch_budget:
        tgt = frontier.popleft()
        if tgt in graph.blocks or tgt in seen_this_call:
            continue
        seen_this_call.add(tgt)
        blk = gql.fetch_block(tgt)
        if not blk:
            continue
        if graph.ingest(blk):
            fresh += 1
            fetched += 1
            for r in blk.get("proof_block_refs") or []:
                if r and r not in graph.blocks and r not in seen_this_call:
                    frontier.append(r)
    if verbose and fresh:
        log(f"  ingested {fresh} blocks "
            f"({fetched} via on-demand); graph now "
            f"{graph.num_blocks()} blocks / {graph.total_edges} edges",
            verbose)
    return fresh


def resolve_event_block(gql: GqlClient, graph: Graph, blk_id: str) -> str | None:
    """Look up the event's carrier block; return the graph key."""
    if not blk_id:
        return None
    if blk_id in graph.blocks:
        return blk_id
    blk = gql.fetch_block(blk_id)
    if not blk:
        return None
    graph.ingest(blk)
    return graph.key_of(blk)


def ingest_events(gql: GqlClient, graph: Graph,
                  pending: dict[str, PendingEvent],
                  seen_msg_ids: set[str],
                  ev_limit: int,
                  observation_window_s: float,
                  max_wait_anchor_s: float,
                  event_dst_filter: str,
                  verbose: bool) -> int:
    new_ev = 0
    msgs = gql.fetch_bridge_extouts(limit=ev_limit)
    for m in msgs:
        mid = m.get("id")
        if not mid or mid in seen_msg_ids:
            continue
        seen_msg_ids.add(mid)
        # optional dst-filter
        if event_dst_filter and (m.get("dst") or "") != event_dst_filter:
            continue
        blk_id = m.get("block_id")
        x_key  = resolve_event_block(gql, graph, blk_id) if blk_id else None
        if x_key is None:
            # Register a placeholder; if the block never resolves we
            # emit `event_block_unresolved`.  We still allow a graceful
            # deadline so subsequent polls have a chance to catch it.
            wall = time.time()
            ev = PendingEvent(
                msg_id=mid, x_key=blk_id or f"<unresolved:{mid}>",
                x_thread="<unknown>", x_seq=-1,
                first_seen_wall=wall,
                deadline_wall=wall + max_wait_anchor_s,
            )
            ev.unresolved_x = True
            pending[mid] = ev
            new_ev += 1
            continue
        x_rec = graph.blocks[x_key]
        wall  = time.time()
        ev = PendingEvent(
            msg_id=mid,
            x_key=x_key,
            x_thread=x_rec["thread"],
            x_seq=x_rec["seq"],
            first_seen_wall=wall,
            # Two deadlines merged: hard orphan cap + soft observation
            # window that only starts counting once we have first path.
            deadline_wall=wall + observation_window_s + max_wait_anchor_s,
        )
        pending[mid] = ev
        new_ev += 1
        if verbose:
            same = "same-thread-0" if is_thread_0(x_rec["thread"]) else \
                   f"thread {short_thread(x_rec['thread'])}"
            log(f"  new event {mid} in {same} @ seq={x_rec['seq']}", verbose)
    return new_ev


def reevaluate(pending: dict[str, PendingEvent],
               graph: Graph,
               max_hop_budget: int,
               max_paths_per_event: int,
               observation_window_s: float,
               verbose: bool) -> None:
    """Re-run BFS for each pending event.  Update .paths and record
    first-path / best-path wall times."""
    now = time.time()
    for ev in pending.values():
        if ev.unresolved_x:
            continue
        # Skip trivial same-thread-0 events (handled at close time).
        if is_thread_0(ev.x_thread):
            continue
        found = enumerate_paths(graph, ev.x_key,
                                max_hop_budget=max_hop_budget,
                                max_paths=max_paths_per_event)
        if not found:
            continue
        new_added = 0
        prev_shortest = ev.shortest_length
        for p in found:
            k = path_key(p)
            if k in ev.path_keys:
                continue
            ev.path_keys.add(k)
            ev.paths.append(p)
            new_added += 1
        if new_added:
            ev.paths.sort(key=lambda p: p["length"])
            best_now = ev.paths[0]["length"]
            if ev.first_path_wall is None:
                ev.first_path_wall = now
            if prev_shortest is None or best_now < prev_shortest:
                ev.shortest_length      = best_now
                ev.shortest_first_wall  = now
                ev.best_path_wall       = now
        # Move deadline in: once we've had a path for `observation_window_s`,
        # we can close.
        if ev.first_path_wall is not None:
            closer = ev.first_path_wall + observation_window_s
            if closer < ev.deadline_wall:
                ev.deadline_wall = closer


def emit_close(ev: PendingEvent, graph: Graph,
               observation_window_s: float,
               outfh) -> str:
    if ev.unresolved_x:
        outcome = "event_block_unresolved"
    elif is_thread_0(ev.x_thread):
        outcome = "same_thread_trivial"
    elif not ev.paths:
        outcome = "orphan_timeout"
    else:
        outcome = "ok"

    # Length histogram.
    length_hist: dict[int, int] = collections.Counter()
    for p in ev.paths:
        length_hist[p["length"]] += 1

    record = {
        "schema":                "direction_b_multipath.v1",
        "event_msg_id":          ev.msg_id,
        "event_first_seen_wall": ev.first_seen_wall,
        "x_block_id":            ev.x_key,
        "x_thread":              ev.x_thread,
        "x_seq":                 ev.x_seq,
        "x_is_thread0":          is_thread_0(ev.x_thread),
        "outcome":               outcome,
        "observation_window_s":  observation_window_s,
        "num_paths":             len(ev.paths),
        "shortest_length":       ev.shortest_length,
        "shortest_path":         ev.paths[0] if ev.paths else None,
        "all_paths":             ev.paths,
        "length_histogram":      {str(k): v for k, v in sorted(length_hist.items())},
        "T_first_path_wall_s":   (ev.first_path_wall - ev.first_seen_wall)
                                  if ev.first_path_wall else None,
        "T_best_path_wall_s":    (ev.best_path_wall - ev.first_seen_wall)
                                  if ev.best_path_wall else None,
        "graph_size_at_close":   {"blocks": graph.num_blocks(),
                                  "edges":  graph.total_edges},
        "closed_wall":           time.time(),
    }
    outfh.write(json.dumps(record, separators=(",", ":")) + "\n")
    outfh.flush()
    return outcome


def sweep_closed(pending: dict[str, PendingEvent], graph: Graph,
                 observation_window_s: float, outfh,
                 verbose: bool) -> int:
    now = time.time()
    to_close = [mid for mid, ev in pending.items() if now >= ev.deadline_wall]
    for mid in to_close:
        ev = pending.pop(mid)
        outcome = emit_close(ev, graph, observation_window_s, outfh)
        if verbose:
            best = ev.paths[0]["length"] if ev.paths else "-"
            log(f"  close {mid}: {outcome} best_len={best} "
                f"paths={len(ev.paths)}", verbose)
    return len(to_close)


# ── Signal-driven shutdown ────────────────────────────────────────────────────
_shutdown = False
def _install_signals() -> None:
    def h(signum, frame):
        global _shutdown
        _shutdown = True
    signal.signal(signal.SIGINT, h)
    signal.signal(signal.SIGTERM, h)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--graphql", default="http://localhost/graphql",
                    help="GraphQL endpoint (default: %(default)s).")
    ap.add_argument("--out", required=True,
                    help="Output JSONL path.  One record per closed event.")
    ap.add_argument("--poll-interval", type=float, default=2.0,
                    help="Seconds between polls (default: %(default)s).")
    ap.add_argument("--scan-window", type=int, default=50,
                    help="Number of latest blocks pulled per poll "
                         "(default: %(default)s).")
    ap.add_argument("--fetch-budget-per-poll", type=int, default=200,
                    help="Max on-demand block fetches per poll "
                         "(default: %(default)s).")
    ap.add_argument("--observation-window-s", type=float, default=300.0,
                    help="Seconds to keep watching after the first path "
                         "is found (default: %(default)s).  Also acts as "
                         "the wall-clock window for a longer path to be "
                         "supplanted by a shorter alternative.")
    ap.add_argument("--max-wait-anchor-s", type=float, default=600.0,
                    help="Hard orphan cap: if NO path is found within "
                         "this many seconds, emit `orphan_timeout` "
                         "(default: %(default)s).")
    ap.add_argument("--max-hop-budget", type=int, default=60,
                    help="BFS depth ceiling in hops (default: %(default)s). "
                         "Protocol soft cap is 50 for a same-thread parent "
                         "walk; setting >50 catches indirect paths that "
                         "exceed 50 hops.")
    ap.add_argument("--max-paths-per-event", type=int, default=20,
                    help="Cap on distinct anchors recorded per event "
                         "(default: %(default)s).")
    ap.add_argument("--limit-events", type=int, default=500,
                    help="Max ExtOut messages fetched per poll "
                         "(default: %(default)s).")
    ap.add_argument("--event-dst", default="",
                    help="Filter ExtOut by `dst` (default: accept all).")
    ap.add_argument("--heartbeat-s", type=float, default=30.0,
                    help="Print heartbeat every N seconds "
                         "(default: %(default)s).")
    ap.add_argument("--verbose", action="store_true", default=True)
    ap.add_argument("--quiet", dest="verbose", action="store_false")
    args = ap.parse_args()

    outdir = os.path.dirname(args.out) or "."
    os.makedirs(outdir, exist_ok=True)

    log(f"config: poll={args.poll_interval}s window={args.observation_window_s}s "
        f"orphan_cap={args.max_wait_anchor_s}s scan={args.scan_window} "
        f"fetch_budget={args.fetch_budget_per_poll} "
        f"max_hops={args.max_hop_budget} max_paths={args.max_paths_per_event}",
        args.verbose)
    log(f"output: {args.out}", args.verbose)

    _install_signals()
    gql   = GqlClient(args.graphql)
    graph = Graph()
    pending:      dict[str, PendingEvent] = {}
    seen_msg_ids: set[str]                = set()

    last_beat = time.time()
    events_closed = 0
    with open(args.out, "a", encoding="utf-8") as outfh:
        while not _shutdown:
            t0 = time.time()
            try:
                poll_and_ingest(gql, graph,
                                scan_window=args.scan_window,
                                fetch_budget=args.fetch_budget_per_poll,
                                verbose=args.verbose)
                ingest_events(gql, graph, pending, seen_msg_ids,
                              ev_limit=args.limit_events,
                              observation_window_s=args.observation_window_s,
                              max_wait_anchor_s=args.max_wait_anchor_s,
                              event_dst_filter=args.event_dst,
                              verbose=args.verbose)
                reevaluate(pending, graph,
                           max_hop_budget=args.max_hop_budget,
                           max_paths_per_event=args.max_paths_per_event,
                           observation_window_s=args.observation_window_s,
                           verbose=args.verbose)
                events_closed += sweep_closed(pending, graph,
                                              args.observation_window_s,
                                              outfh, args.verbose)
            except Exception as ex:
                log(f"poll error: {ex!r}", args.verbose)

            if time.time() - last_beat >= args.heartbeat_s:
                last_beat = time.time()
                log(f"heartbeat: pending={len(pending)} closed={events_closed} "
                    f"blocks={graph.num_blocks()} edges={graph.total_edges} "
                    f"gql_calls={gql.gql_calls}", args.verbose)

            slept = time.time() - t0
            remaining = args.poll_interval - slept
            if remaining > 0:
                time.sleep(remaining)

    # Flush remaining events on shutdown as best-effort.
    with open(args.out, "a", encoding="utf-8") as outfh:
        for mid, ev in list(pending.items()):
            emit_close(ev, graph, args.observation_window_s, outfh)
            events_closed += 1
    log(f"shutdown: closed={events_closed}", args.verbose)
    return 0


if __name__ == "__main__":
    sys.exit(main())
