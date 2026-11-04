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

## 2. Glossary

Every term this document uses is defined here, and no other word is used for the same thing.

### Words

| Term | Meaning |
|---|---|
| **AAVE test market** | AAVE V3 deployed by AAVE itself on the test network Arbitrum Sepolia (chain id 421614), as listed in AAVE's address book: the pool `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff`, the aToken `0x460b97BD498E1157530AEb3086301d5225b91216`, and Circle's test USDC `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` as the listed asset. It runs AAVE's own code; only the network and the tokens are test ones. It is not a mock. The bridge takes these addresses from `contracts/ethereum/script/DeployRealBridge.s.sol:101-103`. |
| **aToken, aUSDC** | The token AAVE mints to the bridge for the USDC it supplies. Its balance grows as borrowers pay interest. The bridge holds its address in the immutable `aUSDC` (`contracts/ethereum/src/AckiNackiBridge.sol:127`). |
| **Band** | The range in which the bridge keeps its liquid principal: from the target up to the threshold. A deposit that takes it to the threshold sends everything above the target to the pool; a withdrawal that runs short brings it back to the target. |
| **Book** | The bridge's accounting, as opposed to the tokens it holds: `treasuryBalance` (owed to users) and `suppliedPrincipal` (put into the pool). |
| **Crossing deposit** | A deposit after which the liquid principal is at or above the threshold, so it supplies the pool (DEP-3). |
| **Certora property categories** | The classification used by Certora Prover specifications: valid states, state transitions, variable transitions, high-level properties (§1, §8). |
| **EARS** | Easy Approach to Requirements Syntax (Mavin et al., Rolls-Royce, 2009): five sentence templates for requirements (§1). |
| **Excess** | `X` below. |
| **Fork test** | A Foundry test run with `FOUNDRY_PROFILE=fork FORK_URL=<RPC>`. Forge copies the state of the chain behind the RPC into a local EVM as the test reads it and runs the test there: the code of the real contracts executes, and no transaction reaches the chain. Here the chain is Arbitrum Sepolia and the contracts are the AAVE test market. Layer F. |
| **Haircut** | A switch of `MockAavePool` that models a loss in the pool: a supply haircut credits less aUSDC than the USDC supplied (`setSupplyHaircut`), a redeem haircut returns less USDC than was pulled (`setRedeemHaircut`). |
| **Handler** | The contract a Foundry invariant test drives. It exposes the actions — deposit, withdrawal, owner calls, the mock's failure modes — that the fuzzer calls in random order. |
| **Leftover** | A switch of `MockAavePool` (`setLeftoverOnMaxWithdraw`) that leaves aUSDC behind when the bridge pulls everything, as a paused or illiquid pool would. |
| **Liquid principal** | `L` below. |
| **Local EVM** | The EVM inside `forge test` with no fork: an empty chain in memory, with no RPC. Layers U and I run on it. |
| **Local-net e2e** | `.woodpecker/local-net-e2e.yaml`: a whole Acki Nacki network on one host, working against the public Arbitrum Sepolia, with a bridge deployed fresh on every run ([`tests/e2e/local-net/README.md`](../tests/e2e/local-net/README.md)). Layer E. |
| **`MockAave`** | `contracts/ethereum/test/mocks/MockAave.sol`: a fake pool (`MockAavePool`) and aToken (`MockAUSDC`) written for tests, with switches for haircuts and leftovers. Only on a local EVM (ENV-3). |
| **Multiplier** | `M` below. |
| **Nullifier** | The key `withdrawByProof` records for each paid withdrawal, so that a proof pays once. A reverted withdrawal leaves it unused, and the withdrawal can be retried. |
| **Pool** | `aavePool`, the AAVE V3 `Pool` contract the bridge supplies to and pulls from (`:124`). On Arbitrum Sepolia it is the pool of the AAVE test market. |
| **Principal** | Users' money: the USDC the bridge owes eccUSDC holders, `treasuryBalance`. Interest earned in the pool is yield, not principal. |
| **Pull** | The bridge withdrawing USDC from the pool (`_pullFromAave`, `:1929`). |
| **Refill** | The part of a pull above the shortfall, which brings the liquid principal back to the target (WD-5). |
| **Reserve (AAVE)** | AAVE's name for one asset's market inside the pool, here USDC. It has its own pause and freeze switches and a supply cap, set by AAVE's admin roles. Not the same as `liquidReserveBps`, which the glossary calls the target. |
| **Refuse** | The pool refuses a supply when `approve` returns false, `supply` reverts (reserve paused or frozen, supply cap reached, any other revert), or the pool credits zero aUSDC. |
| **Shortfall** | How much the USDC on the bridge falls short of a payout. |
| **Supply** | The bridge depositing USDC into the pool and receiving aUSDC for it. |
| **Supply cap** | The most USDC the AAVE reserve accepts in total; a supply above it reverts. |
| **Target** | `T` below. |
| **TC-…** | A test case of §9.2. |
| **Threshold** | `H` below. |
| **`yieldRecipient`** | The address that receives harvested yield and skimmed excess; by default the owner (`:799`). |

