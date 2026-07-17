# Audit questions — index

Questions are split by **author / deployment boundary**. Do not merge into a single partner pack.

| Document | Audience | Contents |
|----------|----------|----------|
| **[questions-eth.md](questions-eth.md)** | Ethereum bridge team | `AckiNackiBridge`, verifiers, `verifyBlock`, `withdrawByProof`, `deposit` on L1. **Sent 2026-07-16.** |
| **[questions-an.md](questions-an.md)** | Acki Nacki team (`acki-nacki`) | `USDCBridge`, `DepositVoucher`, `finalizeDeposit`, admin/withdraw on AN. **Ready to send.** |
| **[questions-cross-chain.md](questions-cross-chain.md)** | Both teams | Caps, pause, recipient binding, `dappId` / workchain policy. |

**Policy:** QC = reproduced behaviour + auditor view; **author confirms** bug vs feature. BC = bug candidate with PoC (where noted).

**Closeouts:** ETH → `closeout-eth.md`. AN → **`closeout-an.md`** (draft, author ack pending).

**Registers:** `an-audit-direction.md` (AN test gate), `findings-summary.md` (ETH workstreams).
