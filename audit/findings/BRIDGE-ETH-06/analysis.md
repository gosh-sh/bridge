# BRIDGE-ETH-06 — SHPLONK `.bin` / calldata desync; CI skip

**Class:** **QC** (CI / artefact integrity; pairing is a production-path gate)  
**Status:** **partially patched** — hash pin + no-skip pairing tests landed; 1A/1B/C2 pairing still **FAIL** until n14 regen  
**Area:** `contracts/ethereum/verifiers/*.{bin,_calldata.bin}`, `ShplonkArtefactPairing.t.sol`, `scripts/check_shplonk_artefacts.sh`  
**Source:** Stage II Q&A PDF ETH-6  
**Invariant:** BK-6 / production verifyBlock — committed Yul must accept its committed calldata

## Summary

Committed artefacts under `contracts/ethereum/verifiers/` are eight files. Circuit 4 `.bin` and `_calldata.bin` are a matching pair (both 2026-08-12) and pairing **PASS**. Primary / Fallback / LayerHashes `.bin` were regenerated 2026-08-12; their `_calldata.bin` are still 2026-06-23. Isolated pairing **FAIL** (PDF “3/4 REJECTED”). Shared PI words across 1A/1B/C2 calldata look like one bound export; pairing still fails → VK/bin vs proof desync, not a missing JSON.

`AckiNackiBridgeProductionVerifyBlock.t.sol` skipped the whole suite when `bound_scenario.json` was absent. That file is **not** in the repo. Default `forge test` / GitLab `test:solidity` went green without ever pairing 1A/1B/C2. `scripts/check_eip170_verifier_bins.sh` is size-only; `make pre-push` swallowed EIP-170 on the real verifier dir with `|| true`.

## PoC

```
cd contracts/ethereum && forge test --match-contract ShplonkArtefactPairing -vv
```

| Test | Result (2026-08-27, committed artefacts) |
|------|------------------------------------------|
| `test_eth6_withdrawalCalldata_verifies` | **PASS** |
| `test_eth6_primaryCalldata_verifies` | **FAIL** — bin does not accept calldata |
| `test_eth6_fallbackCalldata_verifies` | **FAIL** |
| `test_eth6_layerHashesCalldata_verifies` | **FAIL** |

No skip if a file is missing (`require` on empty/absent `.bin`).

Hash pin: `contracts/ethereum/verifiers/SHA256SUMS` + `./scripts/check_shplonk_artefacts.sh`.

## Fix (this change)

1. Pin SHA-256 of all eight files. CI / `make pre-push` / `production_preflight` run the pin script (existence + hash + EIP-170). Do **not** `|| true` the production verifier dir.
2. `ShplonkArtefactPairing.t.sol` is the pairing job. Circuit 4 production withdraw tests `require` artefacts instead of returning.
3. Full `verifyBlock` E2E still needs `bound_scenario.json` (layer hashes + genesis as JSON, not only calldata words). That file is generated on n14; the pairing gate does not depend on it.

## Remaining (n14)

Regen Primary / Fallback / LayerHashes **together with** matching `_calldata.bin` (and `bound_scenario.json` if the E2E should be a hard gate). Update `SHA256SUMS`. Pairing tests must go green before mainnet `WIRE_VERIFY_BLOCK=true`.

Until then default `forge test` is **red** on three ETH-6 tests. That is the finding, not a skip to hide.
