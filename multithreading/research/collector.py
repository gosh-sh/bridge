#!/usr/bin/env python3
"""
Direction (b) reachability collector — cross-thread WithdrawalInitiated coverage.

Runs alongside `tests/mt/cli.py test-multithread-cross-thread` and a
WithdrawalInitiated event trigger (`trigger_loop.py`). For every event it
records where the event landed (thread + seq) and — if the event landed on a
non-default thread — the first future thread-0 block Y that transitively
references it via `Y.refs[i>=1] -> B (thread t, seq >= x_seq)` and then B's
same-thread parent chain `refs[0]` down to X.

Model backing this walk lives in
`bridge/multithreading/docs/walker_algorithm.md`:

  * `proof_block_refs[0]` is the same-thread parent (per acki-nacki
    `node/src/types/ackinacki_block/mod.rs:549-552`).
  * `proof_block_refs[1..]` are cross-thread refs; producer emits one ref per
    referenced thread, monotone in that thread's seq (`should_include`,
    `process.rs:530-540`).
  * Enforcement bounds thread-0 lag on each other thread to <=50 finalized
    blocks (`cross_thread_ref_enforcement/mod.rs:56`).

The collector is topology-only: it does NOT reconstruct SHA-256/Poseidon
openings. Its output feeds `analyzer.py` for post-hoc histograms.

Usage:
    python collector.py \
        --graphql http://localhost/graphql \
        --out research/stats/dirb-$(date +%Y%m%d-%H%M).jsonl \
        --poll-interval 2.0 \
        --max-wait-anchor-s 600 \
        --max-parent-hops 128

Output: one JSONL line per event, plus a periodic `[collector]` heartbeat.
"""
from __future__ import annotations
import argparse
import json
import os
import signal
import sys
import time
import urllib.error
import urllib.request
import uuid

# ── Constants ────────────────────────────────────────────────────────────────
# USDCBridge deployment (mirrors research/reachability_collector.py).
USDC_BRIDGE_ACCOUNT_ID = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
USDC_BRIDGE_DAPP_ID    = "0" * 64
# WithdrawalInitiated ExtOut `dst` field. Empty = accept all ExtOut from USDCBridge.
WITHDRAWAL_EVENT_DST_DEFAULT = ""
# 68-hex-char default thread id from bridge-gql-fetcher/src/gql_client.rs:964.
DEFAULT_THREAD_ID_HEX = "0" * 68


# ── Helpers ──────────────────────────────────────────────────────────────────
def is_thread_0(t) -> bool:
    """Accepts int, "0", or hex-string of all zeros (with or without 0x)."""
    if t is None:
        return False
    if isinstance(t, int):
        return t == 0
    if isinstance(t, str):
        s = t.lower()
        if s.startswith("0x"):
            s = s[2:]
        if s == "":
            return False
        return all(c == "0" for c in s)
    return False


def normalize_thread_id(t) -> str:
    """Return a canonical string form so we can key dicts by thread."""
    if t is None:
        return "<none>"
    if isinstance(t, int):
        return f"int:{t}"
    if isinstance(t, str):
        s = t.lower()
        if s.startswith("0x"):
            s = s[2:]
        return s
    return f"raw:{t!r}"


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


