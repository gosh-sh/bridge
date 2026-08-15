# Audit questions — index

Questions are split by **author / deployment boundary**. Do not merge into a single partner pack.

| Document | Audience | Contents |
|----------|----------|----------|
| **[questions-eth.md](questions-eth.md)** | Ethereum bridge team | `AckiNackiBridge`, verifiers, `verifyBlock`, `withdrawByProof`, `deposit` on L1. **Sent 2026-07-16.** |
| **[questions-an.md](questions-an.md)** | Acki Nacki team (`acki-nacki`) | `USDCBridge`, `DepositVoucher`, `finalizeDeposit`, admin/withdraw on AN. **Ready to send.** |
| **[questions-cross-chain.md](questions-cross-chain.md)** | Both teams | Caps, pause, recipient binding, `dappId` / workchain policy. |

**Policy:** QC = reproduced behaviour + auditor view; **author confirms** bug vs feature. BC = bug candidate with PoC (where noted).

**Closeouts:** ETH → `closeout-eth.md`. AN → `closeout-an.md`. **Phase G:** `phase-g-status.md` (`audit-new`).

**ETH deposit baseline (open only):** `baseline-deposit-eth.md` · locked tests → `baseline-deposit-eth-locked.md` · delta → `delta-deposit-eth.md`.

**ETH deposit test directions (3-agent catalog):** `test-directions-deposit-eth-catalog.md` (68 TD, dedup) · index → `test-directions-deposit-eth.md`.

**Registers:** `an-audit-direction.md` (AN test gate), `findings-summary.md` (ETH workstreams).
