# AN invariants — USDCBridge / DepositVoucher

Proposed properties for Phase **F7** (property tests). Map to pytest / Hypothesis with header `# INV: AN-*`.

**Scope:** contract logic in tvm-debugger; ZK opcode = trust boundary (fixtures only).

---

## Deposit / finalizeDeposit

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-DEP-1 | `amount == 0` → revert before mint (204) | unit ✅ | covered |
| AN-DEP-2 | `len(publicInputs) < 352` → no mint path | unit ✅ QC-AN-08 | covered |
| AN-DEP-3 | `fr[2] > uint64::MAX` → revert 214 before mint | unit ✅ QC-AN-01 | covered |
| AN-DEP-4 | Bad / garbage proof → 220, state unchanged | unit ✅ | covered |
| AN-DEP-5 | Same valid proof twice → second finalize does not increase `getTotalBridged` | integration ✅ DEP-AN-12 | covered |
| AN-DEP-6 | Tamper PI limb without new proof → 220 | integration ✅ BC-AN-01 tamper | covered |
| AN-DEP-7 | Two valid proofs, same depositId, different dappId → **no** second mint (`f.dappId=0`, BC-AN-01 closed) | integration ✅ | covered |
| AN-DEP-8 | `anAccount == 0` → `ERR_ZERO_RECIPIENT` before ZK; ETH keeps `InvalidAnAccount` | unit ✅ QC-AN-10 | closed Stage II |
| AN-DEP-9 | Successful cross-chain mint: `Δ getTotalBridged == amount` | integration + F7 property | ✅ F7-C1 |
| AN-DEP-10 | `contractAddr` in PI not allowlisted (BC-AN-02) | integration pre-ZK | partial |

---

## DepositVoucher / MessagePipeline

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-VCH-1 | Voucher address deterministic from `(depositId, contractAddr, dappId)` | unit / property | ✅ F7-B |
| AN-VCH-2 | `confirmDeposit` only from derived voucher address (207) | unit ✅ | covered |
| AN-VCH-3 | Missing `_depositVoucherCode` → ZK ok, no mint | integration ✅ QC-AN-07 | covered |
| AN-VCH-4 | Pipeline step order: deploy → confirm → minted counter | integration ✅ DEP-AN-11 | covered |

---

## Withdraw (initiateWithdrawal)

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-WD-1 | No ECC attached → 216 | unit ✅ | covered |
| AN-WD-2 | Multiple ECC → 217 | unit ✅ | covered |
| AN-WD-3 | `tokenId != USDC_ECC_ID` → 221 | unit ✅ | covered |
| AN-WD-4 | `amount == 0` → revert | unit ✅ | covered |
| AN-WD-5 | `recipient.len > 64` → 218 | unit ✅ | covered |
| AN-WD-6 | Happy burn: `getTotalBridged` burned leg increases | unit ✅ | covered |
| AN-WD-7 | Burned ≤ prior minted (per-token bridge counter) | property F7 | ✅ F7-C3 |

---

## Admin / TIP-3

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-ADM-1 | Non-owner `mintAndSend` → 209 | unit ✅ | covered |
| AN-ADM-2 | Wrong nonce → 215 | unit ✅ | covered |
| AN-ADM-3 | Owner mint increases `_totalMinted` not `_totalMintedBridgeByToken` | unit + property | ✅ F7-C2 |
| AN-TIP-1 | `onTransferReceived` only from `_usdcWallet` | unit ✅ | covered |

---

## Cross-counter (observability — QC-AN-05)

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-ACC-1 | No on-chain link `_totalMinted` ↔ `_totalMintedBridgeByToken` | code review | documented QC |
| AN-ACC-2 | After N cross-chain mints: `bridgeMinted >= bridgeBurned` (per token) | property F7 | ✅ F7-C4 |

---

## F7 property-test mapping

| F7 task | Targets | Status |
|---------|---------|--------|
| F7-A | AN-DEP-2,8,9 — PI parse properties (Hypothesis, no debugger) | ✅ |
| F7-B | AN-VCH-1 — replay key hash collision smoke | ✅ |
| F7-C | AN-ADM-3, AN-ACC-2, AN-DEP-9, AN-WD-7 — counter monotonicity | ✅ |
| F7-D | AN-DEP-4 — garbage proof storm, no mint | ✅ |
| F7-E | AN-WD-1..5 — withdraw recipient / ECC properties | ✅ |
| F7-F | Post-author BC/QC regressions | blocked on disposition |

---

## MessagePipeline (order fuzz — F8)

| ID | Invariant | Verify via | Status |
|----|-----------|------------|--------|
| AN-MPL-1 | While draining inflight queue, `getTotalBridged.minted` never decreases | fuzz | ✅ F8 |
| AN-MPL-2 | Random `process_one_at` schedule never mints more than FIFO canonical oracle | fuzz | ✅ F8 |
| AN-MPL-3 | Replay same proof in batch + random order ≤ single-success mint cap | fuzz | ✅ F8 |
| AN-MPL-4 | Permuted deploy order for two fresh deposits reaches same FIFO total | fuzz | ✅ F8 |
| AN-MPL-5 | Stateful machine: interleave finalize / deliver / shuffle / replay | fuzz | ✅ F8b |
| AN-MPL-6 | After deposit-only mint: withdraw burned ≤ minted, monotone | fuzz | ✅ F8c |
| AN-MPL-7 | Enqueue finalize while inflight + batch delivery (deep SM) | fuzz | ✅ F8b-deep |
| AN-MPL-8 | Bounce probe to missing dest does not over-mint bridged leg | fuzz | ✅ F8f |
| AN-MPL-9 | Deploy message retry after success is idempotent (no double mint) | fuzz | ✅ F8f |
| AN-MPL-10 | Ten-proof pool (0..9) under random pipeline schedules | fuzz | ✅ F8e |
| AN-ACC-3 | Owner mint leaves bridged minted unchanged; deposit cap still holds | fuzz | ✅ F8d |
| AN-QC-05 | Owner ECC + withdraw can make bridged burned > bridged minted | fuzz/doc | ✅ QC |

**Harness:** `MessagePipeline.reorder_inflight` / `process_one_at` / `peek_inflight` (`test_base.py`).  
**Limitation:** single-threaded debugger queue — not full multi-block TVM scheduler.
