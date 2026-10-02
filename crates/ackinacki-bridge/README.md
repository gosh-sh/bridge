# ackinacki-bridge

End-user CLI for the Acki Nacki ↔ EVM bridge, one subcommand per direction:

- **`withdraw`** (Acki Nacki → EVM) withdraws USDC from an Acki Nacki
  multisig to an EVM recipient. It is the operator-facing counterpart to
  the relayer daemon: the daemon owns the continuous bundle-proving stream
  (`verifyBlock`); this CLI owns per-withdrawal composition (multisig burn →
  capture → Circuit-4 SHPLONK proof → `withdrawByProof`). Most of this README
  is about it.
- **`deposit`** (EVM → Acki Nacki) deposits USDC from an EVM wallet to an
  Acki Nacki account. The wallet signs from a QR code and keeps its keys;
  the CLI proves the deposit on this machine and finalizes it on Acki Nacki.
  Its reference is the [Deposit](#deposit) section below.

**In a hurry?** [QUICKSTART.md](QUICKSTART.md) is one withdrawal or one
deposit in a few steps, starting with `scripts/install.sh`. This README is
the reference behind it.

For withdrawals this README is the **default-user runbook**: point the CLI at the
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
| `--verifiers-dir`       | `BRIDGE_VERIFIERS_DIR`      | Committed withdrawal verifier: `BridgeWithdrawalAggregatorVerifier.bin` (compared with the chain), `.sol` (the proof self-check) and `_calldata.bin` (word 23 is the adapter `vkDigest` pin) |
| `--params-dir`          | `BRIDGE_PARAMS_DIR`         | KZG ceremony + generated pk/vk. Needs `kzg_bn254_21.srs`; see Step 0 |
| `--snark-dir`           | `BRIDGE_SNARK_DIR`          | Aggregator scratch (must be absolute; smoke scripts canonicalize) |
| `--work-dir`            | `BRIDGE_WORK_DIR`           | Per-withdrawal working directory |
| `--pk-cache-dir`        | `BRIDGE_PK_CACHE_DIR`       | Warm-start pk cache (optional; defaults to `$BRIDGE_PARAMS_DIR/pk_cache`) |
| `--state-dir`           | `BRIDGE_WITHDRAW_STATE_DIR` | Per-withdrawal idempotency state dir (optional; defaults to `$HOME/.bridge-withdraw-state`) |

These are `withdraw`'s. `deposit` shares the endpoint and bridge keys and adds
its own (`BRIDGE_DEPOSIT_PROVER_DIR`, `BRIDGE_DEPOSIT_STATE_DIR`,
`BRIDGE_DEPOSIT_CONFIRMATIONS`, `BRIDGE_WC_PROJECT_ID`); see [Command
line](#command-line) under Deposit.

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
  https://storage.googleapis.com/aptos-circuit-testing-setups/ptau/powersOfTau28_hez_final_21.ptau \
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

#### Reaching `params/` from your own shell

Several maintenance commands below act on the params directory, and one of
them deletes files, so they must resolve it the way the CLI does — shell
environment first, profile second. A plain `. "$BRIDGE_CONFIG"` inverts that
precedence: `dotenvy` does not overwrite what the shell already set, so the
profile would beat an explicit export. Declare this once per shell, from
`crates/ackinacki-bridge/`:

```bash
# Prints the resolved directory on stdout, diagnostics on stderr.
params_dir() {
  # `printenv`, not `${VAR+x}`: the shell's test is true for a variable that
  # was assigned but never exported, and the CLI reads the process
  # environment. An exported-but-empty value is refused rather than guessed,
  # because clap refuses it too.
  if BRIDGE_PARAMS_DIR=$(printenv BRIDGE_PARAMS_DIR); then
    [ -n "$BRIDGE_PARAMS_DIR" ] ||
      { echo "BRIDGE_PARAMS_DIR is exported but empty" >&2; return 1; }
  else
    BRIDGE_CONFIG=$(printenv BRIDGE_CONFIG) && [ -n "$BRIDGE_CONFIG" ] ||
      { echo "BRIDGE_CONFIG is not exported, so the CLI would load no profile" >&2
        echo "at all. Use \`export\`, or pass --params-dir to both." >&2; return 1; }
    # `dotenvy` parses KEY=value; `.` executes the file. On these constructs
    # the two disagree, so refuse rather than resolve to whichever this shell
    # happens to produce. The shipped profile is plain assignments.
    grep -E '^[[:space:]]*(export[[:space:]]+)?BRIDGE_PARAMS_DIR=' "$BRIDGE_CONFIG" |
      grep -q '[$`]' &&
      { echo "BRIDGE_PARAMS_DIR in $BRIDGE_CONFIG uses \$ or backticks; pass" >&2
        echo "--params-dir explicitly and give these commands the same path." >&2
        return 1; }
    BRIDGE_PARAMS_DIR=$( set -a; . "$BRIDGE_CONFIG"; printf '%s' "${BRIDGE_PARAMS_DIR-}" )
    [ -n "$BRIDGE_PARAMS_DIR" ] ||
      { echo "BRIDGE_PARAMS_DIR is absent from $BRIDGE_CONFIG" >&2; return 1; }
  fi
  printf '%s' "$BRIDGE_PARAMS_DIR"
}
```

`--params-dir` is not covered and cannot be: it belongs to a run that has not
happened yet. If you intend to pass it, pass the same path to these commands.

Three things are worth running against the result. Free space, before the
first withdrawal on a host:

```bash
PD=$(params_dir) && echo "params -> $PD" && df -h "$PD"
```

Key regeneration up front, instead of letting stage 5 do it — this is also
how you prepare `bridge-verifier-daemon`, which reads the verifying key and
never generates one. The event prover takes no `--params-dir` and reads
`./params` relative to the working directory
(`bridge-event-halo2-prover/src/main.rs:38,151`), hence the symlink:

```bash
PD=$(realpath "$(params_dir)") &&
MANIFEST=$(realpath ../bridge-prover-libraries/Cargo.toml) &&
WORK=$(mktemp -d) && ln -s "$PD" "$WORK/params" &&
( cd "$WORK" && cargo run --release --manifest-path "$MANIFEST" \
    -p bridge-event-halo2-prover -- --selftest )
rm -rf "$WORK"
```

Cache repair, when `probe_event_keys` reports `corrupt`. This one deletes
files, so the `params ->` line above is worth reading before you run it:

```bash
PD=$(params_dir) && echo "params -> $PD" &&
cargo run --release --manifest-path ../bridge-prover-libraries/Cargo.toml \
  -p bridge-prover-lib --bin probe_event_keys -- --params-dir "$PD" --repair
