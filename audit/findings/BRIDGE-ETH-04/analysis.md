# BRIDGE-ETH-04 — guardian pause vs #20

**Class:** **OK** (product)  
**Status:** **closed — keep #20; incident plan recorded (re-review ETH-04 Medium, risk accepted)**  
**Area:** `AckiNackiBridge` (no `pause` / `whenNotPaused` / `BridgePaused`)  
**Source:** Stage II Q&A PDF ETH-4; #20 / TD-58

## Summary

The PDF asked for an on-chain `paused` flag and guardian so ETH-1 (congruent nullifier) could be stopped. Pause on this bridge was **removed by design** in #20: deposit HOL / pause asymmetry (TD-58). `DepositPauseAsymmetry.t.sol` pins that `deposit` / `verifyBlock` / `withdrawByProof` are not bridge-pause gated.

After Wave 1, ETH-1 and ETH-2 are gated with `FieldElementOutOfRange`. That removes the specific incident the PDF wanted pause for. Restoring pause would reverse #20 without a new product decision.

Incident controls that remain:

- Circuit 4 unwired (`bridgeWithdrawalVerifier = 0`) at deploy, or a replacement deploy.
- Circle USDC pause/blacklist on the bridge address (TD-24 / TD-56).
- Owner AAVE: `emergencyWithdrawAll`, `harvestYield`.
- Relayer halt off-chain.

A **scoped** pause (withdraw + verifyBlock only, deposits open) is a possible future product; owner 2026-09-08 confirmed it is not this generation.

## PoC

Existing: `audit/spec/ethereum/DepositPauseAsymmetry.t.sol` (`test_td58_deposit_no_bridge_pause_gate`).

## Docs

`docs/audit/eth-qc-hardening-runbook.md` A4-Q2 / ETH-04 incident plan (who, Circle channel, replacement deploy, unwithdrawn funds).
