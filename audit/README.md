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

## Phase status

**Active branch:** `audit-new` (canonical code = `origin/main`). See `reports/phase-g-status.md`.

| Track | Artifact | Status |
|-------|----------|--------|
| ETH overlay | `spec/ethereum/` | **69/69** on `audit-new` |
| AN pytest | `spec/an/` | **74** `make audit-an-test` |
| F10 relayer | `crates/deposit-relayer-daemon/tests/f10_*` | **68** cargo tests |
| Closeout ETH | `reports/closeout-eth.md` | G1 synced — 9 QC open |
| Closeout AN | `reports/closeout-an.md` | G1 synced — BC-AN-02 + QC open |
| Deposit test catalog | `reports/test-directions-deposit-eth-catalog.md` | 68 TD, 3-agent dedup, PoC backlog |
| **G2** | BC-AN-02 disposition | **next** |
| **G3** | QC-OFF-06 live path | partial in main |
| **G5** | E-AN-01 shellnet E2E | deferred |

Legacy phase table (ETH A–E, AN F0–F10): completed before merge; counts updated above.

Run ETH: `make pre-push-audit`  
Run AN: `make audit-an-test` (after `./scripts/sync_an_contracts.sh`)  
Run F10 relayer: `make audit-deposit-relayer-test`  
Gas benchmark: `./scripts/run_gas_benchmark.sh` → `audit/reports/gas-cost-benchmark.md` (network gas/USD provenance: `audit/reports/.gas-benchmark-networks.json`)
