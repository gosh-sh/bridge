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
│   └── questions.md       # QC items for partners
└── tools/                 # Optional scanners (antipatterns, PI layout)
```

## ID conventions

| Prefix | Use |
|--------|-----|
| `BRIDGE-XXX` | Security finding (BC) |
| `QC-NN` | Question to partner / dev |
| Test IDs `U/I/V/E/R-NN` | Indexed in `audit/TEST_INDEX.md` when suite grows |

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

Run: `cd audit/spec/ethereum && FOUNDRY_PROFILE=audit forge test` (47 tests)
