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

1. **Preflight** — everything that can be checked before anything is
   broadcast, on **both** chains.

   *Acki Nacki side:* `--from-keys` file mode is exactly `0400` and its
   two halves are a real key pair, `--from` is an active single-custodian
   multisig whose owner matches `--from-keys`, USDCBridge resolves via
   GraphQL, multisig ECC[3] balance ≥ amount.

   *EVM side, and this runs on `--dry-run` too* — none of it needs a
   signing key: `eth_chainId` must equal `--to-chain`, `--bridge-address`
   must hold code, the withdrawal verifier stack must walk (adapter →
   `shplonkVerifier` → `yulVerifier`, code at every level), the bridge's
   pinned `(dappFr, accFr)` must be the pair this withdrawal will prove,
   and `treasuryBalance` must already cover the amount. **So a dry run can
   fail for EVM reasons — a wrong RPC, a wrong `--bridge-address`, a
   half-wired deploy, a drained treasury — not only AN-side ones.**

   *Real runs only*, because these need the submit-only flags: the burner
   key parses, the KZG ceremony resolves at k=20 **and** k=21, the
   Circuit-4 key cache is usable, `aggregate-proof` is prebuilt and
   answers `--help`, and the output directories are writable with room for
   what will be written. This is the one part of preflight that writes:
   it creates those directories and drops a short-lived probe file in
   each.
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
   relayer), `BridgeState::from_contract` builds a byte-for-byte mirror.
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
- **`tvm-cli` on `PATH`**, needed only by that fixture script (the CLI
  itself talks to the chain in-process and needs no external binary).
  It is platform-specific and is **not** shipped in this repo. Check
  before running Step 2:

  ```bash
  crates/ackinacki-bridge/scripts/check_fixture_prereqs.sh
  ```

  `CLI_NAME=/path/to/tvm-cli` overrides whatever is discovered on
  `PATH`.

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

The CLI auto-sources the file pointed to by `$BRIDGE_CONFIG` at
startup (default: `config/bridge_config`, a symlink to
`bridge_config.shellnet`) before clap reads any `env=` attr, so the
env-var form is the normal path — a `withdraw` invocation carries only
the five per-request intent flags (`--from` / `--from-keys` / `--to` /
`--to-chain` / `--amount`) unless you want to override a plumbing
value one-off. Precedence: **explicit `--flag` > shell env > profile
file > compiled default.**

Switch networks by pointing at a sibling profile file:
```bash
export BRIDGE_CONFIG=./config/bridge_config.local     # local devnet
export BRIDGE_CONFIG=./config/bridge_config.mainnet   # placeholder (unfilled)
```

| Flag                    | Env var / profile key       | Purpose |
|-------------------------|-----------------------------|---------|
| `--gql-endpoint`        | `BRIDGE_GQL_ENDPOINT`       | AN GraphQL for account queries + event capture |
| `--usdc-bridge-account` | `USDC_BRIDGE_ACCOUNT_ID`    | On-chain USDCBridge acc id (required in profile) |
| `--anchor-layer`        | `BRIDGE_ANCHOR_LAYER`       | `auto` (default), `1`, or `2` — must match the deploy's anchoring mode |
| `--i-know-the-wait`     | `BRIDGE_I_KNOW_THE_WAIT`    | Acknowledge L2's ~91 min chain-time budget when `--anchor-layer 2` |
| `--rpc-url`             | `RPC_URL`                   | EVM JSON-RPC — used both for polling coverage and submitting `withdrawByProof` |
| `--bridge-address`      | `BRIDGE_ADDRESS`            | Deployed `AckiNackiBridge` — the sole source of prover state |
| `--eth-private-key`     | `BURNER_PRIVATE_KEY`        | Signer for `withdrawByProof` (distinct from `--from-keys`) |
| `--aggregator-dir`      | `BRIDGE_AGGREGATOR_DIR`     | Circuit-4 aggregator artifacts |
| `--verifiers-dir`       | `BRIDGE_VERIFIERS_DIR`      | Precomputed inner verifier keys |
| `--params-dir`          | `BRIDGE_PARAMS_DIR`         | KZG ceremony + generated pk/vk. Needs `kzg_bn254_21.srs`; see Step 0 |
| `--snark-dir`           | `BRIDGE_SNARK_DIR`          | Aggregator scratch (must be absolute; smoke scripts canonicalize) |
| `--work-dir`            | `BRIDGE_WORK_DIR`           | Per-withdrawal working directory |
| `--pk-cache-dir`        | `BRIDGE_PK_CACHE_DIR`       | Warm-start pk cache (optional; defaults to `$BRIDGE_PARAMS_DIR/pk_cache`) |
| `--state-dir`           | `BRIDGE_WITHDRAW_STATE_DIR` | Per-withdrawal idempotency state dir (optional; defaults to `$HOME/.bridge-withdraw-state`) |

