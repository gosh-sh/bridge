# `deposit-relayer` operator runbook

verified-against: `840bd120`
covers: `crates/deposit-relayer-daemon/src/`, `scripts/ursus/deposit-relayer.{service,env.example}`

How to run the EVM → Acki Nacki deposit relayer so that it does not get stuck, does not
skip anyone, and gets back on its feet after a crash, a redeploy or a lost host. It
answers audit questions QC-OFF-01, -02, -08 and -11. Claims cite the symbol that
implements them; where a statement is a recommendation rather than a property of the
code, it says so.

## 1. What the daemon owns, and what it does not

The daemon watches `AckiNackiBridge` on Ethereum for `Deposit` events. For each one it
proves the deposit with `deposit-prover` and calls `finalizeDeposit` on the AN-side
bridge, which deploys the deposit's `DepositVoucher`. The voucher mints the USDC to the
recipient through `confirmDeposit`.

- **Source of truth: the `DepositVoucher` on AN.** A deposit is finalized exactly when
  its voucher exists. The voucher's address is derived from `(depositId, source
  contract, chainId)` and the voucher code the bridge carries
  (`voucher::voucher_account_id`). A second `finalizeDeposit` for the
  same deposit fails in the voucher constructor with exit 51, so no deposit can be minted
  twice, whoever submits it (`submitter::classify_exit`).
- **`state.json` is a cache plus a to-do list** (`state::RelayerState`). It holds the
  next deposit to work on (`last_processed_deposit_id + 1`) and the log-scan cursor
  (`scan_done_through_block`), both of which can be rebuilt. It also holds
  `parked_deposit_ids`, which nothing else records. **Losing the file loses that list.**
- **USDC never leaves Ethereum because of the relayer.** If the daemon is down, deposits
  wait; nobody loses funds, and they are delivered once a relayer runs again.

## 2. Setup that avoids the known problems

| Setting | Value | Why |
|---|---|---|
| Daemons per AN bridge | **exactly one** | `StateLock` (an advisory `flock` on `<state>.lock`) only stops a second daemon on the *same host and file*. Two hosts with the same keys would both prove and submit every deposit. The vouchers keep that safe, but it wastes proofs and the two senders compete. |
| `DEPOSIT_RELAYER_STATE` | an absolute path on persistent disk | Read as `--state`. Without it, the file lands in the working directory as `./deposit-relayer-state.json`. |
| State file backup | copy it every few minutes, with the keys | The file is replaced atomically (temp file, `fsync`, `rename`, in `RelayerState::save`), so a plain `cp` always reads a complete file. The copy is what keeps `parked_deposit_ids` if the host dies. |
| `AN_DAPP_ID` | `0` | `eccUSDCBridge` pins the deposit's dappId to 0 (QC-OFF-09). Changing it later trips the deployment binding (§6). |
| `FROM_BLOCK` / `BRIDGE_DEPLOY_BLOCK` | at or just below the oldest deposit still to deliver | The log scan starts here until `state.json` has a cursor. If it is above an undelivered deposit, every tick fails, naming the blocks it scanned (`EthLogSource::fetch`). |
| `START_DEPOSIT_ID` | first deposit not yet finalized on the current AN bridge | Used only while `state.json` has no progress. A too-low id is only a cheap re-check when `FROM_BLOCK` still covers that deposit (the voucher is active, so it is not proven again). If `FROM_BLOCK` is above it — the row above, "just below the oldest still to deliver" — `fetch` finds no log and every tick fails; raise `START_DEPOSIT_ID` or lower `FROM_BLOCK`. |
| `SKIP_AFTER_ATTEMPTS` | **recommendation:** a value that takes a few hours of backoff to reach (for example 64) | 0 (the default) never parks, so one broken deposit blocks the queue until someone intervenes (§3). |
| `--backoff-initial-secs` / `-max-secs` / `-multiplier` | defaults 5 / 120 / 2 | A zero initial delay or a multiplier of 0 is rejected at start-up (`BackoffConfig::validate`); either would busy-loop. |
| `DEPOSIT_RELAYER_METRICS_ADDR` | loopback or a monitoring-only interface | Prometheus `GET /metrics`; alerts in §7. |
| systemd | `Restart=on-failure`, `TimeoutStopSec=180`, `KillMode=mixed` (as in `scripts/ursus/deposit-relayer.service`) | A tick can take minutes while the prover runs, and shutdown is checked only between ticks. |
| GraphQL endpoint | HTTPS, on a node that also serves `/v2/messages` | A gateway that only answers `/graphql` cannot submit. Plain HTTP needs `--allow-insecure-graphql`. |

## 3. The queue: strict order, parking, manual finish (QC-OFF-01)

The daemon works on one deposit at a time, in `depositId` order: `next_target =
last_processed + 1` (`RelayerState::next_target`). It never jumps ahead to a later
deposit while the current one is unresolved. This is deliberate, and an out-of-order
mode was considered and rejected. In order, the queue has one cursor and one next
deposit, and a stuck deposit is visible as a stalled queue. Out of order, some users
would be paid while an older deposit waits with no obvious signal, and the older one is
easy to forget.

What a stuck deposit costs: everyone behind it waits. What gets it moving:

1. **On its own**, a transient failure (RPC blip, prover crash, a pending submit) is
   retried with exponential backoff. Waiting is free: since QC-OFF-10, a retry reuses the
   deposit it already located and makes no `eth_getLogs` call.
2. **With `--skip-after-attempts N`**, after N consecutive *failed* attempts (a proof
   failure or a rejection by AN) the deposit is **parked**. It is appended to
   `parked_deposit_ids`, the queue moves on, and the log shows `daemon: deposit parked;
   run finalize-one manually`. Pending submits (§4) and "deposit not made yet" do not
   count toward N.
3. **By hand**, finish a parked deposit:

   ```bash
   deposit-relayer prove-one --rpc-url "$RPC_URL" --bridge-address "$BRIDGE_ADDRESS" \
     --deposit-id <N> --from-block <block at or below the deposit> \
     --deposit-prover-dir "$DEPOSIT_PROVER_DIR" \
     --fetch-deposit-data-bin "$DEPOSIT_FETCH_DEPOSIT_DATA_BIN" \
     --export-vk-blob-bin "$DEPOSIT_EXPORT_VK_BLOB_BIN" \
     --export-blake2b-proof-bin "$DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN" \
     --out-dir ./bundle-<N>
   deposit-relayer finalize-one --bundle-dir ./bundle-<N>   # AN_* settings from the env file
   ```

   Fix the cause first (table in §5); a proof AN rejected will be rejected again. Then
   `finalize-one` reports `ACCEPTED` or `already finalised`. Neither command takes the
   state lock, so the daemon can keep running.
4. **Clear the entry.** The daemon never removes ids from `parked_deposit_ids`, and the
   `deposit_relayer_parked_deposits` gauge counts them. Stop the daemon, delete the
   finished ids from the file, then start it again. An edit made while the daemon runs
   is overwritten by its next save.

## 4. Pending, timeouts and backoff (QC-OFF-11)

Each tick ends in one outcome (`relayer::TickOutcome`). The daemon resets backoff to
`--backoff-initial-secs` after progress and doubles it (up to `--backoff-max-secs`) after
anything else (`Relayer::run_until_shutdown`).

| Outcome | Meaning | Automatic handling | Counts toward parking |
|---|---|---|---|
| `finalized`, `already_finalized` | voucher deployed / already there | next deposit | resets the count |
| `not_yet_available` | deposit not made, or not `--confirmations` deep | wait | no |
| `an_pending` | the outcome is not known yet | retried; on the next tick the voucher pre-check (`AnSubmitter::is_finalized`) answers before any re-prove | **no** |
| `proof_failed` | the prover failed or timed out (900 s, `SubprocessProverConfig::timeout`) | retried | yes |
| `an_rejected` | AN refused the deposit (exit code in the log) | retried | yes |
| `skipped` | parked after N failures | next deposit; manual finish (§3) | resets the count |
| `error` | the tick itself failed (RPC down, state file unwritable) | retried with backoff | no |

`an_pending` comes from three places:

- `finalizeDeposit` was not confirmed within the fixed 60-second confirm timeout
  (`AnSubmitConfig::confirm_timeout_secs`; no CLI flag);
- it exited 0, but its voucher deploy and `confirmDeposit` were not both visible within
  that time (`AnInterfaceSubmitter::settle`);
- the lane is paused, `ERR_PAUSED` (231). The daemon keeps retrying until the owner
  unpauses.

None of these is evidence against the deposit. If a pending submit did land, the next
tick sees the voucher and moves on without proving again. If it did not, the next tick
proves and submits again, and a duplicate cannot mint. **Nothing to do by hand** unless
pending persists (alert in §7). If it does, check the AN endpoint and `isPaused()` on
the bridge.

## 5. Rejections: what the exit code tells you

The daemon logs the decoded reason (`submitter::describe_exit_code`,
`describe_voucher_exit`). Exit codes that point at bridge configuration hit every
deposit, so parking will not help; fix the setting, and parked deposits then finish with
`finalize-one`.

| Exit | Where | Cause | Fix |
|---|---|---|---|
| 220 `ERR_INVALID_ZKPROOF` | `finalizeDeposit` | the bridge's `VK_BLOB` is not this prover's VkBlob | deploy the matching pair (bridge VK and prover) |
| 222 `ERR_UNSUPPORTED_SRC_CHAIN` | `finalizeDeposit` | `(chainId, source contract)` not allowlisted | owner: `setTrustedL1Bridge(chainId, l1Bridge, true)` |
| 224 `ERR_UNKNOWN_BLOCK` | `finalizeDeposit` | the deposit's block hash is not an accepted anchor | confirm with `scripts/deposit_anchor_params.py --verify`, then admit it (`setAcceptedBlockHash` while owner anchors are on, otherwise the light client) |
| 204 / 230 | `finalizeDeposit` | zero amount / zero recipient was proven | genuinely bad deposit; leave it parked |
| 9 | voucher deploy | the bridge's `_depositVoucherCode` predates the voucher ABI (the 2026-07-02 outage) | recompile and redeploy the bridge and voucher as a pair |
| 51 | voucher deploy | already finalized | none; reported as `already_finalized` |

Settings 222 and 224 do not survive an AN bridge code upgrade. After every deploy or
upgrade they must be set again before the first deposit can finalize.

## 6. `state.json` across redeploys (QC-OFF-08)

The file is stamped with the deployment it belongs to: EVM chain id, EVM bridge address
and `AN_DAPP_ID` (`state::DeploymentIdentity`). If a daemon starts with a different one,
it refuses with `state.json deployment mismatch`, so a cursor from one bridge cannot
silently skip another bridge's deposits.

`--force-state` accepts the mismatch by **starting the file over**. Progress, scan
cursor and parked ids are discarded, the old values are logged at `warn`, and the daemon
begins at `--start-deposit-id` (`RelayerState::ensure_deployment`). Checklist before
using it:

1. Is this really a new deployment? A typo in `BRIDGE_ADDRESS` also mismatches. Fix the
   env instead.
2. Write down `parked_deposit_ids` from the old file. They are dropped, and each one
   still needs `finalize-one` against the bridge it belongs to.
3. Set `START_DEPOSIT_ID` (0 for a new EVM bridge) and `FROM_BLOCK` (its deploy block).
4. Keep a copy of the old file, then start once with `--force-state`, and remove the
   flag again. Leaving it on accepts the next mismatch without asking.

Other cases:

- **Crash between submit and save.** The next tick finds the voucher and records the
  deposit as `already_finalized`; nothing is proven twice.
- **A corrupt or unreadable file** stops the daemon at start-up. Restore the backup. If
  there is none, deleting the file is safe apart from the parked ids: the daemon
  restarts at `START_DEPOSIT_ID` and skips finalized deposits by their vouchers.
- **Two daemons on one file**: the second fails to take the lock and exits.

## 7. Monitoring

Suggested alert rules over the exported metrics (`metrics` module). The thresholds are
recommendations, not tested values.

| Condition | Meaning |
|---|---|
| `time() - deposit_relayer_last_tick_timestamp_seconds > 900` | daemon dead or hung (a tick includes the prover, up to 900 s) |
| `deposit_relayer_eth_deposit_counter - deposit_relayer_target_deposit_id > 0` and `time() - deposit_relayer_last_finalized_timestamp_seconds > 3600` | deposits are waiting and none was delivered for an hour |
| `deposit_relayer_attempts_since_progress >= 3` | the current deposit keeps failing; read the log for the exit code |
| `deposit_relayer_parked_deposits > 0` | manual finish needed (§3) |
| `increase(deposit_relayer_ticks_total{outcome="an_pending"}[30m]) > 10` | AN slow, unreachable, or the lane paused |
| `increase(deposit_relayer_eth_get_logs_total{outcome="error"}[15m]) > 0` | the Ethereum RPC is refusing log queries |

`deposit_relayer_last_finalized_deposit_id` only covers the current process; after a
restart it stays unset until the first delivery. Use `target_deposit_id` for the
backlog.

## 8. When the primary is down (QC-OFF-02)

The daemon is a single point of liveness, not of safety: while it is down, deposits
wait. Nobody can lose funds or be paid twice.

**Recommendation:** a cold standby rather than a second live daemon. Prepare a host with
the same binary, env file and AN keys, and keep the service disabled. To fail over:

1. Make sure the primary is really stopped (`systemctl stop`, or the host powered off).
   The lock does not reach across hosts.
2. Copy the latest backup of `state.json` to the standby's `DEPOSIT_RELAYER_STATE`. If
   there is no backup, start without a file and set `START_DEPOSIT_ID` to the
   primary's last `target_deposit_id`. If it is lower, keep `FROM_BLOCK` at or
   below that deposit — then the daemon only re-checks. A `FROM_BLOCK` above it
   makes every tick fail (`EthLogSource::fetch` finds no log).
3. Start the service and watch for `finalized` / `already_finalized` ticks.

A one-off `finalize-one` by another operator while the daemon runs is safe: the slower
submit gets exit 51 and is reported as already finalized.

No service-level targets are defined in the code. How quickly a dead daemon must be
replaced, and how long a parked deposit may wait, is a production decision for the
bridge operator.
