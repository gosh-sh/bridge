> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# USDT Deposits Migration — Summary

Branch: `feat/usdt-deposits` (local only; not pushed)

## What changed

### Contract (`AckiNackiBridge.sol`)
- **Deposits**: `deposit()` → `deposit(uint256 amount)` — pulls USDT via `transferFrom` (no `payable`).
- **Token wiring**: new immutable `usdt`; constructor is now  
  `(oracle, usdt, aavePool, aUSDT, verifyBlockConfig, bridgeWithdrawConfig)`.
- **Decimals**: `USDT_UNIT = 1e6`, `MAX_DEPOSIT_AMOUNT = 100 USDT` (was `100 ether`).
- **AAVE**: removed WETH gateway path; `supplyToAave` / `withdrawFromAave` / `emergencyWithdrawAll` / `harvestYield` use direct ERC-20 `aavePool.supply` / `withdraw` on the USDT market.
- **Renames**: `aWETH` → `aUSDT`, `aWethBalance()` → `aUsdtBalance()`.
- **Withdrawals**: `withdrawByProof` pays out **USDT** via `usdt.transfer` (was native ETH `call{value}`).
- **Removed**: `receive()` fallback and `IWrappedTokenGatewayV3` dependency.

### Deploy scripts
- **`DeployRealBridge.s.sol`**: resolves USDT + AAVE USDT market per chain (mainnet + Sepolia from [aave-address-book](https://github.com/bgd-labs/aave-address-book)).
- **`DeployTestBridge.s.sol`**: requires `USDT_ADDRESS` env (defaults to Sepolia Aave-faucet USDT); documents faucet → approve → deposit flow.

### Sepolia addresses (Aave address book)
| Role | Address |
|------|---------|
| USDT (underlying) | `0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0` |
| aUSDT | `0xAF0F6e8b0Dc5c913bbF4d14c22B4E78Dd14310B6` |
| AAVE V3 Pool | `0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951` |
| Faucet | `0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D` |

> Note: the task file listed `0xAF0F…` as “USDT”; that address is the **aUSDT** aToken. Deposits use the **underlying** `0xaA8E…`.

### Tests
- New `MockERC20` (6 decimals), updated `MockAave` (ERC-20 pool), `UsdtTestLib` helpers.
- All deposit / AAVE / pause / withdraw / fuzz tests updated for USDT units.
- **`AckiNackiBridgeAaveFork.t.sol`**: now forks **Sepolia** USDT market (opt-in via `FORK_URL`).

### Rust (`crates/eth-frontend`)
- `deposit(uint256)` ABI + `approve` before deposit.
- `fund_usdt_from_aave_faucet`, `ensure_usdt_approval`, `fund_faucet_and_deposit` for E2E.
- User instruction constant: *“Swap your ETH to USDT on Uniswap before depositing…”*

## Verification

```bash
cd contracts/ethereum && forge build && forge test   # 153 passed, 1 skipped (fork)
cargo build -p eth-frontend && cargo test -p eth-frontend   # 3 passed
```

Fork tests (optional):

```bash
FORK_URL=https://ethereum-sepolia-rpc.publicnode.com \
  forge test --match-contract AckiNackiBridgeAaveForkTest -vv
```

## Open questions / follow-ups

1. **Deposit prover / AN side**: deposit event `amount` is now 6-decimal USDT units — confirm the Halo2 circuit and AN `finalizeDeposit` public inputs match (no implicit `1e18` scaling).
2. **Circuit 4 `tokenId`**: bridge still requires `tokenId == 0` for USDT payouts; partner may need a dedicated USDT token id if the AN event schema distinguishes assets.
3. **Mainnet USDT `approve`**: real Tether may need a zero-then-set approve pattern; Sepolia Aave test-USDT is standard ERC-20.
4. **WASM frontend** (`frontend/`): still uses legacy `deposit()` + ETH; not updated in this pass (task scoped `crates/eth-frontend`).
5. **USER_GUIDE.md**: still describes ETH deposits; should be updated in a docs pass.
