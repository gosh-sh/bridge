# TD-24 — USDC blacklist / pause mid-campaign

PoC: `BlacklistableERC20.sol`, `DepositUsdcBlacklistPause.t.sol`.  
Cross-ref: TD-34 (no L1 refund), TD-56 (mainnet fork gap), #20 (no bridge pause on deposit).

Mainnet USDC proxy fork — **TD-56** (`DepositUsdcMainnetFork.t.sol`, opt-in `FORK_URL`).

## Scenario → revert?

| Scenario | `deposit()` | treasury / custody |
|----------|-------------|-------------------|
| User blacklisted before deposit | `Blacklisted()` (token) | unchanged (0) |
| Token paused | `Paused()` (token) | unchanged (0) |
| Deposit ok, then blacklist sender | n/a (no retro pull) | unchanged |
| Unblacklisted control | success | credited |
| Bridge address blacklisted | `Blacklisted()` | unchanged (0) |

Bridge checks `transferFrom` bool; USDC-style hooks **revert** → whole `deposit` reverts (fail-closed). No `treasuryBalance` credit without transfer.

## Verdict: **QC** (not BC)

1. Blacklist/pause blocks new deposits at token layer — no phantom ledger credit.
2. Post-deposit blacklist does not claw back custody or `treasuryBalance`.
3. **Ops QC:** bridge has no pause on `deposit()`; USDC proxy pause/blacklist is external liveness risk — monitor Circle pause events.
4. Rebase not modeled (USDC no rebase); FoT covered in TD-23.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*Blacklist*' -q
    cd audit/spec/ethereum && forge test --match-path '*Pause*' -q
    cd audit/spec/ethereum && forge test --match-contract 'Deposit' -q
