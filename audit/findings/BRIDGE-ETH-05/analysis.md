# BRIDGE-ETH-05 — withdraw-on without verifyBlock

**Class:** **QC** (deploy / product-dead payout path)  
**Status:** **open → patched in this change** (`WithdrawRequiresVerifyBlock`, `PartialVerifyBlockWiring`; mainnet `envBool` on `DeployRealBridge`)  
**Area:** `AckiNackiBridge` constructor, `DeployRealBridge`, `DeployGenesisCursorBridge`, `DeployReuseVerifiersBridge`  
**Source:** Stage II Q&A PDF ETH-5  
**Invariant:** WD / BK — `withdrawByProof` can only pay against an in-window `finalRoot` written by `verifyBlock`

## Summary

`withdrawByProof` keys payouts on layer-window anchors. Those anchors only appear after a successful `verifyBlock`. Constructor previously allowed Circuit 4 wired (`bridgeWithdrawalVerifier ≠ 0`) with the 1A/1B/C2 triple all zero. Windows stay empty; every withdraw reverts `UnknownAnchor`. That is not a drain — it is a silently dead product.

`DeployRealBridge` compounded it: `WIRE_VERIFY_BLOCK` defaulted false via `vm.envOr`, and `USE_AXIOM_ORACLE` defaulted false (Mock). Mainnet could ship Mock oracle + withdraw-on + verifyBlock-off.

## PoC

Gate: `cd contracts/ethereum && forge test --match-test test_constructor_withdrawWithoutVerifyBlock -vv`

| Test | What it shows |
|------|----------------|
| `test_constructor_withdrawWithoutVerifyBlock_reverts` | Withdraw verifier + accFr set, verifyBlock triple zero → `WithdrawRequiresVerifyBlock`. |
| `test_constructor_partialVerifyBlockWiring_reverts` | 1 of 3 attestation/layer verifiers zero → `PartialVerifyBlockWiring`. |
| `test_constructor_verifyBlockOnly_succeeds` | verifyBlock-only (withdraw off) remains legal. |
| `test_constructor_withdrawEnabledWithoutAccFr_reverts` | `accFr == 0` still `InvalidBridgeWithdrawalIdentity` (checked first). |

## Fix

Constructor: if any of the three verifyBlock addresses is set, all three must be set. If Circuit 4 is set, the triple must be fully wired and `accFr ≠ 0`.

`DeployRealBridge` on `chainid == 1`: `USE_AXIOM_ORACLE` and `WIRE_VERIFY_BLOCK` via `vm.envBool` (unset reverts); Mock oracle forbidden. `DeployGenesisCursorBridge` / `DeployReuseVerifiersBridge` refuse `chainid == 1` (they always deploy Mock).

## Notes

- Deposit-only (`disabled()` + `disabledWithdraw()`) stays legal for tests and bring-up.
- This does not pin Axiom vs Mock on Sepolia; operators still choose. Mainnet is the hard gate.