`BURNER_PRIVATE_KEY` is intentionally **not** shipped in any profile —
every operator brings their own; see Step 1. `NETWORK` (tvm-cli
`--url` target) is also in the profile and consumed by
`scripts/deploy_msig_and_mint.py`.

## Quick start

Six ordered steps, the first a one-off. All commands run from
`crates/ackinacki-bridge/`;
paths in the CLI invocation are relative to that directory.

```bash
cd crates/ackinacki-bridge
export BRIDGE_CONFIG=./config/bridge_config              # symlink → bridge_config.shellnet
# The CLI auto-sources this. We ALSO source it into the current shell
# so the `cast call $BRIDGE_ADDRESS …` steps below can reach it.
set -a && source "$BRIDGE_CONFIG" && set +a
```

### Two preconditions the command line does not show

The published invocation is exactly:

    ackinacki-bridge withdraw --from <dapp_id::account_id> --from-keys <path> \
                              --to <0x…> --to-chain <chain-id> --amount <usdc> \
                              [--dry-run] [--yes] [--non-interactive] [--json]

`--yes` and `--non-interactive` are not the same flag and are not
alternatives. `--yes` answers the confirmation; `--non-interactive`
promises that nothing may ever wait for an answer, and a real run given
it without `--yes` is refused with exit 2 rather than left to block —
`--non-interactive requires --yes or --dry-run`. A wrapper that means "do
not hang" wants both.

For it to resolve, two things must be true, and neither is visible in the
command itself:

1. `$BRIDGE_CONFIG` points at a profile file. Everything else the CLI
   needs — endpoints, bridge address, prover dirs — comes from there.
2. The working directory is `crates/ackinacki-bridge/`, because every path
   in every shipped profile is relative to it.

`--dry-run` needs nothing beyond those two. A **real** withdrawal also
needs `BURNER_PRIVATE_KEY` exported (Step 1): it is per-operator and is
deliberately in no profile. A real run without it refuses at stage 1,
naming every missing value at once.

### Step 0 — Provision the KZG ceremony (one-off, ~30 min)

A real withdrawal proves Circuit 4 and aggregates it on this machine, so it
needs the Hermez Perpetual Powers of Tau ceremony on disk. The directory
`../bridge-prover-libraries/params/` is gitignored — a fresh checkout has
nothing in it. `--dry-run` does not need any of this; skip to Step 1 if you
are only preflighting.

Exactly one file is required: **`kzg_bn254_21.srs` (~256 MB)**. Degree 21,
not 19, because `KeyManager` loads every circuit's ceremony at startup —
including the K=21 fallback circuit a withdrawal never proves — and the
SHPLONK aggregator for `BridgeWithdrawalAggregatorVerifier` is also K=21.
Degrees 17/19/20 are derived from it automatically on first use.

The K=21 ceremony `.ptau` is **not** auto-downloaded (the shared SHA-256
trust anchor only reaches K=20), so fetch it once by hand:

```bash
mkdir -p ~/.cache/halo2-kzg-srs
curl -L --fail --progress-bar \
  https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_21.ptau \
  -o ~/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau      # ~2.4 GB

cd ../bridge-prover-libraries
cargo build --release --bin bootstrap_hermez_srs
./target/release/bootstrap_hermez_srs --k 21 --params-dir ./params
cd ../ackinacki-bridge
```

> `scripts/bootstrap_hermez_srs.sh` at the repo root is a **different**
> tool: it writes K=20 into `crates/bridge-snark-utils/params/` and will not
> satisfy the CLI. Use the Rust bin above.

