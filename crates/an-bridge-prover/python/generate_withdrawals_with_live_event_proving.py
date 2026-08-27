"""
End-to-end driver for the Acki Nacki → Ethereum bridge Circuit 4
(`WithdrawalInitiated`) proving pipeline. Supports local devnet and
shellnet — select with `MODE` env var (`local` (default) | `shellnet`).

Pipeline (single event):
  deploy + fund multisig → fire `initiateWithdrawal` → capture event
  via GQL → wait for verifier daemon to reach the W-aligned key block
  → run the three Rust bins (private-witness-export, witness-builder,
  halo2-prover) → assert daemon verdict (`verified && anchor_matched`).

Scope:
  AN side only. This driver covers the L1 (single-chain) Circuit 4 event
  proving path against the on-AN `USDCBridge`. It does NOT drive the
  Ethereum side (no `withdrawByProof` submission to the L1 EVM bridge)
  and it does NOT exercise the L2 (multi-chain / thinned) path.
  Sepolia submission is covered by `bridge-withdraw-e2e-cli`.
  TODO: extend / add a parallel driver for the L2 path (currently only
  the L1 topology is embedded here).

Assumed running services (this script does NOT start them):
  - `bridge-prover-daemon`   — produces bundle proofs.
  - `bridge-verifier-daemon` — reads Circuit 4 proofs from
    `$PROVER_DIR/proofs/` and writes verdicts back as `*.result.json`.
  Both must be healthy and ingesting the target cluster before this
  script runs; a stuck daemon manifests as a verifier-state or
  daemon-result timeout below.

Env vars (all optional, defaults are MODE-dependent):
  MODE                  local | shellnet  (default: local)
  PROVER_DIR            default: parent of this script
  NETWORK, GRAPHQL_URL, WORK_DIR
  USDC_BRIDGE_KEY_PATH  override path to the keypair file used to sign
                        `USDCBridge.mintAndSend`. When unset and MODE=local,
                        the bundled `python/contracts/USDCBridge.keys.json`
                        is auto-overwritten from the sibling
                        `acki-nacki/config/USDCBridge.keys.json` (escape
                        hatch for out-of-tree deployments). Either way the
                        orchestrator validates the chosen key against the
                        on-chain `USDCBridge.getOwnerPubkey()` before
                        proceeding, so a stale/wrong key fails fast instead
                        of bouncing later with TVM exit_code 209.
  ACKI_NACKI_ROOT       override sibling acki-nacki checkout (used by both
                        bk_set and USDCBridge key materialization in local
                        mode). Default: `<PROVER_DIR>/../acki-nacki`.

Prereqs (the script does not start these):
  - Live cluster reachable at GRAPHQL_URL.
  - `bridge-prover-daemon` and `bridge-verifier-daemon` both running
    in $PROVER_DIR (see "Assumed running services" above).
  - $PROVER_DIR/params/ has primary + layer + event VK/PK.
  - Release builds of bridge-event-private-witness-export,
    bridge-event-witness-builder, bridge-event-halo2-prover.

Exit 0 iff daemon accepts the Circuit 4 proof; artefacts kept under
$WORK_DIR on failure.
"""

