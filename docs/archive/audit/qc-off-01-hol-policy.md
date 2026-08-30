# QC-OFF-01 — keep strict depositId HOL (author answer)

**Audience:** Gosh / Acki Nacki authors (`sergeyegorov160977@gmail.com`).  
**Priority:** highest remaining item after item 45 (reviewer `sergeyegorov160977@gmail.com`).  
**Rejects:** [gosh-sh/bridge#34](https://github.com/gosh-sh/bridge/issues/34) (out-of-order / completed-ids set).  
**Ops:** `docs/audit/deposit-relayer-operator-runbook.md`.

## Verdict

**Yes.** Intentional strictness plus manual recovery.

The deposit relayer processes ids in order: `next_target = last_processed + 1`. It does not jump to a higher `depositId` that is already in L1 logs. A stuck id (bad proof, perpetual AN `Rejected`, wrong config) holds every later id — head-of-line blocking, **not** lost USDC on L1 (funds stay in the Ethereum bridge until that deposit is finalized or parked).

Mitigation is **skip, not reordering:** after N consecutive failures (`--skip-after-attempts`) the daemon parks the id in `state.json` (`parked_deposit_ids`) and advances the cursor. Parked ids are finished by the operator with `prove-one` + `finalize-one`.

L1 `depositId` is dense (`0, 1, 2, …` from the Ethereum counter). A hole in AN finalization is from park/skip, not from the chain skipping an id.

## What out-of-order finalize would mean (not chosen)

Today the relayer does not finalize `depositId=5` until `id=4` is closed **or** parked via skip. Out-of-order would mint `id=5` on AN while `id=4` stays stuck, then try 4 later.

We do not take that path:

- Operators lose a single readable queue: some users already have ECC, others wait with no explicit HOL signal.
- A parked `id=4` still needs **manual** recovery; the set of “already minted on AN” is no longer a prefix of L1 ids, so an old id is easier to forget.
- Strict HOL is one cursor and one next id. Out-of-order buys liveness flexibility at the cost of ops complexity.

Issue #34 is correct that AN `finalizeDeposit` does not require monotonic ids. That is not a reason to drop the operator invariant. Permissionless submitters may still finalize any proven id; the **official daemon** stays sequential.

## Production vs CLI default

| Surface | Skip |
|---------|------|
| CLI `deposit-relayer daemon` | `--skip-after-attempts 0` (disabled) — tests / explicit strict mode |
| Production systemd `scripts/ursus/deposit-relayer.service` | `SKIP_AFTER_ATTEMPTS=64` (env file may override) |

Park SLA: treat a `deposit parked` log as a page; run `finalize-one` within the backup-relayer window in the operator runbook.
