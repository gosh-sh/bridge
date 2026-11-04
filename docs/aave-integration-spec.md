# AAVE integration of `AckiNackiBridge` — requirements

| | |
|---|---|
| Status | **Normative.** What the AAVE integration must do once gosh-sh/bridge#142 is done: the change request on that PR (items 1–13) and the decisions recorded in gosh-sh/bridge-audit#120. The code does not meet it yet. |
| Written against | `d58c8553`, the #142 branch. A `file:line` points at that commit and marks behaviour that stays as it is; a requirement without one is new. Re-verify against the merge commit and bump this line when #142 lands. |
| Genre | Design and correctness. The operator runbook is [`aave-yield.md`](aave-yield.md); the verified contract reference is [`EVM-contracts-spec.md`](EVM-contracts-spec.md). |
| Paths | Relative to the repository root. |

## 1. Method

Requirements are written in **EARS** (Easy Approach to Requirements Syntax). Each one has an ID and
one of five forms:

| Form | Template | Becomes |
|---|---|---|
| Ubiquitous | The bridge shall … | a test that the behaviour holds |
| Event-driven | When ‹trigger›, the bridge shall … | a test that fires the trigger |
| State-driven | While ‹state›, the bridge shall … | a test in that state |
| Unwanted behaviour | If ‹condition›, then the bridge shall … | a negative test that forces the condition |
| Optional feature | Where ‹feature›, the bridge shall … | not used here |

System-wide properties follow the **property categories of the Certora Prover**: valid states
(invariants that hold in every reachable state), state transitions, variable transitions (which
functions may change a variable, and in which direction) and high-level properties. Each becomes a
Foundry invariant or a fuzz test.

