#!/usr/bin/env python3
"""
P4 — reachability collector (topology-only).

Polls the acki-nacki GraphQL endpoint for new WithdrawalInitiated events emitted
from USDCBridge, then for each new event walks `proof_block_refs` Direction (a) —
X → thread-0 Y — and appends one JSONL record per event to the output file.

**Topology only.** No SHA-256, no Poseidon, no L7 opening reconstruction. If the
walk terminates at `thread_id == 0`, `outcome=ok`; otherwise one of
`empty_refs | stuck | too_deep | gql_error`.

Usage:
    python research/reachability_collector.py --out research/stats/session.jsonl
    python research/reachability_collector.py --out … --graphql http://localhost:8700/graphql
    python research/reachability_collector.py --out … --max-hops 32 --poll-interval 2.0

The collector snapshots the baseline of pre-existing WithdrawalInitiated messages
on startup and only reports events with `msg_id NOT IN baseline`.
"""
from __future__ import annotations
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

_HERE = os.path.dirname(os.path.abspath(__file__))
_VENDORED = os.path.join(_HERE, "vendored")
# Import from the vendored helper without side effects. We only need the USDCBridge
# account/dapp ids and the ExtOut dst filter.
sys.path.insert(0, _VENDORED)

# The vendored `helper.bridge_e2e` reads `helper.common` at import time and that in
# turn expects `tvm-cli` on PATH etc. We do NOT want that side-effect. Cherry-pick
# the two constants we need from the source lines instead of importing.
_USDC_BRIDGE_LEGACY = "0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
USDC_BRIDGE_ACCOUNT_ID = _USDC_BRIDGE_LEGACY.split(":", 1)[1]
USDC_BRIDGE_DAPP_ID    = "0" * 64  # USDCBridge deployed under DEFAULT_DAPP_ID
# The `dst` field on the emitted external-out message that identifies WithdrawalInitiated.
# Mirrors `helper.bridge_e2e.WITHDRAWAL_EVENT_DST` — cherry-picked so we can run without
# invoking helper.common setup.
WITHDRAWAL_EVENT_DST = ""  # populated at runtime from helper if importable; see below.

try:
    from helper.bridge_e2e import WITHDRAWAL_EVENT_DST as _WED  # noqa: WPS433
    WITHDRAWAL_EVENT_DST = _WED
except Exception:  # noqa: BLE001
    # Helper import failed (missing tvm-cli on PATH etc). Fall back to filtering by
    # `src == USDC_BRIDGE_ADDRESS_LEGACY` alone; caller can override with --dst.
    pass


