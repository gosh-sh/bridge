# TD-52 — Prover subprocess timeout / zombie on cancel

PoC: `tests/td_52_prover_subprocess_timeout.rs`, fix `prover.rs::run_example`.

## Timeout path (ASCII)

```
relayer tick
    │
    ▼
SubprocessProofGenerator::generate
    │
    ▼
run_example: cmd.spawn() + kill_on_drop(true)
    │
    ▼
wait_with_output() inside timeout(...)
    │
    ├─ success ──► next example step / bundle
    │
    └─ timeout ──► child.kill().await + child.wait().await
                      │
                      ▼
                 ProofGeneration Err ("timed out")
                      │
                      ▼
                 TickOutcome::ProofFailed (TD-07: no last_processed advance)
```

## Zombie probe results (PoC)

| Probe | Child killed? | Relayer outcome |
|-------|---------------|-----------------|
| `HangProofGenerator` sleep 600s / 200ms timeout | yes (`kill` + `wait`) | `ProofFailed` |
| Sleep stub `fetch_deposit_data` / 100ms timeout | yes (`/proc` dead) | `ProofFailed` via `generate` Err |
| `MockProofGenerator` control | n/a | `Finalized` |

## Ops QC

Monitor for orphan prover children after daemon restarts or hung ticks:

    pgrep -af 'cargo run --release --example export_blake2b_proof'
    pgrep -af 'target/release/examples/export_blake2b_proof'

Expected: no survivors after `ProofFailed` tick. Stale PIDs → investigate timeout / OOM / manual `kill`.

## Cross-refs

| TD | Link |
|----|------|
| TD-07 | Proof failure does not advance scan cursor / `last_processed` |
| TD-51 | Relayer fail-closed on RPC inconsistency |
| TD-53 | E2E runbook (daemon fail → prove-one → restart) |

## Verdict: **META/QC**

Timeout path fail-closed; explicit kill closes TD-52 zombie gap from `cmd.output()` drop. Ops should still monitor orphan `cargo` during long proving windows.

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_52 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_07_scan_cursor_proof_failure -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_51 -- --nocapture
