# Audit spec — Acki Nacki (TVM / gosh-solidity)

Overlay tests for Phase F AN contract audit. Upstream contracts: `../acki-nacki` → `audit/spec/an-contracts/`.

## Quick start

```bash
./scripts/setup_an_audit_tools.sh
cd audit/spec/an && python3 -m pytest unit/test_toolchain_smoke.py -q
```

After syncing contracts:

```bash
./scripts/sync_an_contracts.sh
cd audit/spec/an-contracts && ./build.sh
make audit-an-test
```

## Docs

| File | Purpose |
|------|---------|
| `BUILD.md` | Toolchain, compile, debugger limits |
| `AGENT_CONTEXT.md` | **Subagent briefing** — AN specifics + file map |
| `../../knowledge/01-05` | Acki Nacki / TVM background (from dex) |
| `../../reports/an-audit-plan.md` | Phase F plan |

## Conventions

Same BC/QC policy as ETH (`audit/reports/closeout-eth.md`):

- Header: `# INV: …` or `# QC: …`
- Use `MessagePipeline` for multi-contract async flows
- Snapshot pattern: `integration/conftest.py` (`state_snapshot`, `assert_state_unchanged`)

## vs dex audit

Reused from `../dex/audit/tests-head/`: `test_base.py`, `MessagePipeline`, integration snapshots.

**Not** ported: DEX invariants (Hypothesis machines), wasm matcher, n22 e2e harness.
