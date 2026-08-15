# TD-15 — depositId collision without `srcChainId` (multi-L2)

PoC: `crates/deposit-relayer-daemon/tests/td_15_deposit_id_collision.rs`.

**DEP-N-5** replay identity: `(srcChainId, depositId, contractAddr, dappId)`. Checked across bridge + voucher in `scripts/check_voucher_abi_consistency.py`; patch `USDCBridge_12pi_chainid_allowlist.patch` (TD-04 context — not merged on all deployments).

## CREATE2 same bridge address scenario

Two L2s deploy `AckiNackiBridge` at the same address (CREATE2 salt). Each chain’s `depositCounter` is independent → both can emit `depositId = 42` for the same `contractAddr` and `dappId`. Without `srcChainId` in the voucher key, the second chain’s deposit is treated as replay: AN returns already-finalized, **no mint**, L1 funds stuck (no refund).

## PoC verdict (mock)

| Nullifier model | Sepolia id=42 then Base id=42 | Verdict |
|-----------------|-------------------------------|---------|
| DEP-N-5 `(chainId, depositId, contract, dappId)` | two `Finalized` | **OK** — intended policy |
| Legacy `depositId` only | second `AlreadyFinalized`, mint count 1 | **BC risk** — documents pre-patch gap |

Same-chain replay → `AlreadyFinalized` in both models (**OK**).

Finding `BRIDGE-XXX` not opened: PoC shows patch-shaped policy prevents silent swallow; live AN without DEP-N-5 remains deployment risk (TD-04).
