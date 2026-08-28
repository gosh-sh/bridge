# Live CLI Withdraw E2E Runbook — shellnet → Sepolia (Circuit 4)

Operational guide for driving a full AN→ETH withdrawal E2E through the
`bridge-withdraw-e2e-cli` binary against a deployed `AckiNackiBridge` on
Sepolia. This is the operator-facing counterpart to the daemon: the
daemon owns the continuous bundle-proving stream; this CLI owns the
per-withdrawal composition (burn → capture → Circuit-4 SHPLONK proof →
`withdrawByProof`).

**Scope of this runbook.** The end-user withdrawal path, driven by the
new CLI. Every stage of the pipeline is in-process — `--from` composes
the AN multisig `sendTransaction`, the tool broadcasts it, waits for the
matching `WithdrawalInitiated` ExtOut event, **resurrects the prover's
`BridgeState` mirror by reading the deployed `AckiNackiBridge` contract
at `--bridge-address`** (no local `prover_state.json` needed), polls
that contract until the covering L1/L2 bundle has landed, produces the
Circuit-4 SHPLONK proof, calls `dry_run_withdraw`, and (unless
`--dry-run`) submits `withdrawByProof`. **Assumes** the bundle lane
(Circuits 1A + 2 via `daemon-live`) is running _somewhere_ — not
necessarily on the same host as the CLI — feeding `verifyBlock`
transactions to the bridge. That lane is covered in
[`live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md).

**Parallel with the pre-CLI flow.** The predecessor runbook
[`live_withdrawByProof_runbook.md`](../../bridge-relayer-daemon/docs/live_withdrawByProof_runbook.md)
documents the same six-stage pipeline behind the older `relayer
withdraw-e2e` subcommand, driven by a separate Python burn script and
manual coordination. Every case here has a counterpart there; where
diagnostic signatures overlap, this doc cross-references rather than
duplicating.

> **Notation.** `seq_no` = Acki Nacki block sequence number.
> "Covering bundle" = the first bundle whose `key_seq_no ≥ event_seq_no`
> that is verified on-chain. Withdrawals can only be submitted after
> their covering bundle lands (Circuit 4 anchor-chain verifies against
> a `layer_hashes` root that `verifyBlock` has already committed).
> With `W=128, P=8`, an **L1** bundle covers `W·P = 1024` seq_nos
> (~5.7 min chain-time at 3 seq/s). An **L2** bundle covers
> `W² = 16384` seq_nos (~91 min chain-time). Case 8+ covers the L2 flow.

---

## Table of Contents

- [Quick resume checklist (returning mid-flow)](#quick-resume-checklist-returning-mid-flow)
- [What the CLI does differently from `relayer withdraw-e2e`](#what-the-cli-does-differently-from-relayer-withdraw-e2e)
- [Exit-code catalog](#exit-code-catalog)
- [Idempotency semantics](#idempotency-semantics)
- [Timing model — inherited from the daemon runbook](#timing-model--inherited-from-the-daemon-runbook)
- [Reference addresses (current deploy)](#reference-addresses-current-deploy)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — First-time E2E from a fresh deploy (optimal sequence)](#case-1--first-time-e2e-from-a-fresh-deploy-optimal-sequence)
- [Case 2 — Follow-up withdrawal on an existing deploy](#case-2--follow-up-withdrawal-on-an-existing-deploy)
- [Case 3 — Event captured but daemon far behind head](#case-3--event-captured-but-daemon-far-behind-head)
- [Case 4 — Prover subprocess timeout / OOM](#case-4--prover-subprocess-timeout--oom)
- [Case 5 — On-chain `withdrawByProof` revert](#case-5--on-chain-withdrawbyproof-revert)
- [Case 6 — Multisig key drift / preflight refusal](#case-6--multisig-key-drift--preflight-refusal)
- [Case 7 — `WithdrawTreasuryShortfall` — bridge treasury empty](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty)
- [Case 8 — Fresh L2 deploy: first E2E withdrawal](#case-8--fresh-l2-deploy-first-e2e-withdrawal)
- [Case 9 — Sequential L2 withdrawals (stress-test loop)](#case-9--sequential-l2-withdrawals-stress-test-loop)
- [L2 timing model](#l2-timing-model)
- [Health checks](#health-checks)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning mid-flow)

```bash
cd /Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/crates/an-bridge-prover
export BRIDGE_CONFIG_DIR=./L1_config
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
export BRIDGE=$BRIDGE_ADDRESS   # ergonomics; same value

# 1. Is the bundle daemon alive and current?
pgrep -af 'relayer daemon-live' || echo "DAEMON NOT RUNNING"
echo "chain last_seen   = $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')"
echo "prover_state mtime: $(stat -f '%Sm' "$BRIDGE_CONFIG_DIR/state/prover_state.json" 2>/dev/null || echo MISSING)"

# 2. Any in-flight CLI withdrawal state?
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$BRIDGE_CONFIG_DIR/withdraw-state}"
ls -lt "$STATE_DIR"/*.json 2>/dev/null | head -3
# Each file's `status` field: Reserved | Burned | Proved | Submitted | Confirmed | Failed
# `Confirmed` = safe to fire a new (distinct) withdrawal.
# Anything else + same (from,to,to_chain,amount) = duplicate refusal (exit 3)
# unless you pass --allow-retry.

# 3. Latest CLI smoke logs
ls -lt "$BRIDGE_CONFIG_DIR/work_dir"/withdraw_smoke_*.log 2>/dev/null | head -3
```

---

## What the CLI does differently from `relayer withdraw-e2e`

The predecessor was a daemon subcommand that **captured** a
`WithdrawalInitiated` event fired independently by
`python/test_deploy_and_withdraw_only.py`. Coordination was manual
(baseline-before-burn ordering enforced by
`launch_withdraw_e2e_real.sh`) to avoid the "baseline excludes our own
event" trap (Case 2b in the daemon runbook).

The new CLI **fires the burn itself**. The full user input surface —
source multisig `--from`, owner keyfile `--from-keys`, destination
`--to --to-chain`, `--amount` — feeds a single in-process pipeline:

1. **Preflight** — key-file 0600, `--from` is an active
   single-custodian multisig, owner pubkey matches `--from-keys`,
   USDCBridge resolves via GraphQL, multisig ECC[3] balance ≥ amount.
2. **Idempotency reserve** — SHA-256 dedup key over
   `(from, to, to_chain, amount)`; refuse duplicate in-flight unless
   `--allow-retry`.
3. **Burn** — compose the multisig `sendTransaction` payload calling
   `USDCBridge.initiateWithdrawal(dstChainId, recipient)` using
   `tvm_client` (no `tvm-cli` shell-out); broadcast; record AN tx hash.
   **Bounce defaults to `true`** so USDC returns to the multisig on any
   bridge revert (the Python driver used `bounce=false`; the CLI's
   default is the safer of the two).
4. **Capture** — wait for the matching `WithdrawalInitiated` ExtOut
   event using `replay_latest = true` (the youngest matching event is
   unambiguously ours because we fired the burn seconds ago).
5. **Resurrect + wait for coverage** — read the deployed
   `AckiNackiBridge` at `--bridge-address` via
   `EthBridgeClient::read_full_state` and poll until
   `storedLastSeenBlockSeqNo` has advanced past the covering bundle
   boundary (`ceil(burn_seq / stride) * stride`, stride = 1024 for L1,
   16 384 for L2). When it has, `BridgeState::from_contract` builds a
   byte-for-byte mirror — no local `prover_state.json` needed. The
   parallel bundle-lane daemon (running anywhere) is what advances the
   contract; the CLI just waits.
6. **Prove** — enrich the resurrected `BridgeState` (single-shot, no
   retry), then produce the Circuit-4 SHPLONK aggregate in-process
   (Poseidon C4 prover) + subprocess (`aggregate-proof`).
7. **Submit** — call `dry_run_withdraw` first; unless `--dry-run`,
   broadcast `withdrawByProof` and wait for the receipt.

Every stage transition is persisted to a per-withdrawal state file
(`$BRIDGE_WITHDRAW_STATE_DIR/<sha256>.json`) so a mid-flight crash
leaves a resumable trace (v2: `--resume`). See
[Idempotency semantics](#idempotency-semantics).

**No baseline-before-burn wrapper needed.** `local_smoke.sh` /
`live_smoke.sh` under `crates/bridge-withdraw-e2e-cli/scripts/` are
straight wrappers: source the per-mode env file, export five identity
vars, invoke the binary.

---

## Exit-code catalog

Distinguishing "nothing broadcast" from "broadcast, outcome unknown" is
the whole point of the exit-code discipline — scripts that pattern-match
on a single non-zero would blind an operator to the difference that
matters for money.

| Code | Meaning | Nothing broadcast? | Remediation entry point |
|------|---------|-------------------|-------------------------|
| 0    | Success (or `--dry-run` returned OK) | — | — |
| 2    | Preflight refused — key perms, `--from` not an active MS, etc. | ✓ nothing | [Case 6](#case-6--multisig-key-drift--preflight-refusal) |
| 3    | Duplicate in-flight refused — same dedup key already exists | ✓ nothing | [Idempotency semantics](#idempotency-semantics) |
| 10   | AN burn broadcast, capture failed to observe outcome — reconcile via GQL | ✗ AN burn WAS broadcast | [Case 6](#case-6--multisig-key-drift--preflight-refusal) |
| 11   | Burn confirmed, `WithdrawalInitiated` capture timed out (>300 s poll) | ✗ AN burn done | [Case 3](#case-3--event-captured-but-daemon-far-behind-head) |
| 12   | Capture succeeded, Circuit-4 proof failed | ✗ AN burn done, no ETH tx | [Case 4](#case-4--prover-subprocess-timeout--oom) |
| 13   | Proof succeeded, `withdrawByProof` reverted / dry-run reverted | ✗ AN burn done, no ETH tx | [Case 5](#case-5--on-chain-withdrawbyproof-revert), [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty) |

**Rule.** Exit codes 10–13 all leave the AN burn broadcast. The USDC is
gone from the source multisig regardless of exit code ≥10; the question
is whether the EVM side saw the withdrawal. `--dry-run` fails at exit
codes 2, 3, or 13 only — the burn is never broadcast in dry-run mode.

---

## Idempotency semantics

**Dedup key.** SHA-256 of `{from}|{to}|{to_chain}|{amount}` (all ASCII,
`amount` as micro-USDC integer). Two withdrawals with identical tuples
collide; anything different (recipient, amount, or chain) does not.

**State-file lifecycle** (each file lives at `$STATE_DIR/<sha256>.json`):

| Status | Set when | Persisted fields |
|--------|----------|-----------------|
| `Reserved` | idempotency reserve succeeds | dedup tuple, timestamp |
| `Burned` | multisig `sendTransaction` broadcast | + `an_tx_hash` |
| `Proved` | Circuit-4 proof produced | + `withdrawal_msg_id`, `block_seq_no`, `block_id` |
| `Submitted` | EVM `withdrawByProof` sent | + `eth_tx_hash` |
| `Confirmed` | EVM receipt observed | + `eth_block_number` |
| `Failed` | any stage errors out | + `stage`, `reason` (no secrets) |

**Duplicate refusal (exit 3).** A file exists with status ≠ `Confirmed`
and ≠ `Failed`. The CLI prints the file path and the recorded stage.

**`--allow-retry`.** v1 blunt override — deletes the state file and
proceeds. Use only after manually confirming the earlier attempt is
terminated (e.g. AN tx reverted; GQL shows no `WithdrawalInitiated`
message id).

**Never persisted:** `--from-keys` file contents, `--eth-private-key`,
any signed messages, any raw witness. State files hold only
chain-observable identifiers.

**Cleanup rule.** `Confirmed` files are keepable forever (small; they
are your on-chain audit trail). `Failed` files are safe to prune once
the corresponding AN/ETH tx status is reconciled. `Reserved` files >24 h
old with no `an_tx_hash` are safe to prune — the burn never happened.

---

## Timing model — inherited from the daemon runbook

The CLI does not change the timing math. See the parent runbook's
[Timing model](../../bridge-relayer-daemon/docs/live_withdrawByProof_runbook.md#timing-model--why-fresh-deploy-demos-need-tight-lookahead)
section — the L1 fast-case ~17 min and L2 ~101 min figures still apply
end-to-end because the same subprocess prover does the same C4 work.

**One structural difference.** With the daemon subcommand, "wait for
covering bundle" was a manual step between burn and prove — and the
enricher checked `prover_state.json` on disk. With the CLI, capture
blocks until `WithdrawalInitiated` is observed, then the CLI polls
`AckiNackiBridge.storedLastSeenBlockSeqNo()` every 30 s until it has
advanced past the covering bundle boundary. Total budget: 300 s ExtOut
capture + 120 min coverage-wait ceiling (matches the daemon-side
enricher timeout so operator-facing patience is identical). Once
coverage is observed, `read_full_state` + `BridgeState::from_contract`
produce the enricher's input in one RPC round-trip, and enrich+prove
run single-shot. Timeouts still map to exit 11.

---

## Reference addresses (current deploy)

Snapshotted from the parent runbook — update both files together per
deploy. Source of truth: `../../../bridge-deployer.txt` (latest
Deploy #N section) and
`contracts/ethereum/broadcast/DeployShellnetE2EBridge.s.sol/11155111/run-latest.json`.

**Deploy #8 (2026-08-17)** — same values used by both runbooks:

| Item | Value |
|------|-------|
| Network | Sepolia (11155111) |
| AckiNackiBridge | `0x59dE8848bD5B3F1BD02AF9D269ab313AFa1d900B` |
| PrimaryAggregatorVerifier | `0x80089e338834826e54362e439d554f3e0facfbc9` |
| FallbackAggregatorVerifier | `0xe6f3b60b24ee3750f455054a1320fcbf5a11784c` |
| LayerHashesAggregatorVerifier | `0xcae03555a63c7a78a2c0554093cb98c2aa307aa7` |
| BridgeWithdrawalAggregatorVerifier (C4) | `0xe0bd31797b3d64dec85a0957304ae5ccc29efd2e` |
| MockBlockHeaderOracle | `0x312e2e8f159cae9d85cc0a0dfdf0cf1184a08f5a` |
| Bootstrap seed seq_no | `8768512` (W·P=1024-aligned) |
| Genesis bk_set_commitment | `0x08eb0a1892e4f75a8b5c8cff69322f95bf0437c371903998c9365fbe293ca71c` |
| Genesis prev_max_level_layer_hash | `0x28df66280644ceb9c08e0a8a5ac924939521577006f00c1b1c6538e6c0474449` |
| WITHDRAW_ACC_FR (USDCBridge account_id as Fr) | `0x1a1a…1a1a` (canonical, palindromic) |
| Relayer wallet (EVM signer for `withdrawByProof`) | `0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307` |

**WITHDRAW_ACC_FR verification (once per deploy):**

```bash
cast call $BRIDGE 'expectedWithdrawAcc()(uint256)' --rpc-url $RPC_URL
# Must print: 11806252235961651298089590628336806290921645495320214372650577192963691649562
```

If this differs, every C4 submit will revert on the equality check —
**STOP and redeploy** with the correct `WITHDRAW_ACC_FR` exported
before `deploy_bridge_bundle.sh`.

---

## Binary + env prerequisites

**Working directory:** `crates/an-bridge-prover/` (same as the daemon —
the CLI is symlinked in here so it can pull halo2-heavy prover deps).

**One-time build:**

```bash
cd crates/an-bridge-prover

# 1. CLI binary
cargo build --release -p bridge-withdraw-e2e-cli
#   -> ./target/release/bridge-withdraw-e2e-cli

# 2. Circuit-4 subprocess prover (the CLI shells out to this)
cargo build --release -p bridge-event-halo2-prover
#   -> ./target/release/bridge-event-halo2-prover

# 3. Aggregator subprocess (used by C4 aggregate stage)
cd ../bridge-evm-aggregator && cargo build --release && cd ../an-bridge-prover
```

**Env sanity** (in addition to bundle-lane vars from the parent runbook):

```bash
cd crates/an-bridge-prover
export BRIDGE_CONFIG_DIR=./L1_config     # or ./L2_config
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a

for v in \
  RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT \
  BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR BRIDGE_PARAMS_DIR
do
  [ -n "${!v}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
```

**Env sources (CLI-specific additions):**

- Bundle-lane env (`shellnet.common` + `$BRIDGE_CONFIG_DIR/env`)
  supplies everything the daemon uses — the CLI reuses those same vars
  (`RPC_URL`, `BRIDGE_ADDRESS`, `BRIDGE_GQL_ENDPOINT`, aggregator/verifier/params
  dirs). `BRIDGE_ADDRESS` is the single load-bearing input: the CLI
  reads `BridgeState` out of that contract, so a wrong value silently
  waits against the wrong state.
- `BRIDGE_WITHDRAW_STATE_DIR` (new; optional) — per-withdrawal
  idempotency state dir. Defaults to `$BRIDGE_CONFIG_DIR/withdraw-state/`.

**Not needed by the CLI (removed vs. earlier revisions):**

- `PROVER_STATE_PATH` — the CLI no longer reads any local
  `prover_state.json`. It resurrects `BridgeState` from the contract at
  every invocation.
- `--window-size` — the on-chain window shape is fixed at 128
  (`HISTORY_PROOF_WINDOW_SIZE`); no operator knob.

**Per-invocation caller vars** (used by the smoke wrappers):

- `WITHDRAW_FROM` = `<dapp_id>::<account_id>` of the source AN multisig
- `WITHDRAW_FROM_KEYS` = path to that multisig owner's `keys.json` (mode `0600`)
- `WITHDRAW_TO` = EVM recipient (`0x…` or `eip155:<id>:0x…`)
- `WITHDRAW_TO_CHAIN` = numeric EIP-155 chain id (`11155111` for Sepolia)
- `WITHDRAW_AMOUNT` = decimal USDC (e.g. `1.000000`), ≤ 6 fractional digits

---

## Case 1 — First-time E2E from a fresh deploy (optimal sequence)

**When to use:** Contract just deployed. Bundle daemon just cold-started.
Minimum wall-time demo.

**Trigger conditions:** Fresh `AckiNackiBridge` deployed; daemon
cold-started; fresh chain head available.

### Step 0 — Anchor freshness check

```bash
cd crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Check: (chain_head - seed_seqno) < 200 blocks
```

### Step 1 — Deploy the bridge

```bash
cd crates/an-bridge-prover
set -a && source shellnet.common && set +a
PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL=1 ./scripts/deploy_bridge_bundle.sh
```

### Step 2 — Unpause

```bash
export BRIDGE=<new_address>
cast send $BRIDGE 'unpause()' --rpc-url $RPC_URL --private-key $OWNER_PK
cast call $BRIDGE 'paused()(bool)' --rpc-url $RPC_URL   # false
```

### Step 2.5 — Seed the bridge treasury

See [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty)
for the seed-once procedure. Fresh deploy → treasury is 0; the C4
submit will revert `WithdrawTreasuryShortfall` on any burn until
seeded.

### Step 3 — Cold-start the bundle daemon

Follow the parent runbook's
[Case 1](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md#case-1)
cold-start. Verify within 30 s: log line `seed_policy=Explicit(<seed>)`
and `prover_state.json` mtime advances.

### Step 4 — Preflight the CLI (dry-run)

```bash
cd bridge/   # repo root
export BRIDGE_CONFIG_DIR=crates/an-bridge-prover/L1_config

export WITHDRAW_FROM=<dapp_id>::<account_id>    # source multisig
export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json
export WITHDRAW_TO=0x742d35Cc6634C0532925a3b844Bc454e4438f44e
export WITHDRAW_TO_CHAIN=11155111
export WITHDRAW_AMOUNT=1.000000

crates/bridge-withdraw-e2e-cli/scripts/local_smoke.sh
```

**Equivalent raw invocation:**

```bash
cd crates/an-bridge-prover
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)
SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$BRIDGE_CONFIG_DIR/work_dir/shplonk-snark'))")

./target/release/bridge-withdraw-e2e-cli withdraw \
  --dry-run \
  --yes \
  --from        "$WITHDRAW_FROM" \
  --from-keys   "$WITHDRAW_FROM_KEYS" \
  --to          "$WITHDRAW_TO" \
  --to-chain    "$WITHDRAW_TO_CHAIN" \
  --amount      "$WITHDRAW_AMOUNT" \
  --gql-endpoint      "$BRIDGE_GQL_ENDPOINT" \
  --anchor-layer auto \
  --rpc-url           "$RPC_URL" \
  --bridge-address    "$BRIDGE_ADDRESS" \
  --eth-private-key   "$RELAYER_PRIVATE_KEY" \
  --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
  --params-dir        "$BRIDGE_PARAMS_DIR" \
  --snark-dir         "$SNARK_DIR_ABS" \
  --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
  --work-dir          "$BRIDGE_CONFIG_DIR/work_dir" \
  --state-dir         "$BRIDGE_CONFIG_DIR/withdraw-state" \
  2>&1 | tee "$BRIDGE_CONFIG_DIR/work_dir/withdraw_dry_${TS}.log"
```

**Expected log signature (dry-run OK):**

```
INFO preflight: --from-keys mode=0600, owner pubkey resolves via GQL, ECC[3]≥amount
INFO idempotency: reserved sha256=<hex> at $STATE_DIR/<sha256>.json (dry-run: skipped)
INFO burn: (dry-run) would broadcast sendTransaction dest=<usdc_bridge>
INFO capture: (dry-run) would wait for WithdrawalInitiated
INFO enrich_witness: anchor_layer_mode=Auto, chosen L=1, key_seq_no=<K>
INFO subprocess_prover: bridge-event-halo2-prover start
INFO subprocess_prover: aggregate SHPLONK ok, calldata_len=<bytes>
INFO submit_withdraw: dry-run eth_call OK — would submit withdrawByProof(...)
```

Exit 0 → the pipeline is green end-to-end. Any non-zero → jump to the
corresponding case per [exit-code catalog](#exit-code-catalog).

### Step 5 — Real submit

Same invocation, drop `--dry-run` (or use the wrapper):

```bash
crates/bridge-withdraw-e2e-cli/scripts/live_smoke.sh
```

**Success signal:**

```
INFO submit_withdraw: withdrawByProof confirmed tx=0x… block=<n>
```

**On-chain verification:**

```bash
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -100
```

---

## Case 2 — Follow-up withdrawal on an existing deploy

**When to use:** Bridge already unpaused, treasury seeded, daemon
running for hours/days.

**Precheck — daemon current:**

```bash
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
HEAD=$(curl -s -X POST https://shellnet.ackinacki.org/graphql \
  -H 'content-type: application/json' \
  -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
  | jq -r '.data.blockchain.blocks.edges[0].node.seq_no')
echo "lag = $((HEAD - LAST))  (want < 1024)"
```

If lag < 1024 → the covering bundle will land within one W·P stride.
Run `live_smoke.sh` directly.

If lag > 1024 → [Case 3](#case-3--event-captured-but-daemon-far-behind-head).

**Followed by:** Case 1 Step 4 (dry-run), then Step 5 (real submit). No
redeploy, no unpause, no treasury seed.

---

## Case 3 — Event captured but daemon far behind head

**Scenario:** CLI exited 11 (capture timeout). Either the event was
never observed (poll expired) or the enricher blocked waiting for the
covering bundle.

**Diagnostic — distinguish the two sub-cases:**

```bash
# 1. Grep the log for capture progress
grep -E 'capture: (matched|polling|timed out)' $LOG_PATH | tail -5

# If "matched dst=…:026a" appears but no "enrich_witness: filling" →
# event was captured; enricher blocked. Sub-case 3a.
# If "polling" only, no "matched" → event never seen. Sub-case 3b.
```

### Sub-case 3a — Enricher blocked on covering bundle

**Root cause:** `event_seq_no=E`, `storedLastSeenBlockSeqNo()=L`,
`E − L > W·P` (>1024 on L1, >16384 on L2). Covering bundle not yet
on-chain.

```bash
E=<event_seq_no from log: "capture: matched … seq_no=E">
L=$(cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
COVER=$(( (E + 1023) / 1024 * 1024 ))
BUNDLES_TO_WAIT=$(( (COVER - L) / 1024 ))
WALL_MIN=$(( BUNDLES_TO_WAIT * 12 ))
echo "event=$E last_seen=$L covering=$COVER  wait ≈ ${WALL_MIN} min"
```

**Remediation:**

- If `WALL_MIN < 60`: The CLI has already exited (300 s + 120 min
  enricher budget spent). Re-run the same command — idempotency will
  refuse (exit 3) unless `--allow-retry`. Because the burn already
  broadcast, DO NOT fire a fresh burn; use `--allow-retry` after the
  covering bundle lands:

  ```bash
  watch -n 720 'cast call $BRIDGE_ADDRESS storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC_URL'
  # Once L ≥ COVER, re-run:
  crates/bridge-withdraw-e2e-cli/scripts/live_smoke.sh --allow-retry
  # (v1 blunt override; v2 will be --resume)
  ```

  Because the AN burn is already done, the CLI will replay the capture
  path (`replay_latest = true`) and pick up the same event. Proof is
  deterministic per `(event, prover_state)`.

- If `WALL_MIN ≥ 60`: Redeploy is warranted only on fresh testnet.
  Follow the parent runbook's Case 3 remediation — redeploy at a fresh
  W·P boundary, re-seed daemon, then fire a NEW burn (the old
  state-file's dedup tuple stays valid; use `--allow-retry` OR change
  the amount by 1 micro-USDC to sidestep dedup).

### Sub-case 3b — Event never observed

**Root cause:** The burn AN tx never produced a `WithdrawalInitiated`
ExtOut — most likely the USDCBridge rejected the call (invalid dst chain,
paused USDCBridge, insufficient allowance, bounce came back).

```bash
# Check AN-side status via GQL using an_tx_hash from the state file
STATE_FILE="$STATE_DIR/<sha256>.json"
AN_TX=$(jq -r '.an_tx_hash' "$STATE_FILE")
# Query the message tree; look for exit_code or aborted
```

**Remediation:** If the burn aborted, exit is 10, not 11. State file
records `Failed` with `stage=burn`. Fix the underlying issue (drift →
[Case 6](#case-6--multisig-key-drift--preflight-refusal); pause → [Case 5](#case-5--on-chain-withdrawbyproof-revert)), prune the failed state file, re-run.

---

## Case 4 — Prover subprocess timeout / OOM

**Symptom (CLI exit 12):**

```
ERROR prove: bridge-event-halo2-prover exited status=<code>
   OR
ERROR prove: timeout after 1800s
```

**Trigger conditions:** Out of disk, OOM, RAM swap-thrash, params
missing, PK cache corrupt.

**Checks:**

```bash
# Params size (Circuit 4 needs ~17 GB)
du -sh $BRIDGE_PARAMS_DIR/

# Free disk
df $BRIDGE_PARAMS_DIR/

# RAM headroom: C4 K=19 needs ~40 GB peak
```

**Remediation:**

- Free resources; re-run with the same tuple — `--allow-retry` if the
  first attempt left a `Reserved` state file. The witness under
  `$BRIDGE_CONFIG_DIR/work_dir/witness_event_<seq>.json` is
  deterministic and reusable; do NOT delete it between attempts.
- If cold-cache slowness is the real issue (not OOM), bump the timeout:

  ```bash
  ./target/release/bridge-withdraw-e2e-cli withdraw \
    ...same flags as Case 1 Step 4... \
    --prover-timeout-s 3600
  ```
- If the log shows swap thrash, the OOM is real — don't just extend
  timeout. Move to a bigger host or shrink another workload.

---

## Case 5 — On-chain `withdrawByProof` revert

**Symptom (CLI exit 13):** Dry-run or real submit fails with a Sepolia
revert. State file records `Failed` with `stage=submit` and (when
available) the decoded revert reason.

**Selector → error mapping:** (same as parent runbook; C4 verifier
errors are unchanged by the CLI)

| Selector | Error | Root cause |
|----------|-------|-----------|
| implicit | `AttestationProofRejected()` | C4 proof public inputs mismatch. Usually `acc_fr` drift OR `layer_hashes[1]` not yet verified (covering bundle not on-chain yet). |
| implicit | `WithdrawalAlreadyExecuted(msg_id)` | Same `msg_id` reused. Fire fresh burn. |
| implicit | `AnchorNotFound(key_seq_no)` | Covering bundle's `layer_hashes[1]` not on-chain. → [Case 3](#case-3--event-captured-but-daemon-far-behind-head). |
| implicit | `PausedError()` | Bridge paused. |
| `0xbb651fce` | `WithdrawTreasuryShortfall(uint256,uint256)` | `pub.amount > treasuryBalance`. → [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty). |

**Decode a revert:**

```bash
# CLI logs the selector + decoded params where possible; if not:
cast 4byte <selector>

# Full trace against the deployed verifier
CALLDATA=$(jq -r '.calldata_hex' "$BRIDGE_CONFIG_DIR/proofs/proof_event_<seq>.json")
PI=$(jq -c '.public_inputs' "$BRIDGE_CONFIG_DIR/proofs/proof_event_<seq>.json")
cast call $BRIDGE_ADDRESS \
  'withdrawByProof(bytes,uint256[13])' \
  "$CALLDATA" "$PI" \
  --rpc-url $RPC_URL --trace
```

**Remediation:** Fix the on-chain condition; re-run the SAME CLI
invocation with `--allow-retry`. Proof is deterministic per
`(event, prover_state, chain_state)` — if chain state changed
(treasury seeded, unpaused, covering bundle landed), the proof will
regenerate against the new state.

**Do not delete `work_dir/witness_event_*.json`** between attempts;
regeneration is expensive.

---

## Case 6 — Multisig key drift / preflight refusal

**Two flavors, distinguishable by exit code:**

### 6a — Preflight refusal (exit 2)

CLI never broadcast anything. Common causes with the human message the
CLI prints:

- `arg-invalid: --from-keys`: file mode is not `0600` → `chmod 600 <path>`
- `arg-invalid: --from`: not `dapp_id::account_id` shape, or dapp_id is
  wrong workchain
- `preflight: multisig at --from is not deployed / not single-custodian`
- `preflight: owner pubkey from --from-keys does not match multisig getOwnerKey`
- `preflight: multisig ECC[3] balance = X, need Y` (insufficient USDC)
- `preflight: USDCBridge account_id does not resolve via GQL`

**Remediation:** Fix the specific issue. Preflight is side-effect free
— no state file was written, no burn attempted.

### 6b — Burn broadcast, outcome unknown (exit 10)

`sendTransaction` broadcast but the CLI could not observe the resulting
message on GQL within its budget. Typical root cause: local
`USDCBridge.shellnet.keys.json` public key ≠ on-chain
`getOwnerPubkey`, so the USDCBridge internally rejected the
`initiateWithdrawal` call (TVM exit_code=209 signature error).

**Diagnostic:**

```bash
cd crates/an-bridge-prover
LOCAL_PUB=$(jq -r '.public' python/contracts/USDCBridge.shellnet.keys.json)
python3 -c "
from python.helper.tonos_helper import get_owner_pubkey
print(get_owner_pubkey(
  address='0:<USDCBridge_acc_id>',
  gql='$BRIDGE_GQL_ENDPOINT',
))"
# Compare with $LOCAL_PUB
```

**Remediation:** Overlay the current keypair (from Sehor or the
acki-nacki config repo):

```bash
cp ../../../acki-nacki/config/USDCBridge.keys.json \
   python/contracts/USDCBridge.shellnet.keys.json
```

Then prune the `Failed` state file and re-run.

---

## Case 7 — `WithdrawTreasuryShortfall` — bridge treasury empty

**Symptom (CLI exit 13):** Dry-run or submit reverts with selector
`0xbb651fce`: `WithdrawTreasuryShortfall(<pub.amount>, <treasuryBalance>)`.

**Trigger:** Fresh deploy (treasury=0) OR prior deposit < current burn
amount.

**Seed the treasury:**

```bash
cd crates/an-bridge-prover
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a

export USDC=0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8
export FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D
export WALLET=$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)
export AMOUNT=10000000    # 10.000000 USDC — demo safety margin

# 1. Mint test USDC
cast send $FAUCET 'mint(address,address,uint256)' $USDC $WALLET $AMOUNT \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

# 2. Approve bridge
cast send $USDC 'approve(address,uint256)' $BRIDGE_ADDRESS $AMOUNT \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

# 3. Deposit (dummy AN destination; no live AN-side indexer on shellnet)
cast send $BRIDGE_ADDRESS 'deposit(uint256,int8,bytes32)' \
  $AMOUNT 0 0x1111111111111111111111111111111111111111111111111111111111111111 \
  --rpc-url $RPC_URL --private-key $RELAYER_PRIVATE_KEY

# 4. Confirm
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL
# -> 10000000
```

**Why dummy AN destination is safe on shellnet:** no live AN-side
listener consumes the phantom `Deposit` event. **Do NOT use** on a
live bridge with an active AN-side indexer — you'll create a ghost
credit.

**Remediation:** After deposit, re-run the CLI with `--allow-retry`.
Proof regenerates against the new chain state (treasury balance
component of the check).

Scale the seed to cover all planned burns in the session — 10 USDC is
the demo default.

---

## Case 8 — Fresh L2 deploy: first E2E withdrawal

**When to use:** `BRIDGE_ANCHOR_LEVEL=2` L2-anchoring end-to-end
exercise. Bundle stride is `W² = 16384` seq_nos (~91 min chain-time)
vs L1's 1024 (~5.7 min).

### Step L0 — Emit L2 genesis anchors

```bash
cd crates/an-bridge-prover/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --level 2 \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Emits GENESIS_ANCHOR_LEVEL=2, GENESIS_LAST_SEEN_BLOCK_SEQ_NO=<W²-aligned>, etc.
```

### Step L1 — Deploy with L2 wiring

```bash
cd crates/an-bridge-prover
set -a && source shellnet.common && set +a
PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL=2 ./scripts/deploy_bridge_bundle.sh
```

**Post-deploy sanity — verify W²-alignment:**

```bash
export BRIDGE=<new_address>
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
python3 -c "print('L2-aligned:', $LAST % 16384 == 0, 'last_seen:', $LAST)"
# If False → redeploy (L1 seed consumed by mistake)
```

### Step L2 — Unpause + treasury seed

Identical to Case 1 Step 2 + [Case 7](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty).

### Step L3 — Cold-start daemon under L2

Follow the parent runbook's Case 8 Step L3; expected log signature:

```
INFO daemon-live: anchor_mode=L2, bundle_stride=16384
INFO daemon-live: on-chain last_seen=<seed>, stride-aligned=OK
INFO daemon-live: seed_policy=Explicit(<seed>), anchor_level=2
```

**Startup drift refusals — STOP and fix:**

- `refuse: on-chain last_seen (=X) % 16384 != 0` → redeploy with L2 genesis
- `refuse: anchor_level mismatch (state=1, cfg=2)` → delete stale L1
  `prover_state.json`, restart

### Step L4 — Wait for first L2 bundle

```bash
watch -n 60 'cast call $BRIDGE storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC_URL'
# Wait until value bumps by exactly 16384
```

Takes ~91 min chain + ~10 min prover ≈ 101 min worst-case, ~50 min
typical.

### Step L5 — Run the CLI with **explicit** L2

**Do NOT use `--anchor-layer auto` on L2.** The enricher requires the
operator to acknowledge the wait budget.

```bash
cd bridge/
export BRIDGE_CONFIG_DIR=crates/an-bridge-prover/L2_config

# Same identity vars as Case 1 Step 4
export WITHDRAW_FROM=<dapp_id>::<account_id>
export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json
export WITHDRAW_TO=0x…
export WITHDRAW_TO_CHAIN=11155111
export WITHDRAW_AMOUNT=1.000000

# For now the smoke wrappers hardcode L1 auto; the L2 raw invocation:
cd crates/an-bridge-prover
set -a && source "$BRIDGE_CONFIG_DIR/env" && set +a
TS=$(date +%Y%m%d_%H%M%S)
SNARK_DIR_ABS=$(python3 -c "import os; print(os.path.abspath('$BRIDGE_CONFIG_DIR/work_dir/shplonk-snark'))")

./target/release/bridge-withdraw-e2e-cli withdraw \
  --dry-run \
  --yes \
  --from        "$WITHDRAW_FROM" \
  --from-keys   "$WITHDRAW_FROM_KEYS" \
  --to          "$WITHDRAW_TO" \
  --to-chain    "$WITHDRAW_TO_CHAIN" \
  --amount      "$WITHDRAW_AMOUNT" \
  --gql-endpoint      "$BRIDGE_GQL_ENDPOINT" \
  --prover-state-path "$BRIDGE_CONFIG_DIR/state/prover_state.json" \
  --window-size 128 \
  --anchor-layer 2 \
  --i-know-the-wait \
  --rpc-url           "$RPC_URL" \
  --bridge-address    "$BRIDGE_ADDRESS" \
  --eth-private-key   "$RELAYER_PRIVATE_KEY" \
  --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
  --params-dir        "$BRIDGE_PARAMS_DIR" \
  --snark-dir         "$SNARK_DIR_ABS" \
  --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
  --work-dir          "$BRIDGE_CONFIG_DIR/work_dir" \
  --state-dir         "$BRIDGE_CONFIG_DIR/withdraw-state" \
  2>&1 | tee "$BRIDGE_CONFIG_DIR/work_dir/withdraw_l2_dry_${TS}.log"
```

**Expected log signature (L2 differences):**

```
INFO enrich_witness: anchor_layer_mode=Explicit(2), i_know_the_wait=true
INFO enricher: filling ... timeout_s=7200        # 2 h (post-2026-08-18)
INFO resolved anchor: L2 (mode=Explicit(2), auto_escalated=false)
INFO chain built: anchor_layer=L2, active_links=1, ...
INFO enricher: witness ready  layer_idx=1        # 0-indexed → L2; ANY OTHER VALUE = L1 fallback bug
```

**Ground-truth:** `layer_idx=1` confirms L2-anchoring. Anything else =
accidental L1 fallback; investigate before submitting.

**If the enricher times out (120 min):** Daemon never landed the
covering L2 bundle in 2 h. Bundle-lane issue → check `daemon-live` logs.
The CLI exits 11.

Then real submit — drop `--dry-run`.

---

## Case 9 — Sequential L2 withdrawals (stress-test loop)

**When to use:** After Case 8 succeeds, drive 2–3 more burns through
the same L2 rails.

**Session budget:** ~101 min/bundle + ~5 min per CLI run ≈ ~2 h between
successful withdrawals; 3 cycles fit in ~6 h.

**Per cycle (N = 2, 3, …):**

```bash
# 1. Confirm previous WithdrawalExecuted landed
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -1000 | tail -5

# 2. Confirm treasury still funded (seed once at Case 8 Step L2)
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL

# 3. Vary the amount by 1 micro-USDC so the dedup key differs from cycle N-1
#    (otherwise the CLI refuses exit 3; --allow-retry works too but is blunter).
export WITHDRAW_AMOUNT=1.00000$N

# 4. Run the CLI (Case 8 Step L5 invocation), wait for its 101-min budget
```

**Watch between cycles:**

- Fresh Circuit-2 bundle every ~91 min (daemon log).
- `LayerAnchorAppended` fires **twice per bundle** (L1 + L2). Missing
  L2 emissions → lost `verifyBlock` on bundle lane; pause and inspect.
- `storedLastSeenBlockSeqNo` should be a 16384-multiple between cycles.
- CLI state dir grows one file per cycle; each stays `Confirmed`.

**Cycle N ≥ 2 revert is almost certainly a [Case 5](#case-5--on-chain-withdrawbyproof-revert) mode**, not L2-specific.
Start with the Case 5 catalog.

---

## L2 timing model

See parent runbook's
[L2 timing model](../../bridge-relayer-daemon/docs/live_withdrawByProof_runbook.md#l2-timing-model)
— unchanged by the CLI. Key scalars:

- W (layer 1 window size): 128
- P (bundles per L1 layer): 8
- W·P (L1 stride): 1024 seq_nos (~5.7 min chain-time at 3 seq/s)
- W² (L2 stride): 16384 seq_nos (~91 min chain-time)
- Prover wall-time per bundle: ~12 min (warm PK cache)
- Circuit-4 event proof: ~5 min warm PK, ~20 min cold
- ENRICH_TIMEOUT: 120 min (post-2026-08-18)

**Consequence for demos:** L2 stress testing is NOT a "quick demo".
Plan half-day per 3-cycle run.

---

## Health checks

**CLI-lane snapshot:**

```bash
# Latest CLI state files (one per unique (from,to,chain,amount) tuple)
ls -lt $BRIDGE_CONFIG_DIR/withdraw-state/*.json 2>/dev/null | head -3
# Peek at the newest
jq . "$(ls -t $BRIDGE_CONFIG_DIR/withdraw-state/*.json | head -1)" 2>/dev/null

# Latest captured witness
ls -lt $BRIDGE_CONFIG_DIR/work_dir/witness_event_*.json 2>/dev/null | head -3

# Latest generated proof
ls -lt $BRIDGE_CONFIG_DIR/proofs/proof_event_*.json 2>/dev/null | head -3

# Circuit 4 PK cache (should exist after first successful run)
ls -lh $BRIDGE_PARAMS_DIR/pk_cache/ 2>/dev/null
```

**Sepolia snapshot:** (same commands as parent runbook)

```bash
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block 0

cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL

cast balance 0xb586356D52eAee055Ca569Ff412DFeFFc5bB2307 --rpc-url $RPC_URL --ether
```

---

## File & state reference

**Root:** `crates/an-bridge-prover/` (build cwd; matches the daemon).

```
crates/an-bridge-prover/
├── L1_config/                       ← BRIDGE_CONFIG_DIR for L1 mode
│   ├── env                          ← sources shellnet.common + layers deploy-specific vars
│   ├── state/prover_state.json      ← WRITTEN by daemon-live; READ by CLI
│   ├── work_dir/
│   │   ├── witness_event_<seq>.json ← enriched witness (input to Circuit 4)
│   │   ├── shplonk-snark/           ← intermediate SHPLONK artifacts
│   │   └── withdraw_smoke_*.log     ← CLI stdout+stderr (via scripts)
│   ├── proofs/proof_event_<seq>.json← aggregated SHPLONK calldata + PI
│   └── withdraw-state/              ← per-withdrawal idempotency state (NEW to CLI)
│       └── <sha256>.json            ← one per unique (from,to,chain,amount)
├── L2_config/                       ← parallel layout for L2 mode
├── target/release/
│   ├── bridge-withdraw-e2e-cli      ← this CLI
│   ├── bridge-event-halo2-prover    ← Circuit 4 subprocess
│   └── relayer                      ← daemon-live (parent runbook)
└── bridge-withdraw-e2e-cli/         ← symlink → ../bridge-withdraw-e2e-cli
```

**Never persisted anywhere the CLI writes:**

- `--from-keys` file contents
- `--eth-private-key`
- Any signed AN or ETH messages
- Any raw witness field values (the witness JSON files hold witness
  data, but those are outputs of the enricher and hold no key
  material)

**Safe to prune between demos:**

- `work_dir/`, `proofs/` — regeneration is deterministic; ~5 min per
  proof with warm PK cache.
- `withdraw-state/<sha256>.json` files with status `Confirmed` — keep
  for audit; `Failed` — safe to prune once reconciled; `Reserved` >24 h
  old with no `an_tx_hash` — safe to prune.

**Do NOT touch between demos:**

- `params/` and `params/pk_cache/` — cold cache costs ~20 min per
  proof; warm cache is ~5 min.
- `state/prover_state.json` — daemon-owned; deleting it forces a full
  cold restart of the bundle lane.

---

## Change log / known incidents

**2026-08-25 — CLI first commit** (`9319c9a`)
`bridge-withdraw-e2e-cli` wired end-to-end: burn + idempotency +
orchestrator + output. Supersedes the `relayer withdraw-e2e`
subcommand for end-user withdrawals (the daemon still exposes it for
smoke testing).

**2026-08-27 — Helper scripts** (`fe52aac`)
`scripts/local_smoke.sh` (dry-run) and `scripts/live_smoke.sh` (real
submit) added; CHANGELOG entry documents env vars, exit codes,
idempotency semantics.

**Inherited from the parent runbook** (still apply):

- **2026-08-13 — fire-window gone (`7bb3da9`)** — horizontal-chain
  event proving removed the fire-window constraint. `--anchor-layer
  auto` picks L1 or L2. No coordination needed on burn timing.

- **2026-08-06 — `WIRE_WITHDRAW_BY_PROOF` mandatory (`a43993b`)** —
  Sepolia deploys MUST provide `WITHDRAW_ACC_FR` at construction; every
  Deploy #4+ has C4 baked in.

- **2026-08-18 — Level-parametric contracts (`bf0d41a`)** — bridge
  constructor is level-opaque (stores only genesis scalar).
  `_layerWindows` populated on first `verifyBlock` across active
  layers. No Solidity change for L2 deploys.

- **Deploy #7 vs #8 timing lesson (2026-08-17)** — fresh-deploy best
  practice: run `compute_bridge_anchors --at-head` within 3 min of
  `forge script`; fire burn within ~5 min of daemon startup. Miss
  either and wait grows by ~12 min per additional bundle of catch-up.

- **ENRICH_TIMEOUT bump 90 → 120 min (post-2026-08-18,
  `driver.rs:172`)** — L2 worst-case is ~101 min chain + ~10 min
  prover; the previous 90-min budget would timeout. Change applies to
  the CLI too (same enricher code path).