# ── Minimal GraphQL client (stdlib only) ─────────────────────────────────────
class GqlClient:
    def __init__(self, url: str, timeout: int = 15,
                 user_agent: str = "direction-b-collector/1.0"):
        self.url = url
        self.timeout = timeout
        self.user_agent = user_agent

    def _post(self, query: str) -> dict:
        req = urllib.request.Request(
            self.url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json",
                     "User-Agent": self.user_agent},
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
        data = self._post(q)
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
        """Fetch a block by hash OR block_id. Returns dict with
        {hash, block_id, seq_no, thread_id, proof_block_refs, prev_hash} or None."""
        for key in ("hash", "block_id"):
            q = f'''{{
              blockchain {{
                block({key}: "{block_hash_or_id}") {{
                  hash block_id seq_no thread_id proof_block_refs prev_hash
                }}
              }}
            }}'''
            try:
                data = self._post(q)
            except (urllib.error.URLError, TimeoutError):
                return None
            b = (((data.get("data") or {}).get("blockchain") or {}).get("block"))
            if b is not None:
                return b
        return None

    def fetch_latest_blocks(self, limit: int = 20) -> list[dict]:
        """Order by seq_no desc across all threads. Same query shape as
        `runbooks/run_multipath_session.md` verification snippet."""
        q = f'''{{
          blockchain {{
            blocks(order_by:{{seq_no:desc}}, limit: {limit}) {{
              edges {{ node {{
                hash block_id seq_no thread_id proof_block_refs prev_hash
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

    def fetch_thread0_by_height(self, height: int) -> dict | None:
        """Fetch thread-0 block at a specific seq_no via blockByHeight."""
        q = f'''{{
          blockchain {{
            blockByHeight(thread_id: "{DEFAULT_THREAD_ID_HEX}", height: {height}) {{
              hash block_id seq_no thread_id proof_block_refs prev_hash
            }}
          }}
        }}'''
        try:
            data = self._post(q)
        except (urllib.error.URLError, TimeoutError):
            return None
        return ((data.get("data") or {}).get("blockchain") or {}).get("blockByHeight")


# ── State ────────────────────────────────────────────────────────────────────
class State:
    """In-memory index of thread-0 refs and pending event correlations."""
    def __init__(self):
        # thread_id_norm -> list of dicts (kept sorted by y_seq asc):
        #   {y_seq, y_block_id, y_hash, slot_i, b_seq, b_block_id, b_hash, wall_ts}
        self.thread0_view: dict[str, list[dict]] = {}
        # thread-0 seq_nos we've already indexed (dedup)
        self.thread0_seen_seq: set[int] = set()
        # msg ids we've already emitted (both success + orphan)
        self.emitted_events: set[str] = set()
        # msg ids to skip (baseline at startup)
        self.baseline_events: set[str] = set()
        # pending events awaiting anchor
        # msg_id -> {x_block_hash, x_block_id, x_seq, x_thread_norm, event_wall_ts, deadline_wall}
        self.pending: dict[str, dict] = {}


# ── Thread-0 index maintenance ───────────────────────────────────────────────
def ingest_thread0_block(gql: GqlClient, state: State, y_block: dict,
                         log) -> None:
    """Given a thread-0 block, extract its cross-thread refs (slots 1..) and
    index (y_seq, y_id, slot_i, b_seq, b_id, b_thread) into thread0_view."""
    y_seq = y_block.get("seq_no")
    if y_seq is None:
        return
    y_seq = int(y_seq)
    if y_seq in state.thread0_seen_seq:
        return
    state.thread0_seen_seq.add(y_seq)

    refs = y_block.get("proof_block_refs") or []
    # Slot 0 is same-thread parent — irrelevant for cross-thread anchoring.
    for slot_i in range(1, len(refs)):
        ref_id = refs[slot_i]
        b = gql.fetch_block(ref_id)
        if b is None:
            log(f"    [warn] could not fetch ref block {ref_id} at y_seq={y_seq}")
            continue
        b_thread = normalize_thread_id(b.get("thread_id"))
        b_seq_raw = b.get("seq_no")
        if b_seq_raw is None:
            continue
        entry = {
            "y_seq":       y_seq,
            "y_hash":      y_block.get("hash"),
            "y_block_id":  y_block.get("block_id"),
            "slot_i":      slot_i,
            "b_thread":    b_thread,
            "b_seq":       int(b_seq_raw),
            "b_hash":      b.get("hash"),
            "b_block_id":  b.get("block_id"),
            "wall_ts":     time.time(),
        }
        bucket = state.thread0_view.setdefault(b_thread, [])
        bucket.append(entry)
        log(f"    thread0[y_seq={y_seq}] slot={slot_i} -> "
            f"thread={b_thread[:8]}... b_seq={entry['b_seq']}")


def refresh_thread0_index(gql: GqlClient, state: State, log,
                          scan_window: int = 50, gap_fill_max: int = 200) -> None:
    """Pull the newest block tips, filter to thread-0, ingest any we haven't
    seen; then fill any gap between the highest known thread-0 seq and the
    latest observed seq via blockByHeight (so poll skips don't drop Y's).

    gap_fill_max caps the backfill per refresh — avoids blocking the poll
    loop if the collector fell far behind."""
    latest = gql.fetch_latest_blocks(limit=scan_window)
    thread0 = [b for b in latest if is_thread_0(b.get("thread_id"))]
    if not thread0:
        return
    thread0.sort(key=lambda b: int(b.get("seq_no") or 0))
    max_seen = max(int(b.get("seq_no") or 0) for b in thread0)

    # Ingest the ones we just fetched.
    for b in thread0:
        ingest_thread0_block(gql, state, b, log)

    # Gap-fill via blockByHeight if there are missing seqs below max_seen.
    if state.thread0_seen_seq:
        known_max = max(state.thread0_seen_seq)
        # Only fill upward from the highest contiguous point; if the state
        # is fully populated up to known_max, missing entries between the
        # oldest-seen and known_max are ancient history — skip them.
        start = known_max + 1
    else:
        start = max_seen  # first run: just take the tip.
    end = min(max_seen, start + gap_fill_max - 1)
    for h in range(start, end + 1):
        if h in state.thread0_seen_seq:
            continue
        b = gql.fetch_thread0_by_height(h)
        if b is None:
            log(f"    [gap-fill] blockByHeight({h}) returned null")
            continue
        ingest_thread0_block(gql, state, b, log)


# ── Parent-chain walk (B -> ... -> X within a thread) ────────────────────────
def walk_parent_chain(gql: GqlClient, b_block_id: str, b_hash: str,
                      x_block_id: str, x_hash: str,
                      max_parent_hops: int, log) -> list[dict] | None:
    """Walk refs[0] repeatedly from B down to X. Returns list of hop dicts or None."""
    hops: list[dict] = []
    cur_hash = b_hash or b_block_id
    cur = gql.fetch_block(cur_hash)
    if cur is None:
        return None

    for _ in range(max_parent_hops):
        cur_bid = cur.get("block_id")
        cur_h   = cur.get("hash")
        if cur_bid == x_block_id or cur_h == x_hash:
            return hops
        refs = cur.get("proof_block_refs") or []
        if not refs:
            return None
        parent_ref = refs[0]
        nxt = gql.fetch_block(parent_ref)
        if nxt is None:
            log(f"    [warn] parent walk stuck at block {cur_bid}; "
                f"could not fetch parent {parent_ref}")
            return None
        hops.append({
            "from_block_id":  cur_bid,
            "from_hash":      cur_h,
            "from_seq":       int(cur["seq_no"]) if cur.get("seq_no") is not None else None,
            "from_thread":    normalize_thread_id(cur.get("thread_id")),
            "to_block_id":    nxt.get("block_id"),
            "to_hash":        nxt.get("hash"),
            "to_seq":         int(nxt["seq_no"]) if nxt.get("seq_no") is not None else None,
            "ref_slot_taken": 0,          # parent slot
        })
        cur = nxt

    # Ran out of budget.
    return None


# ── Event ingest ─────────────────────────────────────────────────────────────
def ingest_new_events(gql: GqlClient, state: State, dst_filter: str,
                      limit: int, max_wait_anchor_s: float, log) -> None:
    """Poll USDCBridge ExtOut, add new events to pending set."""
    try:
        nodes = gql.fetch_bridge_extouts(limit=limit)
    except (urllib.error.URLError, TimeoutError) as ex:
        log(f"[warn] GQL fetch_bridge_extouts: {ex}")
        return
    seen = state.emitted_events | state.baseline_events | set(state.pending.keys())
    new = [n for n in nodes if n["id"] not in seen]
    if dst_filter:
        new = [n for n in new if n.get("dst") == dst_filter]
    new.sort(key=lambda n: n.get("created_at") or 0)

    for n in new:
        block_hash = n.get("block_id")
        if not block_hash:
            log(f"[event] msg={n['id']} block_id=null (transaction not yet indexed?)")
            state.pending[n["id"]] = {
                "msg_id": n["id"],
                "x_block_hash": None,
                "x_block_id":   None,
                "x_seq":        None,
                "x_thread":     "<pending-block>",
                "event_wall_ts": time.time(),
                "deadline_wall": time.time() + max_wait_anchor_s,
                "src":          n.get("src"),
                "dst":          n.get("dst"),
                "created_at":   n.get("created_at"),
                "src_dapp_id":  n.get("src_dapp_id"),
            }
            continue

        block = gql.fetch_block(block_hash)
        if block is None:
            log(f"[event] msg={n['id']} block fetch failed for {block_hash}")
            state.pending[n["id"]] = {
                "msg_id": n["id"],
                "x_block_hash": block_hash,
                "x_block_id":   None,
                "x_seq":        None,
                "x_thread":     "<fetch-failed>",
                "event_wall_ts": time.time(),
                "deadline_wall": time.time() + max_wait_anchor_s,
                "src":          n.get("src"),
                "dst":          n.get("dst"),
                "created_at":   n.get("created_at"),
                "src_dapp_id":  n.get("src_dapp_id"),
            }
            continue

        x_thread = normalize_thread_id(block.get("thread_id"))
        x_seq_raw = block.get("seq_no")
        x_seq = int(x_seq_raw) if x_seq_raw is not None else None
        entry = {
            "msg_id":        n["id"],
            "x_block_hash":  block_hash,
            "x_block_id":    block.get("block_id"),
            "x_hash":        block.get("hash"),
            "x_seq":         x_seq,
            "x_thread":      x_thread,
            "x_is_thread0":  is_thread_0(block.get("thread_id")),
            "event_wall_ts": time.time(),
            "deadline_wall": time.time() + max_wait_anchor_s,
            "src":           n.get("src"),
            "dst":           n.get("dst"),
            "created_at":    n.get("created_at"),
            "src_dapp_id":   n.get("src_dapp_id"),
        }
        state.pending[n["id"]] = entry
        log(f"[event] +msg={n['id'][:10]}... "
            f"x_thread={x_thread[:12]}... x_seq={x_seq} "
            f"is_thread0={entry['x_is_thread0']}")


# ── Correlation ─────────────────────────────────────────────────────────────
def find_first_anchor(state: State, x_thread: str, x_seq: int) -> dict | None:
    """First thread-0 entry (Y, B) with B.thread == x_thread and B.seq >= x_seq.
    thread0_view[t] is append-in-y_seq-order; since should_include is monotone
    per thread, the first hit is also the smallest B.seq >= x_seq."""
    for e in state.thread0_view.get(x_thread, []):
        if e["b_seq"] >= x_seq:
            return e
    return None


def close_event(state: State, gql: GqlClient, msg_id: str, ev: dict,
                emit, log, max_parent_hops: int) -> bool:
    """Try to resolve one pending event. Returns True if closed (emitted)."""
    # Case A: unresolved block info at ingest — try once more, else timeout later.
    if ev.get("x_seq") is None:
        if ev["x_block_hash"] is not None:
            block = gql.fetch_block(ev["x_block_hash"])
            if block is not None:
                ev["x_block_id"]   = block.get("block_id")
                ev["x_hash"]       = block.get("hash")
                ev["x_seq"]        = int(block["seq_no"]) if block.get("seq_no") is not None else None
                ev["x_thread"]     = normalize_thread_id(block.get("thread_id"))
                ev["x_is_thread0"] = is_thread_0(block.get("thread_id"))
        if ev.get("x_seq") is None:
            if time.time() > ev["deadline_wall"]:
                emit(_record(ev, outcome="event_block_unresolved",
                             hop_count=None, cross_thread_hops=None,
                             same_thread_hops=None))
                return True
            return False

    # Case B: event lives in thread 0. Trivial.
    if ev.get("x_is_thread0"):
        emit(_record(ev, outcome="same_thread_trivial",
                     hop_count=0, cross_thread_hops=0, same_thread_hops=0))
        return True

    # Case C: cross-thread. Look for anchor.
    hit = find_first_anchor(state, ev["x_thread"], ev["x_seq"])
    if hit is None:
        if time.time() > ev["deadline_wall"]:
            emit(_record(ev, outcome="orphan_timeout",
                         hop_count=None, cross_thread_hops=None,
                         same_thread_hops=None))
            return True
        return False

    # Case D: anchor found. Walk B -> ... -> X.
    log(f"[correlate] msg={ev['msg_id'][:10]}... anchor: "
        f"y_seq={hit['y_seq']} slot={hit['slot_i']} b_seq={hit['b_seq']}")
    parent_hops = walk_parent_chain(
        gql,
        b_block_id=hit["b_block_id"], b_hash=hit["b_hash"],
        x_block_id=ev["x_block_id"], x_hash=ev.get("x_hash"),
        max_parent_hops=max_parent_hops, log=log,
    )
    if parent_hops is None:
        emit(_record(ev, outcome="parent_walk_failed",
                     hop_count=None, cross_thread_hops=1,
                     same_thread_hops=None,
                     anchor=hit))
        return True

    same_thread_hops = len(parent_hops)
    cross_thread_hops = 1  # Y -> B (direct-thread case)
    total_hops = cross_thread_hops + same_thread_hops
    emit(_record(ev, outcome="ok",
                 hop_count=total_hops,
                 cross_thread_hops=cross_thread_hops,
                 same_thread_hops=same_thread_hops,
                 anchor=hit,
                 parent_hops=parent_hops))
    return True


def _record(ev: dict, *, outcome: str,
            hop_count: int | None,
            cross_thread_hops: int | None,
            same_thread_hops: int | None,
            anchor: dict | None = None,
            parent_hops: list[dict] | None = None) -> dict:
    rec = {
        "event_msg_id":  ev["msg_id"],
        "event_wall_ts": time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                       time.gmtime(ev["event_wall_ts"])),
        "close_wall_ts": now_iso(),
        "T_anchor_wall_s": (time.time() - ev["event_wall_ts"]) if outcome == "ok" else None,
        "src":           ev.get("src"),
        "dst":           ev.get("dst"),
        "src_dapp_id":   ev.get("src_dapp_id"),
        "created_at":    ev.get("created_at"),
        "x_block_hash":  ev.get("x_block_hash"),
        "x_block_id":    ev.get("x_block_id"),
        "x_seq":         ev.get("x_seq"),
        "x_thread":      ev.get("x_thread"),
        "x_is_thread0":  ev.get("x_is_thread0"),
        "outcome":       outcome,
        "hop_count":     hop_count,
        "cross_thread_hops": cross_thread_hops,
        "same_thread_hops":  same_thread_hops,
    }
    if anchor:
        rec["anchor"] = {
            "y_seq":      anchor["y_seq"],
            "y_hash":     anchor["y_hash"],
            "y_block_id": anchor["y_block_id"],
            "slot_i":     anchor["slot_i"],
            "b_seq":      anchor["b_seq"],
            "b_hash":     anchor["b_hash"],
            "b_block_id": anchor["b_block_id"],
        }
        # Extra: anchor delay in wall time from event first seen to anchor observed.
        rec["T_anchor_blocks_thread_t"] = (anchor["b_seq"] - ev["x_seq"]) if ev.get("x_seq") is not None else None
    if parent_hops is not None:
        rec["parent_hops"] = parent_hops
    return rec


# ── Driver ───────────────────────────────────────────────────────────────────
def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--out", required=True, help="Output JSONL file path.")
    ap.add_argument("--graphql", default="http://localhost/graphql",
                    help="GraphQL endpoint URL (default local nginx0).")
    ap.add_argument("--poll-interval", type=float, default=2.0,
                    help="Seconds between polls (default: 2).")
    ap.add_argument("--max-wait-anchor-s", type=float, default=600.0,
                    help="Seconds to wait for a cross-thread anchor before "
                         "declaring orphan_timeout (default: 600).")
    ap.add_argument("--max-parent-hops", type=int, default=128,
                    help="Cap on same-thread parent-chain walk length "
                         "(default: 128 — well above the ~50 protocol soft bound).")
    ap.add_argument("--scan-window", type=int, default=50,
                    help="How many latest blocks to pull per thread-0 refresh "
                         "(default: 50).")
    ap.add_argument("--dst", default=WITHDRAWAL_EVENT_DST_DEFAULT,
                    help="ExtOut `dst` filter for WithdrawalInitiated. Empty = "
                         "accept every ExtOut from USDCBridge.")
    ap.add_argument("--limit", type=int, default=500,
                    help="Max ExtOut msgs to fetch per poll (default: 500).")
    ap.add_argument("--heartbeat-s", type=float, default=30.0,
                    help="Seconds between heartbeat log lines (default: 30).")
    ap.add_argument("--verbose", action="store_true",
                    help="Log every thread-0 ref indexed.")
    args = ap.parse_args()

    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)

    def log(msg: str, *, force: bool = False) -> None:
        if args.verbose or force:
            print(msg, flush=True)

    def log_always(msg: str) -> None:
        print(msg, flush=True)

    gql = GqlClient(args.graphql)
    run_id = str(uuid.uuid4())
    state = State()

    # Baseline: existing events at startup are skipped.
    try:
        baseline = gql.fetch_bridge_extouts(limit=args.limit)
    except Exception as ex:  # noqa: BLE001
        print(f"[collector] baseline fetch failed: {ex}", file=sys.stderr, flush=True)
        return 2
    state.baseline_events = {n["id"] for n in baseline}
    log_always(f"[collector] run_id={run_id}")
    log_always(f"[collector] baseline events skipped: {len(state.baseline_events)}")
    log_always(f"[collector] graphql={args.graphql}")
    log_always(f"[collector] writing to: {args.out}")

    fh = open(args.out, "a", buffering=1)

    def emit(rec: dict) -> None:
        rec["run_id"] = run_id
        fh.write(json.dumps(rec) + "\n")
        state.emitted_events.add(rec["event_msg_id"])
        state.pending.pop(rec["event_msg_id"], None)
        log_always(f"[emit] msg={rec['event_msg_id'][:10]}... "
                   f"outcome={rec['outcome']} hops={rec.get('hop_count')} "
                   f"cross={rec.get('cross_thread_hops')} "
                   f"same={rec.get('same_thread_hops')} "
                   f"thread={str(rec.get('x_thread'))[:12]}...")

    stopping = False
    def _sigterm(_sig, _frm):
        nonlocal stopping
        stopping = True
    signal.signal(signal.SIGINT, _sigterm)
    signal.signal(signal.SIGTERM, _sigterm)

    last_hb = 0.0
    total_events = 0
    try:
        while not stopping:
            # (1) Keep the thread-0 ref index warm.
            refresh_thread0_index(gql, state, log,
                                  scan_window=args.scan_window)

            # (2) Ingest new WithdrawalInitiated events.
            ingest_new_events(gql, state, dst_filter=args.dst,
                              limit=args.limit,
                              max_wait_anchor_s=args.max_wait_anchor_s,
                              log=log_always)

            # (3) Try to resolve every pending event.
            for msg_id in list(state.pending.keys()):
                ev = state.pending[msg_id]
                closed = close_event(state, gql, msg_id, ev, emit, log,
                                     max_parent_hops=args.max_parent_hops)
                if closed:
                    total_events += 1

            # (4) Heartbeat.
            if time.time() - last_hb >= args.heartbeat_s:
                last_hb = time.time()
                thr_summary = {t[:12]: len(v) for t, v in state.thread0_view.items()}
                log_always(f"[hb] pending={len(state.pending)} "
                           f"emitted={len(state.emitted_events)} "
                           f"thread0_seen_seq={len(state.thread0_seen_seq)} "
                           f"threads_indexed={thr_summary}")

            time.sleep(args.poll_interval)
    finally:
        fh.close()

    log_always(f"[collector] stopped. total events closed = {total_events}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
