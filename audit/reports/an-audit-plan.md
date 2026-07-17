# Phase F — AN contracts audit plan

**Status:** F1–F4 + F2 landed; **37 passed** pytest (BC-AN-01 dual-proof PoC green). Non-E2E plan: `non-e2e-verification-cycle.md`.

**Scope:** `USDCBridge` / `DepositVoucher` / deposit finalization on Acki Nacki. ETH-side audit (Phases A–E) is closed separately.

---

## Toolchain

| Component | Path |
|-----------|------|
| Compiler | `.tools/sold` → `../TVM-Solidity-Compiler` |
| Debugger | `.tools/tvm-debugger` → `../tvm-sdk` |
| Setup | `scripts/setup_an_audit_tools.sh` |
| Contract sync | `scripts/sync_an_contracts.sh` (`origin/dev`) |
| Spec tests | `audit/spec/an/` |
| Contract workspace | `audit/spec/an-contracts/` |

See `audit/spec/an/BUILD.md` and `audit/spec/an/AGENT_CONTEXT.md` (subagents).

---

## Phases (mirror ETH)

| Phase | Deliverable | Status |
|-------|-------------|--------|
| F0 | Toolchain + pytest harness | ✅ |
| F1 | `manual-audit/F1-usdcbridge-deposit.md` | ✅ (BC-AN-01/02) |
| F2 | `manual-audit/F2-usdcbridge-withdraw-admin.md` | ✅ |
| F3 | Unit + integration tests | ✅ 36 pass (+1 skip w/o bc_an_01 proofs) |
| F4 | MessagePipeline DEP-AN-11/12 | ✅ |
| F5 | BC-AN-01 dual-proof PoC | ⏳ script + partial tests; prove needs chain SRS |
| F6 | Shellnet e2e | deferred |

---

## F1 manual focus

1. **finalizeDeposit** — 11 PI passthrough, VK blob, opcode call
2. **DepositVoucher** — constructor arity, confirmDeposit, ECC mint
3. **Recipient binding** — `anAccountHigh`/`anAccountLow` reassembly (256-bit)
4. **Replay** — depositId nullifier
5. **Trust** — dappId, amount, ECC currency id

Cross-ref ETH QC items that span both sides: QC-A1-2 (USDC trust is ETH-side; AN mint semantics separate).

---

## Knowledge base

Universal AN/TVM docs: `audit/knowledge/` (symlinks to `../dex/knowledge/01-05`).

---

## Out of scope

- tvm-sdk opcode implementation audit (smoke only)
- Full node async semantics not modeled in tvm-debugger (document as test limitation)
