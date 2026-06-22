"""
Shared scaffolding for the Acki Nacki → Ethereum bridge Circuit 4 (event)
proving E2E orchestrators.

Two variants live alongside each other under `python/`:
  - generate_withdrawals_with_live_event_proving.py          (local devnet)
  - generate_withdrawals_with_live_event_proving_shellnet.py (shellnet)

The lanes diverge only in:
  - GraphQL endpoint defaults + User-Agent + per-request timeouts
  - Multisig funding strategy (mintAndSend caller / keys / pre-deploy faucet
    sequence)
  - Stage timeouts (shellnet is slower)

Everything else — addresses, ABI paths, withdrawal params, history-window
constants, GQL query shapes, the four pipeline stages (capture metadata,
wait for verifier state, run three Rust binaries, wait for daemon verdict),
the boundary math, the fire-window loop — is identical, so it lives here.

Layout:
  • Constants section  ── addresses, ABIs, withdrawal params, W/P/MAX_LAYERS
  • `Tracer`           ── monotonic-elapsed log_phase/log helpers
  • `GqlClient`        ── URL+UA+timeout-parameterised GraphQL caller plus
                          the four queries the orchestrators use
  • Pipeline helpers   ── encode_initiate_withdrawal_body,
                          capture_event_metadata, wait_for_verifier_state,
                          run_rust_bin, wait_for_daemon_result,
                          wait_for_fire_window, call_initiate_withdrawal,
                          run_event_proving_steps
"""

import json
import os
import subprocess
import sys
import time
import urllib.request

from helper import common

# `python/` directory (this file lives at python/helper/bridge_e2e.py).
_PY_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# ── Addresses ─────────────────────────────────────────────────────────────────
# tvm-cli v3 requires `<dapp_id>::<account_id>` for CLI args / `account` queries.
# ABI payload `address` fields (e.g. `dest`) still take legacy `0:<acc_id>`.
USDC_BRIDGE_ADDRESS_LEGACY = "0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a"
GIVER_ADDRESS_LEGACY       = "0:1111111111111111111111111111111111111111111111111111111111111111"

USDC_BRIDGE_ADDRESS = common.to_dapp_address(USDC_BRIDGE_ADDRESS_LEGACY)
GIVER_ADDRESS       = common.to_dapp_address(GIVER_ADDRESS_LEGACY)

USDC_BRIDGE_DAPP_ID, USDC_BRIDGE_ACCOUNT_ID = USDC_BRIDGE_ADDRESS.split("::", 1)

# ── ABIs / TVCs (bundled under python/contracts/) ─────────────────────────────
CONTRACTS_DIR    = os.path.join(_PY_DIR, "contracts")
USDC_BRIDGE_ABI  = os.path.join(CONTRACTS_DIR, "USDCBridge.abi.json")
USDC_BRIDGE_KEYS = os.path.join(CONTRACTS_DIR, "USDCBridge.keys.json")
GIVER_ABI        = os.path.join(CONTRACTS_DIR, "GiverV3.abi.json")
GIVER_KEY_PATH   = os.path.join(CONTRACTS_DIR, "GiverV3.keys.json")
MSIG_ABI         = os.path.join(CONTRACTS_DIR, "UpdateCustodianMultisigWallet.abi.json")
MSIG_TVC_STEM    = os.path.join(CONTRACTS_DIR, "UpdateCustodianMultisigWallet")

# Shellnet bridge-owner key (only loaded by the shellnet variant; placed here
# alongside the other bundled key paths for parity).
USDC_BRIDGE_KEYS_SHELLNET = os.path.join(CONTRACTS_DIR, "USDCBridge.shellnet.keys.json")

# ── Withdrawal parameters ─────────────────────────────────────────────────────
WITHDRAWAL_AMOUNT  = 1_000_000
# Two ECC currencies are in play:
#   - ECC[2] Shell  → giver→multisig fund tx; bootstraps vmshell (gas).
#   - ECC[3] USDC   → the only token USDCBridge.initiateWithdrawal accepts.
ECC_ID_FOR_BURN    = 2   # Shell — gas bootstrap (giver has it at genesis)
USDC_TOKEN_ID      = 3   # ECC[3] — token attached to initiateWithdrawal
DST_CHAIN_ID       = 1
RECIPIENT_HEX      = "742d35cc6634c0532925a3b844bc454e4438f44e"

# External-address dst for the WithdrawalInitiated event:
#   makeAddrExtern(WithdrawalInitiatedEmit=618=0x26a, bitCntAddress=256)
WITHDRAWAL_EVENT_DST = ":000000000000000000000000000000000000000000000000000000000000026a"