The first real withdrawal then generates `event_pk.bin` (~2.65 GB) by
keygen, and the aggregator populates `params/pk_cache/`. Budget ~4.3 GB for
a withdraw-only machine, plus headroom — the `~17 GB` figure elsewhere in
these docs is a `params/` shared with a bundle relayer, which also stores
the primary/fallback/layer proving keys a withdrawal never reads. Peak RAM
during Circuit 4 is ~40 GB.

**Do not wipe `params/` between runs** — keygen and the cold aggregator
cache cost minutes each time.

Finally, build the SHPLONK aggregator. A real withdrawal shells out to it,
and the CLI refuses to start one without a **prebuilt** binary it can probe
— it will not fall back to `cargo run`, because a cold build cannot be
verified inside a preflight and an unverified aggregator costs the burn:

```bash
cd ../bridge-evm-aggregator
cargo build --release --bin aggregate-proof
cd ../ackinacki-bridge
```

`--dry-run` does not need this either; it is required only for a real run.

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

The script shells out to `tvm-cli`, which is platform-specific and is
not shipped here. Confirm the one it will pick actually runs on this
machine before spending a deploy on finding out:

```bash
scripts/check_fixture_prereqs.sh
# OK: tvm-cli = /usr/local/bin/tvm-cli (tvm-cli 3.0.5)
```

If it fails, install `tvm-cli` or export `CLI_NAME=/path/to/tvm-cli`.

```bash
eval "$(scripts/deploy_msig_and_mint.sh)"
# → sets WITHDRAW_FROM      (e.g. 2bd287b8ddb28adec2a17863ffc2d6e1c4fc48fbd1c833c6564f7f843588aad0::2bd287b8ddb28adec2a17863ffc2d6e1c4fc48fbd1c833c6564f7f843588aad0)
#     format: <64-hex dapp_id>::<64-hex account_id> — single-custodian, both halves match
# → sets WITHDRAW_FROM_KEYS (e.g. ./work_dir/msig_withdraw_cli.keys.json — the script emits an absolute path)
#     format: path to a keys.json file, mode 0400
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

Preflight-only preview: nothing is broadcast on either side and no
idempotency state is written — a dry run does not even compute the dedup
digest, because it never reserves.

**It is not AN-side only.** Argument validation, key file perms, the
single-custodian check, USDCBridge resolution and the balance check all
run — and so does the whole EVM side, which needs no signing key:
`eth_chainId` against `--to-chain`, code at `--bridge-address`, the
verifier-stack walk, the pinned-identity comparison and the treasury
check. A dry run that fails may be telling you about your RPC or your
bridge deploy, not about your multisig.

What it does **not** do: compose or sign the burn, capture, prove, or run
the `dry_run_withdraw` eth_call. It also does not check the prover
artifacts — the ceremony, the verifier `.bin`, `aggregate-proof`, the key
cache or disk headroom — because those are gated on the submit-only flags
a dry run does not require. Pass `--verifiers-dir` and the deployed-verifier
bytecode comparison joins in; otherwise it is skipped with a warning.

So a clean dry run means "nothing about either chain is misconfigured",
not "the real run will succeed".

Every network endpoint, bridge address, prover-plumbing dir, and the
L2 anchor selection is resolved from `$BRIDGE_CONFIG`. The invocation
therefore carries only the five per-request intent flags:

```bash
cargo run --release -p ackinacki-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  withdraw \
    --dry-run --yes \
    --from       "$WITHDRAW_FROM" \
    --from-keys  "$WITHDRAW_FROM_KEYS" \
    --to         "$WITHDRAW_TO" \
    --to-chain   "$WITHDRAW_TO_CHAIN" \
    --amount     "$WITHDRAW_AMOUNT"
```

Equivalent, shorter form:

```bash
scripts/local_smoke.sh          # same command; canonicalizes $BRIDGE_SNARK_DIR to absolute
```

Any of the plumbing values (`--rpc-url`, `--bridge-address`, etc.)
can still be passed as explicit flags to override the profile for a
single invocation — clap resolves in precedence order **explicit
`--flag` > shell env > profile file > compiled default**.

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
difference that matters for money. `--dry-run` can only produce 0 or
2 — it neither reads nor writes the idempotency store, so exit 3 is
unreachable under it.

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

- `--from-keys` file is not mode `0400` → `chmod 400 <path>`
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
du -sh ../bridge-prover-libraries/params/    # ~3 GB withdraw-only; ~17 GB if this host also runs the bundle relayer
df -h ../bridge-prover-libraries/params/     # free disk headroom — a cold run needs ~4.3 GB (see Step 0)
```

