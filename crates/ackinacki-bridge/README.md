# ackinacki-bridge

End-user CLI for the Acki Nacki ↔ EVM bridge. Currently ships a single
subcommand — `withdraw` — for withdrawing USDC from an Acki Nacki
multisig to an EVM recipient. Additional subcommands (e.g. `deposit`)
are planned. This is the operator-facing counterpart to the relayer
daemon: the daemon owns the continuous bundle-proving stream
(`verifyBlock`); this CLI owns per-withdrawal composition
(multisig burn → capture → Circuit-4 SHPLONK proof → `withdrawByProof`).

This README is the **default-user runbook**: point the CLI at the
pinned shellnet L2 deploy — whose bundle relayer runs on our server —
bring your own Sepolia burner wallet, deploy a fresh AN multisig with
the bundled script, and drive one withdrawal end-to-end. You deploy
no bridge and run no daemon.

Running your own bridge + bundle relayer end-to-end (self-deploy,
stress-test loops, deeper failure-mode diagnostics) is the
**advanced** path — see
[docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md).

## What it does

Given the four user inputs (`--from`, `--from-keys`, `--to`,
`--to-chain`, `--amount`), the tool runs a six-stage in-process
pipeline:

1. **Preflight** — read-only checks: `--from-keys` file mode is `0600`,
   `--from` is an active single-custodian multisig whose owner matches
   `--from-keys`, USDCBridge resolves via GraphQL, multisig ECC[3]
   balance ≥ amount.
2. **Idempotency reserve** — SHA-256 dedup key over
   `(from, to, to_chain, amount)`; refuse a duplicate in-flight unless
   `--allow-retry` is passed.
3. **Burn** — compose the multisig `sendTransaction` payload calling
   `USDCBridge.initiateWithdrawal(dstChainId, recipient)` using
   `tvm_client` in-process (no `tvm-cli` shell-out), broadcast it,
   record the AN tx hash. Bounce defaults to `true` so USDC returns to
   the multisig on any bridge revert.
4. **Capture** — chain-walk from the multisig `an_tx_hash` through
   `USDCBridge.dst_transaction` to the `WithdrawalInitiated` ExtOut
   event, filtering by the specific broadcast tx hash so concurrent
   burns from other operators cannot be mis-selected as ours.
5. **Resurrect + wait for coverage** — read the deployed
   `AckiNackiBridge` at `--bridge-address` via
   `EthBridgeClient::read_full_state` and poll until
   `storedLastSeenBlockSeqNo` has advanced past the covering L2 bundle
   boundary (`W² = 16 384` seq_nos) for the burn's block. Once the
   covering bundle has landed on-chain (fed by the server-side bundle
   relayer), `BridgeState::from_contract` builds a byte-for-byte mirror
   — no local `prover_state.json` needed.
6. **Prove + submit** — enrich the resurrected `BridgeState`
   (single-shot, no retry), produce a Circuit-4 SHPLONK proof via the
   in-process Circuit-4 prover + `aggregate-proof` subprocess, always
   `dry_run_withdraw` first, then unless `--dry-run` is set, submit
   `withdrawByProof` and wait for the receipt.

Every stage transition is persisted to a per-withdrawal state file so
a mid-flight crash leaves a resumable trace (v1: refuse-duplicate +
`--allow-retry` blunt override; v2: `--resume` semantics).

## Prerequisites

- A bridge relayer daemon (`daemon-live`) is running against the same
  `AckiNackiBridge` deploy — for the pinned default path this is our
  server; nothing runs on your side. Its `verifyBlock` submissions
  are what advance the on-chain state the CLI polls in stage 5. No
  shared filesystem or daemon-produced JSON is needed: everything the
  CLI needs lives in contract storage.