# ── minimal GQL client (no dependencies) ─────────────────────────────────────
class GqlClient:
    def __init__(self, url: str, user_agent: str = "reachability-collector/1.0",
                 timeout: int = 15):
        self.url = url
        self.user_agent = user_agent
        self.timeout = timeout

    def _post(self, query: str) -> dict:
        req = urllib.request.Request(
            self.url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": self.user_agent},
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
        nodes = []
        for e in edges:
            n = e["node"]
            if not n.get("block_id"):
                tx = n.get("src_transaction") or {}
                n["block_id"] = tx.get("block_id") if isinstance(tx, dict) else None
            nodes.append(n)
        return nodes

    def fetch_block_with_refs(self, block_hash_or_id: str) -> dict | None:
        """Fetch a block by hash OR block_id, projecting the fields we need for the
        ref-walk. Tries `hash:` first, falls back to `block_id:` — the returned
        `proof_block_refs` are always block hashes (matches what the walk chains on)."""
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
            block = (((data.get("data") or {}).get("blockchain") or {}).get("block"))
            if block is not None:
                return block
        return None


# ── walk logic ────────────────────────────────────────────────────────────────
def _pick_next_hop(cur: dict) -> tuple[str, int] | None:
    """Pick the ref to walk to. Slot 0 is the same-thread parent (not useful for
    cross-thread descent). Search slots 1.. and return `(ref_hash, ref_index)`.
    Preference order: any ref whose hash != prev_hash. If nothing qualifies, return None."""
    refs = cur.get("proof_block_refs") or []
    if len(refs) <= 1:
        return None
    prev_hash = cur.get("prev_hash")
    for i in range(1, len(refs)):
        if refs[i] != prev_hash:
            return refs[i], i
    return None


def walk_refs(gql: GqlClient, event_block_hash: str, *, max_hops: int) -> dict:
    """Walk refs from the event's block toward thread 0. Returns dict with
    `outcome`, `hop_count`, `terminal_*`, `hops`."""
    cur = gql.fetch_block_with_refs(event_block_hash)
    if cur is None:
        return {"outcome": "gql_error", "hop_count": 0, "hops": [],
                "terminal_thread_id": None, "terminal_block_id": None,
                "event_block_thread_id": None, "event_block_seq_no": None,
                "event_block_id": None}

    event_thread_id = cur.get("thread_id")
    event_seq_no = int(cur["seq_no"]) if cur.get("seq_no") is not None else None
    event_block_id = cur.get("block_id")

    hops: list[dict] = []
    for i in range(max_hops):
        thread_id = cur.get("thread_id")
        if thread_id == 0 or thread_id == "0":
            return {"outcome": "ok", "hop_count": i, "hops": hops,
                    "terminal_thread_id": 0,
                    "terminal_block_id": cur.get("block_id"),
                    "event_block_thread_id": event_thread_id,
                    "event_block_seq_no": event_seq_no,
                    "event_block_id": event_block_id}
        pick = _pick_next_hop(cur)
        if pick is None:
            return {"outcome": "empty_refs", "hop_count": i, "hops": hops,
                    "terminal_thread_id": thread_id,
                    "terminal_block_id": cur.get("block_id"),
                    "event_block_thread_id": event_thread_id,
                    "event_block_seq_no": event_seq_no,
                    "event_block_id": event_block_id}
        ref_hash, ref_index = pick
        nxt = gql.fetch_block_with_refs(ref_hash)
        if nxt is None:
            return {"outcome": "gql_error", "hop_count": i, "hops": hops,
                    "terminal_thread_id": thread_id,
                    "terminal_block_id": cur.get("block_id"),
                    "event_block_thread_id": event_thread_id,
                    "event_block_seq_no": event_seq_no,
                    "event_block_id": event_block_id}
        hops.append({
            "from_block_id": cur.get("block_id"),
            "from_block_hash": cur.get("hash"),
            "from_thread_id": thread_id,
            "from_seq_no": int(cur["seq_no"]) if cur.get("seq_no") is not None else None,
            "from_refs_len": len(cur.get("proof_block_refs") or []),
            "ref_index_chosen": ref_index,
            "ref_hash_chosen": ref_hash,
            "to_block_id": nxt.get("block_id"),
            "to_block_hash": nxt.get("hash"),
            "to_thread_id": nxt.get("thread_id"),
            "to_seq_no": int(nxt["seq_no"]) if nxt.get("seq_no") is not None else None,
        })
        # Stuck detector: thread_id didn't decrease AND hash didn't change.
        if (nxt.get("thread_id") == thread_id
                and nxt.get("hash") == cur.get("hash")):
            return {"outcome": "stuck", "hop_count": i + 1, "hops": hops,
                    "terminal_thread_id": thread_id,
                    "terminal_block_id": cur.get("block_id"),
                    "event_block_thread_id": event_thread_id,
                    "event_block_seq_no": event_seq_no,
                    "event_block_id": event_block_id}
        cur = nxt

    return {"outcome": "too_deep", "hop_count": max_hops, "hops": hops,
            "terminal_thread_id": cur.get("thread_id"),
            "terminal_block_id": cur.get("block_id"),
            "event_block_thread_id": event_thread_id,
            "event_block_seq_no": event_seq_no,
            "event_block_id": event_block_id}


# ── driver ────────────────────────────────────────────────────────────────────
def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--out", required=True, help="Output JSONL file path.")
    ap.add_argument("--graphql", default="http://localhost/graphql",
                    help="GraphQL endpoint URL. Local devnet default: "
                         "http://localhost/graphql (nginx0). Direct node port: "
                         "http://localhost:8700/graphql.")
    ap.add_argument("--poll-interval", type=float, default=2.0,
                    help="Seconds between GQL polls for new events (default: 2).")
    ap.add_argument("--max-hops", type=int, default=32,
                    help="Cap on Direction-(a) walk depth (default: 32).")
    ap.add_argument("--dst", default=WITHDRAWAL_EVENT_DST,
                    help="ExtOut `dst` field identifying WithdrawalInitiated "
                         "(empty = accept all ExtOut from USDCBridge).")
    ap.add_argument("--limit", type=int, default=500,
                    help="Max ExtOut messages to fetch per poll (default: 500).")
    args = ap.parse_args()

    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)

    gql = GqlClient(args.graphql)
    run_id = str(uuid.uuid4())

    # Baseline: everything already present at startup is ignored.
    baseline = gql.fetch_bridge_extouts(limit=args.limit)
    baseline_ids = {n["id"] for n in baseline}
    print(f"[collector] run_id={run_id}", flush=True)
    print(f"[collector] baseline events skipped: {len(baseline_ids)}", flush=True)
    print(f"[collector] writing to: {args.out}", flush=True)

    seen = set(baseline_ids)
    total = 0
    try:
        with open(args.out, "a", buffering=1) as fh:  # line-buffered
            while True:
                try:
                    nodes = gql.fetch_bridge_extouts(limit=args.limit)
                except (urllib.error.URLError, TimeoutError) as ex:
                    print(f"[collector] GQL fetch error: {ex}", file=sys.stderr, flush=True)
                    time.sleep(args.poll_interval)
                    continue

                new = [n for n in nodes if n["id"] not in seen]
                if args.dst:
                    new = [n for n in new if n.get("dst") == args.dst]
                new.sort(key=lambda n: n.get("created_at") or 0)

                for n in new:
                    seen.add(n["id"])
                    block_hash = n.get("block_id")
                    if not block_hash:
                        record = {
                            "event_msg_id": n["id"],
                            "event_block_hash": None,
                            "event_wall_ts": time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                                          time.gmtime()),
                            "trigger_run_id": run_id,
                            "outcome": "gql_error",
                            "hop_count": 0, "hops": [],
                            "note": "src_transaction.block_id was null",
                        }
                    else:
                        walk = walk_refs(gql, block_hash, max_hops=args.max_hops)
                        record = {
                            "event_msg_id": n["id"],
                            "event_block_hash": block_hash,
                            "event_block_id": walk.pop("event_block_id"),
                            "event_seq_no": walk.pop("event_block_seq_no"),
                            "event_thread_id": walk.pop("event_block_thread_id"),
                            "event_wall_ts": time.strftime("%Y-%m-%dT%H:%M:%SZ",
                                                          time.gmtime()),
                            "trigger_run_id": run_id,
                            **walk,
                        }
                    fh.write(json.dumps(record) + "\n")
                    total += 1
                    print(f"[collector] +1 event  total={total}  "
                          f"outcome={record['outcome']}  hops={record.get('hop_count')}",
                          flush=True)

                time.sleep(args.poll_interval)
    except KeyboardInterrupt:
        print(f"[collector] interrupted after {total} event(s).", file=sys.stderr,
              flush=True)
        return 0


if __name__ == "__main__":
    sys.exit(main())
