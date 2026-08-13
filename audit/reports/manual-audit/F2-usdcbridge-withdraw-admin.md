# F2 — USDCBridge withdraw / admin / TIP-3 manual audit

**Branch:** `acki-nacki@origin/dev` (synced via `scripts/sync_an_contracts.sh`)  
**Scope:** flows 1–2 in contract header (TIP-3 stripe, owner mint) + outbound cross-chain (`initiateWithdrawal`).  
**Cross-chain inbound:** see `F1-usdcbridge-deposit.md`.

---

## Surface map

| Flow | Entry | AuthZ | Replay / limits |
|------|-------|-------|-----------------|
| TIP-3 → ECC | `onTransferReceived` | `msg.sender == _usdcWallet` | wallet callback; uint64 cap |
| Owner mint | `mintAndSend` | `onlyOwnerPubkey` + `_mintNonce` | monotonic nonce |
| Owner → Accumulator | `mintAndSendAccumulator` | same + whole-USDC + `_mintAccumulatorNonce` | calls fixed `ACCUMULATOR_ADDRESS` |
| AN → external | `initiateWithdrawal` | public + attached ECC | burn + event (ZK off-chain) |
| Owner admin | `setPubkey`, `triggerTransaction`, `updateCode` | owner pubkey | `updateCode` → `onCodeUpgrade` snapshot |

**No pause flag** on AN bridge (contrast ETH `AckiNackiBridge.pause()`).

---

## initiateWithdrawal (outbound)

**Code:** L300–321.

- Exactly one ECC currency; must be `USDC_ECC_ID` (3).
- Amount from `msg.currencies`; burns via `gosh.burnecc`.
- `dstChainId` + `recipient` (≤64 B) passed through to `WithdrawalInitiated` — no on-chain binding to ETH chain id or address format beyond length.
- Counter: `_totalBurnedBridgeByToken[tokenId]` (observability).

**PoC:** `unit/test_usdcbridge_withdraw_admin_negative.py` (WD-AN-01..05), `unit/test_usdcbridge_withdraw_happy.py` (WD-AN-06).

**Auditor view:** contract logic matches “burn + emit for off-chain Circuit 4”; no BC on this surface in isolation.

---

## TIP-3 stripe (`onTransferReceived`)

**Code:** L213–234.

- Only `_usdcWallet` may call; mints ECC[3] to `from` (original TIP-3 depositor).
- Updates `_totalMinted` (separate from `_totalMintedBridgeByToken` used by cross-chain path).
- Same uint64 overflow guard as other mint paths.

**PoC:** `unit/test_usdcbridge_admin.py` (TIP-AN-01..02).

**QC-AN-05 (carried):** `_totalMinted` vs `_totalMintedBridgeByToken` — two counters, no single supply invariant on-chain.

---

## Owner mint (`mintAndSend` / `mintAndSendAccumulator`)

**Code:** L244–287.

| Check | mintAndSend | mintAndSendAccumulator |
|-------|-------------|------------------------|
| Owner pubkey | yes | yes |
| Nonce | `_mintNonce + 1` | `_mintAccumulatorNonce + 1` |
| amount > 0 | yes | yes |
| uint64 cap | yes | yes |
| whole USDC (1e6) | no | yes (`ERR_NOT_WHOLE_USDC`) |
| External call | recipient.transfer | `ACCUMULATOR_ADDRESS.buyShellFor` |

**QC-AN-02:** owner can mint ECC without cross-chain proof — **centralization / ops**, not a bug in the ETH→AN deposit path. Multisig on `_ownerPubkey` is the trust boundary.

**PoC:** `unit/test_usdcbridge_admin.py` (ADM-AN-02..06), `unit/test_usdcbridge_withdraw_admin_negative.py` (ADM-AN-01 nonce).

---

## Admin & upgrade

| Function | Risk | Note |
|----------|------|------|
| `setPubkey` | centralization | rotates owner; no timelock |
| `triggerTransaction` | low | owner ping to wallet-deployed tx contracts |
| `updateCode` | **QC-AN-03** | only path to rotate `_depositVoucherCode` (B2); full storage snapshot in `onCodeUpgrade` |
| `onCodeUpgrade` | upgrade safety | `userCell` non-empty overrides voucher code atomically |

**PoC:** `setPubkey` smoke in `unit/test_usdcbridge_admin.py` (ADM-AN-07).

**Auditor view:** no standalone voucher setter is intentional (PR2112 B2). Upgrade trust = owner key.

---

## QC register (F2)

| ID | Topic | PoC | Auditor view |
|----|-------|-----|--------------|
| QC-AN-02 | Owner `mintAndSend` | ADM-AN-02..03 | Ops mint; not cross-chain |
| QC-AN-03 | `updateCode` rotates voucher | code review | Intended; atomic via `userCell` |
| QC-AN-04 | No bridge pause | — | ETH has pause; AN finalize still callable |
| QC-AN-05 | Split mint counters | code review | Observability only |

**BC on F2 surfaces:** none identified (withdraw burn path consistent).

---

## Cross-refs

- Joint ETH↔AN: `audit/reports/questions-cross-chain.md`
- Deposit BC candidates: BC-AN-01, BC-AN-02 in `F1-usdcbridge-deposit.md`
