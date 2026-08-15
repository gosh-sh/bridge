# TD-36 — Cross-layer cap policy (QC-AN-J1)

PoC: `DepositCrossCapPolicy.t.sol`, `td_36_mint_cap_exceeded.rs`.  
Cross-ref: TD-33, `DepositWhaleCap.t.sol`, TD-34 (stuck L1), TD-65 (HOL), `questions-cross-chain.md` QC-AN-J1.

## L1 cap vs AN cap vs relayer

| Layer | Policy | On exceed |
|-------|--------|-----------|
| L1 per-tx | `MAX_DEPOSIT_AMOUNT = uint64.max` | `DepositTooLarge` |
| L1 aggregate | uncapped (QC-A1-1) | treasury grows |
| AN cumulative | `setMintCap(chainId)` | exit **229** `ERR_MINT_CAP_EXCEEDED` |
| Relayer | maps 229 → `Rejected` + `setMintCap` hint | `AnRejected` HOL (same deposit id) |

L1 accepts deposit → USDC stuck on bridge if AN cap blocks mint (TD-34 ops path). **Not BC** — joint policy mismatch is documented QC; no unbounded AN mint without L1 deposit.

## Verdict: **QC** (QC-AN-J1)

Ops: align L1 marketing cap with AN `setMintCap`; monitor cumulative inflow vs mint cap; raise cap or pause deposits before AN rejects.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*CrossCap*' -q
    cd crates/deposit-relayer-daemon && cargo test td_36 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