Every requirement and property is covered by at least one test case in [§9](#9-traceability). A new
test carries its test case ID (`TC-…`) in its name or NatSpec, so the table can be checked against
the code.

## 2. Terms

| Symbol | Meaning | Source |
|---|---|---|
| `U` | USDC held by the bridge, `usdc.balanceOf(bridge)` | |
| `A` | aUSDC held by the bridge, `aUsdcBalance()` | `contracts/ethereum/src/AckiNackiBridge.sol:1947` |
| `B` | `treasuryBalance`, the principal owed to eccUSDC holders | `:108` |
| `S` | `suppliedPrincipal`, the principal booked as supplied to AAVE | `:133` |
| `L` | liquid principal: `min(U, B − S)` — the part of `B` that sits on the bridge as USDC | new |
| `r` | `liquidReserveBps` | `:138` |
| `T` | target: `B × r / 10 000` | today's reserve, `:1913` |
| `M` | multiplier, an owner setting; default 2× | new |
| `H` | threshold: `M × T` | new |
| Pool | the AAVE V3 pool `aavePool` and its aToken `aUSDC` | `:124`, `:127` |

"The pool refuses" means any of: `approve` returns false, `supply` reverts (reserve paused or
frozen, supply cap reached, any other revert), or the pool credits zero aUSDC.

## 3. Configuration (CFG)

| ID | Form | Requirement |
|---|---|---|
| CFG-1 | Unwanted | If `aavePool` or `aUSDC` is zero, then the constructor shall revert `InvalidAaveAddress`, on every chain. Today one zero reverts and two zeros pass (`:726-729`). |
| CFG-2 | Ubiquitous | `aavePool` and `aUSDC` shall stay `immutable` (`:124`, `:127`). |
| CFG-3 | Ubiquitous | `r` shall default to 1 000 (`:801`) and never exceed `MAX_LIQUID_RESERVE_BPS` = 5 000 (`:65`). Only the owner sets it, with `setLiquidReserveBps` (`:1870`). |
| CFG-4 | Ubiquitous | `M` shall be an owner setting that defaults to 2×. Only the owner sets it. |
| CFG-5 | Unwanted | If a call would leave `M ≤ 1×`, then it shall revert. |
| CFG-6 | Unwanted | If a call to `setLiquidReserveBps` or to the multiplier setter would leave `r × M` above 100%, then it shall revert. |
| CFG-7 | Event-driven | When the owner changes `r` or `M`, the next deposit and the next withdrawal shall use the new value; no redeploy. |
| CFG-8 | Ubiquitous | `aaveEnabled` shall change only through `setAaveEnabled` (`:1863`, owner) and `emergencyWithdrawAll` (`:1712`). |
| CFG-9 | Ubiquitous | The bridge shall have no external function that supplies the pool. `supplyToAave` (`:1665`) is removed. |

## 4. Deposit (DEP)

| ID | Form | Requirement |
|---|---|---|
| DEP-1 | Ubiquitous | The checks of `deposit` shall stay: zero amount, `MAX_DEPOSIT_AMOUNT`, `MAX_TRANSFER_AMOUNT` = 50 USDC, zero account, `MAX_BRIDGE_TVL` = 10 000 USDC counting the principal in AAVE (`:845-852`). |
| DEP-2 | Event-driven | When a deposit succeeds, the bridge shall credit `B` with the USDC it received and emit `Deposit` before any call to the pool (`:853-858`). |
| DEP-3 | Event-driven | When, after that credit, `aaveEnabled` is true and `L ≥ H`, the deposit shall supply `L − T` to the pool in the same transaction, leaving `L = T`. |
| DEP-4 | State-driven | While `L < H` after the credit, the deposit shall make no call to the pool. |
| DEP-5 | State-driven | While `aaveEnabled` is false, a deposit shall make no call to the pool. |
| DEP-6 | Unwanted | If the pool refuses the supply, then the deposit shall still succeed, its USDC shall stay on the bridge and `S` shall not change. |
| DEP-7 | Event-driven | When the supply succeeds, `S` shall grow by the aUSDC the pool actually credited, not by the USDC sent (today's rule, `:1675-1679`; the aUSDC delta is booked, not the amount sent), and the bridge shall emit `SuppliedToAave` (`:1681`). |
| DEP-8 | Ubiquitous | USDC on the bridge above the principal (`U − (B − S)`, when positive) shall never be supplied. |

## 5. Withdrawal (WD)

| ID | Form | Requirement |
|---|---|---|
| WD-1 | Ubiquitous | The checks of `withdrawByProof` shall stay — `dstBridge`, identity, chain, replay, `WithdrawTreasuryShortfall` — and the book shall be debited before any external call (`:1513-1587`). |
| WD-2 | State-driven | While `U` covers the payout, `withdrawByProof` shall pay from `U` and make no call to the pool. |
| WD-3 | Event-driven | When `U` does not cover the payout, the bridge shall pull at least the shortfall from the pool. |
| WD-4 | Unwanted | If the shortfall itself cannot be pulled, then the withdrawal shall revert (`InsufficientTreasury` or `AaveWithdrawFailed`, `:1929-1935`) and its nullifier shall stay unused. |
| WD-5 | Event-driven | When `U` does not cover the payout, the bridge shall also try to pull the target computed on the book after the debit, up to what the position holds, so that after the payout `L` equals that target. |
| WD-6 | Unwanted | If the part above the shortfall cannot be pulled, then the payout shall still succeed. |
| WD-7 | Ubiquitous | A pull may reach aUSDC above `S`: user principal is paid before yield (`:1589-1592`). |

## 6. Owner operations (OWN)

These stay as they are. `migrateTreasury` is listed because the band keeps principal in the pool, so
the owner has to pull it back before a migration.

| ID | Form | Requirement |
|---|---|---|
| OWN-1 | Ubiquitous | `withdrawFromAave(amount)` — owner only — shall pull at most `min(S, A)` and revert `InvalidAmount` above it or at zero (`:1687-1695`). |
| OWN-2 | Event-driven | When the owner calls `emergencyWithdrawAll`, the bridge shall pull everything, set `aaveEnabled` to false, revert `EmergencyLeftoverAToken` if aUSDC is left, and keep any shortfall booked in `S` (`:1704-1724`). |
| OWN-3 | Ubiquitous | `writeOffUnbackedPrincipal` — owner only — shall lower `S` to `A` and revert `NothingToWriteOff` when there is nothing to lower (`:1727`). |
| OWN-4 | Ubiquitous | `harvestYield(amount)` — owner only — shall pay `yieldRecipient` at most `accruedYield()` and never take `U + A` below `B` (`HarvestWouldBreakSolvency`, `:1744-1754`). |
| OWN-5 | Ubiquitous | `skimExcessUsdc(amount)` — owner only — shall pay `yieldRecipient` at most `excessUsdc()` (`:1776-1790`). |
| OWN-6 | Ubiquitous | `yieldRecipient` shall default to the owner and follow a new owner when it was still the default (`:799`, `:1895-1900`). |
| OWN-7 | State-driven | While the bridge is paused, `deposit` and `withdrawByProof` shall revert and the owner's AAVE calls and setters shall keep working (`:148-153`, `:1849`). |
| OWN-8 | Unwanted | If `migrateTreasury` is called while the book is not liquid, then it shall revert `TreasuryNotLiquid` (`:1821-1834`). |

## 7. Deployment and environments (ENV)

| ID | Form | Requirement |
|---|---|---|
| ENV-1 | Ubiquitous | The bridge shall be deployed on Arbitrum only. `DeployRealBridge` shall have no path for chain ids 1 and 11155111 (`contracts/ethereum/script/DeployRealBridge.s.sol:105-109`, `:236-246`, `:259-274`), and `DeployReuseVerifiersBridge` and `crates/bridge-prover-libraries/scripts/deploy_bridge_bundle.sh` shall be removed. |
| ENV-2 | Ubiquitous | Every bridge deployed on 421614 — by `DeployRealBridge`, by `DeployShellnetE2EBridge` (today it passes zeros, `contracts/ethereum/script/DeployShellnetE2EBridge.s.sol:146-147`) or by any other script — shall get the pool `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff` and the aToken `0x460b97BD498E1157530AEb3086301d5225b91216` from the constants in `DeployRealBridge.s.sol:101-103`, with no override. `USE_AAVE` shall be removed. |
| ENV-3 | Unwanted | If a script would deploy a mock pool on 421614, then it is wrong: no mock pool is deployed on any public chain. `MockAave` (`contracts/ethereum/test/mocks/MockAave.sol`) is for tests on a local EVM without a fork. |
| ENV-4 | Ubiquitous | Arbitrum One (42161) is out of scope here; `DeployRealBridge` does not support it yet. |

## 8. Properties

Checked by Foundry invariant tests on a local EVM with `MockAave`. The handler calls, in random
order and with random amounts: `deposit`, `withdrawByProof` (mock proof), the owner's AAVE calls,
both setters, `pause` / `unpause`, interest accrual on the mock, and the mock's failure modes
(refused supply, haircut, limited liquidity).

### Valid states

| ID | Property |
|---|---|
| INV-1 | While the pool returns what it holds, `U + A ≥ B`. |
| INV-2 | `1× < M`, `r ≤ 5 000` and `r × M ≤ 100%`. |
| INV-3 | `S ≤ B`: only user principal is booked as supplied. |

### State transitions

| ID | Property |
|---|---|
| ST-1 | After a deposit with `aaveEnabled` true and a pool that does not refuse, `L < H`. |
| ST-2 | After a withdrawal that pulled from the pool and whose extra pull succeeded, `L` equals the target on the new book. |

### Variable transitions

| ID | Property |
|---|---|
| VT-1 | `S` grows only in a deposit's supply. It falls only in `withdrawByProof`, `withdrawFromAave`, `emergencyWithdrawAll` and `writeOffUnbackedPrincipal`. |
| VT-2 | `B` grows only in `deposit` and `fundTreasury`, falls only in `withdrawByProof` and `migrateTreasury`. No AAVE function changes it. |
| VT-3 | `r` and `M` change only through their owner setters. |

### High-level properties

| ID | Property |
|---|---|
| HL-1 | No deposit that passes DEP-1 reverts, whatever the pool does. |
| HL-2 | No withdrawal reverts because of the part above the shortfall. |
| HL-3 | A deposit below the threshold and a withdrawal that `U` covers make no call to the pool, so they pay no AAVE gas. |
| HL-4 | No owner function moves user principal off the bridge: `harvestYield` and `skimExcessUsdc` never take `U + A` below `B`. |

## 9. Traceability

Layers: **U** — unit test on a local EVM with `MockAave`; **I** — Foundry invariant or fuzz test,
same setup; **F** — fork test against the AAVE V3 test deployment on Arbitrum Sepolia,
`FOUNDRY_PROFILE=fork FORK_URL=<Arbitrum Sepolia RPC>`; **E** — the local-net e2e: a local Acki
Nacki network and the public Arbitrum Sepolia ([`tests/e2e/local-net/README.md`](../tests/e2e/local-net/README.md)).

Layers U and I run on every PR (`.woodpecker/solidity.yaml`). Layer F shall run on every PR too;
today no pipeline runs it. Layer E runs on PRs labelled `local-network`.

### 9.1 Existing tests

**Keep** — unchanged. **Keep (setup)** — the assertions stay, but the setup reaches the pool through
deposits instead of `supplyToAave`. **Rewrite** — the same property checked through the new path.
**Delete** — removed with what it tests.

`contracts/ethereum/test/AckiNackiBridgeAave.t.sol`:

| Test | Action | Covers | Layer |
|---|---|---|---|
| `test_constructor_wiresAaveAndUsdc` | Keep | CFG-2, CFG-3, OWN-6 | U |
| `test_constructor_partialAaveWiringReverts` | Keep | CFG-1 | U |
| `test_constructor_zeroUsdcReverts` | Keep | constructor input | U |
| `test_constructor_noAaveIsLegal` | Rewrite: two zeros revert | CFG-1 | U |
| `test_supplyToAave_respectsLiquidReserve` | Rewrite: a deposit that crosses `H` leaves `L = T` | DEP-3 | U |
| `test_supplyToAave_explicitAmount` | Rewrite: the deposit supplies exactly `L − T` | DEP-3 | U |
| `test_supplyToAave_onlyOwner` | Rewrite: the old selector reverts for everyone | CFG-9 | U |
| `test_supplyToAave_whenDisabledReverts` | Rewrite: with `aaveEnabled` false a crossing deposit makes no pool call | DEP-5 | U |
| `test_supplyToAave_nothingToSupplyReverts` | Rewrite: a deposit below `H` makes no pool call | DEP-4 | U |
| `test_supplyToAave_amountExceedsAvailableReverts` | Rewrite: USDC above the principal is not supplied | DEP-8 | U |
| `test_supplyToAave_booksATokenDelta` | Rewrite: `S` grows by the credited aUSDC under a supply haircut | DEP-7 | U |
| `test_supplyToAave_zeroDeltaRevertsAaveSupplyFailed` | Rewrite: zero credited aUSDC — the deposit succeeds, `S` unchanged | DEP-6 | U |
| `test_eth9_approveFalse_reverts` | Rewrite: `approve` false — the deposit succeeds, `S` = 0 | DEP-6 | U |
| `test_withdrawFromAave_preemptivelyTopsUp` | Keep (setup) | OWN-1 | U |
| `test_withdrawFromAave_onlyOwner` | Keep (setup) | OWN-1 | U |
| `test_withdrawFromAave_maxPullsBackedWhenUnderBooked` | Keep (setup) | OWN-1 | U |
| `test_withdrawFromAave_aboveBackedReverts` | Keep (setup) | OWN-1 | U |
| `test_accruedYield_reflectsAaveGrowth` | Keep (setup) | OWN-4 | U |
| `test_harvestYield_sendsToRecipient` | Keep (setup) | OWN-4, OWN-6 | U |
| `test_harvestYield_partialHarvest` | Keep (setup) | OWN-4 | U |
| `test_harvestYield_amountExceedsYieldReverts` | Keep (setup) | OWN-4 | U |
| `test_harvestYield_onlyOwner` | Keep | OWN-4 | U |
| `test_harvestYield_revertsWhenWouldBreakSolvency` | Keep (setup) | OWN-4, HL-4 | U |
| `test_harvestYield_accruedYieldSafeAfterAaveRounding` | Keep (setup) | OWN-4, INV-1 | U |
| `test_emergencyWithdrawAll_pullsEverything` | Keep (setup) | OWN-2, CFG-8 | U |
| `test_eth7_emergencyLeftoverAToken_reverts_andDoesNotOpenHarvest` | Keep (setup) | OWN-2 | U |
| `test_emergencyWithdrawAll_keepsShortfallOnBooks` | Keep (setup) | OWN-2, OWN-3 | U |
| `test_writeOffUnbackedPrincipal_revertsWhenBooksAreBacked` | Keep (setup) | OWN-3 | U |
| `test_writeOffUnbackedPrincipal_revertsWhenBooksAreEmpty` | Keep | OWN-3 | U |
| `test_writeOffUnbackedPrincipal_onlyOwner` | Keep | OWN-3 | U |
| `test_writeOffUnbackedPrincipal_clampsToDust` | Keep (setup) | OWN-3 | U |
| `test_setLiquidReserveBps_capped` | Keep, extend with `r × M` | CFG-3, CFG-6 | U |
| `test_transferOwnership_flowsAllAuthorities` | Keep, extend with the multiplier setter | CFG-4, OWN-6 | U |
| `test_acceptOwnership_movesDefaultYieldRecipient_harvestPaysNewOwner` | Keep (setup) | OWN-6 | U |
| `test_acceptOwnership_keepsExplicitYieldRecipient` | Keep | OWN-6 | U |
| `testFuzz_totalAssetsCoversTreasury` | Rewrite: fuzz deposits, `r` and `M` instead of `supplyToAave` | INV-1 | I |
| `testFuzz_depositAboveTransferLimitLeavesCustodyUnchanged` | Keep | DEP-1 | I |
| `test_tvlCapIncludesPrincipalSuppliedToAave` | Keep (setup) | DEP-1 | U |

`contracts/ethereum/test/AckiNackiBridgeAavePayout.t.sol`:

| Test | Action | Covers | Layer |
|---|---|---|---|
| `test_lastPayoutReachesInterestAboveWrittenOffBook` | Keep (setup) | WD-7, OWN-3 | U |
| `test_phantomBookPayoutRevertsUntilLiquidCovers` | Keep (setup) | WD-4 | U |
| `test_payoutPullsExactShortfall` | Rewrite: the payout pulls the shortfall plus the target | WD-3, WD-5 | U |

`contracts/ethereum/test/AckiNackiBridgePause.t.sol` and
`contracts/ethereum/test/EthAuditQcHardening.t.sol`:

| Test | Action | Covers | Layer |
|---|---|---|---|
| `test_deposit_blockedWhilePaused` | Keep | OWN-7 | U |
| `test_withdrawByProof_blockedWhilePaused` | Keep | OWN-7 | U |
| `test_ownerControls_workWhilePaused` | Keep, extend with the multiplier setter | OWN-7 | U |
| `test_skimExcessUsdc_afterEmergency` | Keep (setup) | OWN-5, OWN-2 | U |
| `test_harvestYield_afterEmergency_revertsNoYield` | Keep (setup) | OWN-4, OWN-5 | U |
| `test_skimExcessUsdc_noExcess_reverts` | Keep | OWN-5 | U |

`contracts/ethereum/test/AckiNackiBridgeArbSepoliaAaveFork.t.sol`:

| Test | Action | Covers | Layer |
|---|---|---|---|
| `test_fork_aTokenIsThisPoolsCircleUsdc` | Keep | ENV-2 | F |
| `test_fork_supplyToAave` | Rewrite: a crossing deposit supplies the test market | DEP-3, DEP-7 | F |
| `test_fork_harvestYield_whileTheMarketHasABorrower` | Keep (setup) | OWN-4 | F |
| `test_fork_withdrawFromAave` | Keep (setup) | OWN-1 | F |
| `test_fork_emergencyWithdrawAll` | Keep (setup) | OWN-2 | F |
| `test_fork_withdrawByProof_topsUpFromAave` | Rewrite: the payout refills to the target | WD-3, WD-5 | F |
| `test_fork_replacesLiveBridgeAsPredecessor` | Keep | bridge replacement, outside this spec | F |

`contracts/ethereum/test/AckiNackiBridgeAaveFork.t.sol` — the four Ethereum Sepolia fork tests
(`test_fork_constructorWiresRealAave`, `test_fork_supplyAndWithdrawRoundTrip`,
`test_fork_yieldAccruesAfterTimeWarp`, `test_fork_emergencyWithdrawAllPullsAaveDown`): **Delete**
(ENV-1). Their scenarios are covered on Arbitrum Sepolia.

### 9.2 New test cases

| ID | Case | Covers | Layer |
|---|---|---|---|
| TC-01 | A deposit that leaves `L` one unit below `H` makes no call to the pool (`vm.expectCall` with count 0). | DEP-4, HL-3 | U |
| TC-02 | A deposit that leaves `L` exactly at `H` supplies `L − T`. | DEP-3 | U |
| TC-03 | The first deposit into an empty bridge (`B` was 0) supplies everything above the target. | DEP-3 | U |
| TC-04 | The pool refuses — one case each for a reverting `supply`, a paused reserve, a frozen reserve, a supply cap, zero credited aUSDC and `approve` false: the deposit succeeds, emits `Deposit`, `U` grows by the deposit, `S` unchanged. `MockAavePool` has no reverting mode today; the case needs one or `vm.mockCallRevert`. | DEP-6, HL-1 | U |
| TC-05 | USDC sent to the bridge directly, then a crossing deposit: the directly sent USDC stays on the bridge, `S ≤ B`. | DEP-8, INV-3 | U |
| TC-06 | Multiplier setter: owner only; default 2×; reverts at 1× and below. | CFG-4, CFG-5 | U |
| TC-07 | `setLiquidReserveBps` and the multiplier setter each revert when `r × M` would exceed 100%. | CFG-6 | U |
| TC-08 | After the owner changes `r` or `M`, the next deposit uses the new threshold. | CFG-7 | U |
| TC-09 | A withdrawal that `U` covers makes no call to the pool. | WD-2, HL-3 | U |
| TC-10 | A withdrawal with a shortfall leaves `L` equal to the target on the new book. | WD-5, ST-2 | U |
| TC-11 | The pool can return the shortfall but not the extra: the payout succeeds and `L` is below the target. | WD-6, HL-2 | U |
| TC-12 | The pool cannot return the shortfall: the withdrawal reverts and the nullifier stays unused. | WD-4 | U |
| TC-13 | `supplyToAave(uint256)` no longer exists: a call with its selector `0x70ded44d` reverts. | CFG-9 | U |
| TC-14 | Handler-driven invariants INV-1, INV-2, INV-3, ST-1, VT-1, VT-2, VT-3, HL-1, HL-2, HL-4 (§8). | §8 | I |
| TC-15 | A deposit below the threshold makes no call to the test market. | DEP-4 | F |
| TC-16 | The test market's reserve is paused, then frozen, through the pool's admin role on the fork: a crossing deposit succeeds and its USDC stays on the bridge. | DEP-6 | F |
| TC-17 | The test market's supply cap is lowered below the supply on the fork: a crossing deposit succeeds. | DEP-6 | F |
| TC-18 | The reserve's free liquidity is borrowed out on the fork: a withdrawal whose shortfall the pool cannot return reverts; one whose shortfall it can return but not the extra succeeds. | WD-4, WD-6 | F |
| TC-19 | After `vm.warp` with interest accrued, a refill and a harvest keep `U + A ≥ B`. | INV-1, WD-5 | F |
| TC-20 | `DeployRealBridge` and `DeployShellnetE2EBridge` run on the fork: the deployed bridge's `aavePool()` and `aUSDC()` equal the two constants; on any other chain id `DeployRealBridge` reverts. | ENV-1, ENV-2 | F |
| TC-21 | After the deposit (`test_40`–`44`): the bridge's `aavePool()` and `aUSDC()` equal the two constants, and part of the 2 USDC is in the pool (`S > 0`). | ENV-2, DEP-3 | E |
| TC-22 | After `withdrawByProof` (`test_70`–`73`): the bridge pulled from the pool and paid 2 USDC. | WD-3 | E |

## 10. Open questions

Decisions this specification needs and does not make. Each is answered in gosh-sh/bridge-audit#120
before the code is written.

| ID | Question |
|---|---|
| OQ-1 | `excessUsdc()` counts USDC above the whole book, `U − B` (`:1776-1779`). While principal sits in the pool, `U < B`, so USDC sent to the bridge directly cannot be skimmed until it exceeds `B − U`. DEP-8 measures the excess as `U − (B − S)`. Should `excessUsdc()` use the same measure? |
| OQ-2 | `r` = 0 is accepted today (`:1870-1874`). With a zero target the threshold is zero, and every deposit supplies — the per-transaction AAVE gas the band exists to avoid. Should `r` have a lower bound above zero? |
| OQ-3 | DEP-6 lets a deposit succeed when the pool refuses, so a broken pool leaves no trace on chain. Should the bridge emit an event naming the refusal, so that monitoring sees it? |
