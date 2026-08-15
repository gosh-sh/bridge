# TD-31 — `anWorkchain` not in PI; AN credits workchain 0

PoC: `DepositAnWorkchainUnbound.t.sol`, `td_31_an_workchain_unbound.rs`, `td_31_workchain_ignored.rs`.

Cross-ref: `questions-cross-chain.md` **QC-AN-J4** (closed ack — workchain in event ignored on AN).

## Binding matrix

| Layer | `anWorkchain` | Bound? | Notes |
|-------|---------------|--------|-------|
| L1 `Deposit` event | `int8` data word 1 (byte 63 of 128B log data) | Emitted only | Any `int8` accepted; treasury += amount |
| ZK PI slot 5 | `dappIdHigh` | Config tag | Replaced `anWorkchain` slot 2026-06-02 |
| ZK PI slot 6 | `dappIdLow` | Config tag | Not from event |
| ZK PI slots 7–8 | `anAccountHigh/Low` | Event word 2 | Word 1 (WC) not parsed in circuit |
| Relayer `check_binds_to` | `event.an_workchain` | **No** | Same as `promiseCommit` (TD-21) |
| AN mint target | `makeAddrStd(0, account)` | WC **0** | Event WC ignored (QC-AN-J4) |

## Verdict: **QC** (not BC)

1. Proof + MockProver accept witnesses with `anWorkchain` ∈ {127, −1, 42} on Sepolia-chain synthetic witnesses while PI slots 5–8 stay identical to WC=0 baseline. Real `proof_00` Sepolia fixture pins baseline dappId slots (inline receipt patch breaks MPT/header bind).
2. Relayer `check_binds_to` passes when event `an_workchain` ≠ witness WC; mock AN submit finalizes and records `last_mint_workchain() == 0`.
3. **Not BC** — no path to credit a non-zero workchain via proof public inputs; mismatch is policy/UX (user asked WC≠0, AN pays WC0).

Residual QC: user-selected workchain on L1 is not enforced cross-layer; operators must document destination is always base workchain 0 on AN.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*Workchain*' -q
    cd deposit-prover && cargo test td_31 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_31 -- --nocapture
