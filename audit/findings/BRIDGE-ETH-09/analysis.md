# BRIDGE-ETH-09 — `altTokenId` on mainnet; `approve` return ignored

**Class:** **QC** (deploy / token hygiene; not a FoT generalization)  
**Status:** **open → patched in this change** (`DeployRealBridge` mainnet `altTokenId == 0`, `ApproveFailed`)  
**Area:** constructor `bridgeWithdrawalAltTokenId`, `supplyToAave` `usdc.approve`, `DeployRealBridge`  
**Source:** Stage II Q&A PDF ETH-9; QC-A1-2  
**Invariant:** WD-4 — production token id is `0` (bridged USDC); TR-4 — ERC-20 calls must observe their bool returns (USDC, not FoT)

## Summary

`altTokenId` is a shellnet alias (`USDC_ECC_ID = 3`). On mainnet it would accept Circuit 4 proofs whose `tokenId` is not 0 and still pay **USDC** from the treasury — a cross-token confusion if AN ever emits a non-zero id.

The gate is **`DeployRealBridge` on `chainid == 1`**, not the constructor. Constructor cannot use `block.chainid == 1`: Foundry tests `vm.chainId(1)` so Circuit 4 artefacts (whose `dstChainId` is 1 and whose synthetic `tokenId` is not always 0) can withdraw. A constructor/runtime reject would brick those pairing-bound tests and any mainnet-shaped replay.

`deposit` already reverts `TransferFromFailed` if `transferFrom` returns false. `supplyToAave` called `usdc.approve` and ignored the bool. Circle USDC returns bool; ignoring it is inconsistent and would book `suppliedPrincipal` even if a token refused the approve (the mock still writes allowance when returning false).

Do **not** add fee-on-transfer *support* (credit the net received). ETH-11 fail-closed: custody delta must equal `amount` or `deposit` / `withdrawByProof` revert. `DepositFoTInvariant.t.sol` asserts the revert.

## PoC

| Test | What it shows |
|------|----------------|
| `test_eth9_approveFalse_reverts` | Mock `approve` returns false → `ApproveFailed`; `suppliedPrincipal` stays 0. |
| `test_withdrawByProof_shellnetAliasAcceptedOnSepoliaOnly` | Non-zero `altTokenId` still legal off the production script. |
| `DeployRealBridge` | `require(w.altTokenId == 0)` when `chainid == 1`. |

## Fix

`DeployRealBridge` mainnet: `altTokenId` must be 0. `supplyToAave`: `if (!usdc.approve(...)) revert ApproveFailed()`. Helper scripts that default `altTokenId = 3` already refuse mainnet (ETH-5).
