# `deposit-relayer` operator runbook

verified-against: `e529ad96ddc18b05df06b0658c91dfd5c30578fe` (PR base)
plus [PR #127](https://github.com/gosh-sh/bridge/pull/127) for queue, anchor
readiness, backoff, monitoring and the outstanding-deposit description in
section 1, and bridge-audit#139 (the relayer sends a receipt proof instead of
a Halo2 proof) throughout. After merge, replace this stamp with the merge commit. The rest of
section 1 and sections 5, 6 and 8 retain their original verification against
`6f5de05c5f21e9938fae00b99ec925e2b4e81087` rather than a fresh verification.

covers: `crates/deposit-relayer-daemon/src/`, `scripts/ursus/deposit-relayer.{service,env.example}`

How to run the EVM → Acki Nacki deposit relayer so that it does not get stuck, does not
skip anyone, and gets back on its feet after a crash, a redeploy or a lost host. It
answers audit questions QC-OFF-01, -02, -08 and -11. Claims cite the symbol that
implements them; where a statement is a recommendation rather than a property of the
code, it says so.

## 1. What the daemon owns, and what it does not

The daemon watches `AckiNackiBridge` on Ethereum for `Deposit` events. For each one it
reads the deposit block's header and the receipts of that block from the Ethereum RPC,
builds the Merkle proof of the deposit transaction's receipt, and calls
`finalizeDeposit(chainId, header, txIndex, receiptProof, logIndex)` on the AN-side
bridge (`witness::ReceiptWitnessBuilder`, `deposit_receipt::fetch`). The bridge checks
the header against its accepted blocks, walks the proof, reads the `Deposit` log out of
the receipt itself and deploys the deposit's `DepositVoucher`. The voucher mints the
USDC to the recipient through `confirmDeposit`. There is no prover: building the proof
takes a few RPC calls and no noticeable memory.

