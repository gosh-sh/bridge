# Wave 2 — security-first audit plan

**Branch:** `audit-new`  
**Updated:** 2026-08-15  
**Prerequisite:** Phase 2 deposit-ETH handoff closed (`7bd7366`); baseline walkthrough 2026-08-15.

## Main sync policy

| Remote | Role | Action |
|--------|------|--------|
| `github/main` | Devs canonical main | `git fetch github` before each sprint; merge if `audit-new..github/main` non-empty |
| `origin/main` | Modus-ponens mirror | May lag github; track both |
| `audit-new` | Audit overlay | Code conflicts → prefer **main** code; audit docs → keep audit version |

**2026-08-15:** `audit-new` already contains `github/main` @ `a7a1130` (merge-base = github tip). Next check after dev pushes.

## Deposit verify tiers (backlog)

See `deposit-verify-three-tier-backlog.md`.

| Tier | Status |
|------|--------|
| 1 Mock | CI |
| 2 Standalone SHPLONK | TD-43 + T2-1 all fixtures (`td_43_all_fixture_triples_pass_opcode_triple`) | **CI** |
| 3 tvm-debugger SDK | T3-2/T3-3 backlog |
| 4 shellnet live | ops defer |

## Security workstreams (priority)

| ID | Scope | Entry |
|----|-------|-------|
| W2-1 | ETH `withdrawByProof` + treasury/nullifier | `eth-audit-plan.md` A3, `manual-audit/A3-withdraw.md` |
| W2-2 | AN withdraw / burn / admin | `manual-audit/F2-usdcbridge-withdraw-admin.md` |
| W2-3 | Anchor / attestation trust (DEP-N-4) | `PROJECT_FACTS.md`, block header oracle |
| W2-4 | Cross-circuit binding (4-circuit arch) | `docs/architecture/four_circuit_architecture.md` |
| W2-5 | tvm-sdk opcode tier 3 | `deposit-verify-three-tier-backlog.md` T3-* |
| W2-6 | `../acki-nacki` sync drift | `scripts/sync_an_contracts.sh` each sprint |

## Gates (regression)

    make pre-push-audit
    bash scripts/check_deposit_audit_gates.sh
    AN_AUDIT_INTEGRATION=1 bash scripts/ci_an_audit.sh

## Deliverables per sprint

1. Delta note in `delta-deposit-eth.md` (main merge + findings).
2. New BC only with `audit/findings/BRIDGE-XXX/`.
3. Update `phase-g-status.md` when focus shifts.
