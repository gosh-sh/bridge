# BC-AN-01 — dappId not bound to L1 event

**Class:** BC candidate (High)  
**Status:** open — full dual-proof PoC landed (2026-07-17)  
**Area:** `USDCBridge.finalizeDeposit` replay key

## Summary

Replay anchor is `tvm.hash(abi.encode(depositId, contractAddr, dappId))`. The deposit circuit binds `depositId`, `contractAddr`, and receipt fields from the L1 event, but `dappId` is supplied via prover config (`AN_DAPP_ID`), not read from the Ethereum `Deposit` log.

Two valid proofs over the **same** Sepolia receipt with different `dappId` values could yield two distinct voucher addresses and two ECC mints for one L1 deposit.

## PoC (landed in audit)

| Test | What it shows |
|------|----------------|
| `test_bc_an_01_tampered_dapp_id_rejects_same_proof` | PI dappId is ZK-bound — cannot reuse proof bytes |
| `test_bc_an_01_replay_key_differs_by_dapp_id` | Contract replay key splits on dappId limbs |
| `test_bc_an_01_double_mint_same_deposit_two_dapp_ids` | **Full fund-loss PoC** — two finalize+mint for one depositId |
| `integration/test_bc_f8f_regressions.py` | F8-F source regression (no `EXPECTED_DAPP_ID`) |

Regenerate dual proofs (Hermez SRS — matches audit USDCBridge VK `724687a4…`):

```bash
./scripts/bootstrap_hermez_srs_k18.sh
./scripts/audit/generate_bc_an_01_dual_proofs.sh
cd audit/spec/an && python3 -m pytest integration/test_bc_an_01_dapp_id_double_mint.py::test_bc_an_01_double_mint_same_deposit_two_dapp_ids -q
```

See `audit/knowledge/hermez_kzg_pins.md` for repo/branch pins.

## Mitigation options

- On-chain `EXPECTED_DAPP_ID` immutable in `USDCBridge`.
- Remove `dappId` from replay hash if single-dapp deployment.
- Bind `dappId` in-circuit to a constant or L1 event field.
