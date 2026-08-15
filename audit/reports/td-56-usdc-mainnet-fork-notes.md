# TD-56 — Fork real USDC proxy pause/blacklist

PoC: `audit/spec/ethereum/DepositUsdcMainnetFork.t.sol`.

USDC mainnet proxy: `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` (`DeployRealBridge.s.sol`).

Opt-in fork: `FORK_URL` unset → `vm.skip` (CI-safe). `require(chainid == 1)` — token realism, not deposit-chain allowlist (TD-41).

## Scenario matrix (mirrors TD-24)

| Scenario | `deposit()` | treasury / custody |
|----------|-------------|-------------------|
| User blacklisted before deposit | revert (token hook) | 0 |
| Token paused | revert | 0 |
| Deposit ok, then blacklist sender | 2nd deposit revert | unchanged |
| Control deposit | ok | credited |
| Bridge address blacklisted | revert | 0 |

Roles: `blacklister()` / `pauser()` read from live proxy; `vm.prank` for admin ops; restored after each test.

## Cross-refs

| ID | Link |
|----|------|
| TD-24 | `DepositUsdcBlacklistPause.t.sol` (mock) |
| TD-58 | No bridge `pause()` (#20) |
| TD-41 | Deposit chains ≠ L1 mainnet; fork is token realism only |

## Verdict: **META/QC**

Production FiatToken hooks match mock fail-closed behaviour; external ops risk, not BC.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*Blacklist*' -q
    FOUNDRY_PROFILE=fork FORK_URL=https://eth.llamarpc.com \
      forge test --match-contract DepositUsdcMainnetForkTest -vv
