# AAVE V3 Yield Integration — Design and Correctness

This article documents the AAVE V3 integration that puts idle bridge ETH to work earning yield, and provides a step-by-step protocol for verifying the integration is correct.

The integration was added on top of the existing `AckiNackiBridge` and is **opt-in**: passing `address(0)` for the AAVE constructor arguments leaves the bridge in plain-ETH mode (the original behaviour).

---

## 1. Goals and Non-Goals

### Goals

- Earn yield on the deposited ETH that would otherwise sit idle in `AckiNackiBridge`.
- Keep `deposit()` and `withdraw()` user-experience identical:
  - `deposit()` stays cheap — no AAVE call on the user path.
  - `withdraw()` works whether the funds are currently held as ETH or as aWETH.
- Prevent any path through which the operator can divert user principal to themselves.
- Keep the integration removable: a single owner call (`emergencyWithdrawAll`) returns all funds to ETH and disables further supplies.

### Non-Goals

- Multi-asset support. Only native ETH is bridged, so we only need WETH/aWETH.
- Borrowing or leverage. We only `supply` and `withdraw`.
- Active rebalancing. The owner (or a keeper bot) rebalances manually via `supplyToAave` / `withdrawFromAave`.

---

## 2. Architecture

```
                 ┌─────────────────────────────────────┐
                 │         AckiNackiBridge             │
   user ───────► │  deposit()    (cheap, no AAVE)      │
                 │  withdraw()   (auto-pulls if short) │
                 │                                     │
   owner ──────► │  supplyToAave()                     │
                 │  withdrawFromAave()                 │
                 │  emergencyWithdrawAll()             │
                 │  harvestYield()                     │
                 │                                     │
                 │  state: ETH balance + aWETH balance │
                 │  book:  treasuryBalance, suppliedPrincipal
                 └─────────────────┬───────────────────┘
                                   │
              wethGateway          ▼
              ┌────────────────────────────────────┐
              │ WrappedTokenGatewayV3              │
              │  depositETH  →  WETH.deposit       │
              │              →  Pool.supply(WETH)  │
              │  withdrawETH →  Pool.withdraw(WETH)│
              │              →  WETH.withdraw      │
              │              →  ETH back to bridge │
              └────────────────┬───────────────────┘
                               │
                               ▼
              ┌─────────────────────────────────────┐
              │ AAVE V3 Pool (mainnet)              │
              │  mints aWETH (rebasing) on supply   │
              │  burns aWETH on withdraw            │
              └─────────────────────────────────────┘
```

Mainnet addresses (hardcoded in `script/DeployRealBridge.s.sol`):

