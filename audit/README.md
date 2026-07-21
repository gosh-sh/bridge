# Audit overlay — Acki Nacki Bridge

Pruvendo-style audit workspace (adapted from **ammalgam** `Pruvendo/test/` and **dex** `audit/`).

## Layout

```
audit/
├── PROJECT_FACTS.md       # Domain facts — single source for agents
├── README.md              # this file
├── findings/              # BRIDGE-XXX — one dir per bug candidate
│   └── BRIDGE-001/
│       ├── test.t.sol     # or .rs / .py
│       ├── analysis.md
│       └── regression.t.sol
├── spec/                  # New invariant / property tests
│   ├── ethereum/          # Foundry (FOUNDRY_PROFILE=audit optional)
│   └── rust/              # Relayer / deposit-relayer / interface
├── reports/
│   ├── findings-summary.md
│   ├── questions.md           # index → eth / an / cross-chain
│   ├── questions-eth.md
│   ├── questions-an.md
│   └── questions-cross-chain.md
└── tools/                 # Optional scanners (antipatterns, PI layout)
```

## ID conventions

| Prefix | Use |
|--------|-----|
| `BRIDGE-XXX` | Bug **candidate** (BC) — PoC shows likely-wrong behaviour; not a confirmed bug until team agrees |
| `QC-NN` | **Question after PoC** — behaviour reproduced; need intent to classify bug vs feature |
| Test IDs `U/I/V/E/R-NN` | Indexed in `audit/reports/test-matrix.md` |

**BC vs QC:** both require PoC. QC = «проверили, не уверены в intent». BC = «проверили, уверены что так быть не должно (кандидат в баг)». See `audit/reports/closeout-eth.md`.

## Methodology sources

- **ammalgam**: BC/QC/OK, two-stage PoC, fuzz antipatterns, invariant registry
- **dex**: don’t fit tests to code, vuln tests assert correct behavior, dual-tree baseline vs HEAD (here: mock vs real verifier paths)

## Invariant labels

Canonical list: `docs/operations/bridge_verification.md` (DEP-#, LH-#, CC-#, …).

Every spec test header should cite the invariant(s) it covers.

## Phase status (ETH)

| Phase | Artifact | Status |
|-------|----------|--------|
| A | `reports/manual-audit/A1..A4.md` | done |
| B | `reports/test-matrix.md` | done |
| C | `spec/ethereum/*.t.sol` | **done** (29 unit) |
| D | handlers + invariants | **done** (11 fuzz/inv) |
| E | E2E gaps | **done** (7 E2E) |
| Closeout | `reports/closeout-eth.md` | **draft** — QC open, author ack pending |
| AN closeout | `reports/closeout-an.md` | **draft** — 2 BC + QC open |
| CI | `test:solidity:audit` in `.gitlab-ci.yml` | ✅ |
| Local gate | `make pre-push-audit` | ✅ |
| **F — AN** | `spec/an/` + `reports/an-audit-plan.md` | **F0–F10** 74 pytest ✅; relayer + prover overlay ✅ |

Run ETH: `make pre-push-audit`  
Run AN: `make audit-an-test` (after `./scripts/sync_an_contracts.sh`)  
Run F10 relayer: `make audit-deposit-relayer-test`  
Gas benchmark: `./scripts/run_gas_benchmark.sh` → `audit/reports/gas-cost-benchmark.md`
