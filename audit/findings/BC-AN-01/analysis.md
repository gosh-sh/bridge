# BC-AN-01 — dappId not bound to L1 event

**Class:** was BC candidate (High)  
**Status:** **closed (ack Stage II 2026-07-21)** — author ack repo 2026-07-20 + PDF: «fixed. Принудительно dapp_id делаю 0».  
**Area:** `eccUSDCBridge.finalizeDeposit` replay key (upstream `acki-nacki@contracts/bridge`)

## Summary

Replay anchor is `tvm.hash(abi.encode(depositId, contractAddr, dappId, chainId))`. On audited `dev` tip, `dappId` came from PI limbs (prover config `AN_DAPP_ID`), not the L1 `Deposit` log — two valid proofs with different dapp limbs could double-mint.

**Fix (`contracts/bridge` / `eccUSDCBridge` v1.3.x):** `f.dappId = 0` always; PI dapp limbs ignored for voucher identity. Audit VkBlob pin `9dacd998…` (12 PI, 384 B public inputs).

**Fixtures (2026-08-14):** Regenerate dual-proof PoC after sync to `contracts/bridge` — `public_inputs.bin` must be **384 B** (not legacy 352 B / 11 PI):

```bash
./scripts/audit/generate_bc_an_01_dual_proofs.sh
# verify: wc -c audit/spec/an/fixtures/bc_an_01/dapp_a/public_inputs.bin → 384
```

## PoC (historical — pre-fix behaviour)

| Test | What it shows |
|------|----------------|
| `test_bc_an_01_tampered_dapp_id_rejects_same_proof` | PI dappId is ZK-bound — cannot reuse proof bytes |
| `test_bc_an_01_replay_key_differs_by_dapp_id` | PI dapp limbs differ ⇒ different blobs |
| `test_bc_an_01_double_mint_same_deposit_two_dapp_ids` | **Regression** — second finalize must NOT mint (post-fix) |
| `integration/test_bc_f8f_regressions.py` | F8-F: `f.dappId = 0` pinned |

Regenerate dual proofs (Hermez SRS k=18 — matches audit VkBlob pin `9dacd998…`, 12 PI):

```bash
./scripts/bootstrap_hermez_srs_k18.sh   # if missing deposit-prover/data/kzg_params_18.srs
./scripts/audit/generate_bc_an_01_dual_proofs.sh
cd audit/spec/an && python3 -m pytest integration/test_bc_an_01_dapp_id_double_mint.py -v
```

## Resolution (landed + author ack)

- `f.dappId = 0` in `_parsePublicInputs` (`acki-nacki@contracts/bridge`, `eccUSDCBridge` v1.3.x).
- Audit sync: `./scripts/sync_an_contracts.sh` (default `contracts/bridge`) + `preserve_audit_vk_blob.sh`.
- **Author (2026-07-20):** accepted as closed — no multi-dapp-per-deposit; `AN_DAPP_ID` config no longer affects voucher identity on AN.
