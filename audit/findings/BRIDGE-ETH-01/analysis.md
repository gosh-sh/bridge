# BRIDGE-ETH-01 — congruent nullifier double-pays

**Class:** **BC** (High — fund loss)  
**Status:** **open → patched in this change** (`FieldElementOutOfRange` on `pub.nullifier`)  
**Area:** `AckiNackiBridge.withdrawByProof`, `BridgeWithdrawalAggregatorVerifier`, Halo2 Yul `mod(calldataload, f_q)`  
**Source:** Stage II Q&A PDF ETH-1  
**Invariant:** WD-7 — a nullifier pays out at most once (`audit/reports/manual-audit/invariants-extended.md`)

## Summary

The SHPLONK Yul verifier reduces every public instance `mod f_q` (`BN254_R`). The R15 adapter compares the *raw* 32-byte word against `pub.nullifier`. The bridge keys `_nullifiers[bytes32(pub.nullifier)]` by that same raw word.

An attacker who already holds a valid Circuit 4 aggregator calldata for nullifier `N` can submit the same proof with instance slot 20 and `pub.nullifier` both set to `N + k·R`. Pairing still accepts (same Fr). The mapping treats `N` and `N+k·R` as distinct keys. Treasury pays twice.

Other public inputs are not a second drain: `amount + R` overflows any realistic treasury (`WithdrawTreasuryShortfall`); `recipientHi/Lo + R` fail the 80-bit range check; identity/`dstChainId`/`tokenId` are pinned to constructor / `block.chainid` values.

## PoC

Gate: `cd contracts/ethereum && forge test --match-contract AckiNackiBridgeEthFieldCongruence -vv`

| Test | What it shows |
|------|----------------|
| `test_eth1_productionYul_acceptsNullifierPlusR` | Committed Circuit 4 `.bin` + `_calldata.bin`: canonical verifies; `nullifier += R` in instance *and* `pub` still verifies. |
| `test_eth1_withdrawByProof_nullifierPlusR_doesNotPayTwice` | Full bridge with that production adapter: honest `N` pays once; `N+R` must revert `FieldElementOutOfRange`. **Before the gate this call succeeded and paid a second time.** |
| `test_eth1_yulModelMock_nullifierPlusR_doesNotPayTwice` | Same mapping invariant without `.bin` (adapter-style raw compare). |

Pre-gate forge output (2026-08-27):

```
[PASS] test_eth1_productionYul_acceptsNullifierPlusR()
[FAIL: next call did not revert as expected] test_eth1_withdrawByProof_nullifierPlusR_doesNotPayTwice()
```

## Fix

Reject `pub.nullifier >= BN254_R` (and `pub.finalRoot`) *before* the mapping lookup. Do **not** reduce-and-key: that would alias two caller-supplied words onto one slot and hide the unreduced submission. Honest prover already emits canonical Fr.

## Notes

- A3 withdraw pass (`audit/reports/manual-audit/A3-withdraw.md`) missed this: it treated WD-7 as exact-word replay only.
- Adapter byte-for-byte compare does **not** close the hole: the attacker mutates the instance word *together with* `pub`.
- `docs/operations/bridge_verification.md` BK-6 already notes that Yul auto-reduction is real and that adapters compare before pairing — ETH-1 is the fund-loss dual of that observation.