- USDCBridge is deployed and unpaused; treasury is seeded on the EVM
  side (the pinned deploy is shared — prior withdrawals may have
  drained it; you'll top it up in Step 3 if short).
- The `--from` multisig is deployed, single-custodian, and holds ≥
  amount USDC in ECC[3]. `scripts/deploy_msig_and_mint.sh` does both
  in one shot — see Step 2 below.

## Timing model

The pinned shellnet deploy is **L2-anchored**: bundles land at
`W² = 16 384` seq_no boundaries ≈ 91 min of chain-time at ~3 seq/s.
End-to-end wall time from `withdraw` invocation to
`withdrawByProof` receipt is dominated by the wait for the next
covering L2 bundle:

| Phase                                      | Budget                       |
|--------------------------------------------|------------------------------|
| Capture (`WithdrawalInitiated` poll)       | up to 300 s                  |
| Wait for covering L2 bundle on-chain       | up to ~91 min (typical ~45)  |
| Enrich + Circuit-4 SHPLONK proof           | ~5 min warm PK, ~20 min cold |
| `withdrawByProof` submit + receipt         | ~30 s                        |
| **End-to-end (typical / worst case)**      | **~50 min / ~101 min**       |

Plan a half-day for stress-test loops of 3+ cycles. Advanced users
running their own bridge can opt into the L1 fast-lane (stride 1024
seq_nos ≈ 5.7 min chain-time) — see the advanced runbook.

## Environment / flags

Every environment variable has a corresponding `--flag` override. The
env-var form is intended for scripted use (all pinned to the shipped
shellnet values in `config/bridge_config`); the flag form for one-off
overrides.

| Flag                    | Env var                     | Purpose |
|-------------------------|-----------------------------|---------|
| `--gql-endpoint`        | `BRIDGE_GQL_ENDPOINT`       | AN GraphQL for account queries + event capture |
| `--usdc-bridge-account` | `USDC_BRIDGE_ACCOUNT_ID`    | On-chain USDCBridge acc id; default is shellnet canonical |
| `--anchor-layer`        | —                           | `auto` (default), `1`, or `2` — must match the deploy's anchoring mode |
| `--i-know-the-wait`     | —                           | Acknowledge L2's ~91 min chain-time budget when `--anchor-layer 2` |
| `--rpc-url`             | `RPC_URL`                   | EVM JSON-RPC — used both for polling coverage and submitting `withdrawByProof` |
| `--bridge-address`      | `BRIDGE_ADDRESS`            | Deployed `AckiNackiBridge` — the sole source of prover state |
| `--eth-private-key`     | `BURNER_PRIVATE_KEY`        | Signer for `withdrawByProof` (distinct from `--from-keys`) |
| `--aggregator-dir`      | `BRIDGE_AGGREGATOR_DIR`     | Circuit-4 aggregator artifacts |
| `--verifiers-dir`       | `BRIDGE_VERIFIERS_DIR`      | Precomputed inner verifier keys |
| `--params-dir`          | `BRIDGE_PARAMS_DIR`         | KZG SRS params (~17 GB) |
| `--pk-cache-dir`        | `BRIDGE_PK_CACHE_DIR`       | Warm-start pk cache (optional; defaults to `$BRIDGE_PARAMS_DIR/pk_cache`) |
| `--state-dir`           | `BRIDGE_WITHDRAW_STATE_DIR` | Per-withdrawal idempotency state dir (optional; defaults to `$HOME/.bridge-withdraw-state`) |

`BURNER_PRIVATE_KEY` is intentionally **not** shipped in
`config/bridge_config` — every operator brings their own; see Step 1.

## Quick start

Five ordered steps. All commands run from `crates/ackinacki-bridge/`;
paths in the CLI invocation are relative to that directory.

```bash
cd crates/ackinacki-bridge
set -a && source config/bridge_config && set +a          # RPC, GQL, dirs, BRIDGE_ADDRESS
```

### Step 1 — Create + fund a Sepolia burner wallet

The CLI signs `withdrawByProof` with an EVM key you provide. Never
reuse a wallet that holds real funds; never commit the private key.

```bash
# 1a. Generate a fresh wallet (keep the private key OUT of the repo).
cast wallet new
# Successfully created new keypair.
# Address:     0x1234…
# Private key: 0x…

# 1b. Fund it with Sepolia ETH from either:
#     - pk910 PoW faucet:            https://sepolia-faucet.pk910.de/
#     - Google Cloud Web3 faucet:    https://cloud.google.com/application/web3/faucet/ethereum/sepolia
#     ~0.05 ETH covers many withdrawals; refill when balance drops below ~0.02 ETH.

# 1c. Export the private key in your shell — the CLI reads it via clap `env` attr.
export BURNER_PRIVATE_KEY=0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef   # ← REPLACE. 32-byte hex, 0x-prefixed.
```

### Step 2 — Deploy a fresh AN multisig + mint 1 USDC

`scripts/deploy_msig_and_mint.sh` deploys a single-custodian
`UpdateCustodianMultisigWallet` on shellnet and seeds it with 1 USDC
on ECC[3] via `USDCBridge.mintAndSend`, then prints two eval-able env
lines on stdout. Everything else (tvm-cli output, tracer logs) goes
to stderr, so the `eval` line does not pollute the shell.

```bash
eval "$(scripts/deploy_msig_and_mint.sh)"
# → sets WITHDRAW_FROM      (e.g. 2bd287b8ddb28adec2a17863ffc2d6e1c4fc48fbd1c833c6564f7f843588aad0::2bd287b8ddb28adec2a17863ffc2d6e1c4fc48fbd1c833c6564f7f843588aad0)
#     format: <64-hex dapp_id>::<64-hex account_id> — single-custodian, both halves match
# → sets WITHDRAW_FROM_KEYS (e.g. ./work_dir/msig_withdraw_cli.keys.json — the script emits an absolute path)
#     format: path to a keys.json file, mode 0600
```

Set the remaining per-invocation vars yourself:

```bash
export WITHDRAW_TO=0x742d35Cc6634C0532925a3b844Bc9e7595f0aC49                   # ← REPLACE. Sepolia recipient EOA (20-byte hex, 0x-prefixed, EIP-55).
export WITHDRAW_TO_CHAIN=11155111                                                # Sepolia chain id.
export WITHDRAW_AMOUNT=1.000000                                                  # Decimal USDC, ≤ 6 fractional digits, no rounding.
```

### Step 3 — Confirm bridge treasury covers your amount

The C4 submit reverts `WithdrawTreasuryShortfall(amount, 0)` if
`treasuryBalance() < WITHDRAW_AMOUNT` — the on-chain USDC that pays
the recipient comes from the bridge treasury. The pinned deploy is
shared; prior withdrawals may have drained it.

```bash
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL
# Compare against your WITHDRAW_AMOUNT scaled to µUSDC (6 decimals: 1 USDC = 1_000_000).
```

If it covers, skip to Step 4. If short, top up from Circle's public
Sepolia faucet (the pinned deploy is wired to Circle canonical Sepolia
USDC at `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`; that same
contract is what Circle's faucet dispenses):

```bash
# Sanity-check what the bridge expects (defends against USDC rotations):
cast call $BRIDGE_ADDRESS 'usdc()(address)' --rpc-url $RPC_URL
# expect: 0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238

# a) Open https://faucet.circle.com, select Ethereum Sepolia, paste your burner
#    address (cast wallet address --private-key $BURNER_PRIVATE_KEY), and request
#    USDC — 10 USDC per call, throttled per address/IP. Repeat if you need more.
# b) Approve + deposit into the bridge:
export USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238
export AMOUNT=10000000    # 10.000000 USDC in µUSDC — comfortably covers a 1 USDC burn
cast send $USDC 'approve(address,uint256)' $BRIDGE_ADDRESS $AMOUNT \
  --rpc-url $RPC_URL --private-key $BURNER_PRIVATE_KEY
cast send $BRIDGE_ADDRESS 'deposit(uint256,int8,bytes32)' \
  $AMOUNT 0 0x1111111111111111111111111111111111111111111111111111111111111111 \
  --rpc-url $RPC_URL --private-key $BURNER_PRIVATE_KEY
cast call $BRIDGE_ADDRESS 'treasuryBalance()(uint256)' --rpc-url $RPC_URL
```

The dummy AN destination is safe on shellnet — no live AN-side
listener consumes the phantom `Deposit` event. **Do NOT use** on a
live bridge with an active AN-side indexer — you'll create a ghost
credit.

### Step 4 — Dry-run

Preflight-only preview: argument validation, key file perms,
single-custodian check, USDCBridge resolution, balance check, and the
idempotency-key digest all run. Nothing else — no burn, no capture,
no prove, no `dry_run_withdraw` eth_call. Useful for sanity-checking
flags and config before the real submit.

The exemplary command below is exactly what `scripts/local_smoke.sh`
runs, expanded so you can see every flag and its (real) value.
All `../…` paths are relative to `crates/ackinacki-bridge/`:

```bash
mkdir -p ./work_dir ./withdraw-state

cargo run --release -p ackinacki-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  withdraw \
    --dry-run --yes \
    --anchor-layer 2 --i-know-the-wait \
    --from            "$WITHDRAW_FROM" \
    --from-keys       "$WITHDRAW_FROM_KEYS" \
    --to              "$WITHDRAW_TO" \
    --to-chain        11155111 \
    --amount          "$WITHDRAW_AMOUNT" \
    --gql-endpoint    https://shellnet.ackinacki.org/graphql \
    --rpc-url         https://ethereum-sepolia-rpc.publicnode.com \
    --bridge-address  0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7 \
    --eth-private-key "$BURNER_PRIVATE_KEY" \
    --aggregator-dir  ../bridge-evm-aggregator \
    --verifiers-dir   ../../contracts/ethereum/verifiers \
    --params-dir      ../bridge-prover-libraries/params \
    --snark-dir       ./work_dir/shplonk-snark \
    --pk-cache-dir    ../bridge-prover-libraries/params/pk_cache \
    --work-dir        ./work_dir \
    --state-dir       ./withdraw-state
```

Equivalent, shorter form once you've confirmed the invocation shape:

```bash
scripts/local_smoke.sh          # same command; reads paths from config/bridge_config
```

**Expected dry-run log — success markers on stderr:**

```
INFO stage 1/6: preflight
INFO preflight ok multisig_ecc3=<amount> usdc_bridge=<dapp>::<acc>
INFO stage 2/6: idempotency (skipped for --dry-run)
INFO dry-run: skipping burn / capture / prove / submit

withdraw complete:
  amount:       1.000000 USDC
  AN tx:        (dry-run)
  msg id:       (dry-run)
  block:        seq=0 id=(dry-run)
  proof:        0 bytes, 0 public inputs, self_verified=false
  ETH tx:       (none) (dry-run-ok)
```

`ETH tx: (none) (dry-run-ok)` + shell exit `0` = preflight passed
and the CLI touched nothing beyond it.

### Step 5 — Real submit

Drop `--dry-run`, re-run the same invocation (or `scripts/live_smoke.sh`
for the wrapper form). Expect all six stages, then the summary block:

```
INFO stage 1/6: preflight
INFO stage 2/6: idempotency reserve
INFO stage 3/6: burn (multisig sendTransaction → USDCBridge.initiateWithdrawal)
INFO stage 4/6: capture WithdrawalInitiated event (targeted by an_tx_hash)
INFO stage 4b/6: resurrect BridgeState from AckiNackiBridge + wait for covering bundle
INFO enricher: witness ready  layer_idx=1        # 0-indexed → L2; ANY OTHER VALUE = L1 fallback bug
INFO stage 5/6: Circuit-4 SHPLONK proof (in-process C4 → aggregator subprocess)
INFO stage 6/6: submit withdrawByProof
INFO withdrawByProof paid out tx=0x…             # ← definitive on-chain payout marker

withdraw complete:
  amount:       1.000000 USDC
  AN tx:        0x…
  msg id:       0x…
  block:        seq=<N> id=0x…
  proof:        <NNNN> bytes, 10 public inputs, self_verified=true
  ETH tx:       0x… (confirmed)
```

**Success contract — three independent checks:**

1. Shell exit code: `echo $?` → `0`.
2. Stderr ends with a `withdraw complete:` block whose last line is
   `ETH tx: 0x… (confirmed)`.
3. The log contains all six stage lines and the
   `withdrawByProof paid out tx=0x…` line.

Grep verdict from the saved log (both smoke wrappers `tee` to
`./work_dir/withdraw_{smoke_live,smoke,dry}_<ts>.log`):

```bash
LOG=$(ls -t ./work_dir/withdraw_*_*.log | head -1)
grep -E 'stage [1-6]/6|withdrawByProof paid out|withdraw complete:|^error:' "$LOG"
grep 'layer_idx=' "$LOG"    # must print `layer_idx=1` on the pinned L2 deploy
grep -E '^error:|^ERROR|ProofFailed|reverted|timed out' "$LOG"
```

## Exit codes

Distinguishing "nothing broadcast" from "broadcast, unknown outcome"
is the whole point of the exit-code discipline — scripts that
pattern-match on a single non-zero would blind an operator to the
difference that matters for money. `--dry-run` can only produce
0, 2, or 3.

| Code | Meaning | Nothing broadcast? | Where to look |
|------|---------|-------------------|---------------|
| 0    | Success (or `--dry-run` returned OK) | — | — |
| 2    | Preflight refused — key perms, `--from` not an active MS, balance short, etc. | ✓ nothing | Error scenarios § "Preflight refused" |
| 3    | Duplicate in-flight refused — same dedup key already exists | ✓ nothing | § Idempotency semantics |
| 10   | AN burn broadcast, capture failed to observe outcome | ✗ AN burn WAS broadcast | § "Burn broadcast, outcome unknown" |
| 11   | Burn confirmed, `WithdrawalInitiated` capture timed out | ✗ AN burn done | § "Capture timeout" |
| 12   | Capture succeeded, Circuit-4 proof failed | ✗ AN burn done, no ETH tx | § "Prover failed" |
| 13   | Proof succeeded, `withdrawByProof` reverted / dry-run reverted | ✗ AN burn done, no ETH tx | § "On-chain submit reverted" |

Exit codes 10–13 all leave the AN burn broadcast: the USDC has left
the source multisig regardless. The question is whether the EVM side
saw the withdrawal.

## Error scenarios

Short summaries for the default-user path. For deeper diagnostics
(cast-decoding revert selectors, USDCBridge keypair drift, in-process
vs subprocess failure origins, etc.) see the
[advanced runbook](docs/advanced_user_withdraw_runbook.md#case-3--incidents--failure-modes).

### Preflight refused (exit 2)

Nothing was broadcast; no state file written. The CLI prints the
specific reason. Common causes:

- `--from-keys` file is not mode `0600` → `chmod 600 <path>`
- `--from` is not `dapp_id::account_id` shape, or points to a
  multisig with >1 custodian, or the owner pubkey from `--from-keys`
  does not match the on-chain multisig
- `multisig ECC[3] balance < requested` — deploy a new multisig with
  more via Step 2 (bump the amount inside `deploy_msig_and_mint.py`
  if you need more than the 1 USDC default)
- USDCBridge account_id does not resolve via GQL

**Remediation:** fix the specific issue, re-run. Preflight is
side-effect-free.

### Burn broadcast, outcome unknown (exit 10)

`sendTransaction` broadcast but the CLI could not observe the resulting
message on GQL within its budget. Typical root cause on the pinned
shellnet deploy: the bundled `USDCBridge.shellnet.keys.json` in
`bridge-prover-libraries/python/contracts/` has drifted from the
current on-chain owner pubkey — the deploy script would have refused
in this case, so if you see this you likely bypassed the deploy step.
Re-run `scripts/deploy_msig_and_mint.sh` (it validates the key against
`getOwnerPubkey` before minting) and start over.

### Capture timeout (exit 11)

Two sub-cases, distinguishable by grepping `capture:` in the log:

- `capture: matched dst=…` present → capture succeeded; the enricher
  is blocked waiting for the covering L2 bundle to land on-chain. The
  server-side bundle relayer landed no bundle in the CLI's 120 min
  budget. Wait for the covering bundle (monitor
  `storedLastSeenBlockSeqNo` with the health-check snippet below);
  once it advances past your event's `ceil(seq / 16384) * 16384`
  boundary, re-run with `--allow-retry` (the CLI replays capture, does
  NOT re-fire the burn).
- `capture: polling` only, no `matched` → the burn AN tx never
  produced a `WithdrawalInitiated` (USDCBridge internally rejected
  the call). Should have surfaced as exit 10, not 11; see § "Burn
  broadcast, outcome unknown".

### Prover failed (exit 12)

Circuit-4 in-process prover or the `aggregate-proof` subprocess
errored. Almost always local resource pressure (disk, RAM, corrupt
PK cache) or cold-cache slowness beyond the default timeout.

```bash
du -sh ../bridge-prover-libraries/params/    # ~17 GB expected
df   ../bridge-prover-libraries/params/      # free disk headroom
```

**Remediation:** free resources, re-run with `--allow-retry`. If the
first attempt was a cold cache and merely slow (not OOM/thrashing),
bump `--prover-timeout-s 3600` on the retry. Don't delete
`./work_dir/witness_event_<seq>.json` between attempts — it's
deterministic and reused.

### On-chain submit reverted (exit 13)

`withdrawByProof` reverted on Sepolia. The most common cause on the
pinned deploy is `WithdrawTreasuryShortfall` — the treasury drained
below your amount between Step 3's check and Step 5. Top it up as in
Step 3, re-run with `--allow-retry`; the proof regenerates
deterministically against the topped-up state and the resume path
does NOT re-fire the AN burn (this prevents double-spend on the AN
side).

Other Sepolia reverts (proof PI mismatch, anchor not found,
withdrawal already executed) are advanced-runbook territory — see the
selector → error table in
[docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md#case-3c--on-chain-withdrawbyproof-revert).

### Health check — is the server-side bundle daemon alive?

```bash
cast call $BRIDGE_ADDRESS 'storedLastSeenBlockSeqNo()(uint64)' --rpc-url $RPC_URL --json | jq -r '.[0]'
# In L2 mode this counter only jumps at 16384-block bundle boundaries
# (~91 min chain-time). Alive-signal via cadence:
cast logs --address $BRIDGE_ADDRESS --from-block latest-2000 \
  'BlockVerified(uint256,uint64,uint8,uint8)' --rpc-url $RPC_URL \
  | grep -c BlockVerified
# 0 events in ~2000 Sepolia blocks (~7 h) = stalled bundle daemon;
# ≥1 = normal.
```

## Idempotency semantics

**Dedup key.** SHA-256 of `{from}|{to}|{to_chain}|{amount}` (all
ASCII, `amount` as micro-USDC integer). Two withdrawals with
identical tuples collide; anything different (recipient, amount, or
chain) does not.

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

**`--allow-retry`.** Resume-in-place; the CLI keeps the prior record
verbatim and skips any stage that already completed:

- `Burned` / `Captured` / `Proved` → skip burn, resume from capture.
  Prior `an_tx_hash` reused, so **the burn is never broadcast twice.**
- `Failed` → clean-slate restart. No flag needed.
- `Submitted` → **refused even with `--allow-retry`.** There is a
  broadcast EVM tx whose receipt we never observed; re-broadcasting
  risks a double payout. Reconcile the `eth_tx_hash` on-chain first,
  then either wait (rerun will become `Confirmed`) or edit the state
  file to `Failed` manually.
- `Confirmed` → **refused always.** The withdrawal already paid out.
  Fire a new withdrawal with a different (amount, recipient, chain).

**Cleanup rule.** `Confirmed` files are keepable forever (small; they
are your on-chain audit trail). `Failed` files are safe to prune once
the corresponding AN/ETH tx status is reconciled. `Reserved` files
>24 h old with no `an_tx_hash` are safe to prune — the burn never
happened.

## Safety

- `--from-keys` file contents are never logged, printed, or persisted
  anywhere the CLI writes. The owner public key derived from the file
  IS written (to state files, printed on preflight) — it's an
  on-chain-observable identifier, not a secret.
- ETH signer key (`--eth-private-key`) same discipline; never
  persisted, never logged.
- Idempotency state files under `--state-dir` contain only
  chain-observable identifiers (AN tx hash, WithdrawalInitiated
  msg id, block seq no, ETH tx hash).

## Scripts

Located under `scripts/`:

| Script                     | Purpose |
|----------------------------|---------|
| `deploy_msig_and_mint.sh`  | Deploy a fresh single-custodian AN multisig + mint 1 USDC on ECC[3]. Emits eval-able `export WITHDRAW_FROM=…` / `WITHDRAW_FROM_KEYS=…` lines on stdout. See Step 2. |
| `deploy_msig_and_mint.py`  | Python driver behind the `.sh`. Override defaults via `MODE=`, `NETWORK=`, `GRAPHQL_URL=`, `WORK_DIR=`, `USDC_BRIDGE_KEY_PATH=` env vars. |
| `local_smoke.sh`           | `--dry-run` wrapper — same command as Step 4, reads paths from `config/bridge_config`. |
| `live_smoke.sh`            | Real-submit wrapper — same command as Step 5. |

Both smoke wrappers source `config/bridge_config` and expect the five
identity vars (`WITHDRAW_FROM`, `WITHDRAW_FROM_KEYS`, `WITHDRAW_TO`,
`WITHDRAW_TO_CHAIN`, `WITHDRAW_AMOUNT`) plus `BURNER_PRIVATE_KEY` to
be exported in the shell.

## File layout

```
crates/ackinacki-bridge/                       ← run cwd
├── config/
│   └── bridge_config                          ← single per-CLI env file (RPC, GQL, dirs, BRIDGE_ADDRESS)
├── docs/
│   └── advanced_user_withdraw_runbook.md      ← self-deploy + deep failure diagnostics
├── scripts/                                   ← see § Scripts
├── src/                                       ← Rust crate source
└── work_dir/                                  ← created on first run
    ├── witness_event_<seq>.json               ← enriched witness (input to Circuit 4)
    ├── proof_event_<seq>.json                 ← aggregated SHPLONK calldata + PI
    ├── shplonk-snark/                         ← intermediate SHPLONK artifacts
    └── withdraw_{smoke,smoke_live,dry}_*.log  ← CLI stdout+stderr via the smoke wrappers

$HOME/.bridge-withdraw-state/                  ← default idempotency state dir
└── <sha256>.json                              ← one per unique (from,to,chain,amount)
                                               #   override with BRIDGE_WITHDRAW_STATE_DIR

../bridge-prover-libraries/                    ← halo2 sub-workspace (shared with the daemon)
├── params/                                    ← BRIDGE_PARAMS_DIR (SRS + pk/vk, ~17 GB)
│   └── pk_cache/                              ← Circuit-4 PK cache
└── target/release/ackinacki-bridge            ← this CLI when pre-built (cargo run --release also caches here)

../bridge-evm-aggregator/                      ← BRIDGE_AGGREGATOR_DIR — SHPLONK aggregator source
└── target/release/aggregate-proof             ← the CLI's only subprocess

../../contracts/ethereum/verifiers/            ← BRIDGE_VERIFIERS_DIR — precomputed inner verifier keys
```

**Safe to prune between demos.** `work_dir/`, `proofs/` — regeneration
is deterministic; ~5 min per proof with warm PK cache.
`withdraw-state/<sha256>.json` files with status `Confirmed` — keep
for audit; `Failed` — safe to prune once reconciled; `Reserved` >24 h
old with no `an_tx_hash` — safe to prune.

**Do NOT touch between demos.** `../bridge-prover-libraries/params/`
and its `pk_cache/` — cold cache costs ~20 min per proof; warm cache
is ~5 min.

## Not this CLI's job

- Multisig deploy is `scripts/deploy_msig_and_mint.sh` (Python-based;
  mirrors the vendored driver).
- USDC treasury seeding on the EVM side is manual `cast` + Circle
  faucet (Step 3).
- Continuous bundle proving is a daemon (`daemon-live`) running
  somewhere — for the pinned default path this is our server; the CLI
  only waits for its output to land on-chain (stage 5).
- Full `--resume` semantics → v2 (v1 has refuse-duplicate + blunt
  `--allow-retry` override).
- Self-deploy of `AckiNackiBridge` + running your own bundle
  relayer → [docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md).

## Layout note

Physical crate at `crates/ackinacki-bridge/`. Symlinked into the
`bridge-prover-libraries` sub-workspace at
`crates/bridge-prover-libraries/ackinacki-bridge/` so it can depend on
the halo2-heavy prover crates while remaining excluded from the root
workspace. Building via `cargo run --release -p ackinacki-bridge
--manifest-path ../bridge-prover-libraries/Cargo.toml -- …` (as in
Step 4) drives cargo through the sub-workspace and drops the binary
at `../bridge-prover-libraries/target/release/ackinacki-bridge`.

Vendored ABIs live under `abi/`:
- `USDCBridge.abi.json` — for encoding the `initiateWithdrawal` body cell
- `UpdateCustodianMultisigWallet.abi.json` — for the outer `sendTransaction`
