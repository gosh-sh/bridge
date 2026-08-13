# Archive

This directory holds **historical, superseded, or agent-generated** material moved out of the repo root and active `docs/` tree during the 2026-07 documentation refactor (`docs/refactor-cleanup` branch).

Nothing here is deleted — files are preserved for audit trail and context.

## Contents

| Path | What |
|------|------|
| [`AGENTS.md`](AGENTS.md) | Former Cursor/agent context monolith (859 lines). Useful technical content was extracted into `docs/`; this file is kept for reference only. |
| [`.cursor/`](.cursor/) | Former agent rules (SSH hosts, dual-remote git) and deposit E2E skill. Operational content extracted to `docs/operations/evm_an_deposit_e2e_runbook.md`. |
| [`docs/legacy/`](docs/legacy/) | Superseded v1 integration plan, frontend UI aspirational README |
| [`docs/partner-qa/`](docs/partner-qa/) | Dated partner question packs (Circuit 4, Phase 0) |
| [`docs/gap-analyses/`](docs/gap-analyses/) | Closed VK/deposit gap analyses (shellnet E2E green 2026-07) |
| [`docs/handoffs/`](docs/handoffs/) | Dated operator handoffs (M7 prover, live relayer, live driver refactor) |
| [`docs/e2e-milestones/`](docs/e2e-milestones/) | Point-in-time E2E milestone reports |
| [`docs/reviews/`](docs/reviews/) | Partner/audit review notes (Alina packs, bridge EVM review rounds) |
| [`docs/metrics/`](docs/metrics/) | v1 single-circuit proof metrics |
| [`docs/plans/`](docs/plans/) | Historical design plans (BK-set update without Circuit 3) |
| [`docs/agent_scratch/`](docs/agent_scratch/) | Agent task logs and branch summaries |

## When to consult the archive

- Tracing **why** a design decision was made → `AGENTS.md` Decision Log sections, `docs/handoffs/`, partner Q&A
- Reproducing **legacy** proof formats → `docs/legacy/verifying_an_proof_v1.md`
- Understanding **closed** gaps → `docs/gap-analyses/`

For current architecture and operations, start at [`../docs/README.md`](../docs/README.md).