# ── History-window math constants ─────────────────────────────────────────────
# W = HISTORY_PROOF_WINDOW_SIZE. Production: 128.
# P = THINNING_FACTOR_P. Prover proves every P-th key block; bundle width = W*P.
# Keep both in sync with `history_proof::HISTORY_PROOF_WINDOW_SIZE`
# (acki-nacki/node/libs/history-proof/src/lib.rs) and
# `bridge-prover-lib::THINNING_FACTOR_P`.
W           = 128
P           = 4
MAX_LAYERS  = 10


# ── Tracing ───────────────────────────────────────────────────────────────────

class Tracer:
    """Monotonic-elapsed log prefix carrier. Each orchestrator instantiates one
    at top of `main()` so the T+ counter zeroes at the start of the run."""

    def __init__(self):
        self._t0 = time.time()

    def t_prefix(self) -> str:
        elapsed = int(time.time() - self._t0)
        m, s = divmod(elapsed, 60)
        return f"[T+{m:02d}:{s:02d}]"

    def log_phase(self, msg: str):
        print(f"\n{self.t_prefix()} === {msg} ===", flush=True)

    def log(self, msg: str):
        print(f"{self.t_prefix()} {msg}", flush=True)


# ── GraphQL client ────────────────────────────────────────────────────────────