### Symbols

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
| `X` | excess: `min(U − (B − S), U + A − B)` when positive, else 0 — USDC above the liquid part of the principal that the bridge can give away and still cover its debt | new; today `excessUsdc()` is `U − B`, `:1776-1779` |

## 3. Configuration (CFG)

| ID | Form | Requirement |
|---|---|---|
| CFG-1 | Unwanted | If `aavePool` or `aUSDC` is zero, then the constructor shall revert `InvalidAaveAddress`, on every chain. Today one zero reverts and two zeros pass (`:726-729`). |
| CFG-2 | Ubiquitous | `aavePool` and `aUSDC` shall stay `immutable` (`:124`, `:127`). |
| CFG-3 | Ubiquitous | `r` shall default to 1 000 (`:801`) and stay between 1 000 and `MAX_LIQUID_RESERVE_BPS` = 5 000 (`:65`). Only the owner sets it, with `setLiquidReserveBps` (`:1870`). |
| CFG-10 | Unwanted | If a call would set `r` below 1 000, then it shall revert. Today only the upper bound is checked and 0 passes (`:1870-1872`); a zero target would make every deposit and every withdrawal call the pool. |
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
| DEP-9 | Event-driven | When the pool refuses the supply, the bridge shall emit an event with the amount it tried to supply and the reason: the pool's revert data, or a distinct reason for `approve` returning false and for zero credited aUSDC. The name of the event is the author's choice. |

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
| WD-8 | Event-driven | When the part above the shortfall cannot be pulled, the bridge shall emit an event with the amount it did not pull and the pool's revert data. The name of the event is the author's choice. |

## 6. Owner operations (OWN)

These stay as they are, except OWN-5, which measures the excess as `X`. `migrateTreasury` is listed
because the band keeps principal in the pool, so the owner has to pull it back before a migration.

| ID | Form | Requirement |
|---|---|---|
| OWN-1 | Ubiquitous | `withdrawFromAave(amount)` — owner only — shall pull at most `min(S, A)` and revert `InvalidAmount` above it or at zero (`:1687-1695`). |
| OWN-2 | Event-driven | When the owner calls `emergencyWithdrawAll`, the bridge shall pull everything, set `aaveEnabled` to false, revert `EmergencyLeftoverAToken` if aUSDC is left, and keep any shortfall booked in `S` (`:1704-1724`). |
| OWN-3 | Ubiquitous | `writeOffUnbackedPrincipal` — owner only — shall lower `S` to `A` and revert `NothingToWriteOff` when there is nothing to lower (`:1727`). |
| OWN-4 | Ubiquitous | `harvestYield(amount)` — owner only — shall pay `yieldRecipient` at most `accruedYield()` and never take `U + A` below `B` (`HarvestWouldBreakSolvency`, `:1744-1754`). |
| OWN-5 | Ubiquitous | `excessUsdc()` shall return `X`, and `skimExcessUsdc(amount)` — owner only — shall pay `yieldRecipient` at most `X` (`:1776-1790`). USDC sent to the bridge directly can then be skimmed while principal sits in the pool, and never when it covers a loss in the pool. |
| OWN-6 | Ubiquitous | `yieldRecipient` shall default to the owner and follow a new owner when it was still the default (`:799`, `:1895-1900`). |
| OWN-7 | State-driven | While the bridge is paused, `deposit` and `withdrawByProof` shall revert and the owner's AAVE calls and setters shall keep working (`:148-153`, `:1849`). |
| OWN-8 | Unwanted | If `migrateTreasury` is called while the book is not liquid, then it shall revert `TreasuryNotLiquid` (`:1821-1834`). |
| OWN-9 | Unwanted | If `migrateTreasury`'s `approve` returns false, or the successor takes a different amount than the book, then it shall revert `WithdrawTransferFailed` (`:1837-1841`). |
| OWN-10 | Unwanted | If the USDC transfer to `yieldRecipient` fails, then `harvestYield` and `skimExcessUsdc` shall revert `WithdrawTransferFailed` and leave the books unchanged (`:1767-1768`, `:1789-1790`). |
| OWN-11 | Unwanted | If `fundTreasury` is called with zero, then it shall revert `InvalidAmount` (`:1806`). |

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
| INV-2 | `1× < M`, `1 000 ≤ r ≤ 5 000` and `r × M ≤ 100%`. |
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
same setup; **F** — fork test against the AAVE test market,
`FOUNDRY_PROFILE=fork FORK_URL=<Arbitrum Sepolia RPC>`; **E** — the local-net e2e: a local Acki
Nacki network and the public Arbitrum Sepolia ([`tests/e2e/local-net/README.md`](../tests/e2e/local-net/README.md)).

