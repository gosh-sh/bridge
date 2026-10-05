# ackinacki-bridge — quick start

The short path for both directions:

- **[Withdrawing](#withdrawing-usdc)** moves USDC from an Acki Nacki multisig
  to an address on an EVM chain. About 50 minutes, most of it waiting for
  the bundle that covers your burn.
- **[Depositing](#depositing-usdc)** moves USDC from an EVM wallet to an Acki
  Nacki account. About 17 minutes by estimate, plus however long the bridge
  owner takes to anchor your block.

Both start with the same install, section 1. [README.md](README.md) is the
full reference, and
[docs/advanced_user_withdraw_runbook.md](docs/advanced_user_withdraw_runbook.md)
is for people running their own bridge deploy.

---

## 1. Install

```bash
curl -fLO https://raw.githubusercontent.com/gosh-sh/bridge/main/crates/ackinacki-bridge/scripts/install.sh
less install.sh          # it downloads and installs; read it before running it
bash install.sh
```

Nothing is compiled: it downloads the CLI, the `aggregate-proof` subprocess
it shells out to, the verifier bytecode and source the proof is checked
against, the Hermez KZG ceremony, the deposit prover, and the same ceremony
at degree 18 for deposits — about 350 MB in total, most of it the ceremony.
Neither Rust, a Solidity compiler nor a checkout of the repository is
needed. Every release asset is verified against the release's `SHA256SUMS`
before it is written, and the deposit ceremony also against the Hermez
[s]·G2. It wants ~8 GB free: withdrawal keys take ~6 GB, and the deposit
prover writes a ~1.3 GB proving key on its first proof.

`--check` reports what is missing and downloads nothing. `--prefix` installs
somewhere other than `~/.local/share/ackinacki-bridge`. When it finishes it
prints the two lines that put the install on your `PATH` and name the
profile.

A withdrawal's `--dry-run` needs none of the prover artifacts. A real
withdrawal needs all of them, and stage 1 refuses without them — before
anything is broadcast. A deposit checks its prover on every run, dry runs
included.

An installation made before deposits existed has no deposit prover: run the
new `install.sh` over it. It fetches what is missing and appends the deposit
settings to your profile, leaving the rest of it as it was.

---

## Withdrawing USDC

Moves USDC from an Acki Nacki multisig to an address on an EVM chain. One
withdrawal takes **about 50 minutes**, most of it spent waiting for the
bundle that covers your burn; the worst case is ~101 minutes.

**The burn is irreversible.** Once stage 3 broadcasts, the USDC has left the
multisig whatever happens next. Everything before it is designed to refuse
instead of guessing, which is why the first two steps below are worth the
time they take.

## 2. What you need to have

- **A single-custodian AN multisig** holding at least the amount in ECC[3],
  and its owner key file at mode `0400`. This is the wallet the USDC is
  coming out of, so you have it already. (If you are only testing, the
  repository's `scripts/deploy_msig_and_mint.sh` deploys one and seeds it
  with 1 USDC — that one needs a checkout and `tvm-cli`, neither of which a
  withdrawal does.)
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
plumbing, so it never checks the verifier files, the ceremony or the key
cache. That is what `scripts/install.sh --check` is for.

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

---

## Depositing USDC

Moves USDC from an EVM wallet to an Acki Nacki account, where it arrives as
eccUSDC. Your wallet signs; the CLI never sees its key. One deposit should
take **about 17 minutes** — an estimate from its parts, not a timed run —
plus however long the bridge owner takes to anchor your block by hand; the
proof itself takes under a minute.

**Once the deposit transaction is sent, the USDC is in the bridge.**
Everything before it refuses instead of guessing. Everything after it can
be resumed, except the two outcomes only the bridge operator can settle
(exit 35 and 37, section 12).

## 8. What you need for a deposit

- **The install from section 1.** It brings the deposit prover and its
  ceremony; `install.sh --check` tells you whether they are in place.
- **An EVM wallet that speaks WalletConnect, on a plain account** — see
  [section 13](#13-which-wallets-work) first. It needs the USDC you deposit
  and a little Sepolia ETH for two transactions, `approve` and `deposit`.
  On the shellnet deploy the token is Circle's Sepolia USDC,
  `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`, which Circle's faucet hands
  out.
- **The recipient**, the Acki Nacki account as `dapp_id::account_id`. If the
  account is deployed, the dapp must be the one it lives in; if it does not
  exist yet, the deposit creates it.
- **About 5 GB of free memory** for the proof.

## 9. Set your values

With the two lines `install.sh` printed in effect (`PATH` and
`BRIDGE_CONFIG`):

```bash
export DEPOSIT_TO=<dapp_id>::<account_id>        # the Acki Nacki recipient
export DEPOSIT_AMOUNT=1.000000                   # USDC, at most 6 decimals
```

Sepolia is the only deposit network in this version; Ethereum mainnet is
not one at all. Everything else — the RPC, the Acki Nacki endpoint, the
bridge address, the prover directory, the state directory — comes from the
profile.

## 10. Dry run

```bash
ackinacki-bridge deposit --dry-run --network sepolia \
  --amount "$DEPOSIT_AMOUNT" --to "$DEPOSIT_TO"
echo "exit=$?"
```

Exit 0 means the RPC is on Sepolia and serves whole blocks, the EVM bridge
is deployed and not paused, the Acki Nacki bridge answers, is new enough, is
not paused and trusts this EVM bridge, the recipient's dapp matches, and the
prover directory is complete with the right ceremony. It prints both
transactions and whom the wait for the anchor would wait for — `waiting for
the bridge owner` on shellnet. Nothing is sent and no wallet is involved.

A dry run still writes to disk: it creates the state and work directories if
they are missing and holds the state directory's lock while it runs. So it
answers exit 3 while another deposit run on this machine is between its
preflight and the confirmation of its deposit, or while an earlier deposit of
the same amount to the same recipient has an unknown outcome (section 12).

## 11. The deposit

```bash
ackinacki-bridge deposit --network sepolia \
  --amount "$DEPOSIT_AMOUNT" --to "$DEPOSIT_TO"
echo "exit=$?"
```

1. Preflight runs, then the CLI prints `operation <op-id> (keep it: …)`.
   **Keep the operation id.** It is how an interrupted deposit continues.
2. A QR code. In the wallet, open WalletConnect, scan it, and approve the
   connection. If the wallet is on another network, it is asked to switch
   to Sepolia or to add it.
3. **Sign the account check.** The wallet shows a message that starts with
   `Acki Nacki bridge deposit check` and names the operation, the bridge, the
   amount and the network. Signing it sends nothing and costs nothing: it
   shows the account is a plain one whose deposit can be proven.
4. **Confirm `approve`**, the bridge's spending limit, set to exactly the
   amount. It is skipped when your allowance already covers the amount. Keep
   the limit as proposed: a lower one stops the run with exit 21 before any
   USDC moves. So does an `approve` that does not show on chain within five
   minutes (`--pair-timeout-s`); run the deposit again.
5. **Confirm `deposit`**, the transaction that moves the USDC. Do not change
   the amount, and do not speed it up with different parameters: a deposit
   that differs from the request is not finalized by the CLI (exit 35).
6. From here the CLI works alone. On shellnet the status line reads `waiting
   for the bridge owner` and prints the call the owner has to make,
   `setAcceptedBlockHash(…)`. Pass it on if nobody is watching.

| Step | What happens | How long |
|------|--------------|----------|
| 1 preflight | both chains and the prover are checked | seconds |
| 2 wallet pairing | QR code, connection, account check | as fast as you confirm |
| 3 approve | the spending limit | a block after you confirm |
| 4 deposit request | **the USDC goes into the bridge** | a block after you confirm |
| 5 EVM confirmation | 12 confirmations | ~2.5 min |
| 6 block anchor | **the bridge owner anchors your block by hand** | **from ~13 min** |
| 7 proof | on this machine | under a minute |
| 8 finalizeDeposit | a message to the Acki Nacki bridge; it pays the gas | seconds |
| 9 credit | the eccUSDC reach the recipient, checked by the deposit's identity | seconds |

Step 6 is the wait. Leave it running; if it stops anyway,
`ackinacki-bridge deposit --resume <op-id>` picks it up.

The run ends with `deposit complete:` on stdout: the amount, the recipient,
the operation, the deposit transaction and its `depositId`, `anchored by:
bridge owner`, the `confirmDeposit` and delivery transactions, and the
recipient's balance before and after.

## 12. Deposit exit codes

| Code | Meaning | Your USDC |
|------|---------|-----------|
| 0 | credited | on Acki Nacki |
| 2 | refused before the deposit was requested; an `approve` may have been sent | not moved |
| 3 | another deposit is in the way; the message names it | untouched |
| 20 | the wallet did not pair; you rejected the connection, the check or the deposit; or it is a smart-contract account | untouched |
| 21 | `approve` failed, was rejected, or did not show on chain (or could not be confirmed) in time, or the limit was lowered | untouched |
| 22 | the deposit transaction reverted | not taken; gas spent |
| 30 | the deposit transaction has not been found yet | maybe sent |
| 31–34 | the deposit is on chain; the Acki Nacki side has not finished | in the bridge; resumable |
| 35, 37 | the CLI cannot finalize this deposit | in the bridge; only the operator can help |

**2, 3, 20, 21 and 22: nothing is stuck.** Fix what the message names and
run again.

**30 to 34: resume, never repeat.** Run `ackinacki-bridge deposit --resume
<op-id>`. The same command without `--resume` is a second deposit. After exit
30, look in the wallet first: if the deposit is there, resume; if the wallet
never sent it, `--abandon <op-id>` releases the operation. README.md, "After
exit 30", has the details.

**35 and 37: go to the operator.** The message says what to give them: the
transaction hash, the `depositId`, the amount, the bridge address and the
operation id.

## 13. Which wallets work

- **Mobile wallets with WalletConnect** scan the QR code. This is the main
  path.
- **Desktop wallets that take a WalletConnect URI** (`wc:…`) as text work
  too: `--uri-only` prints the URI instead of the picture, and `--qr-out
  pairing.png` also writes the QR code to a file.
- **Browser-extension wallets** (MetaMask in the browser and the like) do
  not take a `wc:` URI. In this version they work only with `--qr-mode eip681
  --from-address 0x…`: one `ethereum:` QR code per transaction. **The risk of
  this mode: such a code cannot ask for a type-2 (EIP-1559) transaction. A
  wallet that sends a legacy transaction makes the deposit unprovable, and
  the USDC stays in the bridge until the operator returns it (exit 35).** The
  CLI asks you to accept that before it shows the codes. If the wallet lets
  you choose, pick an EIP-1559 fee setting.

Refused before any transaction is sent (exit 20):

- **Safe and other smart-contract accounts, and ERC-4337 accounts.** Their
  transactions go through a contract or an EntryPoint, and the deposit
  circuit cannot prove them. The CLI refuses an account that holds code, and,
  with WalletConnect, one whose signature on the check message is not made
  with the account's own key — which catches an ERC-4337 account that is not
  deployed yet. `--qr-mode eip681` has no signature check: there an
  undeployed ERC-4337 account is not caught, and its deposit ends at exit 35
  with the USDC in the bridge. Deposit from a plain account.

Accepted, with a warning:

- **EIP-7702 accounts**, a plain account delegating to a contract. **Turn off
  gas sponsoring (a paymaster) and batched calls for this deposit.** A
  sponsored or batched transaction goes through another contract and cannot
  be proven: the USDC would stay in the bridge (exit 35).
