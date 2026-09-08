# Advanced-user Withdraw Runbook — self-deploy path

Operational guide for advanced users deploying their own
`AckiNackiBridge` + running their own bundle relayer end-to-end,
then driving withdrawals through the `ackinacki-bridge` CLI against
that self-deployed bridge. Also the reference for deeper failure-mode
diagnostics (revert selectors, keypair drift, in-process vs subprocess
failure origins) beyond the summaries in the default-user README.

> **Default users should not read this doc first.** See
> [`../README.md`](../README.md) — it covers the pinned shellnet L2
> deploy end-to-end, from wallet creation to `withdrawByProof`
> receipt, and is the shorter path for anyone not deploying their own
> bridge.

**What this doc adds over the README:**

- Full L1 vs L2 timing model (both strides), for choosing an anchor
  level when you deploy your own bridge.
- The Case 1b self-deploy sequence (Steps L0–L5): emit genesis
  anchors → `deploy_bridge_bundle.sh` → treasury seed → cold-start
  the daemon → wait for first bundle → run the CLI.
- Case 2: sequential stress-test loops against your own deploy.
- Case 3: full incident catalog with cast-decoding, revert selector
  → error mapping, and USDCBridge keypair-drift diagnostic. The
  README summarizes each of these for the default-user path; this
  doc has the underlying details.
- File-and-state reference for the deploy artifacts (not just the
  CLI-side files).

**Scope boundary.** The CLI invocation itself (all flags, expected
logs, success contract, exit codes, idempotency semantics) is
identical to the default path — see the README rather than
duplicating it here. This doc only shows the invocation shape when
the values differ (Step L5).

**L2 is the production shape; L1 is testing-only.** Circuits and the
CLI code paths are the same on both; L1 exists as a fast-lane for
smoke-testing your own bridge — 5.7 min chain-time per bundle vs
91 min for L2, so ~17 min end-to-end vs ~101 min.

> **Notation.** `seq_no` = Acki Nacki block sequence number.
> "Covering bundle" = the first bundle whose `key_seq_no ≥ event_seq_no`
> that is verified on-chain. Withdrawals can only be submitted after
> their covering bundle lands (Circuit 4 anchor-chain verifies against
> a `layer_hashes` root that `verifyBlock` has already committed).

---

## Table of Contents

