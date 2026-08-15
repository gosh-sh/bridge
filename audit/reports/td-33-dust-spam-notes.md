# TD-33 — Dust / proof-spam DoS economics

PoC: `DepositDustSpam.t.sol`, `td_33_dust_spam_hol.rs`.

Cross-ref: **QC-A1-1** / **A1-F2** (per-tx cap only), **TD-26** (HOL skip), **TD-36** (cross cap policy).

## L1 (Foundry)

| N dust (`amount=1`) | `depositCounter` | `treasuryBalance` | Aggregate cap |
|---------------------|------------------|-------------------|---------------|
| 40 (single user) | 40 | 40 | none |
| 50 (multi user) | 50 | 50 | none (QC) |

`MAX_DEPOSIT_AMOUNT` limits per tx only; min deposit = 1 base unit (1e-6 USDC).

## Relayer (K=5 dust ids 0..4 + honest id 5)

| Scenario | Ticks to honest finalize | Notes |
|----------|------------------------|-------|
| Poison id 0, no skip | — (blocked) | 8× `ProofFailed` on 0; honest never mints |
| Poison id 0, `skip_after=3` | **8** | skip on 3rd attempt → finalize 1..5 |
| All dust OK, no poison | **6** | 1 prove tick per id (O(N) cost) |

Prove calls = deposits finalized (one `generate()` per tick when healthy).

## Verdict: **QC** (not BC)

1. Dust does not bypass DEP-N5 nullifier — replay → `AlreadyFinalized`.
2. Economic grief: spam dust fills treasury + forces O(N) prove work; poisoned dust at low id blocks honest users until `--skip-after-attempts`.
3. **Not BC** — no safety/nullifier bypass via dust amounts.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*Dust*' -q
    cd crates/deposit-relayer-daemon && cargo test td_33 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test