class GqlClient:
    """Configured GraphQL caller. Shellnet's reverse proxy 403s the default
    Python User-Agent — pass a custom UA. Bump `timeout` for the public
    endpoint (over the wire) vs local devnet."""

    def __init__(self, url: str, user_agent: str = "bridge-e2e-orchestrator/1.0",
                 timeout: int = 15):
        self.url = url
        self.user_agent = user_agent
        self.timeout = timeout

    def _gql(self, query: str):
        req = urllib.request.Request(
            self.url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json", "User-Agent": self.user_agent},
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            return json.loads(resp.read().decode())

    def fetch_bridge_extouts(self, limit: int = 500):
        """Fetch recent ExtOut messages from USDCBridge with all fields needed
        for downstream Circuit 4 witness construction in one round-trip.

        v3 GQL schema: `blockchain.account(account_id:, dapp_id:)` (the old
        `account(address:)` form was removed)."""
        q = f'''{{
          blockchain {{
            account(account_id: "{USDC_BRIDGE_ACCOUNT_ID}", dapp_id: "{USDC_BRIDGE_DAPP_ID}") {{
              messages(msg_type: [ExtOut], last: {limit}) {{
                edges {{ node {{
                  id boc body src dst created_at
                  block_id src_dapp_id
                  src_transaction {{ id block_id }}
                }} }}
              }}
            }}
          }}
        }}'''
        data = self._gql(q)
        edges = (data.get("data") or {}).get("blockchain", {}).get("account", {}) \
            .get("messages", {}).get("edges", []) or []
        nodes = []
        for e in edges:
            n = e["node"]
            # Message.block_id is null on ExtOut in this schema; fall back to
            # the owning transaction's block_id, which is populated.
            if not n.get("block_id"):
                tx = n.get("src_transaction") or {}
                tx_block_id = tx.get("block_id") if isinstance(tx, dict) else None
                if tx_block_id:
                    n["block_id"] = tx_block_id
            nodes.append(n)
        return nodes

    def fetch_account_dapp_id(self, account_id: str, dapp_id: str) -> str:
        """v3 GQL takes split account_id / dapp_id; returns the on-chain
        dapp_id (often the same as the input for zerostate-deployed
        contracts, but surfaced for parity with the exporter's
        expectations)."""
        q = f'''{{ blockchain {{
            account(account_id: "{account_id}", dapp_id: "{dapp_id}") {{
                info {{ dapp_id }}
            }}
        }} }}'''
        data = self._gql(q)
        info = (data.get("data") or {}).get("blockchain", {}).get("account", {}).get("info") or {}
        return info.get("dapp_id") or ""

    def find_block_by_hash(self, block_hash: str):
        """Resolve a block via direct `block(hash:)` lookup.

        Note: what `Message.src_transaction.block_id` actually returns is
        the block's `hash` field (BOC hash), NOT its consensus `block_id`.
        The block's real `block_id` and `envelope_hash` come from this
        lookup."""
        q = f'''{{
          blockchain {{
            block(hash: "{block_hash}") {{
              hash block_id seq_no height envelope_hash key_block
            }}
          }}
        }}'''
        data = self._gql(q)
        return (data.get("data") or {}).get("blockchain", {}).get("block")

    def fetch_latest_block_seq_no(self) -> int:
        q = '{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }'
        try:
            data = self._gql(q)
            edges = data["data"]["blockchain"]["blocks"]["edges"]
            return int(edges[-1]["node"]["seq_no"])
        except Exception:
            return -1


# ── tvm-cli body encoding ─────────────────────────────────────────────────────

def encode_initiate_withdrawal_body(dst_chain_id: int, recipient_hex: str) -> str:
    params = json.dumps({
        "dstChainId": str(dst_chain_id),
        "recipient":  recipient_hex,
    })
    cmd = (f"{common.TVM_CLI} -j body --abi {USDC_BRIDGE_ABI} "
           f"initiateWithdrawal '{params}'")
    out = subprocess.check_output(cmd, shell=True, stderr=subprocess.STDOUT).decode().strip()
    return json.loads(out)["Message"]


def call_initiate_withdrawal(msig_address: str, msig_abi: str, msig_key_path: str,
                             dst_chain_id: int, recipient_hex: str):
    payload = encode_initiate_withdrawal_body(dst_chain_id, recipient_hex)
    params = {
        "dest":    USDC_BRIDGE_ADDRESS_LEGACY,   # ABI `address` field — legacy form
        "value":   "1000000000",
        "cc":      {str(USDC_TOKEN_ID): str(WITHDRAWAL_AMOUNT)},   # ECC[3] USDC required by USDCBridge
        "bounce":  False,
        "flags":   1,
        "payload": payload,
    }
    return common.call_contract(
        msig_address, msig_abi, msig_key_path,
        "sendTransaction", params, True,
    )


# ── Pipeline stages ───────────────────────────────────────────────────────────

def capture_event_metadata(tracer: Tracer, gql: GqlClient,
                           baseline_msg_ids: set,
                           event_indexer_timeout_s: int) -> dict:
    """Wait for a newly-emitted WithdrawalInitiated event to surface, then
    resolve its block via GQL. Returns the merged metadata dict consumed by
    `bridge-event-private-witness-export`."""
    tracer.log_phase("Waiting for emitted WithdrawalInitiated event")
    deadline = time.time() + event_indexer_timeout_s
    target = None
    last_log = -1
    while time.time() < deadline:
        nodes = gql.fetch_bridge_extouts(limit=500)
        matched = [
            n for n in nodes
            if n.get("dst") == WITHDRAWAL_EVENT_DST
            and n.get("id") not in baseline_msg_ids
        ]
        if len(matched) != last_log:
            tracer.log(f"  matched={len(matched)} new event(s)")
            last_log = len(matched)
        if matched:
            matched.sort(key=lambda n: n.get("created_at") or 0)
            target = matched[-1]
            break
        time.sleep(2)

    if target is None:
        raise RuntimeError("did not observe a WithdrawalInitiated event within "
                           f"{event_indexer_timeout_s}s")

    tracer.log(f"  event message id:    {target['id']}")
    tracer.log(f"  src_transaction.block_id (== Block.hash): {target['block_id']}")
    tracer.log(f"  src_dapp_id:         {target.get('src_dapp_id')}")

    # `target['block_id']` is actually the block's `hash` field — use it for
    # the direct `block(hash:)` lookup to obtain the real consensus block_id
    # and envelope_hash.
    block = gql.find_block_by_hash(target["block_id"])
    if block is None:
        raise RuntimeError(f"block(hash:{target['block_id']}) returned null")
    tracer.log(f"  block seq_no={block['seq_no']} height={block['height']} "
               f"key_block={block['key_block']}")

    # Pull USDCBridge's own dapp_id. Workchain root has empty dapp_id —
    # pad to 32 zero bytes which is what the exporter's hex parser accepts.
    account_dapp_id_hex = gql.fetch_account_dapp_id(USDC_BRIDGE_ACCOUNT_ID, USDC_BRIDGE_DAPP_ID)
    if not account_dapp_id_hex:
        account_dapp_id_hex = "0" * 64
    tracer.log(f"  account_dapp_id:  {account_dapp_id_hex}")

    account_id_hex = USDC_BRIDGE_ACCOUNT_ID
    assert len(account_id_hex) == 64

    return {
        "event_boc_b64":   target["boc"],
        "block_id":        block["block_id"],   # consensus block_id from block lookup
        "block_hash":      block["hash"],
        "block_seq_no":    int(block["seq_no"]),
        "block_height":    int(block["height"]),
        "envelope_hash":   block["envelope_hash"],
        "account_dapp_id": account_dapp_id_hex,
        "account_id":      account_id_hex,
        "message_id":      target["id"],
    }


def wait_for_verifier_state(tracer: Tracer, gql: GqlClient, prover_dir: str,
                            min_seq_no: int, timeout_s: int) -> int:
    """Block until the verifier daemon has ingested at least up to
    `min_seq_no` (i.e. its state file reports
    `stored_last_seen_block_seq_no >= min_seq_no`)."""
    state_path = os.path.join(prover_dir, "state", "verifier_state.json")
    tracer.log_phase(f"Waiting for verifier state to reach seq_no >= {min_seq_no}")
    tracer.log(f"  state file: {state_path}")
    deadline = time.time() + timeout_s
    last_log_seq = -1
    while time.time() < deadline:
        try:
            with open(state_path) as f:
                state = json.load(f)
            current = int(state.get("stored_last_seen_block_seq_no", 0))
            if current != last_log_seq:
                latest = gql.fetch_latest_block_seq_no()
                tracer.log(f"  verifier at seq_no={current}, chain at seq_no={latest}")
                last_log_seq = current
            if current >= min_seq_no:
                tracer.log(f"  verifier reached seq_no={current} (>= {min_seq_no})")
                return current
        except FileNotFoundError:
            tracer.log("  state file not present yet — daemon may still be starting")
        except json.JSONDecodeError:
            # Mid-write race — try again next tick.
            pass
        time.sleep(5)
    raise TimeoutError(
        f"verifier state did not reach seq_no {min_seq_no} within "
        f"{timeout_s}s — is bridge-verifier-daemon running?"
    )


def run_rust_bin(tracer: Tracer, prover_dir: str, name: str, args: list,
                 timeout_s: int, parse_last_json: bool = True):
    """Invoke a release Rust binary, log its progress, return either the
    parsed last-non-empty-line JSON summary (default) or the raw stdout."""
    bin_path = os.path.join(prover_dir, "target", "release", name)
    if not os.path.isfile(bin_path):
        raise FileNotFoundError(
            f"{bin_path} not found — did you `cargo build --release -p {name}` "
            f"in {prover_dir}?"
        )
    cmd = [bin_path] + args
    tracer.log(f"  $ {' '.join(cmd)}")
    proc = subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout_s,
        cwd=prover_dir,
    )
    if proc.returncode != 0:
        # Stderr carries tracing output; stdout would normally have the JSON.
        print(proc.stderr, file=sys.stderr)
        raise RuntimeError(f"{name} exited with code {proc.returncode}")

    if not parse_last_json:
        return proc.stdout

    last_non_empty = next(
        (ln for ln in reversed(proc.stdout.splitlines()) if ln.strip()),
        None,
    )
    if not last_non_empty:
        raise RuntimeError(f"{name} produced no stdout")
    try:
        return json.loads(last_non_empty)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"{name} last stdout line is not valid JSON: {last_non_empty[:200]!r} "
            f"({e})"
        )