- [Timing model — L1 vs L2](#timing-model--l1-vs-l2)
- [Wallet + bridge_config for a self-deploy](#wallet--bridge_config-for-a-self-deploy)
- [One chain-invariant sanity check](#one-chain-invariant-sanity-check)
- [Binary + env prerequisites](#binary--env-prerequisites)
- [Case 1b — L2 self-deploy end-to-end](#case-1b--l2-self-deploy-end-to-end)
  - [Step L0 — Emit L2 genesis anchors](#step-l0--emit-l2-genesis-anchors)
  - [Step L1 — Deploy with L2 wiring](#step-l1--deploy-with-l2-wiring)
  - [Step L2 — Treasury seed](#step-l2--treasury-seed)
  - [Step L3 — Cold-start daemon under L2](#step-l3--cold-start-daemon-under-l2)
  - [Step L4 — Wait for first L2 bundle](#step-l4--wait-for-first-l2-bundle)
  - [Step L5 — Run the CLI](#step-l5--run-the-cli)
  - [L1 fast-lane variant](#l1-fast-lane-variant)
- [Case 2 — Sequential withdrawals (stress-test loop)](#case-2--sequential-withdrawals-stress-test-loop)
- [Case 3 — Incidents & failure modes](#case-3--incidents--failure-modes)
  - [Case 3a — Capture timeout — advanced diagnostics](#case-3a--capture-timeout--advanced-diagnostics)
  - [Case 3b — Prover subprocess timeout / OOM](#case-3b--prover-subprocess-timeout--oom)
  - [Case 3c — On-chain `withdrawByProof` revert](#case-3c--on-chain-withdrawbyproof-revert)
  - [Case 3d — Multisig / USDCBridge key drift](#case-3d--multisig--usdcbridge-key-drift)
  - [Case 3e — `WithdrawTreasuryShortfall`](#case-3e--withdrawtreasuryshortfall)
- [Health checks](#health-checks)
- [File & state reference (full)](#file--state-reference-full)

---

## Timing model — L1 vs L2

Key scalars:

- `W` (layer-1 window size): 128
- `P` (bundles per L1 layer): 8
- `W·P` (L1 stride): 1024 seq_nos ≈ 5.7 min chain-time at ~3 seq/s
- `W²` (L2 stride): 16 384 seq_nos ≈ 91 min chain-time
- Bundle prover wall-time: ~12 min (warm PK cache)
- Circuit-4 event proof: ~5 min warm PK, ~20 min cold
- Enricher wait-for-coverage timeout: 120 min
- Capture (`WithdrawalInitiated` poll) timeout: 300 s

**End-to-end wall-time from `withdraw` invocation to
`withdrawByProof` receipt:**

| Mode | Bundle stride (chain-time) | Fast case | Worst case |
|------|----------------------------|-----------|------------|
| L1   | ~5.7 min                   | ~17 min   | ~30 min    |
| L2   | ~91 min                    | ~50 min   | ~101 min   |

**Consequence for demos:** L2 stress testing is NOT a "quick demo".
Plan a half-day per 3-cycle run. L1 fits a 3-cycle run in ~1 h and is
the right choice for iterating on the deploy pipeline itself.

---

## Wallet + bridge_config for a self-deploy

The CLI resolves all network config from the profile pointed to by
`$BRIDGE_CONFIG` (default: `config/bridge_config`, a symlink to
`bridge_config.shellnet`). Only `BRIDGE_ADDRESS` is deploy-dependent
— every other key (`RPC_URL`, `BRIDGE_GQL_ENDPOINT`,
`BRIDGE_PARAMS_DIR`, `BRIDGE_AGGREGATOR_DIR`, `BRIDGE_VERIFIERS_DIR`,
`BRIDGE_ANCHOR_LAYER`, `BRIDGE_I_KNOW_THE_WAIT`,
`USDC_BRIDGE_ACCOUNT_ID`) is deploy-independent.

**Three ways to point the CLI at your self-deployed bridge, cheapest first:**

1. **Shadow-env for a single shell**  — the shortest path:
   ```bash
   export BRIDGE_CONFIG=./config/bridge_config           # shellnet defaults
   export BRIDGE_ADDRESS=0xYourDeploy                    # shell env wins
   ```
2. **Edit the local profile**  — persistent across shells, one-line sed:
   ```bash
   export BRIDGE_CONFIG=./config/bridge_config.local
   sed -i.bak "s/^# *BRIDGE_ADDRESS=.*/BRIDGE_ADDRESS=0xYourDeploy/" "$BRIDGE_CONFIG"
   ```
3. **Add a named profile** — for a real ongoing environment (staging,
   private testnet, mainnet):
   ```bash
   cp config/bridge_config.shellnet config/bridge_config.staging
   $EDITOR config/bridge_config.staging                  # replace URLs + BRIDGE_ADDRESS
   export BRIDGE_CONFIG=./config/bridge_config.staging
   ```
   No rebuild required — `$BRIDGE_CONFIG` is honored at every CLI
   startup.

When you stand up your own bridge with `scripts/deploy_bridge_bundle.sh`,
the wrapper writes the new address (plus daemon-only settings like
`BRIDGE_BOOTSTRAP_SEQNO`, `BRIDGE_ANCHOR_LEVEL`) into
`../bridge-prover-libraries/L{1,2}_config/env`. Those daemon-only
settings stay there — the CLI does not read them. Only
`BRIDGE_ADDRESS` needs to migrate into your CLI-side profile.

For wallet creation + Sepolia funding follow the same steps as the
default README (`cast wallet new`, pk910 or Google Cloud faucet); the
mechanics are identical.

## One chain-invariant sanity check

Regardless of L1 vs L2, run once and confirm:

```bash
cast call $BRIDGE_ADDRESS 'expectedWithdrawAcc()(uint256)' --rpc-url $RPC_URL
# Must print: 11806252235961651298089590628336806290921645495320214372650577192963691649562
```

If this returns anything else, every C4 submit will revert on the
`WITHDRAW_ACC_FR` equality check. Redeploy with the correct
`WITHDRAW_ACC_FR` exported before `deploy_bridge_bundle.sh`.

---

## Binary + env prerequisites

**Working directory:** `crates/ackinacki-bridge/` (the standalone
CLI crate). The crate source lives there; a symlink from
`../bridge-prover-libraries/ackinacki-bridge` pulls it into the halo2
sub-workspace so it can share the prover deps and its built binary
lands at `../bridge-prover-libraries/target/release/ackinacki-bridge`.

**One-time build (pre-built binaries — faster startup than `cargo run
--release` for repeated invocations):**

```bash
# 1. CLI binary — built out of the halo2 sub-workspace so it shares
#    the prover deps.
cd crates/bridge-prover-libraries
cargo build --release -p ackinacki-bridge
#   -> ./target/release/ackinacki-bridge

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

**Step 2 is mandatory, not an optimisation.** Preflight refuses to
proceed unless `target/release/aggregate-proof` exists and answers
`--help`, and it will not accept the `cargo run --release` fallback the
runtime would otherwise take: verifying that path means paying for a
cold build inside a check whose whole point is to be instant, and "the
crate looks present" is not verification. A missing or unrunnable
aggregator therefore costs one command now instead of the burn plus up
to 91 minutes of anchor wait later.

Step 1 (the CLI itself) is still just a convenience — `cargo run
--release` works, and prebuilding only saves the ~2 s cargo startup per
invocation.

**Env sanity** (run cwd = the CLI crate):

```bash
cd crates/ackinacki-bridge
export BURNER_PRIVATE_KEY=0x…                                # your own Sepolia burner
export BRIDGE_CONFIG=./config/bridge_config                  # symlink → bridge_config.shellnet
# CLI auto-sources this; we ALSO source into the current shell so the
# sanity loop below can see the values.
set -a && source "$BRIDGE_CONFIG" && set +a

for v in \
  RPC_URL BRIDGE_ADDRESS BURNER_PRIVATE_KEY BRIDGE_GQL_ENDPOINT \
  BRIDGE_AGGREGATOR_DIR BRIDGE_VERIFIERS_DIR BRIDGE_PARAMS_DIR \
  USDC_BRIDGE_ACCOUNT_ID BRIDGE_ANCHOR_LAYER
do
  [ -n "${!v}" ] && echo "  ok  $v" || echo "  FAIL $v"
done
```

**Per-invocation caller vars** (used by the smoke wrappers, same as
the default-user path):

- `WITHDRAW_FROM` = `<dapp_id>::<account_id>` of the source AN multisig
- `WITHDRAW_FROM_KEYS` = path to that multisig owner's `keys.json` (mode `0400`)
- `WITHDRAW_TO` = EVM recipient (`0x…` or `eip155:<id>:0x…`)
- `WITHDRAW_TO_CHAIN` = numeric EIP-155 chain id (`11155111` for Sepolia)
- `WITHDRAW_AMOUNT` = decimal USDC (e.g. `1.000000`), ≤ 6 fractional digits

`scripts/deploy_msig_and_mint.sh` emits the first two as eval-able
`export …` lines — see README Step 2.

---

## Case 1b — L2 self-deploy end-to-end

**When to use:** You want to exercise the full ecosystem from
scratch — deploy your own `AckiNackiBridge` + verifier bundle on
Sepolia and run your own bundle relayer. This is the production
shape (L2). For the L1 fast-lane variant see the note at the end of
this section.

### Step L0 — Emit L2 genesis anchors

```bash
cd crates/bridge-prover-libraries/bridge-prover-lib
cargo run --release --bin compute_bridge_anchors -- \
  --level 2 \
  --at-head \
  --gql-endpoint https://shellnet.ackinacki.org/graphql
# Emits GENESIS_ANCHOR_LEVEL=2, GENESIS_LAST_SEEN_BLOCK_SEQ_NO=<W²-aligned>, etc.
```

### Step L1 — Deploy with L2 wiring

```bash
cd crates/bridge-prover-libraries
set -a && source shellnet.common && set +a
PRIVATE_KEY=$BURNER_PRIVATE_KEY LEVEL=2 ./scripts/deploy_bridge_bundle.sh
```

**Post-deploy sanity — verify W²-alignment:**

```bash
export BRIDGE=<new_address>
LAST=$(cast call $BRIDGE 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
python3 -c "print('L2-aligned:', $LAST % 16384 == 0, 'last_seen:', $LAST)"
# If False → redeploy (L1 seed consumed by mistake)
```

Then either **(a) point BRIDGE_CONFIG at your local profile** and paste
the address into it — cleanest for repeated self-deploy work:

```bash
export BRIDGE_CONFIG=../ackinacki-bridge/config/bridge_config.local
sed -i.bak "s/^# *BRIDGE_ADDRESS=.*/BRIDGE_ADDRESS=$BRIDGE/" "$BRIDGE_CONFIG"
```

**or (b) shadow the shellnet-profile default for a single invocation**:

```bash
export BRIDGE_ADDRESS=$BRIDGE   # shell env wins over profile-file value
```


### Prover artifacts for a self-deploy

The withdrawal proves and aggregates on this machine, so
`../bridge-prover-libraries/params/` must hold the Hermez ceremony before
a real run. The provisioning commands live in one place — **README
Step 0** — and are not repeated here; follow them, then come back.

> **Do not point two different builds at one `params/` during a
> rollout.** Nothing locks that directory. Two key generations running
> against it interleave, and the interleaving is not always detectable:
> if one process writes its keys and another overwrites them before the
> first records its manifest, the manifest ends up describing the second
> process's files under the first process's revision number. Every
> integrity check then passes on a cache holding the wrong keys.
>
> Give the new build its own `--params-dir` until every consumer — CLI,
> bundle relayer, verifier daemon — has been upgraded. Merging back
> afterwards is a copy, not a race.

### Running your own verifier: `--allow-verifier-drift`

The CLI pins the SHPLONK verifier it was built against and refuses a
`BridgeWithdrawalAggregatorVerifier.bin` that does not match it byte for
byte. That is right for the pinned shellnet deploy: a mismatched
verifier means `aggregate-proof` produces calldata your bridge will
reject, and without the check you find out in stage 5 — after the burn.

If you ran your own `deploy_bridge_bundle.sh`, your verifier is
legitimately different, and `--allow-verifier-drift` is how you say so.
It suppresses **only** the byte-comparison against the pinned artifact.
It does not weaken any other check, and it does not make a wrong
verifier work.

Before you pass it, three things must be true, and only you can
establish them:

1. `BRIDGE_VERIFIERS_DIR` points at the `verifiers/` directory produced
   by *your* deploy run — not at the repo's pinned
   `contracts/ethereum/verifiers/`.
2. The `.bin` in that directory is the one your bridge actually has on
   chain. Verify it, do not assume it:

   ```bash
   ADAPTER=$(cast call "$BRIDGE_ADDRESS" 'bridgeWithdrawalVerifier()(address)' --rpc-url "$RPC_URL")
   WRAPPER=$(cast call "$ADAPTER" 'shplonkVerifier()(address)'  --rpc-url "$RPC_URL")
   YUL=$(    cast call "$WRAPPER" 'yulVerifier()(address)'      --rpc-url "$RPC_URL")
   # gen_evm_verifier_shplonk emits a 32-byte CREATE prelude before the
   # runtime payload; eth_getCode returns only the payload.
   diff <(cast code "$YUL" --rpc-url "$RPC_URL") \
        <(printf '0x%s' "$(tail -c +33 "$BRIDGE_VERIFIERS_DIR/BridgeWithdrawalAggregatorVerifier.bin" | xxd -p | tr -d '\n')")
   ```

   Empty diff, or stop here. This is the same comparison
   `crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/preflight.sh`
   runs (`verify_verifier_lane`), and the CLI's own
   `check_bridge_deploy` runs it for you on every withdraw — the flag
   does **not** turn that off.
3. The proving keys in `--params-dir` were generated for that same
   circuit. A verifier from one deploy and a pk cache from another
   produce a proof that verifies locally and reverts on chain.

If you cannot satisfy (2), the flag is not the fix — re-deploy or
re-fetch your artifacts.

### Step L2 — Treasury seed

Fresh deploy → `treasuryBalance() == 0`. The C4 submit reverts
`WithdrawTreasuryShortfall(pub.amount, 0)` on any burn until the
treasury holds at least the burn amount. Seed it once, up-front,
scaled to cover every burn planned for the session (10 USDC is the
demo default).

`DeployShellnetE2EBridge.s.sol` wires the bridge against **Circle
canonical Sepolia USDC**
(`0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`, see
`contracts/ethereum/script/DeployShellnetE2EBridge.s.sol:27-32`),
which is a real `FiatToken` with a minter allowlist — third parties
cannot self-mint via `cast send`. Fund the burner from Circle's
public faucet <https://faucet.circle.com> first, then approve + deposit
via the same commands as README Step 3.

**Sanity-check what the fresh deploy wired** (defends against future
USDC rotations):

```bash
cast call $BRIDGE_ADDRESS 'usdc()(address)' --rpc-url $RPC_URL
# expect: 0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238
```

**If `deposit()` reverts `ERC20: transfer amount exceeds allowance`
despite a successful `approve`,** you are almost certainly holding
balance of the *wrong* USDC contract (e.g. the retired Pruvendo mock
`0x94a9D9…5e4C8`). Re-run the `usdc()` check above and use whatever
address that returns.

### Step L3 — Cold-start daemon under L2

Follow the daemon-startup steps in
[`../../bridge-prover-libraries/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md`](../../bridge-prover-libraries/bridge-relayer-daemon/docs/live_relayer_bridge_verifyBlock_runbook.md)
under the L2 (`anchor_level=2`) configuration; expected log signature:

```
INFO daemon-live: anchor_mode=L2, bundle_stride=16384
INFO daemon-live: on-chain last_seen=<seed>, stride-aligned=OK
INFO daemon-live: seed_policy=Explicit(<seed>), anchor_level=2
```

**Startup drift refusals — STOP and fix:** these belong to the daemon
(the CLI never reads the daemon's state file). See the daemon runbook
for the up-to-date list; typical L2 signatures include stride
misalignment (`last_seen % 16384 != 0`) and anchor-level drift
between on-disk state and the config file.

### Step L4 — Wait for first L2 bundle

```bash
watch -n 60 'cast call $BRIDGE storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC_URL'
# Wait until value bumps by exactly 16384
```

Takes ~91 min chain + ~10 min prover ≈ 101 min worst-case, ~50 min
typical.

### Step L5 — Run the CLI

Invocation shape is identical to the default-user path (README
Step 4/5) — same `cargo run` command, same set of flags, same
success contract. The only per-deploy differences are the values you
pass:

- `--bridge-address` — your `$BRIDGE_ADDRESS` from Step L1 (already
  in `config/bridge_config` if you ran the `sed` step above)
- `--anchor-layer 2 --i-know-the-wait` — required on L2 (the CLI
  refuses `auto` on L2 because the operator must acknowledge the
  wait budget)

The same 5-step Quick Start applies: wallet + config, deploy msig +
mint (`scripts/deploy_msig_and_mint.sh` works against your own
deploy too — no changes needed), treasury check (should be covered
from Step L2), dry-run, real submit. See README Step 4/5 for the
full `cargo run` invocation and expected log markers.

**Ground-truth log line to grep:** `layer_idx=1` confirms
L2-anchoring in the enricher output. Anything else means L1 fallback
— investigate before submitting.

**If the enricher times out (120 min):** your daemon never landed
the covering L2 bundle. Bundle-lane issue — check your `daemon-live`
logs. CLI exits 12 (ProofFailed); see [Case 3a](#case-3a--capture-timeout--advanced-diagnostics).

### L1 fast-lane variant

Same steps as above with three deltas:

- Step L0: pass `--level 1` instead of `--level 2`.
- Step L1: run `LEVEL=1 ./scripts/deploy_bridge_bundle.sh` and
  verify `last_seen % 1024 == 0` instead of `% 16384`.
- Step L5: drop `--anchor-layer 2 --i-know-the-wait`; leave
  `--anchor-layer auto` (or pass `--anchor-layer 1` explicitly).
  L1 has no acknowledged-wait requirement — stride is ~5.7 min
  chain-time, not ~91.

Log ground-truth: `layer_idx=0` on the L1 fast-lane (0-indexed
layer → L1).

---

## Case 2 — Sequential withdrawals (stress-test loop)

**When to use:** After a first successful withdrawal (default or
self-deploy), drive 2–3 more burns through the same rails to shake
out cycle-N regressions.

**Session budget:** ~101 min/bundle + ~5 min per CLI run ≈ ~2 h
between successful L2 withdrawals; 3 cycles fit in ~6 h. L1 cuts
this to ~30 min/cycle, 3 cycles in ~2 h.

**Per cycle (N = 2, 3, …):**

```bash
# 1. Confirm previous WithdrawalExecuted landed
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' \
  --from-block -1000 | tail -5

# 2. Confirm treasury still funded (seed once at Step L2 for
#    self-deploy; the pinned deploy is shared — check per cycle
#    because prior operators may have drained it).
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL

# 3. Vary the amount by 1 micro-USDC so the dedup key differs from
#    cycle N-1 (otherwise the CLI refuses exit 3; --allow-retry
#    works too but is blunter and does not exercise the reserve path).
export WITHDRAW_AMOUNT=1.00000$N

# 4. Run the CLI (README Step 5 invocation), wait for its bundle budget.
```

**Watch between cycles:**

- Fresh Circuit-2 bundle every ~91 min (daemon log; ~5.7 min on L1).
- `LayerAnchorAppended` fires **twice per bundle** (L1 + L2 anchors)
  in L2 mode. Missing L2 emissions → lost `verifyBlock` on bundle
  lane; pause and inspect.
- `storedLastSeenBlockSeqNo` should be a 16384-multiple between
  cycles on L2 (1024-multiple on L1).
- CLI state dir grows one file per cycle; each stays `Confirmed`.

**Cycle N ≥ 2 revert is almost certainly a
[Case 3c](#case-3c--on-chain-withdrawbyproof-revert) mode**, not
L2-specific. Start with the Case 3c catalog.

---

## Case 3 — Incidents & failure modes

The sub-cases below cover recoverable and non-recoverable failure
paths encountered in production. Each maps 1:1 to an exit-code row
in the README's exit-code table — start with the README summary for
the recovery gist; jump here for the diagnostic commands.

### Case 3a — Capture timeout — advanced diagnostics

**Scenario:** CLI exited 11. Either the event was never observed
(poll expired) or the enricher blocked waiting for the covering
bundle. Distinguish via:

```bash
# `--work-dir` is required plumbing with no default, so substitute the
# path this run was given. `./work_dir` is only what the smoke scripts
# happen to pass; if you passed `--work-dir` on the command line rather
# than exporting it, use that.
WORK_DIR="${BRIDGE_WORK_DIR:-./work_dir}"

LOG=$(ls -t "$WORK_DIR"/withdraw_*_*.log | head -1)

# The capture stage logs exactly one line when it succeeds. Its absence
# is the whole diagnosis.
grep -n 'captured WithdrawalInitiated event' "$LOG"

# Present  → the event WAS captured, so the enricher is what blocked.
#            The line carries `block_seq_no=` — that is the E below.
#            Sub-case 3a-i.
# Absent   → the event was never observed. Sub-case 3a-ii.
```

The enricher's own progress, if you want to see where it stopped:

```bash
grep -nE 'partial witness:|resolved anchor: L' "$LOG" | tail -5
```

#### 3a-i — Enricher blocked on covering bundle

**Root cause:** `event_seq_no=E`, `storedLastSeenBlockSeqNo()=L`,
`E − L > W·P` (>1024 on L1, >16384 on L2). Covering bundle not yet
on-chain.

```bash
E=$(grep -o 'block_seq_no=[0-9]*' "$LOG" | head -1 | cut -d= -f2)   # the captured event's seq_no
L=$(cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]')
# L1 math:
COVER=$(( (E + 1023) / 1024 * 1024 ))
BUNDLES_TO_WAIT=$(( (COVER - L) / 1024 ))
WALL_MIN=$(( BUNDLES_TO_WAIT * 12 ))
echo "event=$E last_seen=$L covering=$COVER  wait ≈ ${WALL_MIN} min"
# For L2 substitute 1024 → 16384 and 12 → 101.
```

**Remediation:**

- `WALL_MIN < 60`: The CLI has already exited (300 s + 120 min
  enricher budget spent). Do NOT fire a fresh burn — the burn already
  broadcast. Wait for the covering bundle then re-run with
  `--allow-retry`:

  ```bash
  watch -n 720 'cast call $BRIDGE_ADDRESS storedLastSeenBlockSeqNo\(\)\(uint64\) --rpc-url $RPC_URL'
  # Once L ≥ COVER, re-run:
  scripts/live_smoke.sh --allow-retry
  # (v1 blunt override; v2 will be --resume)
  ```

  Because the AN burn is already done, the CLI replays the capture
  path (`replay_latest = true`) and picks up the same event. The
  proof is deterministic for a given `(event, on-chain contract
  state)`.

- `WALL_MIN ≥ 60`: Redeploy is warranted only on fresh testnet, and
  only under this self-deploy path — see
  [Case 1b](#case-1b--l2-self-deploy-end-to-end). After redeploy,
  fire a NEW burn. The old state file's dedup tuple stays valid, and
  `--allow-retry` will NOT re-open it — a `Confirmed` record is refused
  unconditionally, and a `Reserved` one with no hash is refused too.
  Change the amount by 1 micro-USDC to get a fresh identity.

#### 3a-ii — Event never observed

**Root cause:** The burn AN tx never produced a
`WithdrawalInitiated` ExtOut — most likely the USDCBridge rejected
the call (invalid dst chain, insufficient allowance, bounce came
back).

```bash
STATE_FILE="$STATE_DIR/<sha256>.json"
AN_TX=$(jq -r '.an_tx_hash' "$STATE_FILE")
# Query the message tree via GQL; look for exit_code or aborted.
```

**Remediation:** An aborted burn exits 10, not 11. What to do next
depends on one field, and the two cases are not interchangeable.

Read it first:

```bash
jq -r '.status, .an_tx_hash' "$STATE_DIR/<sha256>.json"
```

**`an_tx_hash` is set** (`status` is `burned` or later) — the burn was
broadcast and its hash is known. Fix the underlying issue (drift →
[Case 3d](#case-3d--multisig--usdcbridge-key-drift)) and re-run with
`--allow-retry`: the run reuses the recorded hash, skips the burn
entirely, and resumes at capture.

**`an_tx_hash` is `null`** (`status: "reserved"`) — ambiguous. The SDK
either never sent, or sent and failed before returning a hash; the
record cannot tell you which, because the hash is written only after the
send returns.

`--allow-retry` neither resumes nor overrides this: the run refuses with
exit 3. (It used to compose and broadcast a **second** burn here. That is
fixed, and the refusal is now the documented behaviour rather than a
hazard to warn about.) Reconcile on-chain before doing anything:

```bash
# List recent transactions on the multisig and look for a
# sendTransaction to USDCBridge around the time of `reserved_at`.
tvm-cli -j account "$WITHDRAW_FROM"          # ECC[3] balance: did it drop?
# Then query the message tree via GQL for that window.
```

- Balance unchanged and no matching transaction → nothing was sent.
  See **"What `re-run` means once a record exists"** below: a record is
  on disk, and `--allow-retry` alone will refuse it with exit 3.
- A matching transaction exists → **check whether it succeeded before
  concluding anything.** A transaction that reached the chain and
  aborted is not a burn; the CLI classifies exactly this case
  (`aborted`, or `compute.exit_code != 0`) as a rejected call. Marking
  such a run `burned` would strand the withdrawal forever, because the
  resume path would then wait for a `WithdrawalInitiated` event that
  will never appear.

  ```bash
  tvm-cli -j query-raw transactions --filter "{\"id\":{\"eq\":\"<hash>\"}}" \
    --result 'id aborted compute{exit_code} out_messages{id dst}'
  ```

  - `aborted: true` or `exit_code != 0` → the multisig rejected the call
    and the USDC stayed put. Nothing landed — fix the cause, then follow
    the "nothing was broadcast" path below; `--allow-retry` on its own
    refuses, because the record cannot distinguish this from a burn whose
    hash was never written.
  - `aborted: false`, `exit_code: 0` → the multisig *sent* an internal
    message. That is not yet a burn: USDCBridge can abort it in turn,
    and the CLI sends with `bounce: true` precisely so the USDC comes
    back when it does. **Keep walking the tree** — this is the same
    chain `capture_targeted_withdrawal_event` follows, so it is also
    exactly what a resumed run will need to find:

    Use the same `blockchain` API and the same **two-step** shape the
    CLI uses. This is not a stylistic preference: shellnet returns
    `null` for the nested `dst_transaction { out_messages }` even when
    it populates `transaction(hash:) { out_messages }` perfectly, which
    is exactly why `query_msg_dst_tx_out_messages` splits the walk in
    two (`bridge-gql-fetcher/src/gql_client.rs:796`). Asking for the
    nested field returns nothing and looks like "no event was emitted"
    — the single most expensive way to misread this page.

    ```bash
    GQL=https://shellnet.ackinacki.org/graphql   # = $BRIDGE_GQL_ENDPOINT
    q() { curl -sS "$GQL" -H 'content-type: application/json' \
            --data "$(jq -nc --arg q "$1" '{query:$q}')"; }

    # 1. multisig tx → its outbound message to USDCBridge.
    q '{ blockchain { transaction(hash: "<hash>") {
           aborted compute { exit_code } out_messages { id dst } } } }'

    # 2. that message → the id of the transaction that consumed it.
    #    Ask ONLY for the id here; the nested out_messages is null.
    q '{ blockchain { message(hash: "<out_msg_id>") {
           dst_transaction { id } } } }'

    # 3. re-query that transaction directly — this is the step the
    #    nested form silently skips.
    q '{ blockchain { transaction(hash: "<dst_tx_id>") {
           aborted compute { exit_code } out_messages { id dst } } } }'
    ```

    A `null` `dst_transaction` in step 2 means the receiving contract
    has not run **yet**, not that it rejected the call — the CLI polls
    this same shape until it turns non-null. Wait and repeat.

    - destination transaction `aborted: true` or `exit_code != 0` → the
      bridge rejected it and the funds bounced back. **Not** a burn. Do
      not set `status: burned`; fix the cause, then follow the "nothing
      was broadcast" path below.
    - destination transaction clean **and** carrying an outbound ExtOut
      to `:…026a` → that ExtOut is the `WithdrawalInitiated` event. The
      burn landed. Write the multisig tx hash into `an_tx_hash`, set
      `status` to `burned`, and re-run with `--allow-retry` so the run
      resumes at capture instead of re-burning.

  The ExtOut is the only unambiguous evidence. Stopping one hop earlier
  — at "the multisig transaction succeeded" — is what would let you mark
  a bounced call as `burned`, after which the resumed run waits forever
  for an event that was never emitted.

**What "re-run" means once a record exists.** `--allow-retry` resumes a
withdrawal; it does **not** clear one. A record that is `Reserved` with
no `an_tx_hash` — which is what an exit 10 leaves — refuses with exit 3
under `--allow-retry` too, and that is deliberate: the hash is written
only after the send returns, so the record cannot say whether a burn is
on the wire, and re-running would broadcast a second one.

So the two outcomes below lead to different actions, and neither of them
is a bare `--allow-retry`:

- **A burn landed** → write its multisig tx hash into `an_tx_hash`, set
  `status` to `"burned"`, then re-run with `--allow-retry`. The run
  resumes at capture and never re-burns.
- **Nothing was broadcast** → the record may have to go, and the exit-3
  refusal tells you whether that is safe. Re-run the identical command
  and read the liveness line it prints. It says one of **three** things,
  and only one of them permits a deletion:

  | The refusal says | What it means | What you do |
  |---|---|---|
  | "Another process on this host is executing this withdrawal **RIGHT NOW** (it holds the withdrawal lock)" | A live run owns this withdrawal and may be inside `burn::send`. | **Wait** for it and read its outcome. Do not touch the record. |
  | "**No other process** on this host holds this withdrawal, so the record was left by a run that has already exited" | Nobody is mid-send. This is *not* the same as "nothing was broadcast" — a run can exit between the send returning and the hash being written. | Delete the record **only** if the reconciliation above also found no `initiateWithdrawal`. |
  | "Whether another process holds this withdrawal **could not be determined** here (`flock` is unavailable — a network mount, typically)" | The question was never answered. There is no evidence either way. | **Do not delete.** See below. |

  The third line is the one that catches people, because reading the
  procedure by elimination — "it is not the first case, and my
  reconciliation is clean" — lands on a deletion that the message never
  authorised.

**When the liveness verdict is "could not be determined".** On a state
directory that does not support `flock` — NFS without a lock daemon, some
container overlay mounts — every run gets this verdict, including the one
that may be mid-send. The lock is not protecting anything there, so the
CLI cannot tell you whether another run holds the withdrawal, and it says
so rather than guessing.

The on-chain reconciliation is then your *only* evidence, and it is not
sufficient on its own: it can only tell you what has already landed, and
a burn that is in flight right now has landed nowhere yet. So:

1. Move the state directory to local disk
   (`BRIDGE_WITHDRAW_STATE_DIR=$HOME/.bridge-withdraw-state` on a real
   filesystem) and re-run. The verdict becomes answerable, and you are
   back in one of the first two rows. This is the fix, not a workaround —
   the CLI's whole duplicate-burn defence is a `flock` on that directory.
2. If you cannot move it, establish by other means that no run is
   executing this withdrawal — `ps` on every host that shares the mount,
   not just this one — and only then apply the second row's rule.

Deleting the record belongs to the second row and only to the second row.
It is the only local trace that a burn may have been authorised, so it
goes **after** both conditions have been met — the on-chain
reconciliation found no `initiateWithdrawal`, and the refusal said no
other run is executing this withdrawal — and never before. "Could not be
determined" is not that sentence. In the "burn landed" case the record is
edited, not deleted. Deleting it while another run is mid-send is the
second burn that every refusal on this page exists to prevent.

---

### Case 3b — Prover subprocess timeout / OOM

**Symptom (CLI exit 12):** Failure in the CLI's only subprocess —
`aggregate-proof` (SHPLONK aggregation over the in-process Poseidon
C4 SNARK). Failures surface via `SubprocessAggregator`
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

**Trigger conditions:** Out of disk, OOM, RAM swap-thrash.

A missing, truncated or non-Hermez ceremony no longer reaches this stage:
since the stage-1 prover-artifact checks landed, it is refused before the
burn with the provisioning commands (README Step 0). Seeing "params
missing" as the cause of a post-burn `prove failed` means you are on an
older build.

**Checks:**

```bash
du -sh ../bridge-prover-libraries/params/   # ~3 GB withdraw-only; ~17 GB if this host also runs the bundle relayer
# Free space for a cold run, on the ONE filesystem that holds both:
#   3 GiB  event_pk.bin + vk + config (~2.65 GB measured, rounded up)
#   1 GiB  outer SHPLONK aggregator PK (~800 MB measured, rounded up;
#          pk_cache lives inside params/ in the shipped profile)
#   -----
#   4 GiB = ~4.3 GB as df reports it — preflight sums these constants
#          and refuses below the total
df -h ../bridge-prover-libraries/params/
# RAM headroom: C4 K=19 needs ~40 GB peak — this, not disk, is what sizes the host
```

**Remediation:**

- Free resources; re-run with the same tuple — `--allow-retry` if the
  first attempt left a `Reserved` state file. The witness under
  `<--work-dir>/event_<seq>_witness.json` — `$BRIDGE_WORK_DIR` if you
  exported it — is deterministic and reusable; do NOT delete it between
  attempts.
- If cold-cache slowness is the real issue (not OOM), bump the
  timeout:

  ```bash
  # Same 5 intent flags as README Step 5 — everything else comes from
  # $BRIDGE_CONFIG. Only new arg is the timeout override.
  cargo run --release -p ackinacki-bridge \
    --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
    withdraw \
      --from       "$WITHDRAW_FROM" \
      --from-keys  "$WITHDRAW_FROM_KEYS" \
      --to         "$WITHDRAW_TO" \
      --to-chain   "$WITHDRAW_TO_CHAIN" \
      --amount     "$WITHDRAW_AMOUNT" \
      --prover-timeout-s 3600
  ```

- If the log shows swap thrash, the OOM is real — don't just extend
  timeout. Move to a bigger host or shrink another workload.
- If first-bundle `.pk` write ENOSPC-truncated the file, delete the
  truncated `pk_cache` entry and re-run. Verify ≥20 GB free in
  `params_dir` before doing so.

---

### Case 3c — On-chain `withdrawByProof` revert

**Symptom (CLI exit 13):** Dry-run or real submit fails with a
Sepolia revert. State file records `Failed` with `stage=submit` and
(when available) the decoded revert reason.

**Selector → error mapping:** (C4 verifier errors are chain-side and
unchanged by the CLI)

| Selector | Error | Root cause |
|----------|-------|-----------|
| implicit | `AttestationProofRejected()` | C4 proof public inputs mismatch. Usually `acc_fr` drift OR `layer_hashes[1]` not yet verified (covering bundle not on-chain yet). |
| implicit | `WithdrawalAlreadyExecuted(msg_id)` | Same `msg_id` reused. Fire fresh burn. |
| implicit | `AnchorNotFound(key_seq_no)` | Covering bundle's `layer_hashes[1]` not on-chain. → [Case 3a](#case-3a--capture-timeout--advanced-diagnostics). |
| `0xbb651fce` | `WithdrawTreasuryShortfall(uint256,uint256)` | `pub.amount > treasuryBalance`. → [Case 3e](#case-3e--withdrawtreasuryshortfall). |

**Decode a revert:**

```bash
# CLI logs the selector + decoded params where possible; if not:
cast 4byte <selector>
```

**Tracing it yourself.** Three things about the proof file trip people
up, so all three are spelled out here:

1. **It only exists if you asked for it.** By default the proof lives in
   memory and is dropped; pass `--prover-out-dir DIR` and the run writes
   `DIR/proof_event_NNNNNN.json` (`NNNNNN` = the zero-padded anchor
   seq_no). A re-run without it re-proves, deterministically.
2. **Its keys are `proof_hex` and `public_instances_hex`** — an array of
   ten 32-byte hex strings, not a `calldata_hex` / `public_inputs` pair.
3. **Those ten are little-endian Fr**, and `uint256` on the wire is
   big-endian, so each one must be byte-reversed before `cast` sees it.

`pub` is a struct — `WithdrawalPublicInputs` in `AckiNackiBridge.sol`,
ten `uint256` in the order `(tokenId, amount, recipientHi, recipientLo,
dstChainId, senderAccFr, dappFr, accFr, nullifier, finalRoot)` — so the
signature is a parenthesised tuple, not `uint256[13]`:

```bash
P="$PROVER_OUT_DIR/proof_event_$(printf '%06d' "$SEQ").json"

PROOF=0x$(jq -r '.proof_hex' "$P" | sed 's/^0[xX]//')
# ltrimstr + scan/reverse turns each LE Fr into the BE uint256 the ABI
# expects; join wraps the ten into the tuple literal cast wants.
PI=$(jq -r '
  def be: sub("^0[xX]";"") | [scan("..")] | reverse | add;
  "(" + ([.public_instances_hex[] | "0x" + be] | join(",")) + ")"
' "$P")

cast call "$BRIDGE_ADDRESS" \
  'withdrawByProof(bytes,(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256))' \
  "$PROOF" "$PI" \
  --rpc-url "$RPC_URL" --trace
```

**Remediation:** Fix the on-chain condition; re-run the SAME CLI
invocation with `--allow-retry`. The proof is deterministic for a
given `(event, on-chain contract state)` — if the chain state changed
(treasury seeded, covering bundle landed), the proof regenerates
against the new state.

**Do not delete `work_dir/event_*_witness.json`** between attempts;
regeneration is expensive.

---

### Case 3d — Multisig / USDCBridge key drift

**Two flavors, distinguishable by exit code:**

#### 3d-i — Preflight refusal (exit 2)

CLI never broadcast anything. Common causes with the human message
the CLI prints:

- `arg-invalid: --from-keys`: file mode is not `0400` → `chmod 400 <path>`
- `arg-invalid: --from`: not `dapp_id::account_id` shape, or dapp_id
  is wrong workchain
- `preflight: multisig at --from is not deployed / not single-custodian`
- `preflight: owner pubkey from --from-keys does not match multisig getOwnerKey`
- `preflight: multisig ECC[3] balance = X, need Y` (insufficient USDC)
- `preflight: USDCBridge account_id does not resolve via GQL`

**Remediation:** Fix the specific issue. Preflight is side-effect
free — no state file was written, no burn attempted.

#### 3d-ii — Burn broadcast, outcome unknown (exit 10)

`sendTransaction` broadcast but the CLI could not observe the
resulting message on GQL within its budget. Typical root cause:
local `USDCBridge.shellnet.keys.json` public key ≠ on-chain
`getOwnerPubkey`, so the USDCBridge internally rejected the
`initiateWithdrawal` call (TVM exit_code=209 signature error).

**Diagnostic:**

```bash
cd ../bridge-prover-libraries
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

**Do not delete the state file.** Exit 10 means the burn reached the
wire, and that record is the only local trace of it — deleting it is how
the same withdrawal gets burned a second time. Follow
[Case 3a](#case-3a--capture-timeout--advanced-diagnostics) instead: read `.status` and
`.an_tx_hash`, reconcile on chain, and act on what actually landed —
`--allow-retry` resumes only a record that already carries a hash. If the
reconciliation shows a burn, write its hash in and set `status` to
`"burned"` first; if it shows none, see **"What `re-run` means once a
record exists"** in Case 3a.

(The old text here said "prune the `Failed` state file". No `Failed`
record can exist at exit 10 in the first place — the only production
writer of `Failed` is the `withdrawByProof` revert path, which is
exit 13.)

Note that `scripts/deploy_msig_and_mint.sh` validates the key against
`getOwnerPubkey` before minting, so if you always deploy via that
wrapper you should not hit this path.

---

### Case 3e — `WithdrawTreasuryShortfall`

**Symptom (CLI exit 13):** Dry-run or submit reverts with selector
`0xbb651fce`: `WithdrawTreasuryShortfall(<pub.amount>, <treasuryBalance>)`.

**Trigger:** Fresh deploy (treasury=0) OR prior deposit < current
burn amount.

**Seed the treasury:** follow [Step L2](#step-l2--treasury-seed).
Scale `AMOUNT` to cover every burn planned for the session (10 USDC
is the demo default).

**Remediation:** After deposit, re-run the SAME CLI invocation — no
`--allow-retry` needed. The prior state file records `Failed` with
the original `an_tx_hash` still on disk; `reserve()` recognises that
as a post-burn revert and resumes verbatim (returns the prior
record, skipping `burn::send()` in the orchestrator's stage 3 resume
branch). The AN burn is NOT re-fired — this is what prevents the
double-spend on the AN side. Capture replays the same event by
`an_tx_hash`, proof regenerates deterministically against the new
chain state (treasury balance is a component of the check), and
`withdrawByProof` submits against the topped-up treasury.

**Do not delete the state file** between attempts — deletion would
strip the `an_tx_hash` and cause the next run to fire a second
`initiateWithdrawal` (double-burn on AN side).

---

## Health checks

**CLI-lane snapshot** (cwd = `crates/ackinacki-bridge`):

```bash
# Latest CLI state files (one per unique (from,to,chain,amount) tuple)
STATE_DIR="${BRIDGE_WITHDRAW_STATE_DIR:-$HOME/.bridge-withdraw-state}"
ls -lt "$STATE_DIR"/*.json 2>/dev/null | head -3
# Peek at the newest
jq . "$(ls -t "$STATE_DIR"/*.json | head -1)" 2>/dev/null

# Latest captured witness + generated proof (written into --work-dir)
WORK_DIR="${BRIDGE_WORK_DIR:-./work_dir}"
ls -lt "$WORK_DIR"/event_*_witness.json 2>/dev/null | head -3
ls -lt "$WORK_DIR"/proof_event_*.json   2>/dev/null | head -3

# Circuit 4 PK cache (should exist after first successful run)
ls -lh ../bridge-prover-libraries/params/pk_cache/ 2>/dev/null
```

**Sepolia snapshot:**

```bash
cast logs --address $BRIDGE_ADDRESS --rpc-url $RPC_URL \
  'event WithdrawalExecuted(uint256,address,uint256,uint256)' --from-block 0

cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL

cast balance "$(cast wallet address --private-key $BURNER_PRIVATE_KEY)" \
  --rpc-url $RPC_URL --ether
```

**Bundle daemon liveness** (default users can skip — the pinned
deploy runs against our server-side daemon):

```bash
# `storedLastSeenBlockSeqNo` only jumps at bundle boundaries (~91 min
# L2, ~5.7 min L1). Cadence check via BlockVerified events:
cast logs --address $BRIDGE_ADDRESS --from-block latest-2000 \
  'BlockVerified(uint256,uint64,uint8,uint8)' --rpc-url $RPC_URL \
  | grep -c BlockVerified
# 0 events in ~2000 Sepolia blocks (~7 h) = stalled daemon; ≥1 = normal.
```

---

## File & state reference (full)

Includes deploy artifacts that only the self-deploy path uses; for
just the CLI-side files see the README's file layout section.

```
crates/ackinacki-bridge/                       ← CLI run cwd
├── config/
│   ├── bridge_config                          ← default profile symlink → bridge_config.shellnet
│   ├── bridge_config.shellnet                 ← pinned shellnet L2 reference deploy
│   ├── bridge_config.local                    ← local docker-compose devnet
│   └── bridge_config.mainnet                  ← placeholder (uncomment + fill when mainnet lives)
├── docs/
│   └── advanced_user_withdraw_runbook.md      ← this doc
├── scripts/                                   ← see README § Scripts
├── src/                                       ← Rust crate source
└── work_dir/                                  ← created on first run
    ├── event_<seq>_witness.json               ← enriched witness (input to Circuit 4)
    ├── proof_event_<seq>.json                 ← aggregated SHPLONK calldata + PI
    ├── shplonk-snark/                         ← intermediate SHPLONK artifacts
    └── withdraw_{smoke,smoke_live,dry}_*.log  ← CLI stdout+stderr

$HOME/.bridge-withdraw-state/                  ← default idempotency state dir
└── <sha256>.json                              ← one per unique (from,to,chain,amount)
                                               #   override with BRIDGE_WITHDRAW_STATE_DIR

crates/bridge-prover-libraries/                ← halo2 sub-workspace (shared with daemon)
├── params/                                    ← BRIDGE_PARAMS_DIR (SRS + pk/vk; ~3 GB withdraw-only, ~17 GB shared with the relayer)
│   └── pk_cache/                              ← Circuit-4 PK cache
├── target/release/
│   └── ackinacki-bridge                       ← this CLI (built into the sub-workspace)
├── ackinacki-bridge/                          ← symlink → ../ackinacki-bridge
├── L1_config/env  L2_config/env               ← daemon env files (BRIDGE_BOOTSTRAP_SEQNO,
│                                              #   BRIDGE_ANCHOR_LEVEL, BRIDGE_BK_SET_CONFIG) —
│                                              #   the CLI does NOT read these
├── shellnet.common                            ← shared shellnet-network env (BURNER_PRIVATE_KEY,
│                                              #   RELAYER_PRIVATE_KEY, etc.)
└── scripts/
    └── deploy_bridge_bundle.sh                ← the self-deploy wrapper (Step L1)

crates/bridge-evm-aggregator/                  ← SHPLONK aggregator source (BRIDGE_AGGREGATOR_DIR)
└── target/release/
    └── aggregate-proof                        ← the CLI's only subprocess

contracts/ethereum/verifiers/                  ← BRIDGE_VERIFIERS_DIR — precomputed inner verifier keys
```

**Never persisted anywhere the CLI writes:**

- `--from-keys` file contents
- `--eth-private-key`
- Any signed AN or ETH messages
- Any raw witness field values (the witness JSON files hold witness
  data, but those are outputs of the enricher and hold no key
  material)

**Safe to prune between demos:**

- `work_dir/` — regeneration is deterministic; ~5 min per proof with
  warm PK cache.
- `withdraw-state/<sha256>.json` files with status `Confirmed` —
  keep for audit; `Failed` — safe to prune once reconciled.
- `Reserved` with no `an_tx_hash` — **no age makes this safe.** The
  record cannot say whether a burn is on the wire, because the hash is
  written only after the send returns. Re-run the identical command and
  read the exit-3 refusal: it reports whether any process still holds
  the withdrawal, and that plus an on-chain reconciliation
  ([Case 3a](#case-3a--capture-timeout--advanced-diagnostics)) are the
  two conditions for deleting it.

**Do NOT touch between demos:**

- `params/` and `params/pk_cache/` — cold cache costs ~20 min per
  proof; warm cache is ~5 min.