```

`probe_event_keys` without `--repair` reports what the next run will decide
and changes nothing: `warm` and `cold` both need no action, `corrupt` is the
case above, and `blocked` means a directory is sitting where a key file
belongs — usually a bind mount whose host path does not exist. `--repair`
refuses `blocked` without touching anything; remove the directory by hand.

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
idempotency state is written. It does **read** the store, though: its
job is to say what a real run would do, and over a record already on
disk a real run refuses — so a dry run refuses too, with the same exit
code and the same remedy. See "Exit codes" for the four it can produce.

**It is not AN-side only.** Argument validation, key file perms, the
single-custodian check, USDCBridge resolution and the balance check all
run — and so does the whole EVM side, which needs no signing key:
`eth_chainId` against `--to-chain`, code at `--bridge-address`, the
verifier-stack walk, the pinned-identity comparison and the treasury
check. A dry run that fails may be telling you about your RPC or your
bridge deploy, not about your multisig.

What it does **not** do: compose or sign the burn, capture, prove, or run
the `dry_run_withdraw` eth_call. It also does not check the prover
artifacts — the ceremony, the verifier `.bin` and `.sol`, `aggregate-proof`,
the key cache or disk headroom — because those are gated on the submit-only
flags a dry run does not require. Pass `--verifiers-dir` and the
deployed-verifier bytecode comparison joins in; otherwise it is skipped
with a warning.

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
INFO resolved anchor: L2 (mode=Explicit(2), auto_escalated=false)   # L2; "L1" = fallback bug
INFO chain built: anchor_layer=L2, anchor_kb=…, active_links=…, final_root=…
INFO stage 5/6: Circuit-4 SHPLONK proof (in-process C4 → aggregator subprocess)
INFO stage 6/6: submit withdrawByProof
INFO withdrawByProof paid out tx=0x…             # ← definitive on-chain payout marker

withdraw complete:
  amount:       1.000000 USDC
  AN tx:        0x…
  msg id:       0x…
  block:        seq=<N> id=0x…
  proof:        <NNNN> bytes, 11 public inputs, self_verified=true
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
grep -E 'resolved anchor:|anchor_layer=|anchor_stride=' "$LOG"
# On the pinned L2 deploy: `resolved anchor: L2`, `anchor_layer=L2`,
# `anchor_stride=16384`. The log is 1-INDEXED (L1/L2); the witness file
# is 0-indexed, so the same fact reads as 1 there:
# `layer_idx` is nested under `.anchor`; at the top level it is null.
# `height` is the covering bundle, so it must equal the
# `target_covering_seq_no` printed by stage 4b.
jq '.anchor | {layer_idx, height}' work_dir/event_*_witness.json  # 1 = L2, 0 = L1 fallback
grep -E '^error:|^ERROR|ProofFailed|reverted|timed out' "$LOG"
```

## Exit codes

