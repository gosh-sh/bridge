# BC-AN-02 — no L1 bridge address allowlist

**Class:** BC candidate (Medium)  
**Status:** open — author confirm  
**Area:** `USDCBridge.finalizeDeposit` / `fr[3]` contractAddr

## Summary

`contractAddr` (Fr[3]) participates in the replay key but is not checked against a configured expected Sepolia `AckiNackiBridge` address. A proof valid for deposit on bridge deployment A could finalize on shellnet USDCBridge if the circuit only checks “some contract emitted Deposit”, not a specific deployment pin.

## PoC plan

Requires a proof whose `contractAddr` PI differs from the operator’s canonical bridge — dual deployment or crafted circuit config.

## Mitigation

`require(f.contractAddr == EXPECTED_L1_BRIDGE)` (immutable or config).

## Tests

- `integration/test_bc_an_01_dapp_id_double_mint.py::test_bc_an_02_no_l1_bridge_allowlist_pre_zk` — no dedicated revert before ZK.