**Remediation:** free resources, re-run with `--allow-retry`. If the
first attempt was a cold cache and merely slow (not OOM/thrashing),
bump `--prover-timeout-s 3600` on the retry. Don't delete
`./work_dir/event_<seq>_witness.json` between attempts — it's
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
verbatim. **Exactly one stage is ever skipped: the burn.** Capture,
proving and submission re-run from scratch on every attempt, whatever
the recorded status says — the record's later fields are an audit
trail, not a resume point. That is deliberate: the proof is
deterministic for a given `(event, on-chain state)`, and re-deriving it
against the *current* chain state is what lets a retry succeed after
the condition that failed it has been fixed.

- `Burned` / `Captured` / `Proved` → skip the burn, re-run from
  capture. Prior `an_tx_hash` reused, so **the burn is never broadcast
  twice.**
- `Failed` **with** an `an_tx_hash` → same resume, and no flag needed.
  This is the normal `withdrawByProof`-reverted path: the burn happened,
  so it is skipped and everything after it re-runs.
- `Failed` **without** an `an_tx_hash` → **refused.** No production path
  writes that combination — the sole writer of `Failed` is the post-burn
  revert, which by construction has a hash — so it is a hand-edited or
  corrupt record, and acting on it would broadcast a second burn. There
  is no "clean slate" you can ask for; see the cleanup rule below for
  what to do with a record you have reconciled.
- `Submitted` → **refused even with `--allow-retry`.** There is a
  broadcast EVM tx whose receipt we never observed; re-broadcasting
  risks a double payout. Reconcile the `eth_tx_hash` on-chain first,
  then either wait (rerun will become `Confirmed`) or edit the state
  file to `Failed` manually.
- `Confirmed` → **refused always.** The withdrawal already paid out.
  Fire a new withdrawal with a different (amount, recipient, chain).

**Cleanup rule.** `Confirmed` files are keepable forever (small; they
are your on-chain audit trail). `Failed` files are safe to prune once
the corresponding AN/ETH tx status is reconciled.

**Deleting a `Reserved` record is what a second burn looks like.** A
record whose status is `Reserved` with no `an_tx_hash` has two readings,
and nothing in the file distinguishes them: a run is inside the burn
right now and has not come back to write the hash, or a run died in that
window. Age does not tell them apart — a run can be mid-anchor-wait for
90 minutes — so there is no "safe after N hours" rule and this document
used to state one.

Two conditions, both required, before deleting one:

1. **On-chain reconciliation shows no `initiateWithdrawal`** from this
   multisig for this identity (advanced runbook, [Case
   3a](docs/advanced_user_withdraw_runbook.md)).
2. **No process holds the withdrawal.** The CLI answers this for you: run
   the identical command again and read the liveness line in the exit-3
   refusal. It says one of **three** things, and only the middle one is
   the condition you need:

   | The refusal says | What you do |
   |---|---|
   | "Another process on this host is executing this withdrawal **RIGHT NOW**" | **Wait** for that run and read its outcome. |
   | "**No other process** on this host holds this withdrawal, so the record was left by a run that has already exited" | This condition is met. |
   | "Whether another process holds this withdrawal **could not be determined**" | **Do not delete.** The question was never answered. |

   The check is a `flock` the running command holds across the burn, so
   the kernel releases it when that process dies, however it dies — but
   `flock` is not available everywhere. On a filesystem that cannot do it
   (NFS without a lock daemon, some container overlay mounts) every run
   gets the third line, including one that is mid-send. The `warn` line
   the CLI prints just above the refusal says why the verdict is missing;
   the advanced runbook's Case 3a says what to do about it.

   Do not read this list by elimination. "Not the first line, and my
   reconciliation is clean" lands on a deletion that the third line never
   authorised.

With step 1 clean **and** the refusal on the middle line of the table
above, delete the record and re-run; on either of the other two lines,
**do not delete** — including "could not be determined", which is what
every run gets on a filesystem without `flock`. With a burn found in step 1, do
not delete either: write its hash into `an_tx_hash`, set `status` to
`"burned"`, and re-run with `--allow-retry` to resume at capture.

