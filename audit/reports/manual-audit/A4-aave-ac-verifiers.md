# Manual audit — A4: AAVE, owner, pause, verifiers, oracle

**Workstream:** A4
**Subagent model:** Fable 5
**Date:** 2026-07-16
**Files:** AAVE paths, pause, `*Verifier*.sol` / `*Aggregator*.sol`, `AxiomBlockHeaderOracle.sol`
**Invariants:** AC-2,4,5,6, ZK-1..5, OR-1..4
**Verdict:** No fund-loss bug (BC) found. Owner **cannot** steal user principal. 4 hardening / centralization / docs notes (Low/Info), 1 verifier-layer deployment-risk item worth an on-chain guard.

## Checklist

- [x] Owner cannot touch user principal — **holds** (no owner path transfers principal out)
- [x] Yield vs principal (`accruedYield`, `harvestYield`) — **holds** (harvest bounded by yield; principal never decremented)
- [x] `liquidReserveBps` bounds — capped at 50% (`MAX_LIQUID_RESERVE_BPS`), owner-only, affects only supply sizing
- [x] Pause scope vs owner ops — user entrypoints gated; owner AAVE-evacuation ungated by design
- [x] Verifier adapter: proof length, try/catch, address(0) — all correct; **one missing `extcodesize` guard** (A4-01)
- [x] Production SHPLONK vs mock in tests/deploy — prod wires SHPLONK aggregators (real Yul crypto); Groth16 stub adapters are test-only
- [x] Oracle fail-closed (future path) — **holds** (OR-1..OR-4); unused by public surface today

## Scope walkthrough

### 1. Owner ⇏ principal theft (primary property) — HOLDS

`AckiNackiBridge` custodies user principal as `treasuryBalance` (plain USDC + aUSDC book value). The
only outbound USDC transfers to a non-AAVE address are:

- `harvestYield(amount)` → `yieldRecipient`, bounded by `accruedYield() = aUsdcBalance − suppliedPrincipal`.
- `withdrawByProof(...)` → proof-gated payout to the reconstructed `recipient`.

