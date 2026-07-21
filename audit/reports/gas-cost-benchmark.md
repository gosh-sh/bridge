# Gas cost benchmark — AckiNackiBridge (ETH L1 contracts)

**Measured:** 2026-07-21 16:38 UTC
**ETH/USD (Coingecko):** $1,924.28
**Harness:** `audit/spec/ethereum/GasBenchmark.t.sol`
**Script:** `scripts/run_gas_benchmark.sh`
**Profile:** solc 0.8.19, `optimizer_runs=1`, `via_ir=true` (repo default — bytecode-size profile; production may tune `optimizer_runs` for runtime gas).

## 1. Plan

1. Inventory `AckiNackiBridge` entrypoints: user (`deposit`), relayer (`verifyBlock`, `withdrawByProof`), owner/AAVE, views.
2. Forge harness logs `GAS|category|operation|variant|gas` via `vm.snapshotGasLastCall`.
3. Production SHPLONK uses committed `contracts/ethereum/verifiers/*_calldata.bin`; mock paths isolate bridge logic.
4. Fetch live `eth_gasPrice` from public RPCs; USD = gas × gwei × 1e-9 × ETH/USD.
5. Document warm/cold, amount, AAVE pull, proof-size sensitivities.

## 2. Script

    ./scripts/run_gas_benchmark.sh

## 3. Network gas prices at measurement

| Network | Gas price (gwei) | Source |
| ethereum | 15.0 | fallback |
| arbitrum | 0.02 | live |
| base | 0.006 | live |
| optimism | 0.001 | live |
| polygon (MATIC gas units) | 279.5364 | live |

> USD table below uses ETH/USD for ethereum/arbitrum/base/optimism only. Polygon native gas is MATIC — not converted here.

## 4. Measurements (gas)

| Category | Operation | Variant | Gas | Notes |
| owner | emergencyWithdrawAll | default | 12,899 |  |
| owner | harvestYield | 1usdc | 36,589 |  |
| owner | pause | default | 22,264 |  |
| owner | setAaveEnabled | false | 2,408 |  |
| owner | setAaveEnabled | true | 2,436 |  |
| owner | setLiquidReserveBps | 5pct | 2,246 |  |
| owner | setYieldRecipient | default | 1,952 |  |
| owner | supplyToAave | max | 119,915 |  |
| owner | unpause | default | 1,811 |  |
| owner | withdrawFromAave | 5usdc | 10,768 |  |
| relayer | verifyBlock | mock_primary_first | 342,133 | mock crypto |
| relayer | verifyBlock | mock_primary_warm | 92,627 | mock crypto, warm storage |
| relayer | verifyBlock | production_primary | 1,283,234 | SHPLONK prod calldata |
| relayer | withdrawByProof | mock_liquid_only | 53,560 | mock crypto |
| relayer | withdrawByProof | mock_warm | 31,660 | mock crypto, warm storage |
| relayer | withdrawByProof | mock_with_aave_pull | 63,217 | mock crypto, AAVE pull |
| user | deposit | first_10usdc | 74,686 |  |
| user | deposit | max_100usdc | 6,986 | MAX_DEPOSIT_AMOUNT |
| user | deposit | warm_10usdc | 6,986 | warm storage |
| user | erc20_approve | 10usdc | 22,527 |  |
| verifier | fallback_attestation | isolated | 419,985 | SHPLONK prod calldata |
| verifier | layer_hashes | isolated | 346,036 | SHPLONK prod calldata |
| verifier | primary_attestation | isolated | 419,985 | SHPLONK prod calldata |
| verifier | withdrawal_c4 | isolated | 393,055 | SHPLONK prod calldata |
| view | expectedPrevAnchor | cold | 24,851 |  |
| view | isNullifierUsed | cold | 3,759 |  |

## 5. USD estimates

| Operation | Variant | Gas | ethereum | arbitrum | base | optimism |
| erc20_approve | 10usdc | 22,527 | $0.6502 | $0.0009 | $0.0003 | $0.0000 |
| deposit | first_10usdc | 74,686 | $2.1558 | $0.0029 | $0.0009 | $0.0001 |
| deposit | warm_10usdc | 6,986 | $0.2016 | $0.0003 | $0.0001 | $0.0000 |
| deposit | max_100usdc | 6,986 | $0.2016 | $0.0003 | $0.0001 | $0.0000 |
| verifyBlock | mock_primary_first | 342,133 | $9.8754 | $0.0132 | $0.0040 | $0.0007 |
| verifyBlock | production_primary | 1,283,234 | $37.0395 | $0.0494 | $0.0148 | $0.0025 |
| primary_attestation | isolated | 419,985 | $12.1225 | $0.0162 | $0.0048 | $0.0008 |
| layer_hashes | isolated | 346,036 | $9.9881 | $0.0133 | $0.0040 | $0.0007 |
| fallback_attestation | isolated | 419,985 | $12.1225 | $0.0162 | $0.0048 | $0.0008 |
| withdrawal_c4 | isolated | 393,055 | $11.3452 | $0.0151 | $0.0045 | $0.0008 |
| withdrawByProof | mock_liquid_only | 53,560 | $1.5460 | $0.0021 | $0.0006 | $0.0001 |
| withdrawByProof | mock_with_aave_pull | 63,217 | $1.8247 | $0.0024 | $0.0007 | $0.0001 |
| supplyToAave | max | 119,915 | $3.4613 | $0.0046 | $0.0014 | $0.0002 |
| harvestYield | 1usdc | 36,589 | $1.0561 | $0.0014 | $0.0004 | $0.0001 |
| emergencyWithdrawAll | default | 12,899 | $0.3723 | $0.0005 | $0.0001 | $0.0000 |

## 6. Parameter dependencies

| Parameter | Affects | Direction |
| Deposit amount | deposit | ~flat for USDC |
| Warm storage | deposit, verifyBlock, withdraw | 2nd call much cheaper |
| SHPLONK proof (calldata size) | verifyBlock, withdraw | dominates; fixed per circuit artefact |
| numLayers (1..10) | verifyBlock | weak linear (≤10 SSTORE) |
| AAVE utilization | withdrawByProof | +gas when liquid USDC < payout |
| L1 calldata | proof txs on Ethereum | not in execution gas table; budget ~16 gas/non-zero byte separately |

## 7. Relayer / user budgeting

- **ETH→AN deposit (bridge only):** 74,686 gas ≈ $2.1558 on Ethereum @ 15.0 gwei
- **ETH→AN deposit (warm):** 6,986 gas ≈ $0.2016 on Ethereum @ 15.0 gwei
- **AN→ETH verifyBlock (production 1A+2):** 1,283,234 gas ≈ $37.0395 on Ethereum @ 15.0 gwei
- **C4 verify (isolated):** 393,055 gas ≈ $11.3452 on Ethereum @ 15.0 gwei
- **withdrawByProof bridge overhead (mock):** 53,560 gas ≈ $1.5460 on Ethereum @ 15.0 gwei
- **withdrawByProof (prod estimate):** ≈ 446,615 gas (C4 isolated + mock bridge overhead; not yet full E2E on bridge).

Raw log: `audit/reports/.gas-benchmark-raw.txt`
