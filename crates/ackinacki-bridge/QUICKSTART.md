# Withdrawing USDC — quick start

Moves USDC from an Acki Nacki multisig to an address on an EVM chain. One
withdrawal takes **about 50 minutes**, most of it spent waiting for the
bundle that covers your burn; the worst case is ~101 minutes.

This page is the short path. [README.md](README.md) is the full runbook, and
[docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md)
is for people running their own bridge deploy.

**The burn is irreversible.** Once stage 3 broadcasts, the USDC has left the
multisig whatever happens next. Everything before it is designed to refuse
instead of guessing, which is why the first two steps below are worth the
time they take.

---

## 1. Install

**Starting from nothing** — no Rust, no checkout:

```bash
curl -fLO https://raw.githubusercontent.com/gosh-sh/bridge/main/crates/ackinacki-bridge/scripts/bootstrap.sh
less bootstrap.sh        # it installs software; read it before running it
bash bootstrap.sh
```

It installs a C toolchain, git and curl through your package manager (showing
you the command and asking first), installs rustup, clones the repository
into `~/ackinacki-bridge`, and then runs the step below. `--dir` puts the
clone elsewhere, `--check` reports without installing.

**If you already have the repository:**

```bash
cd crates/ackinacki-bridge
scripts/install.sh --check      # what is missing
scripts/install.sh              # install it, asking first
```

That provisions the four things a real withdrawal needs: `solc 0.8.19`, the
Hermez KZG ceremony (~256 MB on disk, ~2.4 GB downloaded once), the
`aggregate-proof` binary and the CLI itself. Budget about 45 minutes and
~20 GB of disk, most of both being the ceremony and the build.

`--dry-run` needs none of them. A real withdrawal needs all four, and stage 1
refuses without them — before anything is broadcast.

## 2. What you need to have

- **A single-custodian AN multisig** holding at least the amount in ECC[3],
  and its owner key file at mode `0400`. `scripts/deploy_msig_and_mint.sh`
  deploys one and seeds it with 1 USDC if you are testing; it needs
  `tvm-cli` on `PATH`, which the CLI itself does not.
- **A Sepolia wallet with ~0.02 ETH** to pay for `withdrawByProof`. Use a
  fresh, disposable one — never a wallet holding real funds.

## 3. Set your values

```bash
export BRIDGE_CONFIG=$PWD/config/bridge_config   # endpoints, bridge address

export WITHDRAW_FROM=<dapp_id>::<account_id>     # the AN multisig
export WITHDRAW_FROM_KEYS=/path/to/owner.keys.json
export WITHDRAW_TO=0xYourRecipient
export WITHDRAW_TO_CHAIN=11155111                # Sepolia
export WITHDRAW_AMOUNT=1.000000                  # USDC, at most 6 decimals

export BURNER_PRIVATE_KEY=0x…                    # the Sepolia signer
```

Everything else — RPC and GraphQL endpoints, the bridge address, the prover
directories — comes from the profile. Exported shell variables win over it,
so if you change the profile, open a new shell or re-`export`.

## 4. Dry run

```bash
BIN=../bridge-prover-libraries/target/release/ackinacki-bridge

$BIN withdraw --dry-run \
  --from "$WITHDRAW_FROM" --from-keys "$WITHDRAW_FROM_KEYS" \
  --to "$WITHDRAW_TO" --to-chain "$WITHDRAW_TO_CHAIN" \
  --amount "$WITHDRAW_AMOUNT"
echo "exit=$?"
```

Exit 0 means nothing about either chain is misconfigured: the multisig is
active and single-custodian, the key file is readable and matches the
on-chain custodian, ECC[3] covers the amount, the RPC answers on the right
chain id, the bridge is deployed with a complete verifier stack, and its
treasury can pay you.

It does **not** mean a real run will succeed — a dry run has no prover
plumbing, so it never checks `solc`, the ceremony or the key cache. That is
what `scripts/install.sh --check` is for.

Nothing is broadcast and nothing is written to disk, and it never
prompts — the confirmation belongs to the burn, which a dry run skips.

## 5. The real withdrawal

```bash
$BIN withdraw \
  --from "$WITHDRAW_FROM" --from-keys "$WITHDRAW_FROM_KEYS" \
  --to "$WITHDRAW_TO" --to-chain "$WITHDRAW_TO_CHAIN" \
  --amount "$WITHDRAW_AMOUNT"
echo "exit=$?"
```

It prints what it is about to do and waits for `y`. Pass `--yes` to skip
that; pass `--non-interactive` to refuse rather than prompt, which is the
shape a CI wrapper wants.

Six stages, and the log names each one:

| Stage | What happens | How long |
|-------|--------------|----------|
| 1 preflight | every check above, plus the prover artifacts | seconds |
| 2 reserve | writes the idempotency record | instant |
| 3 burn | **the irreversible step** — USDC leaves the multisig | ~3 s |
| 4 capture | finds your `WithdrawalInitiated` event | ~2 s |
| 4b coverage | waits for the bundle covering your block | **~45–91 min** |
| 5 prove | Circuit-4 proof and SHPLONK aggregation | ~3–20 min |
| 6 submit | `withdrawByProof` on the EVM chain | ~30 s |

Stage 4b is the wait. Leave it running.

## 6. Exit codes

| Code | Meaning | Your USDC |
|------|---------|-----------|
| 0 | paid out | delivered |
| 2 | refused before sending | untouched |
| 3 | this exact withdrawal already has a record | see below |
| 10 | do not treat this withdrawal as untouched | may be burned |
| 11 | burned, the event was not captured in time | burned |
| 12 | burned, the proof failed | burned |
| 13 | burned, `withdrawByProof` reverted | burned |

**Codes 2 and 3 are safe.** Fix what the message names and run again.

**Codes 10–13 mean a burn may be, or is, on the wire.** Two rules:

1. **Never delete the state record.** It is the only local trace that a burn
   may have happened, and deleting it is how the same USDC gets burned
   twice. The refusal names the file.
2. **Re-run the same command with `--allow-retry`.** It resumes from
   whatever is already done — it does not burn again if the record carries
   an AN transaction hash.

If that does not resolve it, the refusal text names the runbook case to
read. Every one of them tells you what to check on chain first.

## 7. If you run it twice

Withdrawals are keyed on `(from, to, chain, amount)`. Running the identical
command again finds the record from the first run and refuses with exit 3
rather than moving money twice. To send the same amount again, change one of
the four — a different recipient, or an amount one micro-USDC apart.