Layers U and I run on every PR (`.woodpecker/solidity.yaml`). Layer F shall run on every PR too
(DLV-1); today no pipeline runs it. Layer E runs on PRs labelled `local-network`.

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
| `test_setLiquidReserveBps_capped` | Keep, extend with the lower bound and `r × M` | CFG-3, CFG-6, CFG-10 | U |
| `test_transferOwnership_flowsAllAuthorities` | Keep, extend with the multiplier setter | CFG-4, OWN-6 | U |
| `test_acceptOwnership_movesDefaultYieldRecipient_harvestPaysNewOwner` | Keep (setup) | OWN-6 | U |
| `test_acceptOwnership_keepsExplicitYieldRecipient` | Keep | OWN-6 | U |
| `testFuzz_totalAssetsCoversTreasury` | Rewrite: fuzz deposits, `r` within 1 000–5 000 (today from 0, `AckiNackiBridgeAave.t.sol:522`) and `M` instead of `supplyToAave` | INV-1 | I |
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

WD-1 is covered by the 45 tests of `contracts/ethereum/test/AckiNackiBridgeWithdrawByProof.t.sol`,
which stay as they are. ENV-4 states the scope and has no test.

Three branches cannot be reached, so `forge coverage` will always count them as not hit; the author
either removes them or keeps them as defensive checks:

- `skimExcessUsdc` reverting `InvalidRecipient` (`:1785`): `yieldRecipient` is never zero — the
  constructor sets the deployer (`:799`), `setYieldRecipient` refuses zero (`:1877`) and
  `acceptOwnership` sets the new owner (`:1900`).
- After CFG-1, `setAaveEnabled(true)` without a pool (`:1864`) and `aUsdcBalance()` without an
  aToken (`:1948`).

`contracts/ethereum/test/AckiNackiBridgeArbSepoliaAaveFork.t.sol`:

