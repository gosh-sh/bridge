# BRIDGE-ETH-10 — solc 0.8.19 + via_IR FullInliner

**Class:** **QC** (compiler; incorrect codegen class of bugs)  
**Status:** **open → patched in this change** (`solc_version = "0.8.21"`)  
**Area:** `contracts/ethereum/foundry.toml`, `audit/spec/ethereum/foundry.toml`  
**Source:** Stage II Q&A PDF ETH-10  
**Invariant:** compile-time — production bytecode must not be emitted by a known-buggy via-IR pipeline

## Summary

Both Foundry profiles compiled with `solc_version = "0.8.19"` and `via_ir = true`. Solidity 0.8.19 via-IR FullInliner had a documented codegen bug (fixed in 0.8.21). `AckiNackiBridge` is large enough that via-IR is required to avoid stack-too-deep, so the buggy path was the production path.

Pragma on hand-written contracts stays `^0.8.19` (snark-verifier Yul sources under `verifiers/` keep exact `0.8.19` and are **not** compiled by Foundry — deploy uses `.bin`). Foundry's `solc_version` is the pin that matters for `AckiNackiBridge`.

## PoC

No runtime PoC (compiler advisory). Gate: `foundry.toml` `solc_version` ≥ 0.8.21 in both profiles, then `forge test --no-match-contract 'Fork|AaveFork'`.

## Fix

Bump both files to `0.8.21` with `auto_detect_solc = false` so `^0.8.19` sources do not fall back to 0.8.19. Skip `test/fixtures/**` (gitignored snark-verifier dump with exact `pragma solidity 0.8.19`; tests load the sibling `.bin`). Re-run Foundry; watch via-IR / stack-too-deep / runtime size. Generated Halo2 `.bin` files are independent of this solc pin (they are precompiled Yul bytecode).

## Notes

Do not bump the exact `pragma solidity 0.8.19` inside exported `verifiers/*.sol` — those files are artefacts, not the Foundry src set.
