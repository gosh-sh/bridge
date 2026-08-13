# BC-AN-01 — dappId not bound to L1 event

**Class:** was BC candidate (High)  
**Status:** **closed (ack Stage II 2026-07-21)** — author ack repo 2026-07-20 + PDF: «fixed. Принудительно dapp_id делаю 0».  
**Area:** `USDCBridge.finalizeDeposit` replay key

## Summary

Replay anchor is `tvm.hash(abi.encode(depositId, contractAddr, dappId))`. On audited `dev` tip, `dappId` came from PI limbs (prover config `AN_DAPP_ID`), not the L1 `Deposit` log — two valid proofs with different dapp limbs could double-mint.

**Fix (contracts/dex_bridge):** `f.dappId = 0` always; PI dapp limbs ignored for voucher identity. Audit overlay syncs this logic but keeps audit VkBlob `724687a4…` until fixtures rotate.

## PoC (historical — pre-fix behaviour)

| Test | What it shows |
|------|----------------|
| `test_bc_an_01_tampered_dapp_id_rejects_same_proof` | PI dappId is ZK-bound — cannot reuse proof bytes |
| `test_bc_an_01_replay_key_differs_by_dapp_id` | PI dapp limbs differ ⇒ different blobs |
| `test_bc_an_01_double_mint_same_deposit_two_dapp_ids` | **Regression** — second finalize must NOT mint (post-fix) |
| `integration/test_bc_f8f_regressions.py` | F8-F: `f.dappId = 0` pinned |

Regenerate dual proofs (Hermez SRS — matches audit USDCBridge VK `724687a4…`):

```bash
./scripts/bootstrap_hermez_srs_k18.sh
./scripts/audit/generate_bc_an_01_dual_proofs.sh
cd audit/spec/an && python3 -m pytest integration/test_bc_an_01_dapp_id_double_mint.py -q
```

## Resolution (landed + author ack)

- `f.dappId = 0` in `_parsePublicInputs` (`contracts/dex_bridge`, commit `056e6f0a` area).
- Audit sync: `./scripts/sync_an_contracts.sh` + `scripts/preserve_audit_vk_blob.sh` (default).
- **Author (2026-07-20):** accepted as closed — no multi-dapp-per-deposit; `AN_DAPP_ID` config no longer affects voucher identity on AN.