These are `withdraw`'s; `deposit` has its own, under [Deposit exit
codes](#deposit-exit-codes).

Distinguishing "nothing broadcast" from "broadcast, unknown outcome"
is the whole point of the exit-code discipline — scripts that
pattern-match on a single non-zero would blind an operator to the
difference that matters for money. `--dry-run` can produce 0, 2, 3 or
10 — it never reserves and never writes, but it does READ the store, and
its whole job is to report what a real run would do. So a dry run that
finds a record for this identity refuses exactly as the real run would:
**exit 3** where the record is terminal, in flight, or hash-less, and
**exit 10** where some other stage-1 check refuses over a record that
exists. A dry run that cannot name a state directory at all (no `HOME`,
no `--state-dir`) still runs, and reports its refusals as 10 rather than
claiming there is no record — it never opened one to find out.

| Code | Meaning | Nothing broadcast? | Where to look |
|------|---------|-------------------|---------------|
| 0    | Success (or `--dry-run` returned OK) | — | — |
| 2    | Preflight refused — key perms, `--from` not an active MS, balance short, etc. | ✓ nothing, **and no record for this identity on disk** | Error scenarios § "Preflight refused" |
| 3    | Duplicate in-flight refused — same dedup key already exists | ✓ nothing by this run | § Idempotency semantics |
| 10   | This run must not act as if the withdrawal were untouched | ✗ **a burn may be on the wire** | § "Burn broadcast, outcome unknown" |
| 11   | Burn confirmed, `WithdrawalInitiated` capture timed out | ✗ AN burn done | § "Capture timeout" |
| 12   | Capture succeeded, Circuit-4 proof failed | ✗ AN burn done, no ETH tx | § "Prover failed" |
| 13   | Proof succeeded, `withdrawByProof` reverted / dry-run reverted | ✗ AN burn done, no ETH tx | § "On-chain submit reverted" |

**Exit 10 covers six situations, and only the first two involve this
run broadcasting anything.** The first is the send itself: a burn went
out and its outcome is unknown. The second is its mirror — the AN side
did exactly what it was asked and the run **could not write down that
it had**, because the state write failed (full disk, read-only mount,
a directory it may not write). There the outcome is not unknown at all;
the log may even say `capture + prove complete`. The other four are
refusals: a preflight check
that failed on a withdrawal whose burn a PREVIOUS run already recorded;
a state record that exists and could not be read (torn, or in a
directory this process cannot traverse); a reservation on disk this
run may not act on — published but not durable, or reached without
owning the withdrawal lock; and a run that had **nowhere to look**,
because `HOME` is unset and no `--state-dir` was given. None of the four
broadcasts or writes anything — but in the first three a record for this
identity is on disk, and in the fourth the run cannot say whether one
is, which comes to the same instruction: a burn may be on the wire and
nothing may be deleted. What the six share, and what a script should
key on, is "do not treat this identity as untouched".

Exit codes 11–13 all leave the AN burn broadcast: the USDC has left
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
Fix the key and re-run the SAME withdrawal command.

**Whether that resumes depends on the record.** If it carries an
`an_tx_hash`, a re-run **with `--allow-retry`** skips the burn and picks
up at capture — for a `reserved`, `burned`, `captured` or `proved`
record, without the flag that same record is refused with exit 3,
because resuming is what the flag authorises there. (A `failed` record
with a hash is the exception and needs no flag: see § Idempotency
semantics.) If it
does not — which is what an exit 10 out of the send itself leaves,
because the hash is written only after the send returns — the re-run
neither resumes nor goes straight to exit 3: preflight runs first, a
record showing no burn puts the ECC[3] balance check back in force, and
the balance is genuinely spent. Reconcile on chain first (advanced
runbook, Case 3a). If a burn landed, write its hash into the record and
re-run with `--allow-retry`, which is what lets the reservation hand the
recorded burn back; if none landed, the next run refuses with exit 3 and
that refusal is the procedure. Either way the record stays.

**A refusal reading `…, but the state file could not be updated` is a
different animal.** Nothing is wrong with the withdrawal — the write
was. The refusal names the record it failed to write; fix whatever
stopped the write (the directory needs `drwx------`, because the record
is published through a temp file and a `rename`), then read `.status`.
If it carries an `an_tx_hash`, the record is behind by one transition
and is resumable with a plain `--allow-retry` — no hand-editing. Full
procedure: advanced runbook,
[Case 3a-iii](docs/advanced_user_withdraw_runbook.md#3a-iii--the-record-is-behind-the-chain-the-state-write-failed).

**Do not re-run `scripts/deploy_msig_and_mint.sh` here.** It deploys a
fresh multisig, which is a different `--from`, which is a different dedup
identity — the record for the burn already on the wire is orphaned, and
nothing will ever resume it. That script is for standing up a new test
withdrawal, not for recovering one. If the bundled
`USDCBridge.shellnet.keys.json` has drifted from the current on-chain
owner pubkey, correct the key file itself.

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
TIP=$(cast block-number --rpc-url $RPC_URL)
cast logs --address $BRIDGE_ADDRESS --from-block $((TIP - 10000)) \
  'BlockVerified(uint256,uint64,uint8,uint8)' --rpc-url $RPC_URL \
  | grep -E '^  blockNumber' | tail -5
```

Two things that are easy to get wrong here, and both fail SILENTLY —
an empty result reads as "daemon is dead" when it is really "the query
never ran":

* `--from-block` takes a height or one of `earliest|finalized|safe|latest|pending`.
  Arithmetic (`latest-2000`) is not accepted, and a negative offset
  (`-1000`) is parsed as a flag. Compute the height, as above.
* `cast logs` never prints the event NAME — its output is `address`,
  `blockHash`, `blockNumber`, `data`, `logIndex`, `topics`,
  `transactionHash`. Count `blockNumber` lines; `grep -c BlockVerified`
  counts a string that is never there and answers `0` for a perfectly
  healthy daemon.

**Reading it:** events arrive one per bundle, so ~437 Sepolia blocks
apart (~87 min) in L2 mode. What matters is FRESHNESS, not the count:
`COVERAGE_WAIT` is 120 min, so a daemon more than one cadence behind
the tip eats the whole budget after the burn. Last event more than
~600 blocks old = stalled; then check whether the chain is even
producing, before blaming the relayer:

```bash
curl -sS -X POST $BRIDGE_GQL_ENDPOINT -H 'Content-Type: application/json' \
  -d '{"query":"{blockchain{blocks(last:1){edges{node{seq_no gen_utime}}}}}"}'
```

Compare that `seq_no` with the `blockSeqNo` of the last `BlockVerified`
(its second topic). More than one W² = 16384 ahead means the chain is
running and bundles are not being published — a relayer problem. Less
than one W² means the next bundle is simply not due yet.

## Idempotency semantics

**Dedup key.** SHA-256 over four fields joined by `|`, and only the
first of them is ASCII — this matters, because the record's filename
IS this digest and reconciliation procedures ask you to compute it:

    sha256( from_extended ASCII || "|" || to  20 RAW bytes
                                || "|" || to_chain  u64 big-endian
                                || "|" || amount    u128 big-endian, micro-USDC )

Golden vectors, checked against `idempotency.rs::key`
(`from = "a"×64 :: "b"×64`, `to = 0x841709B6842233d8474aeA1d773e8d0F7c7c0B9f`,
`chain = 11155111`):

    1_000_000 → 21e77410bb5b3e6f8e403f09c07357d11e1f4e8f17e2e5ed0d7c9bd4b9f12893
    1_000_001 → c43d9eebd538cbd547f47854fbf491a6b2ea74f45db1b97a16f749173b343462

Two withdrawals with identical tuples collide; anything different
(recipient, amount, or chain) does not.

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

With step 1 clean **and** the refusal on the middle line of the
three-verdict table above, delete the record and re-run; on either of the
other two lines,
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
| `stage_deposit_prover.sh`  | Builds `deposit-prover`'s tools and lays out a deposit prover directory: `stage_deposit_prover.sh ./deposit-prover`. See § Deposit, "The deposit prover". |

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
├── deposit-prover/                            ← BRIDGE_DEPOSIT_PROVER_DIR (deposit), laid out by
│   │                                             scripts/stage_deposit_prover.sh; see § Deposit
│   ├── fetch_deposit_data, export_blake2b_proof, export_vk_blob
│   ├── configs/circuit_params.json
│   └── data/                                  ← kzg_params_18.srs, and the ~1.3 GB proving key
│                                                 the first proof writes
├── deposit-state/                             ← BRIDGE_DEPOSIT_STATE_DIR: <op-id>.json, deposit.lock,
│                                                 <op-id>.lock — never delete or move while an
│                                                 operation in it is unfinished
├── scripts/                                   ← see § Scripts
├── src/                                       ← Rust crate source
└── work_dir/                                  ← created on first run; deposits use <op-id>/ in it
    ├── event_<seq>_witness.json               ← enriched witness (input to Circuit 4)
    ├── shplonk-snark/                         ← intermediate SHPLONK artifacts
    │                                             (proof_event_<seq>.json is NOT here — it is
    │                                              written only under --prover-out-dir)
    └── withdraw_{smoke,smoke_live,dry}_*.log  ← CLI stdout+stderr via the smoke wrappers

$HOME/.bridge-withdraw-state/                  ← default idempotency state dir
└── <sha256>.json                              ← one per unique (from,to,chain,amount)
                                               #   override with BRIDGE_WITHDRAW_STATE_DIR

$HOME/.bridge-deposit-state/                   ← deposit state dir when BRIDGE_DEPOSIT_STATE_DIR is unset
$HOME/.bridge-deposit-work/                    ← deposit work dir when BRIDGE_WORK_DIR is unset

../bridge-prover-libraries/                    ← halo2 sub-workspace (shared with the daemon)
├── params/                                    ← BRIDGE_PARAMS_DIR (SRS + pk/vk; ~3 GB withdraw-only, ~17 GB shared with the relayer)
│   └── pk_cache/                              ← Circuit-4 PK cache
└── target/release/ackinacki-bridge            ← this CLI when pre-built (cargo run --release also caches here)

../bridge-evm-aggregator/                      ← BRIDGE_AGGREGATOR_DIR — SHPLONK aggregator source
└── target/release/aggregate-proof             ← the CLI's only subprocess

../../contracts/ethereum/verifiers/            ← BRIDGE_VERIFIERS_DIR — verifier bytecode (.bin) and source (.sol)
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
- Full `--resume` semantics for `withdraw` → v2 (v1 has refuse-duplicate
  + blunt `--allow-retry` override). `deposit` has `--resume` already.
- Self-deploy of `AckiNackiBridge` + running your own bundle
  relayer → [docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md).

## Deposit

`deposit` moves USDC from an EVM wallet to an Acki Nacki account. The wallet
signs `approve` and `deposit` from a QR code and keeps its keys: **the CLI
never takes an EVM private key**, and it needs no Acki Nacki key either,
because `finalizeDeposit` is an unsigned external message whose gas the Acki
Nacki bridge pays. The rest is the CLI's job: it waits for the `Deposit`
event, waits for the deposit block's anchor on Acki Nacki, proves the deposit
on this machine, sends `finalizeDeposit` and confirms that the eccUSDC reached
the recipient.

[QUICKSTART.md](QUICKSTART.md) walks through one deposit with a wallet. This
section is the reference behind it.

### What a deposit does

Nine steps, drawn as a checklist on stderr with a status line underneath — a
plain append-only log when stderr is not a terminal, NDJSON events on stdout
under `--json`:

| # | Step | What happens |
|---|------|--------------|
| 1 | preflight | Every check that can be made before the wallet is asked (below). A refusal is exit 2 and nothing is sent. The run then creates the **operation** and prints its id. |
| 2 | wallet pairing | You scan the QR code and approve the connection; if the wallet is on another network, the CLI asks it to switch, or to add the network. Then the **account check**: an account holding contract code, other than an EIP-7702 delegation, is refused, and the wallet signs a short message (`personal_sign`) whose signer must be the account itself. A failure is exit 20; nothing is sent. |
| 3 | approve | Skipped when the bridge's allowance already covers the amount. Otherwise `approve(bridge, amount)` for exactly the amount, after resetting a smaller non-zero allowance to 0. The allowance is read back afterwards: if the wallet let you lower the spending limit, the run stops with exit 21 before the deposit is requested. The read-back and the deposit's gas estimate are made at the block the approve landed in, and the nonce the deposit is expected at counts the approve, so an RPC node that has not caught up with that block is asked again, not taken for a lowered limit or a deposit that would revert. |
| 4 | deposit request | The CLI checks again that the EVM bridge is not paused, estimates gas, and asks the wallet for `deposit(amount, 0, account)` as a type-2 transaction without an access list, gas 1.25 × the estimate. The request is written to the operation record before the wallet sees it. |
| 5 | EVM confirmation | The receipt, read again once it is `--confirmations` blocks deep. A negative verdict — reverted, not the deposit that was requested, a shape the circuit cannot prove — is only taken once the block is finalized. The provable shape: type 2, input exactly the 100 bytes requested, an access list of at most 64 bytes RLP, sent straight to the bridge. The wallet session is closed after this step. |
| 6 | block anchor on Acki Nacki | `isAcceptedBlockHash(chainId, blockHash)` every 30 s. The status line says whom it waits for and, for the bridge owner, the exact call to make. It moves on only when the block is anchored, finalized on the EVM side with the same receipt, and the Acki Nacki bridge is not paused. Then it leaves the deposit to the operator's relayer for `--relayer-grace-s`: a deposit finalized in that time is not proven here. A reorg sends the run back to step 5. Every poll, once the receipt is read and before the anchor and the pause, it also looks whether the deposit is finalized already — by the operator's relayer, say, while this run was stopped: then it goes straight to step 9, even while the bridge is paused or the anchor is gone. The voucher shows it while the bridge keeps the voucher code; after a code change, or when the voucher cannot be read, the bridge's `DepositFinalized` events do. Only when nothing could be read is it not known, and the wait goes on. |
| 7 | proof | `fetch_deposit_data` and `export_blake2b_proof` from the prover directory, one proof at a time per directory. The 12 public inputs are compared with the deposit before anything is sent. |
| 8 | `finalizeDeposit` | Before every send the CLI checks that the deposit is not finalized already and that the bridge is not paused, and waits while it is. A refusal with code 231 (paused) is waited out too; 224 (the anchor is gone) goes back to step 6. |
| 9 | credit | Confirmed only by the deposit's identity `(chainId, EVM bridge, depositId)`: the voucher's deployment, its `confirmDeposit`, the bridge transaction that sends ECC[3] to the recipient and emits `DepositFinalized`, and that transfer's delivery. The recipient's balance before and after is printed as a diagnostic and decides nothing: another deposit or a spend moves it too. |

Preflight, in order:

- **EVM** (`--rpc-url`): the chain id is the network's; the bridge has code;
  its `usdc()` is an ERC-20 with 6 decimals; its `paused()` is not `true` (a
  bridge without that getter counts as not paused); the amount is at most
  `u64::MAX` micro-USDC; the sender's USDC balance covers it, when
  `--from-address` names the sender (otherwise the balance is checked after
  pairing); and the RPC serves every receipt and raw transaction of the
  latest block, which is what the prover fetches.
- **Acki Nacki** (`--gql-endpoint`): REST `/v2/` and GraphQL both answer; the
  bridge's `getVersion()` is at least the [minimum bridge
  version](#minimum-bridge-version); `isPaused()` is `false`;
  `isTrustedL1Bridge(chainId, bridge)` is `true`; `getAnchorConfig()` says
  who anchors blocks, and with owner anchors off the light client must pass
  its [readiness checks](#when-only-the-light-client-anchors);
  `getDepositVoucherCodeHash()` is the voucher code this build carries
  (otherwise the CLI cannot compute the voucher's address, and a newer CLI is
  needed); and the recipient: an active account must live in the dapp `--to`
  names, while a missing or undeployed one only gets a warning.
- **Prover** (`--deposit-prover-dir`): both tools are executable and run on
  this host — each is started once with `--help`, without the network and
  with nothing of the environment but `PATH` and `LD_LIBRARY_PATH`, and must
  exit 0 within 10 s; a binary the loader refuses (`GLIBC_2.38' not found`,
  a missing loader) is refused here with the end of its stderr, not at step
  7 with the USDC in the bridge. `configs/circuit_params.json` has
  `base.k = 18` and `base.lookup_bits = 8`, `data/` is writable, and
  `data/kzg_params_18.srs` carries the Hermez ceremony's [s]·G2. A proof made
  with another SRS would be refused on chain, after the deposit.
- **Work directory** (`--work-dir`): created if it is missing, and a file is
  written, synced and removed there. A path that is a file, or a directory
  that cannot be written, is refused with the error the system gave.
- **Unfinished operations** in `--state-dir`: none whose EVM outcome is
  unknown for the same network, bridge, `--to` and amount. Otherwise exit 3,
  with the `--resume` command to run instead. The same check by sender runs
  again after pairing.

`--dry-run` stops there. It prints the calldata of both transactions, the QR
payloads and whom the anchor wait would wait for, and sends nothing. It is not
read-only, though: it creates the state and work directories if they are
missing, writes and removes its probe files in the work directory and in the
prover's `data/`, takes the state directory's lock for as long as it runs, and
closes operations an interrupted run left before the deposit was requested.
So a dry run answers exit 3 too, while another run on this machine holds the
state directory or an operation for the same deposit has an unknown outcome.

### Command line

    ackinacki-bridge deposit --network sepolia --amount 12.500000 --to <dapp_id>::<account_id>

Everything else comes from the profile, exactly as for `withdraw`: explicit
flag > shell environment > profile file > compiled default. The shipped
profiles carry the deposit keys, and `scripts/install.sh` appends them to a
profile it installed before deposits existed.

**Per deposit:**

| Flag | Value | Refused with exit 2 |
|------|-------|---------------------|
| `--network` | `sepolia` (chain id 11155111), the only deposit network in this version | any other name |
| `--amount` | USDC, at most 6 fractional digits, never rounded; above 0 and at most `u64::MAX` micro-USDC | anything else |
| `--to` | `dapp_id::account_id`, both halves 64 hex characters without `0x`, the account non-zero | a wrong shape; an active account that lives in another dapp: `--to names dapp <x>, but account <acc> lives in dapp <y>` |

The bridge takes only the account id, and the eccUSDC land on that account in
whatever dapp it lives in. The dapp in `--to` is your statement of where that
is, and preflight checks it once the account is deployed. A recipient that
does not exist yet, or exists without code, gets a warning instead: the
transfer creates it, and its dapp becomes known only when it is deployed.

**From the profile:**

| Flag | Env var / profile key | Default | Purpose |
|------|-----------------------|---------|---------|
| `--rpc-url` | `RPC_URL` | — | EVM JSON-RPC. It must serve every receipt and raw transaction of a block: the prover reads the whole block |
| `--bridge-address` | `BRIDGE_ADDRESS` | — | `AckiNackiBridge` on the EVM chain |
| `--gql-endpoint` | `BRIDGE_GQL_ENDPOINT` | — | The Acki Nacki host. GraphQL for reads; `finalizeDeposit` goes to `POST /v2/messages` on the same host, so it has to serve both |
| `--usdc-bridge-account` | `USDC_BRIDGE_ACCOUNT_ID` | — | Account id of the Acki Nacki bridge; its dapp is resolved live |
| `--deposit-prover-dir` | `BRIDGE_DEPOSIT_PROVER_DIR` | — | The deposit prover, see [below](#the-deposit-prover) |
| `--state-dir` | `BRIDGE_DEPOSIT_STATE_DIR` | `$HOME/.bridge-deposit-state`; none when `HOME` is unset or empty | Operation records, their locks and claims. Keep it the same for one sender: [why](#operations-resume-and-abandon) |
| `--work-dir` | `BRIDGE_WORK_DIR` | `$HOME/.bridge-deposit-work`; none when `HOME` is unset or empty | `<op-id>/input.json`, `proof.bin` and `public_inputs.bin` of each operation. Each operation records its own, so only a new deposit needs this setting |
| `--confirmations` | `BRIDGE_DEPOSIT_CONFIRMATIONS` | `12` | How deep the receipt must be before step 5 decides |
| `--wc-project-id` | `BRIDGE_WC_PROJECT_ID` | compiled into release builds | WalletConnect Cloud project id. A build from source has none: pass it, or set `ACKINACKI_BRIDGE_WC_PROJECT_ID` when building |

**Time limits**, flags only:

| Flag | Default | What it limits | When it runs out |
|------|---------|----------------|------------------|
| `--pair-timeout-s` | 300 | pairing with the wallet; and the wait for an `approve` to show on chain — its receipt once the wallet returned a hash, or the allowance once the QR code is shown (`eip681`) — with the allowance read back after it, every read, retry and pause included: an RPC that keeps failing ends the wait too | exit 20 for pairing, exit 21 for an `approve` |
| `--recovery-window-s` | 600 | the search for the deposit transaction when the wallet did not return its hash, the session dropped, or the node does not show the hash the wallet returned | exit 30 |
| `--anchor-timeout-s` | 0 = no limit | the anchor wait and the whole of step 8, pauses and sends included | exit 31; exit 34 if a `finalizeDeposit` was in doubt |
| `--relayer-grace-s` | 120 | how long after the anchor the deposit is left to the operator's relayer | the CLI proves it itself |
| `--prover-timeout-s` | 1800 | one proving attempt, both tools | exit 32 |
| `--credit-timeout-s` | 300 | the credit confirmation of step 9 | exit 34 |

Each limit takes at most 315360000 s (ten years), `--pair-timeout-s` at most
2592000 s (30 days, the longest the WalletConnect relay keeps a message); a
larger value is refused with exit 2.

A read that fails is retried without a limit, backing off from 1 to 60 s,
and every attempt is logged as `ERROR` — except inside a step with its own
time limit, which covers the retries too.

**QR code and wallet:**

| Flag | Value | Default or effect |
|------|-------|-------------------|
| `--qr-mode` | `walletconnect`, `eip681` or `both` | `walletconnect` |
| `--from-address` | `0x…`, the sending account | checked against the wallet session in `walletconnect`; required by `eip681` and `both` |
| `--qr-out` | a path ending in `.png` or `.svg` | the WalletConnect QR is written there as well |
| `--uri-only` | flag | print the URI as text, without the QR picture |
| `--qr-invert` | flag | swap dark and light, for a light-on-dark terminal |
| `--wc-relay-url` | URL | `wss://relay.walletconnect.org` |

- **`walletconnect`** — one QR code, a WalletConnect v2 pairing URI (`wc:…`),
  carries the whole deposit: the account check, `approve` and `deposit`. It
  works with mobile wallets that scan it, and with desktop wallets that take a
  `wc:` URI as text (`--uri-only`, `--qr-out`).
- **`eip681`** — an `ethereum:` QR code per transaction, for wallets without
  WalletConnect, browser extensions among them. The wallet returns no
  transaction hash, so step 5 finds the deposit by its `Deposit` event. **An
  EIP-681 code cannot ask for a type-2 transaction**, and a wallet that sends a
  legacy one makes the deposit unprovable (exit 35). The wallet will not add
  the network either, and some wallets drop the call data. The CLI asks you to
  accept that risk: `--yes` accepts it, `--non-interactive` or `--json`
  without `--yes` declines it (exit 20), and Ctrl-C at the question ends the
  run at once, as anywhere else. There is no signature check in this mode,
  only the check of the account's code, so an ERC-4337 account that is not
  deployed yet is not caught: its deposit goes through the account's contract
  and ends at exit 35.
- **`both`** — WalletConnect first, and the EIP-681 codes only if pairing
  itself fails.

**Run modes:**

- `--dry-run` — preflight only, then both transactions and the QR payloads.
- `--resume <op-id>` — continue an operation, [below](#operations-resume-and-abandon).
  Once step 5 is done, its `depositId` works too.
- `--tx-hash <hash>` — with `--resume` only: bind this transaction when the
  search is ambiguous.
- `--abandon <op-id>` — release an operation whose EVM outcome is unknown.

`--json`, `--yes` and `--non-interactive` are global, as for `withdraw`. In a
human run the QR codes and the final summary go to stdout and everything else
to stderr. Under `--json` stdout carries NDJSON events — `step`, `status`,
`retry`, `warn`, `qr` (the URI, no picture), `op_id`, `confirm` — then one
final object: the summary, or the error envelope, which carries `op_id` too.

**Secrets.** `--help` does not show the values of `--rpc-url`,
`--gql-endpoint` and `--wc-project-id` taken from the environment or the
profile. Every line a deposit run prints — logs, the checklist, events,
errors, the prover's stderr — shows a configured URL as
`scheme://host[:port]/…` and never shows the project id: RPC providers put API
keys into the URL, and HTTP clients quote it whole in their errors. The
operation records hold chain identifiers only.

**From a source checkout**, from `crates/ackinacki-bridge/`, once the
[prover directory](#the-deposit-prover) is in place. Until the
[minimum bridge version](#minimum-bridge-version) is set, a build from source
refuses every bridge with exit 2 unless it is built with
`--features dev-unfixed-bridge`, and such a build is for development only:

```bash
export BRIDGE_CONFIG=./config/bridge_config
cargo run --release -p ackinacki-bridge --features dev-unfixed-bridge \
  --manifest-path ../bridge-prover-libraries/Cargo.toml -- \
  deposit --dry-run --network sepolia --amount 1.000000 \
    --to <dapp_id>::<account_id> --wc-project-id <project id>
```

### Networks

Sepolia is the only deposit network in this version. **Ethereum mainnet is
not a deposit network**: the deposit circuit accepts OP Mainnet, World Chain,
Mantle, Base, Arbitrum One, Blast and Sepolia (`crates/deposit-chain-ids`), so
production deposits come from L2s. `bridge_config.mainnet` cannot be used for
`deposit`.

### Deposit timing

The wait for the block anchor dominates. On the shellnet deploy the bridge
owner anchors blocks by hand, and the owner's procedure
(`scripts/deposit_anchor_params.py --verify` at the repository root) wants at
least 64 confirmations: on Sepolia the anchor comes no sooner than ~13 minutes
after the deposit, plus however long the owner takes to act. The status line
prints the owner's call to pass on:
`setAcceptedBlockHash(<chainId>, 0x<blockHash>, true)`.

| Phase | On Sepolia |
|-------|------------|
| Preflight | seconds |
| Pairing, account check, `approve`, `deposit` | as fast as you confirm in the wallet; pairing, and an `approve` that does not show on chain, give up after `--pair-timeout-s` |
| EVM confirmation, 12 blocks | ~2.5 min |
| Block anchor, by the owner | **from ~13 min, plus the owner's reaction; no limit by default** |
| Deposit block finalized on the EVM side | ~13 min, inside the anchor wait |
| Relayer grace | up to 2 min |
| Proof | seconds to fetch the block, then ~45 s cold or ~25 s warm on the [measured host](#the-deposit-prover) |
| `finalizeDeposit` and the credit | under a minute; at most `--credit-timeout-s` |
| **End to end** | **~17 min, plus the owner's reaction** — an estimate summed from the rows above, not a measured run |

With owner anchors disabled, the light client anchors a block once it is
finalized and the next checkpoint is proven: at least two epochs (~13 minutes
on Sepolia) plus the light-client relayer's delay. That mode has its
[own conditions](#when-only-the-light-client-anchors).

Leave the run going. If it is interrupted, `--resume` picks it up where it
stopped.

### Operations, resume and abandon

Each deposit is an **operation**: an id printed right after preflight —
`operation <op-id> (keep it: --resume <op-id> continues this deposit)` — and a
record, `<state-dir>/<op-id>.json` (mode 0600), written before every step that
could make it wrong: before the wallet is asked for the deposit, before the
first `finalizeDeposit`. Ctrl-C, SIGTERM and SIGHUP end a run cleanly: the
prover is stopped, the locks are released, the record keeps the last stage,
and the exit code follows it — 2 before the deposit was requested, 30 while
its transaction is unknown, 31 once it is confirmed, 32 once it is anchored,
34 after that. An operation that has already ended answers as its run did: a
closed one with its recorded code and message (22, 35 and so on), a credited
one with its summary and exit 0.

`--resume <op-id>` continues from the first unfinished step and checks only
what that step still needs. It first compares the command line and the
profile with the record — the EVM chain and bridge, the Acki Nacki bridge
account, and the Acki Nacki network (scheme, host and port of
`--gql-endpoint`) — and refuses a mismatch with exit 2 before doing anything:
under another profile, a resume could finalize the same deposit through
another bridge. A finished operation answers from its record: a credited one
with its summary, a failed one with its exit code and message. A state
directory that cannot be opened, or a record that cannot be read, is exit 34:
how far the operation got is unknown. The operation's lock,
`<state-dir>/<op-id>.lock`, held by another run is exit 3. A lock that cannot
be taken for any other reason leaves the record to answer, read without the
lock: a finished operation as above, any other with the exit code an
interrupt at its stage gives (2 only if the deposit was never requested), and
the operation is left as it is until the lock is fixed.

The EVM chain and bridge, the Acki Nacki bridge, the recipient and the work
directory come from the record, so a finished or failed operation is answered
with `--state-dir` alone, and `--abandon` needs nothing else either. The other
settings are asked for only when the operation needs them: `--rpc-url` and
`--gql-endpoint` to go on with an operation that has not ended, and
`--deposit-prover-dir` while its proof is still to be built — then the prover's
tools are run once and the operation's work directory is checked, as for a
new deposit, before either chain is read. A missing or failed one is
named in the message, with the exit code an interrupt at the operation's stage
gives (30 to 34), except for a proof that is gone and has to be built again:
without `--deposit-prover-dir` that step fails, with exit 32, or 34 once a
`finalizeDeposit` was sent. `--resume <depositId>` needs `--bridge-address` as
well: a deposit id is unique only within its bridge.

An operation that stopped in the anchor wait or after the anchor may have
been finalized meanwhile, by the operator's relayer. A resume of it first
passes its own checks, in this order: the command line against the record,
then `--deposit-prover-dir` with its tools and the work directory (without
reading either chain), then the chains and the Acki Nacki bridge. A command
line that contradicts the record, or an RPC on another chain, is exit 2 as
above; any other refusal there takes the stage's exit code (31 in the anchor
wait, 32 after the anchor), even if the deposit was finalized meanwhile.
Then, in the anchor
wait on every poll once the receipt is read, before the anchor and the
pause, and after the anchor before a proof is built, the resume looks
whether the deposit is finalized already. If it is, the resume only
confirms the credit: it builds no proof, and a bridge paused since, or an
anchor withdrawn since, does not hold it.

**After exit 30.** The wallet was asked for the deposit and its transaction
was not found within `--recovery-window-s`: it may be pending, stuck, or never
sent. Until that is settled, a new deposit with the same network, bridge,
`--to` and amount, or from the same sender, is refused with exit 3 — a second
deposit is not requested while the first one's outcome is unknown.

1. Look for the deposit in the wallet's activity.
2. If it is there, pending or mined: `ackinacki-bridge deposit --resume
   <op-id>`. A resume searches without a time window.
3. If exit 30 listed candidates — several transactions could be this deposit,
   or the wallet's hash is not visible and another deposit took its nonce —
   find yours in the wallet and run `--resume <op-id> --tx-hash <hash>`. The
   hash is checked (the sender, success, a `Deposit` of this bridge, not
   claimed by another operation) before it is bound.
4. If you have made sure the wallet never sent it: `ackinacki-bridge deposit
   --abandon <op-id>`. That lifts the block on new deposits. Should the
   transaction turn up after all, `--resume <op-id>` still continues it; if its
   hash was never recorded, that takes `--tx-hash`.

The CLI never repeats step 4: sending the deposit again would be a second
deposit. A deposit transaction the wallet replaced or cancelled in a finalized
block closes the operation with exit 20: nothing was deposited.

**Why `--state-dir` must not change.** The records, their locks and the claims
(which transaction belongs to which operation) all live in the state
directory. An operation in another directory is invisible: `--resume` cannot
find it, and the checks that stop a second deposit while the first one's
outcome is unknown do not see it. Keep one state directory per sender, and do
not move it or point the profile elsewhere while an operation in it is
unfinished. For the same reason neither `--state-dir` nor `--work-dir` falls
back to the current directory. With `HOME` unset or empty (systemd, cron, many
containers) every `deposit` run needs `--state-dir`, and a new deposit or a
dry run `--work-dir` too, as absolute paths that persist between runs;
without them it is refused with exit 2.

### Deposit exit codes

| Code | Meaning | Your USDC | What to do |
|------|---------|-----------|------------|
| 0 | The credit is confirmed by the deposit's identity, by this run or an earlier one; or `--dry-run` passed | on Acki Nacki | — |
| 2 | Refused before the deposit was requested: preflight, including a paused bridge on either side and a USDC balance below the amount; a filesystem without `flock`; a `--resume` or `--abandon` whose command line contradicts the operation's record. Also the EVM bridge paused, or the deposit's gas estimate reverting, right before the request — by then an `approve` may have been sent | not moved; `approve` gas may be spent | fix what the message names, run again |
| 3 | Another deposit holds the state directory; an operation with an unknown outcome exists for the same deposit or the same sender; the operation is being run by another process | untouched by this run | wait for the other run, or `--resume` / `--abandon` the operation the message names |
| 20 | Wallet: not paired, rejected, timed out; a smart-contract account (code, or a signature not made with the account's key); or the wallet replaced the deposit transaction in a finalized block | untouched | pair again, from a plain account |
| 21 | `approve` reverted, was rejected, would revert, or did not show on chain within `--pair-timeout-s` — or could not be confirmed within it, the RPC failing, with its last error; or the wallet set a spending limit below the amount | not moved; `approve` gas may be spent | fix the cause, run again |
| 22 | The deposit transaction reverted on the EVM side, or succeeded without a `Deposit` event of the bridge | not taken; gas spent | read the cause it names, run again |
| 30 | The wallet was asked; the transaction was not found in the recovery window | possibly in flight | [after exit 30](#operations-resume-and-abandon) |
| 31 | The deposit is confirmed; the Acki Nacki side (the anchor, or a paused bridge) did not come in time | in the EVM bridge | `--resume <op-id>` later |
| 32 | The proof failed or does not match the deposit; or `finalizeDeposit` could not be built, with no earlier send in doubt | in the EVM bridge | fix the [prover](#the-deposit-prover), `--resume <op-id>` |
| 33 | The Acki Nacki bridge refused `finalizeDeposit` | in the EVM bridge | 222: `--resume <op-id>` once the owner restores the allowlist; 220: keep the work directory and report it (the prover does not match the bridge's verification key); other codes: report them |
| 34 | `finalizeDeposit` was sent and the credit was not confirmed in time; or step 8 ran out of time with a send in doubt; or a `--resume` could not open the state directory or read the operation's record | unknown | `--resume <op-id>`; never make a new deposit instead |
| **35** | **The deposit is on chain but cannot be proven, or it is not the deposit that was requested** | **in the EVM bridge; only the operator can finalize or return it** | [hand it to the operator](#handing-a-deposit-to-the-operator) |
| **37** | **The voucher exists and the bridge transaction that should have minted the credit aborted** | **in the EVM bridge, beyond a retry; only the operator can pay it out** | [hand it to the operator](#handing-a-deposit-to-the-operator) |

`--resume <op-id>` continues 30–34. 22, 35 and 37 close the operation for
good, and it blocks no new deposit. The withdrawal codes 10–13 never come from
`deposit`.

### Handing a deposit to the operator

Exit 35 and exit 37 are final: the CLI cannot move the USDC any further. It
prints what the operator needs, and `--resume <op-id>` prints it again from the
record.

- **Exit 35, a shape the circuit cannot prove** — a legacy or type-1
  transaction (EIP-681 wallets may send one), longer call data, a longer access
  list, a call routed through another contract. The USDC is in the EVM bridge,
  which has no refund; only the operator can return it.
- **Exit 35, not the deposit that was requested** — `the wallet broadcast a
  deposit that differs from the request`, with the actual amount and recipient
  next to the requested ones. You changed the amount in the wallet, the wallet
  altered the arguments (it happens with EIP-681), or the wallet sent the call
  through its own contract. The CLI does not prove or finalize a deposit you
  did not ask for. If its shape is provable, the operator or the operator's
  relayer can finalize it to the recipient in its call data; otherwise only
  the operator can return the USDC.
- **Exit 37** — the voucher is spent, so the deposit cannot be finalized again.
  The operator pays it out (`mintAndSend`).

Give the operator the deposit's transaction hash, its `depositId`, the amount
and recipient actually deposited, the EVM bridge address and the operation id;
for exit 37 also the Acki Nacki bridge transaction the message names. The
record holds all of it, though not all in the same place:

```bash
jq '{op_id, bridge: .params.bridge, tx: .tx.tx_hash,
     requested_units: .params.amount_units, requested_to: .params.to,
     deposit_id: .deposit.deposit_id, detail: .failure.detail}' <state-dir>/<op-id>.json
```

- `detail` is the message the run ended with. After exit 35 its `Give the
  bridge operator:` line carries the transaction, what it deposited — every
  `depositId` with its amount and recipient, as `deposited: depositId …, …
  USDC units to account …` — the bridge and the operation; after exit 37 it
  names the operation and the Acki Nacki bridge transaction that aborted.
- `deposit_id` is set only after exit 37. After exit 35 it is `null`: the
  operation was closed before the deposit was recorded, so take the
  `depositId` from `detail`.
- `requested_units` (micro-USDC) and `requested_to` are what was asked for.
  For exit 37 and for an unprovable shape that is also what was deposited.
  When the amount, recipient or sender differs from the request, the `Give
  the bridge operator:` line has what was deposited, and the sentence before
  it has both: `… units to account … from … were deposited, … were
  requested`.

### When a bridge is paused

Each side has an owner switch.

- **The EVM bridge** (`AckiNackiBridge.paused()`): `deposit()` reverts with
  `BridgePaused` while it is on. Preflight refuses (exit 2), and the CLI checks
  again right before it asks the wallet for the deposit, so a pause set during
  pairing or `approve` ends the run with exit 2 before the deposit is
  requested; an `approve` already mined stays, and the next run skips it. A pause
  that lands between that check and the transaction reverts it: exit 22, gas
  spent, no USDC taken. Run again once the owner lifts it.
- **The Acki Nacki bridge** (`isPaused()`): preflight refuses a new deposit
  (exit 2), since it would not finalize until the pause is lifted. Once the
  deposit is made, a pause is waited out, in step 6 and before every
  `finalizeDeposit`, with the status `the bridge is paused by its owner;
  deposits finalize once it is lifted` — within `--anchor-timeout-s` (exit 31,
  resumable). A deposit somebody finalized before the pause — the operator's
  relayer, while the run was stopped — does not wait for it: the CLI looks for
  that before the pause, and confirms the credit.

### The deposit prover

`--deposit-prover-dir` is the working directory of the two tools the CLI
runs, built from `deposit-prover/` as it is:

```
<prover-dir>/
├── fetch_deposit_data
├── export_blake2b_proof
├── export_vk_blob                     not run by the CLI; the release check uses it
├── configs/circuit_params.json        read on every run, even with a cached key
└── data/
    ├── kzg_params_18.srs              the Hermez ceremony at degree 18, ~33 MB
    ├── deposit_prover_k18.<fingerprint>.pk       the proving key, ~1.27 GB,
    ├── deposit_prover_k18.<fingerprint>.pk.bp.json   written by the first proof
    └── .prove.lock
```

`scripts/install.sh` installs all of it. In a source checkout,
`scripts/stage_deposit_prover.sh ./deposit-prover` builds the tools, with
`deposit-prover`'s own `rust-toolchain.toml`, and lays them out where the
shipped profiles point. The SRS then goes into `./deposit-prover/data/`: take
`kzg_params_18.srs` from a release and check it against that release's
`SHA256SUMS`. The release pipeline derives the file from the Hermez k=21
`kzg_bn254_21.srs` with `deposit-prover`'s `downsize_srs` instead of
downloading it, because the storage `deposit-prover/download_trusted_setup.sh`
points at refuses access. From a release that does not carry it yet, derive
it the same way from that release's `kzg_bn254_21.srs`; the staging script
has already built `downsize_srs`:

```bash
../../deposit-prover/target/release/examples/downsize_srs \
  --input kzg_bn254_21.srs --output ./deposit-prover/data/kzg_params_18.srs --k 18
sha256sum ./deposit-prover/data/kzg_params_18.srs
# from v0.2.0's kzg_bn254_21.srs: ca97cea5566cec45421f7b2a945c462da6cb759fd8d22791b41b4a3d479ffefc
```

Whatever its source, preflight refuses a file without the Hermez [s]·G2.

**Memory, disk and time**, next to `withdraw`:

| | `deposit` (k = 18) | `withdraw` (Circuit 4) |
|---|---|---|
| Peak RAM | ~4.4 GB with a cold key cache, ~3.9 GB warm | ~40 GB |
| Disk | ~1.3 GB in `data/` after the first proof, plus ~26 MB of tools | ~4.3 GB on a withdraw-only host (Step 0) |
| Proof, cold key cache (keygen included) | 42 s wall, 6 min 25 s CPU | ~20 min |
| Proof, warm key cache | 23 s wall, 4 min 31 s CPU | ~5 min |

Measured with `export_blake2b_proof` on
`deposit-prover/fixtures/deposit_10proofs/proof_00` (`--degree 18
--max-data-byte-len 256 --max-log-num 20`, as the CLI runs it) on an Intel Core
i5-14600KF with 20 hardware threads and 46 GB of RAM, Ubuntu 24.04 under WSL2.
Keygen took 20 s of the cold run. The prover uses every core, so with fewer
the wall time moves toward the CPU time; `fetch_deposit_data` adds a few RPC
calls. The default `--prover-timeout-s 1800` leaves a wide margin.

Proofs on one prover directory run one at a time. The CLI holds
`data/.prove.lock` for as long as `export_blake2b_proof` runs, and the prover
inherits it, so a prover that outlives a killed CLI still holds it. A second
deposit that reaches step 7 meanwhile waits with `the prover is busy with
another deposit; waiting for it` — two proofs at once would also need twice
the memory. Do not point a `deposit-relayer` at the same directory: it does
not take the lock, and the key cache is not safe to share without it.

**A damaged key cache.** A proof stopped while it was writing the proving key
— `--prover-timeout-s` running out on a cold cache, the machine running out of
memory, the process killed — can leave `data/deposit_prover_k18.*`
half-written, and the proofs after it fail (exit 32). Delete those files, not
`kzg_params_18.srs`, and `--resume <op-id>`; the next proof builds the key
again:

```bash
rm -f <prover-dir>/data/deposit_prover_k18.*
```

### Locks need `flock`

`deposit` keeps three `flock` locks, which the kernel releases when their
holder dies: `<state-dir>/deposit.lock` (one run at a time from the check for
unfinished operations to the confirmed deposit, so two runs cannot both ask a
wallet), `<state-dir>/<op-id>.lock` (one process per operation, `--resume` and
`--abandon` included) and `<prover-dir>/data/.prove.lock` (above). **So
`--state-dir` and `--deposit-prover-dir` must be on a filesystem that supports
`flock`.** On one that does not — NFS without a lock daemon, some FUSE and
container overlay mounts — `deposit` refuses with exit 2 before the wallet is
asked, and names the directory. Here it differs from `withdraw`, which carries
on without its lock and relies on its record: for a deposit the lock is what
keeps two runs from requesting the same deposit, and records alone cannot.

### Minimum bridge version

The CLI sends `finalizeDeposit` again whenever an earlier send's outcome is
unknown, and that is only safe on an Acki Nacki bridge that cannot mint one
deposit twice, whatever happens to its voucher code. The first such bridge
version is the build constant `MIN_BRIDGE_VERSION`
(`src/deposit/an_preflight.rs`). Preflight reads the bridge's `getVersion()`
and refuses an older bridge with exit 2. `--resume` refuses it as well, with
the exit code of the operation's stage, since the deposit may be on chain
already. A bridge newer
than the newest this build was checked against (`1.5.0`) gets a warning.

Until that bridge version exists, `MIN_BRIDGE_VERSION` is unset, and such a
build refuses every bridge: `this build does not know which bridge version it
needs`. The release pipeline refuses to build a tag in that state, so a
released CLI always names its minimum. To develop against a bridge without
that protection, a source build with `--features dev-unfixed-bridge` turns the
refusal into a warning (`development build … never ship this build`). No flag
does this in a released binary.

### When only the light client anchors

`getAnchorConfig()` returns the light client and whether owner anchors are
enabled.

- **Owner anchors enabled** (the shellnet deploy): the owner anchors the
  deposit's block by hand, with no time limit. The light client may anchor it
  too; preflight checks whether it could, but only to word the status line.
- **Owner anchors disabled**: only the light client anchors, and the owner can
  no longer help. Preflight lets a deposit through only if the light client
  demonstrably anchors blocks a deposit can use, and otherwise refuses with
  exit 2, naming the first check that failed:
  1. the light client follows the deposit's chain (`getConfig().l1ChainId`) —
     never the case for an L2, which the light client does not cover;
  2. the bridge accepts the light client's head block under its real hash;
  3. ancestry works and reaches the bridge after the switch: a block between
     two checkpoints, newer than the last `disableOwnerAnchors`, is anchored in
     the bridge; the light client's head is at most 1536 s behind the EVM
     chain's finalized head; its latest ancestry checkpoint is at most 768 s
     behind its head. The bounds are build constants, not flags.

  Ancestry does not run on Acki Nacki today — it does not fit the gas limit
  ([docs/eth-light-client.md](../../docs/eth-light-client.md)) — so check 3
  fails, and a bridge with owner anchors disabled takes no deposit from this
  CLI. The CLI starts accepting them by itself once the checks pass; that
  needs no new CLI release.

**The residual risk of this mode.** Preflight only shows that ancestry worked
recently. If it stops after the deposit is made, the block is not anchored
until ancestry runs again, and the owner cannot anchor it instead. The CLI
keeps checking the same freshness bounds while it waits; once the checkpoint
covering the deposit is proven and ancestry has not reached the block, the
status line says that the light client's ancestry has stopped and the operator
must restart it. The wait then runs to `--anchor-timeout-s` (exit 31,
resumable). The deposit stays in the EVM bridge, and only the operator can get
ancestry going again.

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
- `USDCBridge.abi.json` — the Acki Nacki bridge: the `initiateWithdrawal`
  body cell, and for `deposit` its getters, `finalizeDeposit` and its events
- `UpdateCustodianMultisigWallet.abi.json` — for the outer `sendTransaction`

`deposit` also compiles in `DepositVoucher.tvc` and the voucher and
light-client ABIs from `contracts/an/0.81.0_compiled/exchange/`: the
voucher's address is computed from its code.