| Test | Action | Covers | Layer |
|---|---|---|---|
| `test_fork_aTokenIsThisPoolsCircleUsdc` | Keep | ENV-2 | F |
| `test_fork_supplyToAave` | Rewrite: a crossing deposit supplies the AAVE test market | DEP-3, DEP-7 | F |
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
| TC-04 | The pool refuses — one case each for a reverting `supply`, a paused reserve, a frozen reserve, a supply cap, zero credited aUSDC and `approve` false: the deposit succeeds, emits `Deposit`, `U` grows by the deposit, `S` unchanged, and the refusal event carries the amount and the reason of that case. `MockAavePool` has no reverting mode today; the case needs one or `vm.mockCallRevert`. | DEP-2, DEP-6, DEP-9, HL-1 | U |
| TC-05 | USDC sent to the bridge directly, then a crossing deposit: the directly sent USDC stays on the bridge, `S ≤ B`. | DEP-8, INV-3 | U |
| TC-06 | Multiplier setter: owner only; default 2×; reverts at 1× and below. | CFG-4, CFG-5 | U |
| TC-07 | `setLiquidReserveBps` and the multiplier setter each revert when `r × M` would exceed 100%. | CFG-6 | U |
| TC-08 | After the owner changes `r` or `M`, the next deposit uses the new threshold. | CFG-7 | U |
| TC-09 | A withdrawal that `U` covers makes no call to the pool. | WD-2, HL-3 | U |
| TC-10 | A withdrawal with a shortfall leaves `L` equal to the target on the new book. | WD-5, ST-2 | U |
| TC-11 | The pool can return the shortfall but not the extra: the payout succeeds, `L` is below the target, and the event names the amount not pulled and the reason. | WD-6, WD-8, HL-2 | U |
| TC-12 | The pool cannot return the shortfall, in two cases: it holds less than the shortfall (`InsufficientTreasury`, `:1930`), and it returns less than it was asked for (`AaveWithdrawFailed`, `:1935`, never hit by a test today). Each time the withdrawal reverts and the nullifier stays unused. | WD-4 | U |
| TC-13 | `supplyToAave(uint256)` no longer exists: a call with its selector `0x70ded44d` reverts. | CFG-9 | U |
| TC-14 | Handler-driven invariants INV-1, INV-2, INV-3, ST-1, VT-1, VT-2, VT-3, HL-1, HL-2, HL-4 (§8). | §8 | I |
| TC-15 | A deposit below the threshold makes no call to the AAVE test market. | DEP-4 | F |
| TC-16 | The AAVE test market's reserve is paused, then frozen, through the pool's admin role on the fork: a crossing deposit succeeds, its USDC stays on the bridge, and the refusal event carries the pool's revert data. | DEP-6, DEP-9 | F |
| TC-17 | The AAVE test market's supply cap is lowered below the supply on the fork: a crossing deposit succeeds and the refusal event carries the pool's revert data. | DEP-6, DEP-9 | F |
| TC-18 | The reserve's free liquidity is borrowed out on the fork: a withdrawal whose shortfall the pool cannot return reverts; one whose shortfall it can return but not the extra succeeds and emits the event of WD-8. | WD-4, WD-6, WD-8 | F |
| TC-19 | After `vm.warp` with interest accrued, a refill and a harvest keep `U + A ≥ B`. | INV-1, WD-5 | F |
| TC-20 | `DeployRealBridge` and `DeployShellnetE2EBridge` run on the fork: the deployed bridge's `aavePool()` and `aUSDC()` equal the two constants; on any other chain id `DeployRealBridge` reverts. | ENV-1, ENV-2, ENV-3 | F |
| TC-21 | After the deposit (`test_40`–`44`): the bridge's `aavePool()` and `aUSDC()` equal the two constants, and part of the 2 USDC is in the pool (`S > 0`). | ENV-2, ENV-3, DEP-3 | E |
| TC-22 | After `withdrawByProof` (`test_70`–`73`): the bridge pulled from the pool and paid 2 USDC. | WD-3 | E |
| TC-23 | `setLiquidReserveBps(0)` and `setLiquidReserveBps(999)` revert; `setLiquidReserveBps(1_000)` passes. | CFG-10 | U |
| TC-24 | With principal in the pool, USDC sent to the bridge directly is skimmable at once: `excessUsdc()` equals it, `skimExcessUsdc` pays it to `yieldRecipient`, `B` and `S` unchanged. | OWN-5 | U |
| TC-25 | The pool lost part of the principal (redeem haircut on the mock) and USDC sent directly covers the loss: `excessUsdc()` never exceeds `U + A − B`, and a skim never takes `U + A` below `B`. | OWN-5, HL-4 | U |
| TC-26 | A paused bridge with principal in the pool: `migrateTreasury` reverts `TreasuryNotLiquid`; after `withdrawFromAave(type(uint256).max)` it moves the whole book. No test checks `TreasuryNotLiquid` today. | OWN-8 | U |
| TC-27 | `harvestYield` while the pool returns less than it was asked for: reverts `AaveWithdrawFailed` (`:1763`), `accruedYield()` and the books unchanged. | OWN-4 | U |
| TC-28 | `migrateTreasury` with USDC whose `approve` returns false, and with a successor that takes less than the book: each reverts `WithdrawTransferFailed`, the book stays on the old bridge. | OWN-9 | U |
| TC-29 | USDC whose `transfer` to `yieldRecipient` returns false: `harvestYield` and `skimExcessUsdc` each revert `WithdrawTransferFailed`, `S`, `B` and `accruedYield()` unchanged. | OWN-10 | U |
| TC-30 | `fundTreasury(0)` reverts `InvalidAmount`, from the owner and from the predecessor. | OWN-11 | U |
| TC-31 | `withdrawFromAave(type(uint256).max)` pulls `S` when the pool holds more than `S` (interest), and `A` when it holds less (loss) — both sides of `_backedPrincipal` (`:1920-1924`). | OWN-1 | U |

## 10. Delivery (DLV)

What the change has to bring besides the contract and the tests.

| ID | Form | Requirement |
|---|---|---|
| DLV-1 | Ubiquitous | The Arbitrum Sepolia fork suite (layer F) shall run in CI on every PR. Today no pipeline runs it (`.woodpecker/solidity.yaml`, header). |
| DLV-2 | Ubiquitous | `forge coverage` shall hit every reachable AAVE branch of `AckiNackiBridge.sol`; the unreachable ones are listed in §9.1. On `d58c8553` the file stands at 96% of lines and 76% of branches. |
| DLV-3 | Ubiquitous | `CHANGELOG.md` under `[Unreleased]` shall list, as breaking, the removal of `supplyToAave`, of `USE_AAVE` and of the Ethereum deploy paths, and, as new, the band, the multiplier setter and the two events. |
| DLV-4 | Ubiquitous | `docs/aave-yield.md` and `docs/EVM-contracts-spec.md` shall describe the new behaviour, and the "Written against" line of this document shall name the merge commit. |