import json
import os
import re
import subprocess
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)
os.environ["PATH"] = os.path.join(_HERE, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common
from helper import bridge_e2e as be
from helper.bridge_e2e import (
    USDC_BRIDGE_ADDRESS, USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    WITHDRAWAL_AMOUNT, USDC_TOKEN_ID,
    DST_CHAIN_ID, RECIPIENT_HEX,
    W, P, MAX_LAYERS,
    materialize_usdc_bridge_key_from_node_config,
    validate_usdc_bridge_key,
)
from helper.msig import deploy_multisig

# ── Mode-dependent configuration ─────────────────────────────────────────────
MODE = os.environ.get("MODE", "local").lower()
if MODE not in ("local", "shellnet"):
    raise SystemExit(f"MODE must be 'local' or 'shellnet', got {MODE!r}")
IS_SHELLNET = MODE == "shellnet"

if IS_SHELLNET:
    DEFAULT_NETWORK = "shellnet.ackinacki.org"
    DEFAULT_GRAPHQL = "https://shellnet.ackinacki.org/graphql"
    DEFAULT_WORK    = "work-shellnet"
    _DEFAULT_USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEYS_SHELLNET
    GQL_KWARGS = {"user_agent": "bridge-e2e-orchestrator-shellnet/1.0",
                  "timeout": 30}
    EVENT_INDEXER_TIMEOUT_S    = 240
    VERIFIER_STATE_TIMEOUT_S   = 2400
else:
    DEFAULT_NETWORK = "http://127.0.0.1:80"
    DEFAULT_GRAPHQL = "http://localhost/graphql"
    DEFAULT_WORK    = "work-local"
    _DEFAULT_USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEYS
    GQL_KWARGS = {}
    EVENT_INDEXER_TIMEOUT_S    = 180
    VERIFIER_STATE_TIMEOUT_S   = 1800

# An explicit override pins the key path (no auto-materialize). Otherwise we
# fall back to the MODE-picked bundled file and — in local mode — refresh it
# from the sibling acki-nacki checkout before use.
USDC_BRIDGE_KEY_PATH_OVERRIDE = os.environ.get("USDC_BRIDGE_KEY_PATH")
USDC_BRIDGE_KEY_PATH = USDC_BRIDGE_KEY_PATH_OVERRIDE or _DEFAULT_USDC_BRIDGE_KEY_PATH

DAEMON_RESULT_TIMEOUT_S = 600
RUST_BIN_TIMEOUT_S      = 600

PROVER_DIR  = os.environ.get("PROVER_DIR", os.path.dirname(_HERE))
WORK_DIR    = os.environ.get("WORK_DIR", os.path.join(PROVER_DIR, DEFAULT_WORK))
GRAPHQL_URL = os.environ.get("GRAPHQL_URL", DEFAULT_GRAPHQL)

MSIG_KEY_PATH = os.path.join(WORK_DIR, "msig_withdrawals_e2e.keys.json")

tracer: be.Tracer
gql: be.GqlClient


# ── Local-devnet bk_set materialization ───────────────────────────────────────

def materialize_bk_set_from_node_config():
    acki_nacki_root = os.environ.get(
        "ACKI_NACKI_ROOT",
        os.path.abspath(os.path.join(PROVER_DIR, "..", "acki-nacki")),
    )
    config_dir = os.path.join(acki_nacki_root, "config")
    try:
        names = subprocess.run(
            ["docker", "ps", "--format", "{{.Names}}"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        raise RuntimeError(
            f"failed to enumerate docker containers: {e}. "
            "the local devnet cluster must be running before this test"
        ) from e

    pat = re.compile(r"-node(\d+)-\d+$")
    indices = sorted({
        int(m.group(1))
        for line in names.splitlines()
        for m in [pat.search(line.strip())]
        if m
    })
    if not indices:
        raise RuntimeError(
            "no `*-nodeN-*` containers detected via `docker ps`; "
            "start the cluster (e.g. `make run`) before this test"
        )

    bk_set = {}
    for idx in indices:
        path = os.path.join(config_dir, f"block_keeper{idx}_bls.keys.json")
        if not os.path.isfile(path):
            raise FileNotFoundError(
                f"BLS key file missing: {path} (needed for node{idx})"
            )
        with open(path) as f:
            data = json.load(f)
        try:
            pub = data[0]["public"]
        except (IndexError, KeyError, TypeError) as e:
            raise ValueError(f"unexpected format in {path}: {e}") from e
        bk_set[str(idx)] = pub

    out_path = os.path.join(PROVER_DIR, "bk_set.local.json")
    with open(out_path, "w") as f:
        json.dump(bk_set, f, indent=2)
    tracer.log(f"  bk_set.local.json: {len(bk_set)} signers (indices {indices}) "
               f"sourced from {config_dir}")
    tracer.log(f"  wrote {out_path}")


# ── Driver ────────────────────────────────────────────────────────────────────

def main():
    global tracer, gql
    tracer = be.Tracer()
    gql = be.GqlClient(GRAPHQL_URL, **GQL_KWARGS)

    os.makedirs(WORK_DIR, exist_ok=True)

    network = os.getenv("NETWORK", DEFAULT_NETWORK)
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    tracer.log_phase(f"Prechecks ({MODE})")
    assert os.path.isdir(PROVER_DIR), f"PROVER_DIR not found: {PROVER_DIR}"
    tracer.log(f"  NETWORK:     {network}")
    tracer.log(f"  PROVER_DIR:  {PROVER_DIR}")
    tracer.log(f"  GRAPHQL_URL: {GRAPHQL_URL}")
    tracer.log(f"  WORK_DIR:    {WORK_DIR}")
    tracer.log(f"  W = {W}, P = {P} (bundle = {W*P} blocks), MAX_LAYERS = {MAX_LAYERS}")
    assert common.is_account_active(USDC_BRIDGE_ADDRESS), \
        f"USDCBridge not active at {USDC_BRIDGE_ADDRESS}"
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    if not IS_SHELLNET:
        tracer.log_phase("Materializing bk_set.local.json from local cluster config")
        materialize_bk_set_from_node_config()
        if USDC_BRIDGE_KEY_PATH_OVERRIDE is None:
            tracer.log_phase("Materializing USDCBridge.keys.json from local cluster config")
            materialize_usdc_bridge_key_from_node_config(tracer, PROVER_DIR, USDC_BRIDGE_KEY_PATH)

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    tracer.log(f"  USDC_BRIDGE_KEY_PATH: {USDC_BRIDGE_KEY_PATH}"
               + (" (user-overridden)" if USDC_BRIDGE_KEY_PATH_OVERRIDE else ""))
    validate_usdc_bridge_key(tracer, USDC_BRIDGE_KEY_PATH,
                             overridden=USDC_BRIDGE_KEY_PATH_OVERRIDE is not None)

    baseline = gql.fetch_bridge_extouts(limit=500)
    baseline_ids = {n["id"] for n in baseline}
    tracer.log(f"  baseline ExtOut messages from USDCBridge: {len(baseline_ids)}")

    msig_address, msig_abi = deploy_multisig(
        tracer, gql,
        work_dir=WORK_DIR, msig_key_path=MSIG_KEY_PATH,
        usdc_bridge_key_path=USDC_BRIDGE_KEY_PATH,
        is_shellnet=IS_SHELLNET,
    )

    # No fire-window gating: the witness builder now emits a horizontal
    # forward chain of L1 openings from `H_e = ⌈event_seq/W⌉·W` to
    # `K = ⌈event_seq/(W·P)⌉·(W·P)` (hops ∈ {0, …, P-1}). Fire the event
    # whenever — we only need the verifier to eventually reach `K`.
    tracer.log_phase("Dispatching initiateWithdrawal")
    tracer.log(f"  dstChainId={DST_CHAIN_ID}, recipient=0x{RECIPIENT_HEX}, "
               f"amount={WITHDRAWAL_AMOUNT}, tokenId={USDC_TOKEN_ID}")
    call_result = be.call_initiate_withdrawal(
        msig_address, msig_abi, MSIG_KEY_PATH, DST_CHAIN_ID, RECIPIENT_HEX
    )
    if not common.is_ok(call_result):
        tracer.log(f"  call result (may still emit asynchronously): {call_result}")

    meta = be.capture_event_metadata(tracer, gql, baseline_ids, EVENT_INDEXER_TIMEOUT_S)
    tracer.log("  captured metadata:")
    for k, v in meta.items():
        tracer.log(f"    {k}: {v}")

    event_seq = meta["block_seq_no"]
    key_block_seq, thinned_kb_seq, target_seq = be.compute_target_seq(event_seq)
    hops = (thinned_kb_seq - key_block_seq) // W
    tracer.log_phase("Boundary math")
    tracer.log(f"  event_seq        = {event_seq}")
    tracer.log(f"  key_block_seq    = {key_block_seq}  (H_e: W-aligned L1 tree the event lives in)")
    tracer.log(f"  thinned_kb_seq   = {thinned_kb_seq}  (K: verifier-stored L1 root anchor)")
    tracer.log(f"  target_seq       = {target_seq}    (verifier must reach this seq_no)")
    tracer.log(f"  hops             = {hops}          (active forward-hop chain links)")

    be.wait_for_verifier_state(tracer, gql, PROVER_DIR, target_seq, VERIFIER_STATE_TIMEOUT_S)

    pipeline = be.run_event_proving_steps(
        tracer, PROVER_DIR, WORK_DIR, GRAPHQL_URL, meta,
        rust_bin_timeout_s=RUST_BIN_TIMEOUT_S,
        daemon_result_timeout_s=DAEMON_RESULT_TIMEOUT_S,
        seq_no=0,
    )
    result = pipeline["result"]
    seq_no = pipeline["seq_no"]
    work_event_dir = pipeline["work_event_dir"]
    proofs_dir = pipeline["proofs_dir"]

    tracer.log_phase("Daemon verdict")
    tracer.log(json.dumps(result, indent=2))

    verified       = result.get("verified") is True
    anchor_matched = result.get("anchor_matched") is True
    if not anchor_matched:
        tracer.log(f"  ANCHOR MISMATCH: {result.get('error')}")
    if not verified:
        tracer.log(f"  VERIFICATION FAILED: {result.get('error')}")

    if verified and anchor_matched:
        tracer.log_phase("END-TO-END SUCCESS")
        tracer.log(f"  daemon verified Circuit 4 proof for event in block "
                   f"seq_no={event_seq} at verifier height "
                   f"{result.get('verified_at_block_height')}")
        tracer.log(f"  artefacts: {work_event_dir} + {proofs_dir}/proof_event_{seq_no:06d}.json")
        sys.exit(0)
    else:
        tracer.log_phase("END-TO-END FAILURE")
        tracer.log(f"  artefacts kept for diagnosis under {work_event_dir}")
        sys.exit(1)

if __name__ == "__main__":
    main()
