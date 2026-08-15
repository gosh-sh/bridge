# Agent context — AN (TVM) bridge audit

Use this when spawning subagents on Phase F (`audit/spec/an/`).

## Stack difference vs ETH audit

| ETH (done) | AN (this pass) |
|------------|----------------|
| Foundry / Solidity on EVM | `sold` + gosh-solidity → TVC bytecode |
| Synchronous calls | **Async actor model** — internal messages, bounces, callbacks |
| `forge test` | **pytest** + **`tvm-debugger run`** / **`run-raw`** |
| `audit/spec/ethereum/` | `audit/spec/an/` |

## Required reading (in order)

1. `audit/knowledge/01-blockchain-overview.md` — Acki Nacki / Everscale lineage
2. `audit/knowledge/02-actor-model-and-messages.md` — message types, bounce, async chains
3. `audit/knowledge/03-accounts-and-state.md` — addresses, state_init, workchains
4. `audit/knowledge/04-tvm-execution.md` — gosh-solidity vs Ethereum Solidity
5. `audit/knowledge/05-security-patterns.md` — audit checklist
6. `audit/knowledge/hermez_kzg_pins.md` — **Hermez VK/SRS pins** (not chain ceremony)
7. `audit/spec/an/BUILD.md` — toolchain + debugger limits
8. `audit/reports/an-audit-direction.md` — **priority queue / BC-QC**
9. `audit/reports/non-e2e-verification-cycle.md` — full non-E2E scope
10. `audit/PROJECT_FACTS.md` — bridge flows (deposit **12** PI, finalizeDeposit)

DEX-specific knowledge (`06-dex-overview.md` …) is **not** required for bridge.

## Toolchain paths

```text
.tools/sold           → ../TVM-Solidity-Compiler/target/release/sold
.tools/tvm-debugger   → ../tvm-sdk/target/release/tvm-debugger
audit/spec/an-contracts/tools → ../../../.tools
```

Setup: `./scripts/setup_an_audit_tools.sh`

## Test infrastructure (from dex, adapted)

| File | Role |
|------|------|
| `test_base.py` | `TestBase`, `MessagePipeline`, `patch_state`, debugger wrappers |
| `conftest.py` | session `tb` fixture, `AN_PROJECT_ROOT` |
| `integration/conftest.py` | `state_snapshot` / `assert_state_unchanged` for CEI tests |

**Env:** `AN_PROJECT_ROOT` → `audit/spec/an-contracts/` (has `tools/`, `build/`).

## Contract sources

Upstream: `../acki-nacki` @ **`origin/contracts/bridge`** (`git@github.com:gosh-sh/acki-nacki.git`). `contracts/exchange/` → **`eccUSDCBridge.sol`** (12 PI, `_trustedL1Bridge` SET allowlist), `DepositVoucher.sol` (+ `token/`, `eccconfig/` deps). Отдельный `USDCBridge.sol` в upstream **не** используется — после `sync_an_contracts.sh` смотрите `eccUSDCBridge`.

Sync: `./scripts/sync_an_contracts.sh` (default branch `contracts/bridge`; audit VkBlob pin `9dacd998…` restored via `scripts/preserve_audit_vk_blob.sh`)
Optional vendor clones: `./scripts/setup_audit_vendors.sh` → `audit/vendors/` (gitignored)

## Methodology (same as ETH pass)

- **QC:** PoC + auditor view; author confirms intent
- **BC:** PoC + likely wrong under bridge assumptions
- Tests assert **correct** behaviour (red→green); use `@pytest.mark.xfail` while bug open

## Primary audit targets (Phase F)

1. `USDCBridge.finalizeDeposit` — **12** public inputs (operand `12 × 32` B), VK blob, voucher deploy (audit overlay may document legacy 8-Fr parser — see TD-04)
2. `DepositVoucher` — confirmDeposit path, recipient binding (256-bit account)
3. Replay / nullifier (`usedDepositIds` or equivalent)
4. ECC mint semantics vs proof amount
5. Owner / governance (if any) — centralization QC not BC without fund loss

## Out of scope

- Full `tvm-sdk` opcode implementation audit
- Shellnet / live node E2E (separate track; use `tvm-cli` + GraphQL)
- ETH-side contracts (Phase A–E closed)