- **Source of truth: the `DepositVoucher` on AN.** A deposit is finalized exactly when
  its voucher exists. The voucher's address is derived from `(depositId, source
  contract, chainId)` and the voucher code the bridge carries
  (`voucher::voucher_account_id`). A second `finalizeDeposit` for the
  same deposit fails in the voucher constructor with exit 51, so no deposit can be minted
  twice, whoever submits it (`submitter::classify_exit`).
- **`state.json` is a cache plus a to-do list** (`state::RelayerState`). It holds the
  high-water mark (`last_processed_deposit_id`), unfinished ids waiting for
  anchors, pending submissions or retries (`outstanding_deposits`), and the
  log-scan cursor (`scan_done_through_block`), all of
  which can be rebuilt from the chain. It also holds `parked_deposit_ids`, which
  nothing else records. **Losing the file loses that list, and on failover without a
  copy also loses every outstanding id below the new `START_DEPOSIT_ID`.**
- **USDC never leaves Ethereum because of the relayer.** If the daemon is down, deposits
  wait; nobody loses funds, and they are delivered once a relayer runs again.

## 2. Setup that avoids the known problems

| Setting | Value | Why |
|---|---|---|
| Daemons per AN bridge | **exactly one** | `StateLock` (an advisory `flock` on `<state>.lock`) only stops a second daemon on the *same host and file*. Two hosts with the same keys would both submit every deposit. The vouchers keep that safe, but the bridge pays gas twice and the two senders compete. |
| `DEPOSIT_RELAYER_STATE_DIR`, `DEPOSIT_RELAYER_LOG_DIR` | two absolute directories on persistent disk (`--state-dir`, `--log-dir`; bridge-audit#132) | The state directory holds `deposit-relayer-state.json`, its lock and the temporary file of its atomic write; the log directory the daemon's own log (a file per day, beside stdout). Nothing goes to the system temp directory or `$HOME`. `--work-dir` / `DEPOSIT_RELAYER_WORK_DIR` are gone with the prover; remove them from the command line. |
| State file backup | copy it every few minutes, with the keys | The file is replaced atomically (temp file, `fsync`, `rename`, in `RelayerState::save`), so a plain `cp` always reads a complete file. The copy is what keeps `parked_deposit_ids` if the host dies. |
| bridge ABI | built into the binary (`crates/ackinacki-bridge/abi/USDCBridge.abi.json` at build time) | No ABI file is read (bridge-audit#132); a daemon built from a commit whose ABI lacks a getter the contract has is upgraded, not reconfigured. |
| `RPC_URL` | an endpoint that serves `eth_getLogs`, `eth_getTransactionReceipt`, `eth_getBlockByHash` and **`eth_getBlockReceipts`** for at least the last 7 days, 30 preferred, and `eth_getLogs` back to `FROM_BLOCK` while `state.json` has no cursor | The scan finds a deposit with `eth_getLogs`; the receipt proof needs the receipt of every transaction in the deposit's block, read in one `eth_getBlockReceipts` call. A node without that method fails every deposit as `proof_failed`. A node that keeps ~10 000 blocks, as some public endpoints do, drops receipts after about a day on Ethereum and within an hour on Arbitrum; a deposit older than that cannot be finalized through it. The margin is what lets the relayer catch up after a stall. |
| `FROM_BLOCK` / `BRIDGE_DEPLOY_BLOCK` | at or just below the oldest deposit still to deliver | The log scan starts here until `state.json` has a cursor. If it is above an undelivered deposit, every tick fails, naming the blocks it scanned (`EthLogSource::fetch`). |
| `START_DEPOSIT_ID` | first deposit not yet finalized on the current AN bridge | Used only while `state.json` has no progress. A too-low id is only a cheap re-check when `FROM_BLOCK` still covers that deposit (the voucher is active, so it is not submitted again). If `FROM_BLOCK` is above it — the row above, "just below the oldest still to deliver" — `fetch` finds no log and every tick fails; raise `START_DEPOSIT_ID` or lower `FROM_BLOCK`. |
| `SKIP_AFTER_ATTEMPTS` | **recommendation:** a value that takes a few hours of backoff to reach (for example 64) | 0 (the default) never parks: a broken deposit stays in `outstanding_deposits` and later ids still go through. That queue also contains normal anchor waits; monitor failed attempts and prolonged waits as described in §7. |
| `--backoff-initial-secs` / `-max-secs` / `-multiplier` | defaults 5 / 120 / 2 | A zero initial delay or a multiplier of 0 is rejected at start-up (`BackoffConfig::validate`); either would busy-loop. |
| `DEPOSIT_RELAYER_METRICS_ADDR` | loopback or a monitoring-only interface | Prometheus `GET /metrics`; alerts in §7. |
| systemd | `Restart=on-failure`, `TimeoutStopSec=180`, `KillMode=mixed` (as in `scripts/ursus/deposit-relayer.service`) | Shutdown is checked only between ticks; a tick waits up to 60 s for the AN confirmation. |
| GraphQL endpoint | HTTPS, on a node that also serves `/v2/messages` | A gateway that only answers `/graphql` cannot submit. Plain HTTP needs `--allow-insecure-graphql`. |

## 3. The queue: new work, outstanding retries, parking (QC-OFF-01)

The daemon alternates new work with ready outstanding retries, so continuous
arrivals do not hold older deposits indefinitely and a stuck retry does not
block new deposits. `next_target` walks `last_processed + 1`, skipping parked,
unprovable (cannot be finalized from the receipt, §4) and outstanding ids (`RelayerState::next_target_at`). When that walk
has nothing new to fetch, one ready outstanding id is retried on the same tick.
Retries rotate through finite rounds; new entries join the next round instead
of extending the current round indefinitely. After restart, an existing ready
outstanding deposit gets the first turn. Retries still honor per-id backoff
(`--outstanding-backoff-secs` / `OUTSTANDING_BACKOFF_SECS`, default
2 s, doubled each *failed* attempt up to 64×). A pending submit does not increment
`attempts`, so it stays at the base delay until a receipt-proof failure or an AN
rejection.
Anchor waits also increase their per-id delay with age: the larger of the
existing failure backoff and `min(wait_age_seconds / 4, 60)` seconds, where
age starts at `anchor_wait_started_unix`. Thus an old uncovered block does
not cause an anchor getter, voucher lookup and state write on every daemon
tick. `OUTSTANDING_BACKOFF_SECS=0` disables per-id delays. These deadlines
survive restarts and do not delay fresh deposits behind older waits.
A retry uses the cached event when available, otherwise the stored Deposit
tx's receipt (or its block), so the log-scan cursor can keep moving with later
deposits. An outstanding id that has no stored log — a `state.json`
written by an earlier relayer that only recorded `attempts_since_progress` — is
scanned from the `scan_done_through_block` that file had on load, plus one,
with a fallback to `--from-block` if that snapshot was already past the deposit.

A stuck deposit no longer holds every later user. What gets the stuck one moving:

1. **On its own**, a transient failure (RPC blip, a node missing the block's receipts) is
   retried with that
   per-id backoff on retry turns. A pending submit is retried at the base delay (it
   does not count as an attempt). A retry with a stored tx uses the cached
   event or receipt and makes no `eth_getLogs` walk from the shared cursor.
2. **With `--skip-after-attempts N`**, after N consecutive *failed* attempts (a
   receipt-proof failure or a rejection by AN) the deposit is **parked**. It is appended to
   `parked_deposit_ids`, dropped from outstanding, and the log shows `daemon: deposit
   parked; run finalize-one manually`. Anchor waits, pending submits (§4) and
   "deposit not made yet" do not count toward N. Pending still goes into
   `outstanding_deposits` with backoff, so
   a paused lane does not hold later ids and does not walk those ids toward parking.
3. **By hand**, finish a parked deposit:

   ```bash
   deposit-relayer prove-one --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
     --deposit-id <N> --from-block <block at or below the deposit> \
     --out-dir ./bundle-<N>                                  # writes ./bundle-<N>/witness.json
   deposit-relayer finalize-one --bundle-dir ./bundle-<N>   # AN_* settings from the env file
   ```

   `--tx-hash` with `--log-index` (the position of the log inside the transaction's
   receipt) skips the log scan. Fix the cause first (table in §5); a deposit AN rejected
   will be rejected again. Then `finalize-one` reports `ACCEPTED` or `already
   finalised`. `witness.json` is the block header and receipt proof; it does not go
   stale, but it only finalizes once the AN bridge has accepted that block. Neither command takes the
   state lock, so the daemon can keep running.
4. **Clear the entry.** The daemon never removes ids from `parked_deposit_ids`, and the
   `deposit_relayer_parked_deposits` gauge counts them. Stop the daemon, delete the
   finished ids from the file, then start it again. An edit made while the daemon runs
   is overwritten by its next save.

## 4. Pending, timeouts and backoff (QC-OFF-11)

Each tick ends in one outcome (`relayer::TickOutcome`). Progress or the first
observed wait for a deposit's anchor resets the daemon delay to
`--backoff-initial-secs`, including after idle polls or errors. Repeat anchor
waits reset idle backoff only when no failure backoff is active. After a tick
error, receipt-proof failure or AN rejection, repeat waits preserve the current delay;
further failures increase it up to `--backoff-max-secs`. Progress or a newly
observed wait clears that failure streak (`Relayer::run_until_shutdown`).
Thus a permanently uncovered block cannot repeatedly cancel backoff for a
failing forward RPC, while healthy anchor polling stays at the initial daemon
interval. Per-deposit retry deadlines and age-based delays (§3) still apply.

| Outcome | Meaning | Automatic handling | Counts toward parking |
|---|---|---|---|
| `finalized`, `already_finalized` | voucher deployed / already there | next deposit | resets the count |
| `not_yet_available` | deposit not made, or not `--confirmations` deep | wait | no |
| `waiting_for_anchor` | the confirmed deposit's block is not yet accepted by the AN bridge | save the log reference; a first wait resets daemon delay, repeat waits preserve failure backoff and otherwise reset idle delay; honor per-id retry deadlines; nothing is built or broadcast, later ids remain runnable | **no** |
| `an_pending` | the outcome is not known yet | outstanding with backoff, no extra attempt; ready retries alternate with new work. On the next retry the voucher pre-check (`AnSubmitter::is_finalized`) answers before anything is sent again | **no** |
| `proof_failed` | building the receipt proof failed: an RPC error, a node that does not serve `eth_getBlockReceipts` or no longer has the block, receipts that do not rebuild the block's `receiptsRoot`, or a deposit that moved to another block | retried | yes |
| `an_rejected` | AN refused the deposit (exit code in the log) | retried with a freshly built proof | yes |
| `unprovable` | the deposit cannot be finalized from its receipt: the transaction failed, the log is not a `Deposit` in the one shape the AN bridge reads (3 topics, 4 data words), the transaction or log index is over 65535, or the AN bridge could not check the receipt proof within the 500 000 gas an external message may spend before it is accepted (`deposit_receipt::AN_GAS_CREDIT`; about 75 or more ERC-20 transfers before the `Deposit` in its receipt) | recorded in `unprovable_deposit_ids` and skipped at once; nothing is sent. `finalize-one` cannot help; only the EVM bridge operator can return the funds | no |
| `skipped` | parked after N failures | next deposit; manual finish (§3) | resets the count |
| `error` | the tick itself failed (RPC down, state file unwritable) | retried with backoff | no |

`an_pending` also covers temporary bridge and message refusals:

- `finalizeDeposit` was not confirmed within the fixed 60-second confirm timeout
  (`AnSubmitConfig::confirm_timeout_secs`; no CLI flag);
- it exited 0, but its voucher deploy and `confirmDeposit` were not both visible within
  that time (`AnInterfaceSubmitter::settle`);
- the lane is paused, `ERR_PAUSED` (231), or source-chain capacity is full (235);
- the token is not registered for the source chain (241), or its deposits are closed on AN (243).
  Both clear once the owner calls `setBridgeToken`;
- an external message was already recorded or expired (400 / 402). Observe the
  voucher before retrying; this does not establish that its mint succeeded.

Before building the receipt proof, the daemon reads `isAcceptedBlockHash(chainId, blockHash)`
on its configured AN token bridge. `--confirmations` controls Ethereum log
discovery; it does not guarantee that the light-client relayer has delivered
the block's anchor. A `false` result is `waiting_for_anchor`, not an AN rejection.
A getter error or malformed response remains an `error` and does not allow
submission. An actual `finalizeDeposit` rejection is still
`an_rejected`; the readiness check does not suppress contract failures.

Waiting deposits keep `anchor_wait_started_unix` in their outstanding entry.
The first wait time survives retries and restarts and is cleared once the
anchor is observed (or the deposit is already finalized). Existing state files
without this optional field remain readable.

At the default `info` log level, the daemon logs the start of each anchor wait
once with `deposit_id`, `chain_id`, `block_number` and `block_hash`. It also
warns immediately by default, then at most once every 15 minutes per deposit.
`--anchor-wait-warn-after-secs` (`ANCHOR_WAIT_WARN_AFTER_SECS`, default `0`)
sets the warning delay from the first observed wait. Use a positive delay
only when block coverage is monitored separately and expected anchor delivery
latency is understood; missing anchors can need manual action. The optional
`anchor_wait_last_warned_unix` timestamp survives restarts; existing state
files without it remain readable.
The initial warning says the wait has started; later warnings distinguish
continued waiting from exceeding a configured delay. Expected anchor delivery
may still be in progress. Warnings identify the deposit before saving state,
so a write error does not hide the diagnostic. These warnings do not count
as failed attempts or park the deposit.
Inspect the light-client relayer and block coverage: some blocks can remain
uncovered rather than merely delayed (`docs/eth-light-client.md`, §3.4).
For an anchor wait nothing has been built yet: compare the logged `block_number`
and `block_hash` using `eth_getBlockByNumber` (hex block number, `false`) on
an independent RPC for the logged `chain_id`. Check the returned hash matches
and the block has the required finality. Confirm that manual admission is
needed, rather than normal delivery still being in progress, before the
anchor writer admits a missing hash with
`setAcceptedBlockHash(chainId, blockHash, true)` while owner anchors are
enabled, otherwise through the light client. The deposit daemon never
admits hashes itself.

For pending submissions, if the submit did land, the next tick sees the
voucher and moves on without submitting again. If it did not, a later tick
builds the proof and submits again once the anchor is present, and a duplicate
cannot mint. Pending submissions need no manual action unless the pending status
persists (alert in §7). If it does, check the AN endpoint and `isPaused()`
on the bridge.

## 5. Rejections: what the exit code tells you

The daemon logs the decoded reason (`submitter::describe_exit_code`,
`describe_voucher_exit`). Exit codes that point at bridge configuration hit every
deposit, so parking will not help; fix the setting, and parked deposits then finish with
`finalize-one`.

| Exit | Where | Cause | Fix |
|---|---|---|---|
| 222 `ERR_UNSUPPORTED_SRC_CHAIN` | `finalizeDeposit` | `(chainId, source contract)` not allowlisted | owner: `setTrustedL1Bridge(chainId, l1Bridge, true)` |
| 224 `ERR_UNKNOWN_BLOCK` | `finalizeDeposit` | the deposit's block hash is not an accepted anchor | confirm with `scripts/deposit_anchor_params.py --verify`, then the chain's anchor writer admits it: `setAcceptedBlockHash` while owner anchors are on for that chain, the light client for Ethereum, the Arbitrum block verifier (`eth-lc-relayer arbitrum-anchors`) for Arbitrum |
| 204 / 230 | `finalizeDeposit` | zero amount / zero recipient was proven | genuinely bad deposit; leave it parked |
| 214 `ERR_OVERFLOW` | `finalizeDeposit` | the proven amount does not fit a uint64 | genuinely bad deposit; leave it parked |
| 244 `ERR_NOT_DEPOSIT_LOG` | `finalizeDeposit` | the log at `logIndex` of the proven receipt is not a `Deposit` with 3 topics and 4 data words | the EVM bridge emitting it is not the one this AN bridge reads; the relayer refuses such a log itself, so this points at a relayer bug or a mismatched deployment |
| 300–303, 305 (`EthTrie`) | `finalizeDeposit` | the receipt proof does not lead from the header's `receiptsRoot` to the receipt at `txIndex` (300 short, 301 malformed RLP, 302 node hash, 303 path, 305 no receipt at that index) | retried with a freshly built proof; if it keeps failing, the RPC serves receipts that are not the block's: point `RPC_URL` at another node |
| 304 `ERR_INLINE` (`EthTrie`) | `finalizeDeposit` | the block's receipt trie has a node under 32 bytes, which the bridge cannot walk | cannot be finalized by the relayer; contact the operator |
| 307 `ERR_NO_LOG` (`EthTrie`) | `finalizeDeposit` | the proven receipt has no log at `logIndex` | `logIndex` counts within the receipt, not the block; the relayer computes it from the receipt, so this points at a relayer bug |
| 308 `ERR_RECEIPT_STATUS` (`EthTrie`) | `finalizeDeposit` | the deposit transaction failed on the source chain | no deposit was made; the relayer refuses a failed transaction itself (`unprovable`) |
| 234 `ERR_TRANSFER_TOO_LARGE` | `finalizeDeposit` | proven amount exceeds 50 USDC | contact the operator; old oversized transfers must settle before activating the new pair |
| 235 `ERR_TVL_CAP` | `finalizeDeposit` | chain capacity is full | retried without counting a failed attempt; wait for withdrawals |
| 400 / 402 | message header | identical recorded message / expired message | pending; observe the voucher before retrying |
| 231 `ERR_PAUSED` | `finalizeDeposit` | the lane is paused | pending until `setPaused(false)` |
| 401 | message header | expiry is at least five minutes ahead of block time | use the matching client and check clock synchronization; this build caps finalization expiry at 240 seconds |
| 9 | voucher deploy | the bridge's `_depositVoucherCode` predates the voucher ABI (the 2026-07-02 outage) | recompile and redeploy the bridge and voucher as a pair |
| 51 | voucher deploy | already finalized | none; reported as `already_finalized` |

Settings 222 and 224 do not survive an AN bridge code upgrade. After every deploy or
upgrade they must be set again before the first deposit can finalize.

## 6. `state.json` across redeploys (QC-OFF-08)

The file is stamped with the deployment it belongs to: EVM chain id and EVM bridge
address (`state::DeploymentIdentity`). Files written while the relayer took `AN_DAPP_ID`
also carry a `dapp_id`; it is ignored on load and dropped on the next save, so an existing
file keeps working. If a daemon starts with a different one,
it refuses with `state.json deployment mismatch`, so a cursor from one bridge cannot
silently skip another bridge's deposits.

`--force-state` accepts the mismatch by **starting the file over**. Progress, scan
cursor, parked ids, outstanding ids and `last_finalized_deposit_id` are discarded, the
old values are logged at `warn`, and the daemon begins at `--start-deposit-id`
(`RelayerState::ensure_deployment`). Checklist before using it:

1. Is this really a new deployment? A typo in `BRIDGE_ADDRESS` also mismatches. Fix the
   env instead.
2. Write down `parked_deposit_ids` and the ids in `outstanding_deposits` from the old
   file. They are dropped, and each one still needs `finalize-one` against the bridge
   it belongs to.
3. Set `START_DEPOSIT_ID` (0 for a new EVM bridge) and `FROM_BLOCK` (its deploy block).
4. Keep a copy of the old file, then start once with `--force-state`, and remove the
   flag again. Leaving it on accepts the next mismatch without asking.

Other cases:

- **Crash between submit and save.** The next tick finds the voucher and records the
  deposit as `already_finalized`; nothing is submitted twice.
- **A corrupt or unreadable file** stops the daemon at start-up. Restore the backup. If
  there is none, deleting the file is safe apart from the parked and outstanding
  ids: the daemon restarts at `START_DEPOSIT_ID` and skips finalized deposits by
  their vouchers. An outstanding id below `START_DEPOSIT_ID` is never retried.
- **Two daemons on one file**: the second fails to take the lock and exits.

## 7. Monitoring

Suggested alert rules over the exported metrics (`metrics` module). The thresholds are
recommendations, not tested values.

| Condition | Meaning |
|---|---|
| `time() - deposit_relayer_last_tick_timestamp_seconds > 900` | daemon dead or hung (a tick takes seconds, at most about a minute while it waits for AN) |
| `deposit_relayer_unprovable_deposits > 0` | a deposit cannot be finalized from its receipt; the EVM bridge operator has to return its funds |
| `deposit_relayer_outstanding_deposits > 0` | deposits remain to retry, including normal anchor waits; inspect their outcome and age before treating this as a failure. `target_deposit_id` is the forward id on an idle relayer, so `deposit_counter - target` stays 0 while outstanding ids sit below it |
| `deposit_relayer_waiting_for_anchor > 0` | at least one block anchor is missing; the default warning identifies the deposit and block immediately. Notify the operator promptly if block coverage can require manual admission. For separately monitored automatic coverage, use an age budget based on expected delivery latency with `time() - deposit_relayer_oldest_anchor_wait_timestamp_seconds`; a fixed hour is not a universal grace period. The timestamp is 0 when no deposit is waiting and survives restarts |
| `deposit_relayer_eth_deposit_counter - deposit_relayer_target_deposit_id > 0` and `time() - deposit_relayer_last_finalized_timestamp_seconds > 3600` | new deposits are waiting at the high-water mark and none was delivered for an hour |
| `deposit_relayer_attempts_since_progress >= 3` | the last failed outstanding id's attempt count; it resets when *any* later id finalizes, so it is not a stuck-queue signal on its own |
| `deposit_relayer_parked_deposits > 0` | manual finish needed (§3) |
| `increase(deposit_relayer_ticks_total{outcome="an_pending"}[30m]) > 10` | AN slow, unreachable, or the lane paused |
| `increase(deposit_relayer_eth_get_logs_total{outcome="error"}[15m]) > 0` | the Ethereum RPC is refusing log queries |

`deposit_relayer_last_finalized_deposit_id` is seeded from `state.json` on start and
only moves up. `deposit_relayer_target_deposit_id` shows the retried id during a
retry, then the forward id again. Use `deposit_relayer_outstanding_deposits` for the
stuck-id backlog.

## 8. When the primary is down (QC-OFF-02)

The daemon is a single point of liveness, not of safety: while it is down, deposits
wait. Nobody can lose funds or be paid twice.

**Recommendation:** a cold standby rather than a second live daemon. Prepare a host with
the same binary, env file and AN keys, and keep the service disabled. To fail over:

1. Make sure the primary is really stopped (`systemctl stop`, or the host powered off).
   The lock does not reach across hosts.
2. Copy the latest backup of `deposit-relayer-state.json` into the standby's `DEPOSIT_RELAYER_STATE_DIR`,
   including `outstanding_deposits`. If there is no backup, start without a file and
   set `START_DEPOSIT_ID` to the primary's last `target_deposit_id` **only if that
   file had no outstanding ids**. `target_deposit_id` is the forward id; every
   outstanding id below it is then never delivered, and nothing records it. If it is
   lower, keep `FROM_BLOCK` at or below that deposit — then the daemon only re-checks.
   A `FROM_BLOCK` above it makes every tick fail (`EthLogSource::fetch` finds no log).
3. Start the service and watch for `finalized` / `already_finalized` ticks.

A one-off `finalize-one` by another operator while the daemon runs is safe: the slower
submit gets exit 51 and is reported as already finalized.

No service-level targets are defined in the code. How quickly a dead daemon must be
replaced, and how long a parked deposit may wait, is a production decision for the
bridge operator.

## 9. Deposits from an Arbitrum chain: `arbitrum-anchors`

`daemon` finalizes a deposit only once its block is in the bridge's anchor set
(`isAcceptedBlockHash`). For a bridge on Arbitrum Sepolia or Arbitrum One,
`eth-lc-relayer arbitrum-anchors` puts it there: it calls
`ArbitrumBlockVerifier.proveBlock` on Acki Nacki
(`crates/eth-light-client-relayer/src/arbitrum.rs`). Build `eth-lc-relayer` with
`--features live-submit`. Run one per Arbitrum chain, next to that chain's `daemon`.

| Setting | Value | Why |
|---|---|---|
| `PARENT_RPC_URL` | Sepolia for Arbitrum Sepolia, Ethereum for Arbitrum One; must serve `eth_getBlockReceipts` | The assertion and its receipt are on the parent chain. Checked against the Arbitrum chain at start-up. |
| `ARBITRUM_RPC_URL` | a node that serves `eth_getProof` on blocks a few minutes old, such as a free Alchemy key (which allows 10 blocks per `eth_getLogs`: add `--arbitrum-log-span 10`) | The proofs are taken when each batch lands, 40–60 s after its last block. `sepolia-rollup.arbitrum.io` and publicnode keep about 128 blocks of state, half a minute on Arbitrum, and miss many batches; a missed batch waits for the next one. |
| `--bridge-address`, `--from-block` | `AckiNackiBridge` on the Arbitrum chain and its deploy block | Its `Deposit` logs are the blocks to anchor. Without a state file the relayer refuses to start without `--from-block`. |
| `AN_GRAPHQL_URL`, `AN_KEYS_PATH`, `AN_TOKEN_BRIDGE` | as for `daemon` | Any key signs: `proveBlock` is permissionless. The bridge and verifier ABIs are built in; the verifier's address comes from the bridge (`getL2Verifier`), and the relayer refuses to start before `deployL2Verifier` or when the verifier's `getConfig()` names another chain, parent chain or rollup. |
| `ARBITRUM_ANCHORS_STATE_DIR`, `ARBITRUM_ANCHORS_WORK_DIR`, `ARBITRUM_ANCHORS_LOG_DIR` | three absolute directories on persistent disk (`--state-dir`, `--work-dir`, `--log-dir`) | The state directory holds `arbitrum-anchors-state.json`, backed up like the deposit relayer's: it keeps the proofs taken at batch time until their assertion lands, 30–60 min later, and they cannot be fetched again. The work directory stays empty (the command starts no prover); the log directory holds its own log. |

The proofs wait in the state file until the bridge accepts the parent-chain block
that holds the assertion. That is the parent chain's light client: run
`eth-lc-relayer` with `--submit-ancestry`, since an assertion is rarely in a
checkpoint block. While owner anchors are on, the owner can accept the block by hand
instead.

A restart loses nothing that is in the state file. A batch that lands while the
relayer is down costs a delay: the next batch covers the same blocks, as long as
the deposit block is within the 393 168 blocks (about 27 hours) an Arbitrum
block's state remembers. A deposit block older than that is out of every later
batch's reach: the relayer logs it as an error and stops trying. Only the
owner's `setAcceptedBlockHash` can anchor it then, and only while owner anchors
are on. A fresh state file starts from the current
parent-chain block.

`eth-lc-relayer arbitrum-vector --out <file>` writes one `proveBlock` vector from
the live chains, built the way `arbitrum-anchors` builds its calls, and sends
nothing. The bench (`scripts/l2-block-proof-bench`) and the local-network test take
that file.
