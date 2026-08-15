# TD-53 — E2E runbook: daemon fail → prove-one → finalize-one → restart

PoC: `tests/td_53_e2e_runbook_recovery.rs`, `scripts/td_53_dry_run_recovery_smoke.sh`.

## Recovery flow (ASCII)

```
daemon tick
    │
    ├─ prove timeout / fail ──► ProofFailed (TD-52)
    │         │
    │         ▼
    │   state.json persisted
    │   last_processed: None (or parked after skip)
    │
operator prove-one --out-dir ./out/deposit-<ID>
    │   writes vk_blob.bin, public_inputs.bin, proof.bin
    │
operator finalize-one --bundle-dir ./out/deposit-<ID>
    │   (live: AnInterfaceSubmitter::submit_bundle)
    │
restart daemon (same state.json)
    │
    ├─ nullifier pre-check ──► AlreadyFinalized (id cleared)
    └─ tick ──► Finalized (next depositId)
```

## Recovery scenario table (mock PoC)

| Step | Scenario | Outcome | state.json |
|------|----------|---------|------------|
| (a) | Hang prover / daemon fail | `ProofFailed` | `last_processed`: null, `attempts_since_progress` > 0 |
| (b) | prove-one out_dir + finalize-one | `Finalized` on MockAn | unchanged until restart |
| (c) | Restart same state | `AlreadyFinalized` → `Finalized` dep+1 | `last_processed` advances |
| (d) | skip parks id 0 | `Skipped` | `parked_deposit_ids: [0]`, cursor at 0 |
| (d) | manual finalize-one | AN mint id 0 | parked list unchanged |
| (d) | resume daemon | `Finalized` id 1 | catch-up |

### Example snapshot after (a) proof fail

```json
{
  "last_processed_deposit_id": null,
  "last_attempt_deposit_id": 0,
  "attempts_since_progress": 1,
  "parked_deposit_ids": []
}
```

### Example after (c) restart catch-up

```json
{
  "last_processed_deposit_id": 1,
  "attempts_since_progress": 0,
  "parked_deposit_ids": []
}
```

## Cross-refs

| Doc / TD | Link |
|----------|------|
| Operator runbook | `docs/audit/deposit-relayer-operator-runbook.md` |
| VK redeploy | `docs/operations/shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md` |
| TD-07 | State persist on proof failure |
| TD-37 | Peer `finalize-one` unlock |
| TD-52 | Timeout → `ProofFailed` |
| TD-42 | VK pin for live shellnet |

## CI hooks (TD-53 mock smoke wired)

| Hook | When |
|------|------|
| `scripts/td_53_dry_run_recovery_smoke.sh` | standalone / aggregator |
| `scripts/check_deposit_audit_gates.sh` | after TD-49 mutation smoke (RELAYER block) |
| `make audit-deposit-relayer-test` | full gate bundle |
| `.gitlab-ci.yml` `test:deposit-relayer:audit` | gates + td_53 paths |

**Runtime:** ~seconds (mock `cargo test td_53` + CLI `status`).

Env `TD53_SKIP_CLI_STATUS=1` skips CLI step (local only; CI must pass full smoke).

## Live shellnet (E-AN-01) — deferred QC checklist

1. GraphQL HTTPS (`AN_GRAPHQL_URL`), keys (`AN_KEYS_PATH`) — hot wallet custody
2. `AN_TOKEN_BRIDGE` / `AN_SENDER` in `dapp_id::account_id` form
3. VK blob pin (`check_vk_srs_pin.sh`, TD-42)
4. `prove-one` with Sepolia RPC + `BRIDGE_DEPLOY_BLOCK`
5. `finalize-one` → GraphQL confirm mint
6. Restart daemon → cursor catch-up, no double-mint
7. Optional: competing relayer observes `AlreadyFinalized` (TD-37 / TD-65)

**Status:** deferred — mock PoC covers operator path; live = ops (G5 / `phase-g-status.md`).

## Verdict: **partial META / QC — mock CI smoke wired**

Mock E2E recovery path in `check_deposit_audit_gates.sh`; live shellnet pipeline = ops (E-AN-01 deferred).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_53 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_37_competing_grief_finalize_one -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_52 -- --nocapture
    ./scripts/td_53_dry_run_recovery_smoke.sh