def wait_for_daemon_result(tracer: Tracer, prover_dir: str, seq_no: int,
                           timeout_s: int) -> dict:
    """Wait for `proofs/proof_event_NNNNNN.result.json` to appear in
    `prover_dir` and parse it."""
    path = os.path.join(prover_dir, "proofs", f"proof_event_{seq_no:06d}.result.json")
    tracer.log_phase("Waiting for verifier daemon's result file")
    tracer.log(f"  path: {path}")
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if os.path.exists(path):
            try:
                with open(path) as f:
                    return json.load(f)
            except json.JSONDecodeError:
                # Mid-write race; the daemon writes the file in one go but
                # be defensive — retry on next tick.
                pass
        time.sleep(0.5)
    raise TimeoutError(
        f"daemon result file did not appear within {timeout_s}s: {path}"
    )


def wait_for_fire_window(tracer: Tracer, gql: GqlClient, timeout_s: int):
    """With thinning factor P, the verifier only stores L1 roots at
    (W*P)-aligned key blocks. The witness builder requires the event to land
    in the LAST W-window before the next thinned key block (i.e.
    `event_seq ∈ [K-W, K-1]` for some `K ≡ 0 mod (W*P)`).

    Block until `latest_seq` is inside that firing window. The transaction's
    ExtOut typically lands within 1-3 blocks of dispatch, so we aim for
    `latest_seq` exactly at the start of the last W-window."""
    tracer.log_phase("Waiting for fire window")
    wp = W * P
    fire_deadline = time.time() + timeout_s
    last_logged = -1
    while time.time() < fire_deadline:
        latest = gql.fetch_latest_block_seq_no()
        if latest < 0:
            time.sleep(2)
            continue
        next_thinned = ((latest // wp) + 1) * wp     # next thinned KB strictly after latest
        fire_start = next_thinned - W                # start of its last W-window
        if latest != last_logged:
            tracer.log(f"  latest_seq={latest}, next_thinned_kb={next_thinned}, "
                       f"fire_start={fire_start}")
            last_logged = latest
        if latest >= fire_start:
            tracer.log(f"  ENTERED fire window: latest={latest} ∈ [{fire_start}..{next_thinned})")
            return
        time.sleep(2)
    raise TimeoutError(
        f"chain never entered fire window within {timeout_s}s"
    )


def run_event_proving_steps(tracer: Tracer, prover_dir: str, work_dir: str,
                            gql_endpoint: str, meta: dict,
                            rust_bin_timeout_s: int,
                            daemon_result_timeout_s: int,
                            seq_no: int = 0) -> dict:
    """Steps 5–9 of the pipeline: export partial witness → build full witness
    → produce Circuit 4 proof → wait for daemon verdict. Returns the parsed
    daemon result JSON.

    Identical between local & shellnet drivers (only the timeouts and
    `gql_endpoint` differ)."""
    work_event_dir = os.path.join(work_dir, f"event_{seq_no:06d}")
    os.makedirs(work_event_dir, exist_ok=True)
    partial_path = os.path.join(work_event_dir, "partial.json")
    witness_path = os.path.join(work_event_dir, "witness.json")
    proofs_dir   = os.path.join(prover_dir, "proofs")
    tracer.log(f"  work dir:    {work_event_dir}")
    tracer.log(f"  proofs dir:  {proofs_dir}")

    tracer.log_phase("Step 5: bridge-event-private-witness-export")
    run_rust_bin(
        tracer, prover_dir,
        "bridge-event-private-witness-export",
        [
            "--event-boc-b64",   meta["event_boc_b64"],
            "--block-id",        meta["block_id"],
            "--block-seq-no",    str(meta["block_seq_no"]),
            "--account-dapp-id", meta["account_dapp_id"],
            "--account-id",      meta["account_id"],
            "--envelope-hash",   meta["envelope_hash"],
            "--out",             partial_path,
        ],
        timeout_s=rust_bin_timeout_s,
        parse_last_json=False,  # this binary writes only a file + tracing
    )
    assert os.path.isfile(partial_path), f"exporter did not produce {partial_path}"
    tracer.log(f"  wrote {partial_path}")

    tracer.log_phase("Step 6: bridge-event-witness-builder")
    wb_summary = run_rust_bin(
        tracer, prover_dir,
        "bridge-event-witness-builder",
        [
            "--partial-witness", partial_path,
            "--out",             witness_path,
            "--state",           os.path.join(prover_dir, "state/verifier_state.json"),
            "--gql-endpoint",    gql_endpoint,
            "--layer-idx",       "0",
        ],
        timeout_s=rust_bin_timeout_s,
    )
    tracer.log(f"  summary: {json.dumps(wb_summary, indent=2)}")
    assert os.path.isfile(witness_path), f"witness-builder did not produce {witness_path}"

    tracer.log_phase(f"Step 7: bridge-event-halo2-prover (seq_no={seq_no})")
    ep_summary = run_rust_bin(
        tracer, prover_dir,
        "bridge-event-halo2-prover",
        [
            "--fixture",  witness_path,
            "--out-dir",  proofs_dir,
            "--seq-no",   str(seq_no),
        ],
        timeout_s=rust_bin_timeout_s,
    )
    tracer.log(f"  summary: schema={ep_summary.get('schema_version')}, "
               f"self_verified={ep_summary.get('self_verified')}, "
               f"proof_file={ep_summary.get('proof_file')}")
    if not ep_summary.get("self_verified"):
        tracer.log("  WARNING: prover-side self-verify FAILED — daemon will reject too")

    result = wait_for_daemon_result(tracer, prover_dir, seq_no, daemon_result_timeout_s)
    return {
        "work_event_dir": work_event_dir,
        "proofs_dir":     proofs_dir,
        "seq_no":         seq_no,
        "result":         result,
    }


def compute_target_seq(event_seq: int) -> tuple[int, int, int]:
    """Returns `(key_block_seq, thinned_kb_seq, target_seq)` derived from the
    event's block seq_no.

    The L1 key block at seq_no `H` summarises blocks `[H-W, H-1]`. With
    thinning, the verifier persists L1 roots only at (W*P)-aligned key blocks
    — `target_seq = thinned_kb_seq` is what the verifier daemon must reach
    before Circuit 4 can prove."""
    key_block_seq  = ((event_seq // W) + 1) * W
    thinned_kb_seq = ((event_seq // (W * P)) + 1) * (W * P)
    return key_block_seq, thinned_kb_seq, thinned_kb_seq
