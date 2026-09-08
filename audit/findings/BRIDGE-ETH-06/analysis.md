# BRIDGE-ETH-06 — SHPLONK `.bin` / calldata desync; CI skip

**Class:** **QC** (CI / artefact integrity; pairing is a production-path gate)  
**Status:** **closed** — all four committed pairs (1A/1B/C2/C4) are in the default `ShplonkArtefactPairing` suite after the 2026-09-08 n14 regen  
**Area:** `contracts/ethereum/verifiers/*.{bin,_calldata.bin}`, `ShplonkArtefactPairing.t.sol`, `scripts/check_shplonk_artefacts.sh`  
**Source:** Stage II Q&A PDF ETH-6  
**Invariant:** BK-6 / production verifyBlock — committed Yul must accept its committed calldata

## Summary

Committed artefacts under `contracts/ethereum/verifiers/` are eight files. Circuit 4 `.bin` and `_calldata.bin` were already a matching pair (both 2026-08-12). Primary / Fallback `.bin` (2026-08-12) did not accept `_calldata.bin` (2026-06-23). LayerHashes needed a fresh Yul (inner Circuit 2 re-keygen at TREE_DEPTH=8 / 25 advice). Isolated pairing **FAIL** (PDF “3/4 REJECTED”). Shared PI words across 1A/1B/C2 calldata looked like one bound export; pairing still failed → VK/bin vs proof desync, not a missing JSON.

`AckiNackiBridgeProductionVerifyBlock.t.sol` skipped the whole suite when `bound_scenario.json` was absent. That file is **not** in the repo. Default `forge test` / GitLab `test:solidity` went green without ever pairing 1A/1B/C2. `scripts/check_eip170_verifier_bins.sh` is size-only; `make pre-push` swallowed EIP-170 on the real verifier dir with `|| true`.

## PoC (historical, 2026-08-27 committed artefacts)

```
cd contracts/ethereum && forge test --match-contract ShplonkArtefactPairing -vv
```

| Test | Result (2026-08-27) | Result (2026-09-08 n14 regen) |
|------|---------------------|-------------------------------|
| `test_eth6_withdrawalCalldata_verifies` | **PASS** | **PASS** |
| `test_eth6_primaryCalldata_verifies` | **FAIL** | **PASS** |
| `test_eth6_fallbackCalldata_verifies` | **FAIL** | **PASS** |
| `test_eth6_layerHashesCalldata_verifies` | **FAIL** | **PASS** |

## Fix

1. Pin SHA-256 of all eight files. CI / `make pre-push` / `production_preflight` run the pin script (existence + hash + EIP-170).
2. `ShplonkArtefactPairing.t.sol` is the pairing job for all four circuits. Production withdraw tests `require` artefacts instead of returning.
3. n14 regen (2026-09-08): Poseidon-native self-verify of 1A/1B/C2, Hermez PPoT outer SRS (k=21). 1A/1B Yul bytecode unchanged vs 2026-08-12; calldata rewritten. LayerHashes Yul regenerated (`k_outer=21`, 23111 B, CREATE `extcodehash` `0xd6f78f3b…`). Circuit 4 pair kept.
4. `ShplonkDeployLib` `*_YUL_CODEHASH` pins (CREATE `extcodehash`). Production adapters refuse a `.bin` whose runtime keccak256 does not match (`YulCodehashMismatch`).
5. `export-inner-aggregator` writes `{name}_calldata.bin` next to `{name}.bin`.

Full `verifyBlock` E2E still needs `bound_scenario.json` (layer hashes + genesis as JSON, not only calldata words). That file is generated on n14; the pairing gate does not depend on it.

Hermez k=22 ptau was not fetchable (GCS zkevm + Hermez S3 403). LayerHashes outer is therefore k=21; Yul stays under EIP-170. `AggregatorConfig` for `LayerHashesAggregatorVerifier` matches that degree.