Owner-reachable AAVE functions (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`) move funds
**between AAVE and the bridge contract** — they never send USDC to an EOA. There is **no** owner
function that decrements `treasuryBalance` or sweeps principal to an arbitrary address. Setting
`yieldRecipient = owner` still only lets the owner pull *yield*. Conclusion: the "owner cannot steal
principal" invariant is enforced by construction (AC-2/AC-4).

### 2. `harvestYield` vs principal — HOLDS

`harvestYield` withdraws `amount ≤ accruedYield()` USDC from AAVE and forwards `received` to
`yieldRecipient`, leaving `suppliedPrincipal` untouched. Because `amount ≤ aUsdcBalance − suppliedPrincipal`,
the post-harvest aUSDC balance stays `≥ suppliedPrincipal`, so principal remains fully backed
(cross-checked by fork test `test_fork_yieldAccruesAfterTimeWarp`, which asserts principal is unchanged
after harvest).

### 3. `liquidReserveBps` — bounded

`setLiquidReserveBps` reverts above `MAX_LIQUID_RESERVE_BPS = 5_000` (50%) and is owner-only. The value
only limits `_amountSupplyable()` (how much idle USDC may be pushed into AAVE). Withdrawals
(`withdrawByProof`) pull from AAVE on demand regardless of the reserve, so a manipulated reserve cannot
block or divert user payouts. No security impact.

### 4. Pause scope — correct

`deposit`, `verifyBlock`, `applyBkSetUpdate`, and `withdrawByProof` carry `whenNotPaused`. Owner AAVE
management (`supplyToAave`, `withdrawFromAave`, `emergencyWithdrawAll`, `harvestYield`) is deliberately
**not** pause-gated so funds can be evacuated during an incident. Verified by
`AckiNackiBridgePauseTest` (`test_*_blockedWhilePaused`, `test_ownerControls_workWhilePaused`).
Centralization caveat: see A4-02.

### 5. Verifier adapters — no bypass, one deployment-risk item

- **Groth16 adapters** (`PrimaryVerifier`, `LayerHashesMovementVerifier`, `BridgeWithdrawalVerifier`,
  test/retained): reject `proof.length != 256`, reject `address(0)` in constructor, and wrap
  `verifyProof` in `try/catch`. `verifyProof` returns **nothing** (`external view` with no return) and
  reverts on an invalid proof, so `try { return true } catch { return false }` is semantically correct —
  a `false`-return-without-revert bypass is impossible for this interface (ZK-2/ZK-4/ZK-5).
- **SHPLONK aggregator adapters** (`PrimaryAggregatorVerifier`, `FallbackAggregatorVerifier`,
  `LayerHashesAggregatorVerifier`, `BridgeWithdrawalAggregatorVerifier`, **production**): reject short
  calldata (`proof.length < (12 + NUM_INNER) * 32`) before any `_readInstance`, re-expose inner instances
  at fixed offsets `[12..]` and compare them to the bridge-supplied public inputs, then forward the whole
  `instances‖proof` blob to the Yul verifier via `ShplonkAggregatorVerifierBase._verifyShplonk`. The 12
  KZG accumulator limbs are validated by the Yul pairing, not by the adapter (correct). `_readInstance`
  offset arithmetic exactly matches the min-length gate for each adapter (Primary/Fallback max idx 15 →
  ≥512 B; LayerHashes max idx 25 → ≥832 B; Withdrawal max idx 21 → ≥704 B). No forgery at the adapter
  layer given a correctly deployed Yul verifier (negatives: `ShplonkAggregatorForgeryTest`,
  `FuzzVerifiers`).
- **Deployment-risk (A4-01):** `ShplonkHalo2Verifier.verify` does `(ok,) = yulVerifier.staticcall(...)`
  and treats `ok == true` as "proof valid". A `staticcall` to an address with **no code succeeds**
  (`ok == true`, empty return). The constructor only checks `_yulVerifier != address(0)`; there is **no
  `extcodesize` guard**. If the Yul verifier were ever wired to an empty/EOA address, every proof would
  pass → total verifier bypass on `verifyBlock` / `applyBkSetUpdate` / `withdrawByProof`. Mitigated in
  practice by `ShplonkDeployLib.deployYulFromBin` (rejects empty `.bin` and `create` returning `0`), so
  the standard deploy path always installs code — but nothing enforces this at runtime.

### 6. Production SHPLONK vs mock

`DeployRealBridge` + `ShplonkDeployLib` wire the **SHPLONK aggregator** adapters for all of 1A/1B/2 and
Circuit 4 in production. The gnark-wrapped Groth16 adapters (`PrimaryVerifier`,
`LayerHashesMovementVerifier`, `BridgeWithdrawalVerifier`) and all `Mock*Verifier` contracts are
test-only. The known R15 caveat — "the gnark wrapper's `Define` is an identity stub that does not verify
the inner Halo2 SHPLONK proof" (see `BridgeWithdrawalVerifier` NatSpec, Phase 8) — applies to the
gnark path, which is not the production path. The SHPLONK Yul verifier does perform real verification
(`ShplonkSpikeOnChainTest` / `ShplonkDeployLibTest` confirm tampered calldata reverts). `DeployRealBridge`
starts the bridge **paused** whenever any verifier is wired, requiring an explicit post-sign-off unpause.

### 7. Oracle — fail-closed, unused today

`AxiomBlockHeaderOracle` fails closed: `getBlockHash` reverts for future blocks (`BlockNotYetMined`),
reverts for historical blocks without a witness, and reverts if `blockhash()` returns zero
(OR-1/OR-2/OR-3). `axiomV2Core` is `immutable` (OR-4). The oracle is **not read** by any public
`AckiNackiBridge` entrypoint (`blockHeaderOracle` is set in the constructor and never consulted) — it is
reserved for the future burn-proof flow. Minor notes: `isBlockHashAvailable` optimistically returns
`true` for historical blocks (view-only helper, harmless), and the `uint32(blockNumber)` cast in the
Axiom paths truncates for `blockNumber ≥ 2³²` (not reachable for centuries). Info only (A4-05).

## Findings

| ID | Sev | BC/QC/OK | Summary | PoC / Evidence |
|----|-----|----------|---------|-----|
| A4-01 | Low | QC | `ShplonkHalo2Verifier.verify` trusts `staticcall` success with no `extcodesize` guard; an empty-code/EOA Yul-verifier address makes every proof pass (verifier bypass on all ZK entrypoints). Mitigated by deploy lib, not by runtime. | `ShplonkHalo2Verifier.sol:22`; no code-size check in ctor `:15-18`; deploy guard `ShplonkDeployLib.sol:56-63` |
| A4-02 | Info | OK | Centralization: owner can `pause()` indefinitely, freezing `withdrawByProof` (cross-chain payouts) and `deposit`. No timelock/guardian. Funds not stealable; withdrawals censorable while paused. | `AckiNackiBridge.pause():1179`, `whenNotPaused` on `withdrawByProof:1008` |
| A4-03 | Info | OK | `emergencyWithdrawAll` folds accrued yield into the liquid USDC balance and zeroes `suppliedPrincipal`, so `accruedYield()` reads 0 afterwards and residual yield is no longer harvestable (becomes treasury over-collateral). No user-fund loss. | `emergencyWithdrawAll():1136-1149`, `accruedYield():1258-1262` |
| A4-04 | Info | OK | Docs drift: `bridge_verification.md` AC-5 lists `blockHeaderOracle` (+ non-existent `verifier`, `wethGateway`, `aWETH`) as `immutable`; in code `blockHeaderOracle` is a plain storage var (no `immutable`, but also no setter → effectively fixed). Same drift class: `DeployRealBridge.s.sol:193` console label still says "FallbackVerifier (Groth16)" though 1B is SHPLONK. | `AckiNackiBridge.sol:100`; `docs/operations/bridge_verification.md` AC-5; `DeployRealBridge.s.sol:193` |
| A4-05 | Info | OK | Oracle `isBlockHashAvailable` returns `true` for historical blocks unconditionally; `uint32(blockNumber)` truncation in Axiom paths. View-only / unused; still fails closed via `verifyBlockHash`. | `AxiomBlockHeaderOracle.sol:90-110,148,177` |

## New invariants proposed

| ID | Statement |
|----|-----------|
| A4-INV-1 | For every owner-reachable state, `totalAssets() ≥ treasuryBalance` (solvency); no owner path decreases `treasuryBalance` or transfers principal to an EOA. |
| A4-INV-2 | After `harvestYield(amount)`, `aUsdcBalance() ≥ suppliedPrincipal` still holds (principal stays fully backed; harvest is bounded by `accruedYield()`). |
| A4-INV-3 | `suppliedPrincipal` decreases only via `_pullFromAave` / `emergencyWithdrawAll`; USDC leaves the contract to a non-AAVE address only as yield (`harvestYield`) or a verified payout (`withdrawByProof`). |
| A4-INV-4 | While `paused`, `deposit` / `verifyBlock` / `applyBkSetUpdate` / `withdrawByProof` revert with `BridgePaused`; `supplyToAave` / `withdrawFromAave` / `emergencyWithdrawAll` / `harvestYield` remain owner-callable. |
| A4-INV-5 | Every AN→ETH aggregator adapter returns `true` only if `proof.length ≥ (12 + NUM_INNER)*32`, the re-exposed instances `[12..]` equal the bridge-supplied public inputs, **and** the Yul SHPLONK verifier accepts `instances‖proof`. |
| A4-INV-6 | `ShplonkHalo2Verifier.verify` should return `true` only when the target Yul verifier has non-empty code and does not revert — currently unenforced (A4-01); add `extcodesize > 0` in the wrapper ctor or `verify`. |
| A4-INV-7 | `AxiomBlockHeaderOracle.getBlockHash` fails closed: reverts for future blocks, historical-without-witness, and zeroed recent `blockhash()`. |