| Component | Address |
|---|---|
| AAVE V3 Pool | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` |
| WrappedTokenGatewayV3 | `0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C` |
| aWETH | `0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8` |

On other chains the contract takes the addresses from the constructor — nothing is hardcoded inside `AckiNackiBridge.sol`.

---

## 3. State and Bookkeeping

The bridge separates three quantities:

| Quantity | Meaning | Where stored |
|---|---|---|
| `treasuryBalance` | Sum of user principal currently owed by the bridge (deposits − withdrawals) | `uint256 public treasuryBalance` |
| `suppliedPrincipal` | Book value of ETH currently parked in AAVE (excludes interest) | `uint256 public suppliedPrincipal` |
| Yield | aWETH balance above `suppliedPrincipal` (i.e. AAVE-accrued interest) | derived: `aWETH.balanceOf(this) - suppliedPrincipal` |

`treasuryBalance` is the ground truth for what the bridge owes users. AAVE accounting is bolted on top:

- `address(this).balance` + `aWETH.balanceOf(this)` = total assets (ETH + AAVE position with interest).
- `suppliedPrincipal` is updated only by `supplyToAave` / `withdrawFromAave` / `_pullFromAave` / `emergencyWithdrawAll`. It never moves due to interest accrual.

---

## 4. Lifecycle Flows

### 4.1 Deposit

```solidity
function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) external nonReentrant {
    if (amount == 0) revert InvalidAmount();
    if (amount > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();  // 100 USDC
    if (anAccount == bytes32(0)) revert InvalidAnAccount();
    if (!usdc.transferFrom(msg.sender, address(this), amount)) revert TransferFromFailed();

    uint256 depositId = depositCounter++;
    treasuryBalance += amount;
    emit Deposit(depositId, msg.sender, amount, anWorkchain, anAccount, block.timestamp);
}
```

No AAVE interaction. Funds stay as USDC in the contract until owner routes via `supplyToAave()`.

### 4.2 Owner: route idle ETH into AAVE

```solidity
bridge.supplyToAave(type(uint256).max);  // supply everything above the liquid reserve
bridge.supplyToAave(5 ether);            // supply an exact amount
```

Logic (`_amountSupplyable`):

```
reserve     = treasuryBalance * liquidReserveBps / 10_000
available   = max(0, address(this).balance - reserve)
toSupply    = (amount == max) ? available : amount
require(toSupply > 0 && toSupply <= available)
```

Then:
```
suppliedPrincipal += toSupply
wethGateway.depositETH{value: toSupply}(pool, address(this), 0)
```

The gateway wraps to WETH internally and `pool.supply` mints aWETH **to the bridge**.

### 4.3 User: withdraw

```solidity
function withdraw(...) external nonReentrant {
    // ZK proof verification, double-spend check, ...

    // Effects (CEI)
    processedDeposits[depositId] = true;
    treasuryBalance -= amount;

    // Interaction
    if (address(this).balance < amount) {
        _pullFromAave(amount - address(this).balance);
    }
    recipient.transfer(amount);
}
```

`_pullFromAave(shortfall)`:

```
toPull = min(shortfall, suppliedPrincipal)
wethGateway.withdrawETH(pool, toPull, address(this))
require(received >= shortfall)
suppliedPrincipal -= toPull
```

Note: the gateway pulls aWETH from the bridge using the `type(uint256).max` allowance set in the constructor and unwraps to ETH that lands in the bridge via `receive()`.

Property: `recipient.transfer(amount)` only happens after either the buffer was sufficient or `_pullFromAave` succeeded. State is already updated (`processedDeposits`, `treasuryBalance`), so a revert in the AAVE call rolls everything back atomically.

### 4.4 Yield

```
accruedYield() = max(0, aWETH.balanceOf(this) - suppliedPrincipal)
```

`harvestYield(amount)`:

```
require(0 < amount <= accruedYield())
wethGateway.withdrawETH(pool, amount, address(this))
require(received >= amount)
yieldRecipient.call{value: received}("")
```

Crucially, this path does **not** decrement `suppliedPrincipal`. It only consumes interest. If `accruedYield()` is zero, the call reverts — there is no path that drains principal under the guise of harvesting.

### 4.5 Emergency drain

`emergencyWithdrawAll()`:

```
aaveEnabled = false
wethGateway.withdrawETH(pool, type(uint256).max, address(this))   // full balance
suppliedPrincipal = 0
```

After this:
- All aWETH is converted back to ETH and held by the bridge.
- New supplies are blocked.
- User withdrawals continue to work because they only need ETH balance ≥ `amount`.

---

## 5. Threat Model and Invariants

### 5.1 Threats considered

| Threat | Mitigation |
|---|---|
| Owner steals user principal | `harvestYield` capped by `aWETH.balanceOf(this) - suppliedPrincipal`; no admin path bypasses ZK-proven withdraw. |
| Owner griefs withdrawals (e.g. by leaving 0 % reserve) | `withdraw()` auto-pulls from AAVE; the reserve only affects gas, not solvency. Even reserve = 0 is safe. |
| Reentrancy via aWETH or gateway | `nonReentrant` on `deposit`, `withdraw`, `supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`. CEI strictly preserved in `withdraw()`. |
| AAVE pause / depeg | `emergencyWithdrawAll` returns all funds to ETH; user withdrawals continue. |
| AAVE returns less ETH than requested (gateway / pool slippage) | `_pullFromAave` and `harvestYield` revert with `AaveWithdrawFailed(requested, received)`. The `withdraw()` then reverts as a whole, preserving solvency. |
| Inflation of `liquidReserveBps` to lock funds | Capped at `MAX_LIQUID_RESERVE_BPS = 5000` (50 %). Reserve is opinion, not custody — funds are spendable regardless. |
| Front-running between supply and emergency drain | Both are owner-only and `nonReentrant`. The owner has no ability to grief themselves. |
| Constructor misconfiguration (only some AAVE addresses set) | All-or-none check: passing some but not all reverts with `InvalidAaveAddress`. |

### 5.2 Solvency invariant

At all times, after any sequence of operations:

```
address(this).balance + aWETH.balanceOf(this)  ≥  treasuryBalance
```

That is, the bridge holds at least enough assets (ETH plus the AAVE position, including unrealised interest) to cover all outstanding user claims. This invariant is fuzz-tested (`testFuzz_totalAssetsCoversTreasury`).

It can only be violated by:
1. AAVE losing money (insolvency / depeg) — outside our control.
2. The owner harvesting more than `accruedYield()` — blocked by the `NoYield` check.
3. Direct ETH transfer out of the bridge to a non-user — there is no such function.

### 5.3 Conservation invariant for principal

```
suppliedPrincipal' = suppliedPrincipal
                   + Σ supplyToAave   (additions)
                   − Σ withdrawFromAave − Σ _pullFromAave − Σ emergencyDrain
```

`harvestYield` does not appear in this equation — it is the dedicated invariant making yield strictly separate from principal.

---

## 6. Owner Powers — Explicit Catalogue

| Function | Effect | Bounded by |
|---|---|---|
| `supplyToAave(amount)` | Routes ETH → AAVE | `available = balance − reserve` |
| `withdrawFromAave(amount)` | Pulls ETH back from AAVE preemptively | `suppliedPrincipal` |
| `emergencyWithdrawAll()` | Pulls everything from AAVE; disables supplies | — |
| `harvestYield(amount)` | Sends yield ETH to `yieldRecipient` | `accruedYield()` |
| `setAaveEnabled(bool)` | Toggles new-supply gate | — |
| `setLiquidReserveBps(bps)` | Changes liquid-reserve fraction | `bps ≤ MAX_LIQUID_RESERVE_BPS` |
| `setYieldRecipient(addr)` | Redirects future harvested yield | non-zero address |
| `transferOwnership(addr)` | Rotates owner | non-zero address |

The owner **cannot**:
- Call any function that decreases `treasuryBalance`.
- Transfer ETH out of the bridge to themselves except via `harvestYield`, which is bounded by accrued yield.
- Pause `withdraw()`.
- Modify `processedDeposits` to enable double-spends.
- Replace `verifier` or `blockHeaderOracle` (intentional — these have no admin setters in this contract).

---

## 7. How to Verify Correctness

This section is the **specific protocol for verifying that the AAVE integration is correct**. Each step lists what to check, the file or command to use, and the expected result.

### Step 1 — Unit and fuzz tests

```bash
cd contracts/ethereum
forge test --match-contract AckiNackiBridgeAaveTest -vv
```

Expected: 23 tests pass, including a 256-run fuzz invariant `testFuzz_totalAssetsCoversTreasury`.

The suite covers:

| Property | Test |
|---|---|
| Constructor fully wires AAVE and pre-approves the gateway | `test_constructor_wiresAaveAndApprovesGateway` |
| Partial AAVE wiring is rejected | `test_constructor_partialAaveWiringReverts` |
| Bridge with AAVE disabled still works | `test_constructor_noAaveIsLegal` |
| `supplyToAave` respects the liquid reserve | `test_supplyToAave_respectsLiquidReserve` |
| `supplyToAave` honours explicit amounts | `test_supplyToAave_explicitAmount` |
| Only owner can supply | `test_supplyToAave_onlyOwner` |
| Disabled AAVE blocks new supplies | `test_supplyToAave_whenDisabledReverts` |
| Withdraw uses liquid buffer first | `test_withdraw_usesLiquidBufferFirst` |
| Withdraw auto-pulls shortfall from AAVE | `test_withdraw_pullsShortfallFromAave` |
| `withdrawFromAave` works and is owner-only | `test_withdrawFromAave_*` |
| Yield is visible after AAVE growth | `test_accruedYield_reflectsAaveGrowth` |
| Yield can be harvested in full or partially | `test_harvestYield_sendsToRecipient`, `test_harvestYield_partialHarvest` |
| Over-harvesting reverts | `test_harvestYield_amountExceedsYieldReverts` |
| `emergencyWithdrawAll` drains AAVE | `test_emergencyWithdrawAll_pullsEverything` |
| Withdrawals still work after emergency | `test_emergencyWithdraw_allowsSubsequentUserWithdrawals` |
| Liquid reserve bps is capped | `test_setLiquidReserveBps_capped` |
| Ownership transfer revokes old owner | `test_transferOwnership_flowsAllAuthorities` |
| **Solvency invariant (fuzz)** | `testFuzz_totalAssetsCoversTreasury` |

### Step 2 — Full Foundry suite

```bash
forge test
```

Expected: 174 tests pass across 21 suites.

### Step 3 — Static review of attack surfaces

For each function in `contracts/ethereum/src/AckiNackiBridge.sol`, verify:

| Check | Where to look |
|---|---|
| External calls preceded by state updates (CEI) | `withdraw()` lines 217-228 |
| `nonReentrant` on every mutating function | `deposit`, `withdraw`, `supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield` |
| `onlyOwner` on every administrative function | All AAVE-management functions |
| `harvestYield` cannot decrement `suppliedPrincipal` | The function body never references `suppliedPrincipal` |
| `_pullFromAave` is only callable from `withdraw` (no external entry) | Search for `_pullFromAave(` — should appear exactly twice: in `withdraw` and in its own definition |
| `treasuryBalance` is decremented in withdraw before any external call | `treasuryBalance -= amount;` precedes both `_pullFromAave` and `recipient.transfer` |

A grep that should return only the expected hits:

```bash
grep -n "_pullFromAave\|suppliedPrincipal\|treasuryBalance" contracts/ethereum/src/AckiNackiBridge.sol
```

### Step 4 — Solvency invariant by hand

Deploy with mocks and run the following sequence in a Forge test or REPL:

```
deposit(10 ETH)            ->  treasury=10,  ETH=10,  aWETH=0,   principal=0
supplyToAave(MAX)          ->  treasury=10,  ETH=1,   aWETH=9,   principal=9   (10% reserve)
accrueYield(0.5)           ->  treasury=10,  ETH=1,   aWETH=9.5, principal=9
harvestYield(0.5)          ->  treasury=10,  ETH=1,   aWETH=9,   principal=9    (yield gone, principal kept)
withdraw(5)                ->  treasury=5,   ETH=0,   aWETH=5,   principal=5    (1 from buffer + 4 from AAVE)
emergencyWithdrawAll()     ->  treasury=5,   ETH=5,   aWETH=0,   principal=0
withdraw(5)                ->  treasury=0,   ETH=0,   aWETH=0,   principal=0
```

After every step: `ETH + aWETH ≥ treasury`. This is exactly what `testFuzz_totalAssetsCoversTreasury` enforces over random inputs.

### Step 5 — Attempt to break it (fuzzing-style negative checks)

Try the following from a non-owner (`vm.prank(user1)`):

| Attempt | Expected revert |
|---|---|
| `supplyToAave(...)` | `NotOwner` |
| `withdrawFromAave(...)` | `NotOwner` |
| `emergencyWithdrawAll()` | `NotOwner` |
| `harvestYield(...)` | `NotOwner` |
| `setLiquidReserveBps(...)` | `NotOwner` |
| `setAaveEnabled(...)` | `NotOwner` |
| `setYieldRecipient(...)` | `NotOwner` |
| `transferOwnership(...)` | `NotOwner` |

Try the following as the owner:

| Attempt | Expected revert |
|---|---|
| `harvestYield(accruedYield + 1)` | `NoYield` |
| `setLiquidReserveBps(5001)` | `ReserveBpsTooHigh` |
| `withdrawFromAave(suppliedPrincipal + 1)` | `InvalidAmount` |
| `supplyToAave(available + 1)` | `InvalidAmount` |
| `transferOwnership(0)` | `InvalidRecipient` |

All of these are exercised by `AckiNackiBridgeAaveTest`.

### Step 6 — Mainnet fork sanity check (recommended before deployment)

The unit tests use mocks. Before mainnet, add a fork test using the real AAVE V3 deployment:

```bash
forge test --fork-url $ETH_RPC_URL --match-test test_mainnetFork_supplyAndWithdraw -vvv
```

A representative outline (not yet committed; tracked in `integration_plan.md`):

```solidity
function test_mainnetFork_supplyAndWithdraw() public {
    AckiNackiBridge b = new AckiNackiBridge(
        address(verifier), address(oracle),
        0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2,  // Pool
        0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C,  // WrappedTokenGatewayV3
        0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8   // aWETH
    );
    vm.deal(user1, 10 ether);
    vm.prank(user1); b.deposit{value: 10 ether}();
    b.supplyToAave(type(uint256).max);
    vm.warp(block.timestamp + 30 days);
    assertGt(b.accruedYield(), 0);
    b.emergencyWithdrawAll();
    assertGe(address(b).balance, b.treasuryBalance());
}
```

Verify on a recent block: deposit a known amount, supply to AAVE, advance time, observe non-zero yield, drain everything, confirm solvency.

### Step 7 — Deployment checklist

When deploying with AAVE enabled:

```bash
USE_AAVE=true USE_AXIOM_ORACLE=true \
  forge script script/DeployRealBridge.s.sol \
  --rpc-url $RPC_URL --broadcast
```

Post-deployment verification:

| Check | How |
|---|---|
| `aavePool` matches expected mainnet address | `cast call $BRIDGE "aavePool()(address)"` |
| `wethGateway` matches expected | `cast call $BRIDGE "wethGateway()(address)"` |
| `aWETH` matches expected | `cast call $BRIDGE "aWETH()(address)"` |
| `aaveEnabled == true` | `cast call $BRIDGE "aaveEnabled()(bool)"` |
| Gateway has `type(uint256).max` aWETH allowance | `cast call $aWETH "allowance(address,address)(uint256)" $BRIDGE $GATEWAY` |
| `owner` is the multisig (not the deployer EOA) | `cast call $BRIDGE "owner()(address)"` then `transferOwnership` if needed |
| `yieldRecipient` is the multisig / treasury | `cast call $BRIDGE "yieldRecipient()(address)"` |
| `liquidReserveBps` reflects policy (default 1000 = 10 %) | `cast call $BRIDGE "liquidReserveBps()(uint256)"` |

### Step 8 — Operational invariants to monitor

A keeper / monitoring service should alert if:

```
address(this).balance + aWETH.balanceOf(this) < treasuryBalance      (solvency breach)
suppliedPrincipal > aWETH.balanceOf(this)                            (book ahead of AAVE; should never happen)
aaveEnabled == false  AND  suppliedPrincipal > 0                     (stuck position; manual intervention)
```

These are easy off-chain checks via `eth_call` to the public state getters and `aWETH.balanceOf(bridge)`.

---

## 8. Known Gaps and Future Work

| Gap | Severity | Plan |
|---|---|---|
| Mocks instead of forked AAVE in tests | Medium | Add a `--fork-url` fork test ahead of mainnet deployment (Step 6 above). |
| No on-chain enforcement of "owner is a multisig" | Low | Operational; deploy with multisig as deployer or `transferOwnership` immediately after. |
| Aggressive deposit → instant withdraw could thrash AAVE if buffer is small | Low | Owner can raise `liquidReserveBps`; no protocol violation, only gas waste. |
| AAVE V3 governance can pause WETH market | Low | `emergencyWithdrawAll` already covers this. Monitor AAVE governance proposals. |
| aWETH could in principle become non-rebasing or change semantics in a future AAVE version | Low | All interactions go through `IWrappedTokenGatewayV3`, which abstracts the details. Re-audit on AAVE V4. |

---

## 9. Quick Reference

### Files added by the integration

```
contracts/ethereum/src/IAavePool.sol
contracts/ethereum/src/IWrappedTokenGatewayV3.sol
contracts/ethereum/src/IERC20.sol
contracts/ethereum/test/mocks/MockAave.sol
contracts/ethereum/test/AckiNackiBridgeAave.t.sol
docs/operations/aave/aave_integration.md   (this file)
```

### Files modified

```
contracts/ethereum/src/AckiNackiBridge.sol      (+~250 lines: AAVE storage, owner, AAVE functions)
contracts/ethereum/script/DeployRealBridge.s.sol (USE_AAVE flag, mainnet addresses)
contracts/ethereum/script/DeployTestBridge.s.sol (5-arg constructor)
contracts/ethereum/test/AckiNackiBridgeV2.t.sol  (5-arg constructor)
contracts/ethereum/test/FuzzVerifiers.t.sol      (5-arg constructor)
docs/README.md, docs/architecture/integration_analysis.md, _archive/docs/legacy/integration_plan.md
```

### Public API surface (AAVE-related)

```solidity
// State
IAavePool             public immutable aavePool;
IWrappedTokenGatewayV3 public immutable wethGateway;
IERC20                public immutable aWETH;
bool                  public           aaveEnabled;
uint256               public           suppliedPrincipal;
uint256               public           liquidReserveBps;
address               public           owner;
address               public           yieldRecipient;
uint256               public constant  MAX_LIQUID_RESERVE_BPS = 5000;
uint256               public constant  BPS_DENOMINATOR        = 10_000;

// Owner-only
function supplyToAave(uint256 amount)        external;
function withdrawFromAave(uint256 amount)    external;
function emergencyWithdrawAll()              external;
function harvestYield(uint256 amount)        external;
function setAaveEnabled(bool enabled)        external;
function setLiquidReserveBps(uint256 bps)    external;
function setYieldRecipient(address recipient) external;
function transferOwnership(address newOwner) external;

// Views (anyone)
function aWethBalance()  external view returns (uint256);
function accruedYield()  external view returns (uint256);
function totalAssets()   external view returns (uint256);
```

### Errors

`InvalidAaveAddress`, `NotOwner`, `Reentrancy`, `AaveDisabled`, `ReserveBpsTooHigh`, `NothingToSupply`, `AaveWithdrawFailed(requested, received)`, `NoYield` (in addition to the pre-existing `InvalidAmount`, `DepositTooLarge`, `DepositAlreadyProcessed`, `InvalidProof`, `InsufficientTreasury`, `InvalidVerifier`, `InvalidRecipient`, `InvalidBlockHash`, `InvalidOracle`).
