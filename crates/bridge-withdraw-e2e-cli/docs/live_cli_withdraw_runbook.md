# Live CLI Withdraw E2E Runbook — shellnet → Sepolia (Circuit 4)

Operational guide for driving a full AN→ETH withdrawal E2E through the
`bridge-withdraw-e2e-cli` binary against a deployed `AckiNackiBridge` on
Sepolia. This CLI owns the per-withdrawal composition (burn → capture
→ Circuit-4 SHPLONK proof → `withdrawByProof`).

**L2 is the production path; L1 is testing-only.**

The team-run bundle relayer on our server is configured for **L2**
(`anchor_level=2`, stride `W² = 16384` seq_nos ≈ 91 min chain-time), and
the pinned `BRIDGE_ADDRESS` in [`config/bridge_config`](../config/bridge_config)
is that L2 deploy. **All production withdrawals go through L2** — that
means invoking the CLI with `--anchor-layer 2 --i-know-the-wait` per
[Case 8 Step L5](#step-l5--run-the-cli-with-explicit-l2).

L1-anchoring (stride `W·P = 1024` seq_nos ≈ 5.7 min) exists only as a
development/testing convenience for advanced users deploying their own
bridge + relayer from scratch. It is **not** the path a default user
should take against the pinned deploy. Case 1 walks the L1 flow for
that testing scenario.

**Two audiences.**

1. **Default user.** Point the CLI at the pinned L2 `AckiNackiBridge`
   already deployed on Sepolia (address ships in
   [`config/bridge_config`](../config/bridge_config)); our server-side
   relayer keeps that contract advancing. You bring your own Sepolia
   burner wallet + AN multisig, source `config/bridge_config`, and
   invoke the CLI **with `--anchor-layer 2 --i-know-the-wait`**
   ([Case 8 Step L5](#step-l5--run-the-cli-with-explicit-l2)). **You
   never deploy anything.**
2. **Advanced user.** Deploy your own `AckiNackiBridge` + run your own
   bundle relayer to exercise the full ecosystem from scratch. L1 or L2
   is your choice — Case 1 covers L1 (fast, testing only); Case 8
   covers L2 (production shape). Set `BRIDGE_ADDRESS` in
   `config/bridge_config` to your deploy and follow
   [`live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md)
   for the deploy + relayer setup.

**Scope of this runbook.** The end-user withdrawal path, driven by the
CLI. Every stage of the pipeline is in-process — `--from` composes the
AN multisig `sendTransaction`, the tool broadcasts it, waits for the
matching `WithdrawalInitiated` ExtOut event, **resurrects the prover's
`BridgeState` mirror by reading the deployed `AckiNackiBridge` contract
at `--bridge-address`**, polls that contract until the covering L1/L2
bundle has landed, produces the Circuit-4 SHPLONK proof, calls
`dry_run_withdraw`, and submits `withdrawByProof`. `--dry-run` stops
after preflight — see [Step 4](#step-4--preflight-the-cli-dry-run).
**Assumes** the bundle lane (Circuits 1A + 2) is running _somewhere_ —
either on our server (default user) or on your own host (advanced) —
feeding `verifyBlock` transactions to the bridge.

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
- [How the CLI works](#how-the-cli-works)
- [Exit-code catalog](#exit-code-catalog)
- [Idempotency semantics](#idempotency-semantics)
- [Timing model](#timing-model)
- [Wallet setup and bridge config](#wallet-setup-and-bridge-config)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1 — L1 first-time E2E from a fresh deploy (testing only)](#case-1--l1-first-time-e2e-from-a-fresh-deploy-testing-only)
- [Case 2 — Follow-up withdrawal on an existing deploy](#case-2--follow-up-withdrawal-on-an-existing-deploy)
- [Case 3 — Event captured but daemon far behind head](#case-3--event-captured-but-daemon-far-behind-head)
- [Case 4 — Prover subprocess timeout / OOM](#case-4--prover-subprocess-timeout--oom)
- [Case 5 — On-chain `withdrawByProof` revert](#case-5--on-chain-withdrawbyproof-revert)
- [Case 6 — Multisig key drift / preflight refusal](#case-6--multisig-key-drift--preflight-refusal)
- [Case 7 — `WithdrawTreasuryShortfall` — bridge treasury empty](#case-7--withdrawtreasuryshortfall--bridge-treasury-empty)
- [Case 8 — L2 production path: first E2E withdrawal](#case-8--l2-production-path-first-e2e-withdrawal)
- [Case 9 — Sequential L2 withdrawals (stress-test loop)](#case-9--sequential-l2-withdrawals-stress-test-loop)
- [L2 timing model](#l2-timing-model)
- [Health checks](#health-checks)
- [File & state reference](#file--state-reference)
- [Change log / known incidents](#change-log--known-incidents)

---

## Quick resume checklist (returning mid-flow)

All paths below are relative to the repo root. The CLI is a standalone
tool with its own single config; there is no L1/L2 split.

```bash
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…                       # your own Sepolia burner
set -a && source config/bridge_config && set +a
export BRIDGE=$BRIDGE_ADDRESS                        # ergonomics; same value

# 1. Is the bridge reachable and advancing?
echo "chain last_seen = $(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')"
# Run twice ~1 min apart — the number must increase. If it does not, the
# bundle daemon (owned by whoever operates the deploy — team, or you) has
# stalled; no withdrawal will land until it resumes.

# 2. Any in-flight CLI withdrawal state?
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$HOME/.bridge-withdraw-state}"
ls -lt "$STATE_DIR"/*.json 2>/dev/null | head -3
# Each file's `status` field: Reserved | Burned | Proved | Submitted | Confirmed | Failed
# `Confirmed` = safe to fire a new (distinct) withdrawal.
# Anything else + same (from,to,to_chain,amount) = duplicate refusal (exit 3)
# unless you pass --allow-retry.

# 3. Latest CLI smoke logs (produced by scripts/{local,live}_smoke.sh)
ls -lt ./work_dir/withdraw_smoke_*.log 2>/dev/null | head -3
```

---

## How the CLI works

The CLI **fires the burn itself** and drives the withdrawal end-to-end
in one process. The full user input surface — source multisig `--from`,
owner keyfile `--from-keys`, destination `--to --to-chain`, `--amount`
— feeds a single in-process pipeline:

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
   bridge revert.
4. **Capture** — wait for the matching `WithdrawalInitiated` ExtOut
   event using `replay_latest = true` (the youngest matching event is
   unambiguously ours because we fired the burn seconds ago).
5. **Resurrect + wait for coverage** — read the deployed
   `AckiNackiBridge` at `--bridge-address` via
   `EthBridgeClient::read_full_state` and poll until
   `storedLastSeenBlockSeqNo` has advanced past the covering bundle
   boundary (`ceil(burn_seq / stride) * stride`, stride = 1024 for L1,
   16 384 for L2). When it has, `BridgeState::from_contract` builds a
   byte-for-byte mirror. The bundle-lane relayer (running somewhere,
   possibly on a different host) is what advances the contract; the
   CLI just waits.
6. **Prove** — enrich the resurrected `BridgeState` (single-shot, no
   retry), then produce the Circuit-4 SHPLONK aggregate in-process
   (Poseidon C4 prover) + subprocess (`aggregate-proof`).
7. **Submit** — call `dry_run_withdraw` first; unless `--dry-run`,
   broadcast `withdrawByProof` and wait for the receipt.

Every stage transition is persisted to a per-withdrawal state file
(`$BRIDGE_WITHDRAW_STATE_DIR/<sha256>.json`) so a mid-flight crash
leaves a resumable trace (v2: `--resume`). See
[Idempotency semantics](#idempotency-semantics).

`local_smoke.sh` / `live_smoke.sh` under
`crates/bridge-withdraw-e2e-cli/scripts/` are straight wrappers: source
`config/bridge_config`, export five identity vars, invoke the binary.

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
is whether the EVM side saw the withdrawal. `--dry-run` stops after
preflight — it can only produce exit codes 0, 2, or 3 (preflight OK,
preflight refused, or duplicate refused).

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
| `Captured` | `WithdrawalInitiated` event observed | + `withdrawal_msg_id`, `block_seq_no` |
| `Proved` | Circuit-4 proof produced | (same as Captured) |
| `Submitted` | EVM `withdrawByProof` returned a tx hash | + `eth_tx_hash` |
| `Confirmed` | EVM receipt observed | + `eth_tx_hash` |
| `Failed` | any stage errors out | fields preserved from last successful stage |

**Duplicate refusal (exit 3).** A file exists with status other than
`Failed`. The CLI prints the file path, the recorded stage, and the
prior AN/ETH tx hashes so an operator can reconcile before retrying.

**`--allow-retry`.** Resume-in-place: the CLI keeps the prior record
verbatim (preserving `an_tx_hash`, `withdrawal_msg_id`, `block_seq_no`,
`eth_tx_hash`) and skips any stage that already completed. Concretely:
- Prior `Burned` / `Captured` / `Proved` → skip burn, resume from
  capture. The prior AN tx hash is reused, so **the burn is never
  broadcast twice** (this was a v1 bug — resume used to overwrite the
  record with a fresh `Reserved`, dropping `an_tx_hash`, and the
  orchestrator would then unconditionally re-fire `burn::fire`).
- Prior `Failed` → clean-slate restart. Failed means "we don't know how
  far we got"; the state file is overwritten and every stage runs from
  scratch. No flag needed for this case.
- Prior `Submitted` → **refused even with `--allow-retry`**. There is a
  broadcast EVM tx whose receipt we never observed; re-broadcasting
  risks a double payout. Reconcile the `eth_tx_hash` on-chain first,
  then either wait for confirmation (and rerun; it will become
  `Confirmed`) or edit the state file to `Failed` manually.
- Prior `Confirmed` → **refused always**. The withdrawal already paid
  out. To move funds again, generate a new identity (different amount
  or recipient).

**Never persisted:** `--from-keys` file contents, `--eth-private-key`,
any signed messages, any raw witness. State files hold only
chain-observable identifiers.

**Cleanup rule.** `Confirmed` files are keepable forever (small; they
are your on-chain audit trail). `Failed` files are safe to prune once
the corresponding AN/ETH tx status is reconciled. `Reserved` files >24 h
old with no `an_tx_hash` are safe to prune — the burn never happened.

---

## Timing model

End-to-end wall-time depends on the bundle-lane relayer's cadence
(which the CLI does not control) and the C4 prover step (which it
does).

**L1-anchored (stride = 1024 seq_nos ≈ 5.7 min chain-time at 3 seq/s):**
fast-case ~17 min end-to-end from burn to `withdrawByProof` receipt.

**L2-anchored (stride = 16 384 seq_nos ≈ 91 min chain-time):** ~101 min
end-to-end (worst case), ~50 min typical.

**Per-stage budgets inside the CLI:**

- Capture (`WithdrawalInitiated` ExtOut): 300 s poll ceiling.
- Coverage-wait
  (`AckiNackiBridge.storedLastSeenBlockSeqNo` advances past the
  covering bundle boundary): 120 min ceiling, polled every 30 s.
- Enrich + Circuit-4 SHPLONK proof: ~5 min warm PK cache, ~20 min cold.

Once coverage is observed, `read_full_state` +
`BridgeState::from_contract` produce the enricher's input in one RPC
round-trip, and enrich+prove run single-shot (no retry). Timeouts map
to exit 11.

---

## Wallet setup and bridge config

To run the CLI you need two independent things:

1. **A funded Sepolia wallet you own.** The CLI signs `withdrawByProof`
   with the EVM key you provide. Every operator brings their own — the
   CLI does not ship a shared burner and does not read a
   `RELAYER_PRIVATE_KEY` from any tracked file.
2. **A `bridge_config` file** that points the CLI at a deployed
   `AckiNackiBridge` — either the shared shellnet reference deploy or
   your own (see "Two ways to fill in `BRIDGE_ADDRESS`" below).

Per-deploy addresses (`BRIDGE_ADDRESS`, the four aggregator verifiers,
`MockBlockHeaderOracle`, `BRIDGE_BOOTSTRAP_SEQNO`, genesis
`prev_max_level_layer_hash`) are **not** listed in this runbook by
value — they rotate on every redeploy. Read them from your
`bridge_config` (see below), or from `L{1,2}_config/env` if you deployed
your own bridge with `scripts/deploy_bridge_bundle.sh`.

### Create and fund your wallet

Follow the wallet-bootstrap procedure documented once in the verifyBlock
runbook — do not duplicate it here:

- [`live_relayer_bridge_verifyBlock_runbook.md` — §1 Create a fresh burner wallet](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md#1-create-a-fresh-burner-wallet)
- [`live_relayer_bridge_verifyBlock_runbook.md` — §2 Fund it with Sepolia ETH](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md#2-fund-it-with-sepolia-eth)

Short version: `cast wallet new`, keep the private key in a file
**outside** the repo, and top it up from either
[pk910 PoW faucet](https://sepolia-faucet.pk910.de/) (mine in-browser,
5–15 min per top-up) or the
[Google Cloud Web3 faucet](https://cloud.google.com/application/web3/faucet/ethereum/sepolia)
(0.05 ETH/day, no PoW). The CLI never pays for a deploy, so ~0.05 ETH
covers many withdrawals; refill from either faucet when the balance
falls below ~0.02 ETH.

Never reuse a wallet that holds real funds. Never commit the private
key. Export it in your shell before invoking the CLI:

```bash
export RELAYER_PRIVATE_KEY=0x…              # your own key, from local storage
```

The CLI reads `RELAYER_PRIVATE_KEY` via clap `env` attr. It is **never**
written into `bridge_config` and never persisted by the CLI (see
[File & state reference](#file--state-reference)).

### The `bridge_config` file

`bridge_config` (checked in at
[`config/bridge_config`](../config/bridge_config)) is a single per-CLI
env file that carries every parameter the CLI reads via clap `env`
attrs (see `src/args.rs`), and nothing else. From the CLI crate root
(`crates/bridge-withdraw-e2e-cli/`), source it with
`set -a && source config/bridge_config && set +a` before invoking the
binary.

`bridge_config` targets **only** the CLI. It deliberately omits
daemon-only settings (`BRIDGE_BOOTSTRAP_SEQNO`, `BRIDGE_ANCHOR_LEVEL`,
`BRIDGE_BK_SET_CONFIG`); those belong in the bundle-daemon's own env
file on whichever host runs `daemon-live`, which may be a different
machine from the CLI operator.

Fields the CLI reads:

| Var | Provenance | Meaning |
|-----|-----------|---------|
| `RPC_URL` | shipped | Sepolia RPC endpoint |
| `BRIDGE_GQL_ENDPOINT` | shipped | shellnet GraphQL endpoint |
| `BRIDGE_PARAMS_DIR` | shipped | ~17 GB SRS + per-circuit vk/pk files |
| `BRIDGE_AGGREGATOR_DIR` / `BRIDGE_VERIFIERS_DIR` | shipped | aggregator/verifier trees checked into the repo |
| `BRIDGE_PK_CACHE_DIR` | shipped (commented) | optional; CLI defaults to `$BRIDGE_PARAMS_DIR/pk_cache` |
| `BRIDGE_WITHDRAW_STATE_DIR` | shipped (commented) | optional; per-withdrawal idempotency state dir |
| `BRIDGE_ADDRESS` | deploy-dependent, **you fill in** | deployed `AckiNackiBridge` on Sepolia — see below |

`RELAYER_PRIVATE_KEY` is intentionally NOT part of `bridge_config` —
every user supplies their own via shell env, as above.

### Two ways to fill in `BRIDGE_ADDRESS`

**a) Use the pinned L2 reference deploy (typical user path).** The
shellnet team runs an `AckiNackiBridge` + verifier bundle + L2-anchored
bundle relayer on Sepolia. The current pinned `BRIDGE_ADDRESS` for that
deploy already ships in [`config/bridge_config`](../config/bridge_config),
so `set -a && source config/bridge_config && set +a` is all the CLI
needs. Because the server relayer is L2, you must invoke the CLI with
`--anchor-layer 2 --i-know-the-wait` — see
[Case 8 Step L5](#step-l5--run-the-cli-with-explicit-l2). L1 is not
served by the pinned deploy.

**b) Deploy your own bridge + run your own bundle relayer (advanced).**
Follow [`live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md)
end-to-end — wallet setup, `scripts/deploy_bridge_bundle.sh`, and
starting `daemon-live`. Choose L1 (`LEVEL=1`, faster, testing only) or
L2 (`LEVEL=2`, production shape) at deploy time. Copy the emitted
`BRIDGE_ADDRESS` into your `bridge_config` and point the CLI at your
own instance. The `BRIDGE_BOOTSTRAP_SEQNO` and `BRIDGE_ANCHOR_LEVEL`
also emitted at deploy are consumed by your `daemon-live`, not by the
CLI. All other cases in this runbook apply unchanged.

### One chain-invariant sanity check

Regardless of which deploy you point at, run once and confirm:

```bash
cast call $BRIDGE_ADDRESS 'expectedWithdrawAcc()(uint256)' --rpc-url $RPC_URL
# Must print: 11806252235961651298089590628336806290921645495320214372650577192963691649562
```

If this returns anything else, every C4 submit will revert on the
`WITHDRAW_ACC_FR` equality check. Path (a): the shared deploy is
misconfigured — report and STOP. Path (b): redeploy with the correct
`WITHDRAW_ACC_FR` exported before `deploy_bridge_bundle.sh`.

---

## Binary + env prerequisites

**Working directory:** `crates/bridge-withdraw-e2e-cli/` (the standalone
CLI crate — all commands below cd here first). The crate source lives
here; a symlink from `../an-bridge-prover/bridge-withdraw-e2e-cli` pulls
it into the halo2 sub-workspace so it can share the prover deps and the
built binary lands under `../an-bridge-prover/target/release/`.

**One-time build:**

```bash
# 1. CLI binary — built out of the halo2 sub-workspace so it shares
#    the prover deps.
cd crates/an-bridge-prover
cargo build --release -p bridge-withdraw-e2e-cli
#   -> ./target/release/bridge-withdraw-e2e-cli

# 2. SHPLONK aggregator (the CLI's ONLY subprocess).
#    Circuit-4 SNARK proving itself is in-process
#    (`InProcessCircuit4SnarkProver`); only the outer aggregate stage
#    shells out. Spawn site:
#    crates/bridge-relayer-daemon/src/aggregator.rs:422-432, binary
#    name `aggregate-proof` (constant at aggregator.rs:57).
cd ../../bridge-evm-aggregator
cargo build --release --bin aggregate-proof
#   -> ./target/release/aggregate-proof
```

> The `bridge-event-halo2-prover` binary is **not** used by the CLI.
> It is a daemon-only subprocess prover spawned by
> `SubprocessWithdrawalProver` at
> `crates/bridge-relayer-daemon/src/withdraw_prover.rs:216-226`, and
> the only caller is the daemon binary at
> `crates/bridge-relayer-daemon/src/bin/relayer.rs:1287-1292`. The
> CLI's `run_once_with_state` path
> (`crates/bridge-relayer-daemon/src/withdraw_e2e/driver.rs:324` →
> `Circuit4ShplonkPipeline`) never touches it.

**Env sanity** (run cwd = the CLI crate):

```bash
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…                   # your own Sepolia burner
set -a && source config/bridge_config && set +a

for v in \
  RPC_URL BRIDGE_ADDRESS RELAYER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT \
  BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR BRIDGE_PARAMS_DIR
do
  [ -n "${!v}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
```

**Env sources:**

- [`config/bridge_config`](../config/bridge_config) — the CLI's single
  env file. Carries `RPC_URL`, `BRIDGE_GQL_ENDPOINT`, `BRIDGE_ADDRESS`,
  `BRIDGE_PARAMS_DIR`, `BRIDGE_AGGREGATOR_DIR`, `BRIDGE_VERIFIERS_DIR`,
  and optional `BRIDGE_PK_CACHE_DIR` / `BRIDGE_WITHDRAW_STATE_DIR`.
  `BRIDGE_ADDRESS` is the single load-bearing input: the CLI reads
  `BridgeState` out of that contract, so a wrong value silently waits
  against the wrong state.
- `RELAYER_PRIVATE_KEY` — export in your shell (never in
  `bridge_config`, never committed).
- `BRIDGE_WITHDRAW_STATE_DIR` — optional. Defaults to
  `$HOME/.bridge-withdraw-state/` (see `src/orchestrator.rs::default_state_dir`).

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

## Case 1 — L1 first-time E2E from a fresh deploy (testing only)

> **L1 is testing-only.** The pinned team deploy is L2, and the
> server-side relayer only runs the L2 lane. Default users MUST use
> [Case 8](#case-8--l2-production-path-first-e2e-withdrawal). Case 1 is
> the advanced (self-deploy) fast-lane for smoke-testing your own
> bridge + relayer with the shorter L1 stride (5.7 min chain-time per
> bundle vs L2's 91 min); it exists so you can shake out the pipeline
> quickly before running the same flow against L2.

**When to use:** You're an advanced user, you just deployed your own
`AckiNackiBridge` with `LEVEL=1`, your own bundle relayer just
cold-started, and you want the shortest possible smoke run before
switching to the production L2 shape.

**Trigger conditions:** Fresh L1 `AckiNackiBridge` deployed; your
daemon cold-started with `LEVEL=1`; fresh chain head available.

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

Follow
[`live_relayer_bridge_verifyBlock_runbook.md` — Case 1](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md#case-1)
cold-start. Verify within 30 s: log line `seed_policy=Explicit(<seed>)`
and `prover_state.json` mtime advances.

### Step 4 — Preflight the CLI (dry-run)

```bash
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…                          # your own Sepolia burner

export WITHDRAW_FROM=<dapp_id>::<account_id>            # source multisig
export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json      # owner keys, 0600
export WITHDRAW_TO=0x742d35Cc6634C0532925a3b844Bc454e4438f44e
export WITHDRAW_TO_CHAIN=11155111
export WITHDRAW_AMOUNT=1.000000

./scripts/local_smoke.sh
```

**Equivalent raw invocation:**

```bash
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…
set -a && source config/bridge_config && set +a
mkdir -p ./work_dir
TS=$(date +%Y%m%d_%H%M%S)

../an-bridge-prover/target/release/bridge-withdraw-e2e-cli withdraw \
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
  --snark-dir         "./work_dir/shplonk-snark" \
  --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
  --work-dir          "./work_dir" \
  2>&1 | tee "./work_dir/withdraw_dry_${TS}.log"
```

(`--state-dir` omitted → the CLI defaults to `$HOME/.bridge-withdraw-state/`.
Override with `BRIDGE_WITHDRAW_STATE_DIR=./withdraw-state` in
`bridge_config` if you want per-checkout state.)

**Expected log signature (dry-run OK):**

```
INFO stage 1/6: preflight
INFO preflight ok multisig_ecc3=<amount> usdc_bridge=<dapp>::<acc>
INFO stage 2/6: idempotency (skipped for --dry-run)
INFO dry-run: skipping burn / capture / prove / submit
```

`--dry-run` is **preflight-only** — it validates flags, key file perms,
multisig custodian shape, USDCBridge resolution, and the ECC[3] balance,
then stops. It does **not** compose the burn message, wait for the
`WithdrawalInitiated` event, produce the Circuit-4 proof, or call
`dry_run_withdraw` on the EVM side. Extending the scope to "prove +
`dry_run_withdraw`, skip only the real submit" is on the roadmap; for
now, exit 0 here means "argument shape is sane" and nothing more.

Exit 0 → preflight green; drop `--dry-run` when you are ready to
commit money. Any non-zero → jump to the corresponding case per
[exit-code catalog](#exit-code-catalog).

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

**When to use:** Bridge already unpaused, treasury seeded, relayer
running for hours/days. Applies equally to the pinned L2 team deploy
(default user) and to a self-deployed L1 or L2 instance (advanced).

**Precheck — relayer current:**

```bash
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
HEAD=$(curl -s -X POST https://shellnet.ackinacki.org/graphql \
  -H 'content-type: application/json' \
  -d '{"query":"{ blockchain { blocks(last: 1) { edges { node { seq_no } } } } }"}' \
  | jq -r '.data.blockchain.blocks.edges[0].node.seq_no')
# Stride: L2 = 16384 (production), L1 = 1024 (testing only)
echo "lag = $((HEAD - LAST))  (L2 want < 16384; L1 want < 1024)"
```

If lag is under the applicable stride → the covering bundle will land
within one bundle window. Fire the withdrawal.

If lag exceeds the stride → [Case 3](#case-3--event-captured-but-daemon-far-behind-head).

**Followed by:**
- Default user / L2: [Case 8 Step L5](#step-l5--run-the-cli-with-explicit-l2)
  (dry-run first, then real submit).
- Advanced L1 self-deploy: [Case 1 Step 4](#step-4--preflight-the-cli-dry-run)
  → Step 5.

No redeploy, no unpause, no treasury seed.

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

- If `WALL_MIN ≥ 60`: Redeploy is warranted only on fresh testnet, and
  only under the advanced (self-deploy) path — see
  [Case 8](#case-8--fresh-l2-deploy-first-e2e-withdrawal) for the deploy
  sequence. After redeploy, fire a NEW burn (the old state-file's dedup
  tuple stays valid; use `--allow-retry` OR change the amount by 1
  micro-USDC to sidestep dedup).

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

**Symptom (CLI exit 12):** Failure in the CLI's only subprocess —
`aggregate-proof` (SHPLONK aggregation over the in-process Poseidon C4
SNARK). Failures surface via `SubprocessAggregator`
(`crates/bridge-relayer-daemon/src/aggregator.rs:438-455`):

```
ERROR aggregate-proof timed out after <duration>
   OR
ERROR aggregate-proof exited with <status>: <stderr>
   OR
ERROR failed to spawn aggregate-proof: <err>
```

In-process Circuit-4 failures surface a level up as
`Circuit4ShplonkPipeline::prove failed` (no subprocess involved).

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
  `./work_dir/witness_event_<seq>.json` (relative to the CLI cwd) is
  deterministic and reusable; do NOT delete it between attempts.
- If cold-cache slowness is the real issue (not OOM), bump the timeout:

  ```bash
  ../an-bridge-prover/target/release/bridge-withdraw-e2e-cli withdraw \
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

**Selector → error mapping:** (C4 verifier errors are chain-side and
unchanged by the CLI)

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

# Full trace against the deployed verifier (paths relative to CLI cwd)
CALLDATA=$(jq -r '.calldata_hex' "./work_dir/proof_event_<seq>.json")
PI=$(jq -c '.public_inputs' "./work_dir/proof_event_<seq>.json")
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
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…                          # your own Sepolia burner
set -a && source config/bridge_config && set +a

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

## Case 8 — L2 production path: first E2E withdrawal

**This is the production path** — the pinned team deploy in
[`config/bridge_config`](../config/bridge_config) is L2-anchored and the
server-side bundle relayer only runs the L2 lane. Every default user
lands here.

**When to use:** Any withdrawal against the pinned team deploy, or any
L2 exercise against your own self-deployed bridge.
`BRIDGE_ANCHOR_LEVEL=2`; bundle stride is `W² = 16384` seq_nos (~91 min
chain-time) vs L1's 1024 (~5.7 min).

> **Steps L0–L4 are advanced-only** (fresh L2 deploy + your own
> relayer). Default users pointing at the pinned team deploy **skip
> straight to** [Step L5](#step-l5--run-the-cli-with-explicit-l2) —
> the bridge and L2 relayer are already running on our server. See
> [`live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md)
> for the deploy prerequisites if you're on the advanced path.

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

Follow the daemon-startup steps in
[`live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md)
under the L2 (`anchor_level=2`) configuration; expected log signature:

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

`bridge_config` is a single file — no L1/L2 split on the CLI side. Point
`BRIDGE_ADDRESS` at the L2 deploy (path a: pinned team L2 deploy; path b:
your own `BRIDGE_ADDRESS` from `../an-bridge-prover/L2_config/env`) and
select the layer per-invocation with `--anchor-layer 2`.

```bash
cd crates/bridge-withdraw-e2e-cli
export RELAYER_PRIVATE_KEY=0x…                          # your own Sepolia burner
set -a && source config/bridge_config && set +a         # BRIDGE_ADDRESS must be the L2 deploy

# Same identity vars as Case 1 Step 4
export WITHDRAW_FROM=<dapp_id>::<account_id>
export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json
export WITHDRAW_TO=0x…
export WITHDRAW_TO_CHAIN=11155111
export WITHDRAW_AMOUNT=1.000000

mkdir -p ./work_dir
TS=$(date +%Y%m%d_%H%M%S)

../an-bridge-prover/target/release/bridge-withdraw-e2e-cli withdraw \
  --dry-run \
  --yes \
  --from        "$WITHDRAW_FROM" \
  --from-keys   "$WITHDRAW_FROM_KEYS" \
  --to          "$WITHDRAW_TO" \
  --to-chain    "$WITHDRAW_TO_CHAIN" \
  --amount      "$WITHDRAW_AMOUNT" \
  --gql-endpoint      "$BRIDGE_GQL_ENDPOINT" \
  --anchor-layer 2 \
  --i-know-the-wait \
  --rpc-url           "$RPC_URL" \
  --bridge-address    "$BRIDGE_ADDRESS" \
  --eth-private-key   "$RELAYER_PRIVATE_KEY" \
  --aggregator-dir    "$BRIDGE_AGGREGATOR_DIR" \
  --verifiers-dir     "$BRIDGE_VERIFIERS_DIR" \
  --params-dir        "$BRIDGE_PARAMS_DIR" \
  --snark-dir         "./work_dir/shplonk-snark" \
  --pk-cache-dir      "$BRIDGE_PARAMS_DIR/pk_cache" \
  --work-dir          "./work_dir" \
  2>&1 | tee "./work_dir/withdraw_l2_dry_${TS}.log"
```

**Expected log signature (dry-run, L2):**

```
INFO stage 1/6: preflight
INFO preflight ok multisig_ecc3=<amount> usdc_bridge=<dapp>::<acc>
INFO stage 2/6: idempotency (skipped for --dry-run)
INFO dry-run: skipping burn / capture / prove / submit
```

`--dry-run` at any anchor layer is preflight-only. The L2 enricher /
covering-bundle wait / Circuit-4 prove log lines all belong to the real
run below.

Then real submit — drop `--dry-run`:

```
INFO stage 4b/6: resurrect BridgeState from AckiNackiBridge + wait for covering bundle
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
The CLI exits 12 (ProofFailed — covering-bundle wait is stage 4b of the
prove path).

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

Key scalars:

- W (layer 1 window size): 128
- P (bundles per L1 layer): 8
- W·P (L1 stride): 1024 seq_nos (~5.7 min chain-time at 3 seq/s)
- W² (L2 stride): 16384 seq_nos (~91 min chain-time)
- Prover wall-time per bundle: ~12 min (warm PK cache)
- Circuit-4 event proof: ~5 min warm PK, ~20 min cold
- Enrich timeout: 120 min

**Consequence for demos:** L2 stress testing is NOT a "quick demo".
Plan half-day per 3-cycle run.

---

## Health checks

**CLI-lane snapshot** (cwd = `crates/bridge-withdraw-e2e-cli`):

```bash
# Latest CLI state files (one per unique (from,to,chain,amount) tuple)
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$HOME/.bridge-withdraw-state}"
ls -lt "$STATE_DIR"/*.json 2>/dev/null | head -3
# Peek at the newest
jq . "$(ls -t "$STATE_DIR"/*.json | head -1)" 2>/dev/null

# Latest captured witness + generated proof (written into --work-dir)
ls -lt ./work_dir/witness_event_*.json 2>/dev/null | head -3
ls -lt ./work_dir/proof_event_*.json   2>/dev/null | head -3

# Circuit 4 PK cache (should exist after first successful run)
ls -lh "$BRIDGE_PARAMS_DIR/pk_cache/" 2>/dev/null
```

**Sepolia snapshot:**

```bash
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block 0

cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL

cast balance "$(cast wallet address --private-key $RELAYER_PRIVATE_KEY)" --rpc-url $RPC_URL --ether
```

---

## File & state reference

**Run cwd for the CLI:** `crates/bridge-withdraw-e2e-cli/` (the
standalone CLI crate). Binaries are built out of the halo2 sub-workspace
at `../an-bridge-prover/target/release/`.

```
crates/bridge-withdraw-e2e-cli/            ← run cwd
├── config/
│   └── bridge_config                      ← single per-CLI env file (see runbook)
├── docs/
│   └── live_cli_withdraw_runbook.md       ← this doc
├── scripts/
│   ├── local_smoke.sh                     ← --dry-run wrapper
│   └── live_smoke.sh                      ← real-submit wrapper
├── src/                                   ← Rust crate source
└── work_dir/                              ← created on first run
    ├── witness_event_<seq>.json           ← enriched witness (input to Circuit 4)
    ├── proof_event_<seq>.json             ← aggregated SHPLONK calldata + PI
    ├── shplonk-snark/                     ← intermediate SHPLONK artifacts
    └── withdraw_{smoke,dry}_*.log         ← CLI stdout+stderr (via scripts)

$HOME/.bridge-withdraw-state/              ← default idempotency state dir
└── <sha256>.json                          ← one per unique (from,to,chain,amount)
                                          # override with BRIDGE_WITHDRAW_STATE_DIR

crates/an-bridge-prover/                   ← halo2 sub-workspace (shared with daemon)
├── params/                                ← BRIDGE_PARAMS_DIR (SRS + pk/vk)
│   └── pk_cache/                          ← Circuit-4 PK cache
├── target/release/
│   ├── bridge-withdraw-e2e-cli            ← this CLI (built into the sub-workspace)
│   ├── bridge-event-halo2-prover          ← daemon-only Circuit 4 subprocess (CLI does NOT use it)
│   └── relayer                            ← daemon-live (owner of L{1,2}_config/)
├── L1_config/, L2_config/                 ← daemon-only; CLI does NOT read these
└── bridge-withdraw-e2e-cli/               ← symlink → ../bridge-withdraw-e2e-cli

crates/bridge-evm-aggregator/              ← SHPLONK aggregator
└── target/release/
    └── aggregate-proof                    ← the CLI's only subprocess (spawned by SubprocessAggregator)
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
orchestrator + output.

**2026-08-27 — Helper scripts** (`fe52aac`)
`scripts/local_smoke.sh` (dry-run) and `scripts/live_smoke.sh` (real
submit) added; CHANGELOG entry documents env vars, exit codes,
idempotency semantics.

**Chain-side / prover-side incidents that still shape CLI behavior:**

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