## Safety

- `--from-keys` file contents are never logged, printed, or persisted
  anywhere the CLI writes. The owner public key derived from the file
  is *printed* by preflight, which compares it against the multisig's
  on-chain custodian — an on-chain-observable identifier, not a secret.
  It is **not** written to the state file; `Record` holds only
  `from_extended`, `to_hex`, `to_chain`, `amount_micro`, the timestamp,
  and the chain identifiers listed below.
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
| `deploy_msig_and_mint.py`  | Python driver behind the `.sh`. Consumes the same `$BRIDGE_CONFIG` profile as the Rust CLI — `NETWORK`, `BRIDGE_GQL_ENDPOINT`, `USDC_BRIDGE_KEY_PATH` all come from there. `BRIDGE_WORK_DIR` optional override. |
| `local_smoke.sh`           | `--dry-run` wrapper — same command as Step 4; reads all plumbing from `$BRIDGE_CONFIG` (default: `config/bridge_config`). |
| `live_smoke.sh`            | Real-submit wrapper — same command as Step 5. Same profile handshake. |

All three wrappers pick up the network profile via `$BRIDGE_CONFIG`
(default: `config/bridge_config`, a symlink to `bridge_config.shellnet`)
and expect the five per-withdrawal identity vars (`WITHDRAW_FROM`,
`WITHDRAW_FROM_KEYS`, `WITHDRAW_TO`, `WITHDRAW_TO_CHAIN`,
`WITHDRAW_AMOUNT`) plus `BURNER_PRIVATE_KEY` to be exported in the
shell. Switch networks by re-exporting `$BRIDGE_CONFIG` — no other
change.

## File layout

```
crates/ackinacki-bridge/                       ← run cwd
├── config/
│   ├── bridge_config                          ← default profile symlink → bridge_config.shellnet
│   ├── bridge_config.shellnet                 ← pinned shellnet L2 reference deploy
│   ├── bridge_config.local                    ← local devnet
│   └── bridge_config.mainnet                  ← placeholder
├── docs/
│   └── advanced_user_withdraw_runbook.md      ← self-deploy + deep failure diagnostics
├── scripts/                                   ← see § Scripts
├── src/                                       ← Rust crate source
└── work_dir/                                  ← created on first run
    ├── event_<seq>_witness.json               ← enriched witness (input to Circuit 4)
    ├── proof_event_<seq>.json                 ← aggregated SHPLONK calldata + PI
    ├── shplonk-snark/                         ← intermediate SHPLONK artifacts
    └── withdraw_{smoke,smoke_live,dry}_*.log  ← CLI stdout+stderr via the smoke wrappers

$HOME/.bridge-withdraw-state/                  ← default idempotency state dir
└── <sha256>.json                              ← one per unique (from,to,chain,amount)
                                               #   override with BRIDGE_WITHDRAW_STATE_DIR

../bridge-prover-libraries/                    ← halo2 sub-workspace (shared with the daemon)
├── params/                                    ← BRIDGE_PARAMS_DIR (SRS + pk/vk; ~3 GB withdraw-only, ~17 GB shared with the relayer)
│   └── pk_cache/                              ← Circuit-4 PK cache
└── target/release/ackinacki-bridge            ← this CLI when pre-built (cargo run --release also caches here)

../bridge-evm-aggregator/                      ← BRIDGE_AGGREGATOR_DIR — SHPLONK aggregator source
└── target/release/aggregate-proof             ← the CLI's only subprocess

../../contracts/ethereum/verifiers/            ← BRIDGE_VERIFIERS_DIR — precomputed inner verifier keys
```

**Safe to prune between demos.** `work_dir/`, `proofs/` — regeneration
is deterministic; ~5 min per proof with warm PK cache.
`withdraw-state/<sha256>.json` files with status `Confirmed` — keep
for audit; `Failed` — safe to prune once reconciled. **`Reserved` with
no `an_tx_hash`: there is no age at which deleting one is safe, and the
rule is not repeated here.** It takes the three-verdict procedure under
"Idempotency semantics" above — the exit-3 refusal answers the liveness
question only sometimes, and a shorter version of the rule is how the
wrong one keeps getting copied.

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
