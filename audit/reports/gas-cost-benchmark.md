# Gas cost benchmark — AckiNackiBridge (ETH L1 contracts)

**Gas measurements (Forge):** 2026-07-21T16:46:07Z
**Price snapshot (CoinGecko):** 2026-07-21T16:46:08Z
**ETH/USD:** $1,924.1600
**POL/USD:** $0.0803
**Harness:** `audit/spec/ethereum/GasBenchmark.t.sol`
**Script:** `scripts/run_gas_benchmark.sh`
**Provenance JSON:** `audit/reports/.gas-benchmark-networks.json`
**Profile:** solc 0.8.19, `optimizer_runs=1`, `via_ir=true`

## 1. Methodology

1. **Execution gas** — Foundry `vm.snapshotGasLastCall` on local EVM (same gas units on all EVM chains).
2. **Gas price** — `cast gas-price` → JSON-RPC `eth_gasPrice` at report generation time.
3. **USD** — `cost = gas_used × gas_price_gwei × 10⁻⁹ × native_token_usd`.
4. **L2 (Arbitrum / Base / Optimism)** — native gas token is ETH; same ETH/USD as mainnet.
5. **Polygon** — native gas token is POL; uses POL/USD (`polygon-ecosystem-token`) from CoinGecko.
6. **Sepolia** — testnet gas price for operator estimates only (not mainnet USD planning).

Re-run anytime: `./scripts/run_gas_benchmark.sh` (prices are point-in-time, not historical).

## 2. Network parameters (live at measurement)

| Network | Gas (gwei) | Gas (wei) | Block | Native | USD/token | Source | RPC |
| ethereum | 0.165671 | 165671060 | 25582383 | ETH | $1924.1600 | live_rpc | https://ethereum.publicnode.com |
| sepolia | 1.057373 | 1057373494 | 11320950 | ETH | $1924.1600 | live_rpc | https://ethereum-sepolia.publicnode.com |
| arbitrum | 0.020052 | 20052000 | 486245084 | ETH | $1924.1600 | live_rpc | https://arb1.arbitrum.io/rpc |
| base | 0.006000 | 6000000 | 48931511 | ETH | $1924.1600 | live_rpc | https://mainnet.base.org |
| optimism | 0.001000 | 1000388 | 154526796 | ETH | $1924.1600 | live_rpc | https://mainnet.optimism.io |
| polygon | 277.971646 | 277971645999 | 90631582 | POL | $0.0803 | live_rpc | https://polygon-bor-rpc.publicnode.com |

**Price source:** CoinGecko simple price API (`ethereum`, `polygon-ecosystem-token`).

**Note:** Ethereum mainnet row uses live RPC when reachable; if all RPCs fail, script uses fallback estimate (marked `fallback_estimate`). L2 `eth_gasPrice` is often <0.1 gwei — USD looks small but is correct for current fee market.

## 3. Execution gas (Forge, chain-independent)

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

## 4. USD cost — key operations (all networks)

| Operation | Variant | Gas | ethereum | sepolia | arbitrum | base | optimism | polygon |
| erc20_approve | 10usdc | 22,527 | $0.0072 | $0.0458 | $0.0009 | $0.0003 | $0.0000 | $0.0005 |
| deposit | first_10usdc | 74,686 | $0.0238 | $0.1520 | $0.0029 | $0.0009 | $0.0001 | $0.0017 |
| deposit | warm_10usdc | 6,986 | $0.0022 | $0.0142 | $0.0003 | $0.0001 | $0.0000 | $0.0002 |
| verifyBlock | production_primary | 1,283,234 | $0.4091 | $2.6108 | $0.0495 | $0.0148 | $0.0025 | $0.0287 |
| withdrawal_c4 | isolated | 393,055 | $0.1253 | $0.7997 | $0.0152 | $0.0045 | $0.0008 | $0.0088 |
| withdrawByProof | mock_liquid_only | 53,560 | $0.0171 | $0.1090 | $0.0021 | $0.0006 | $0.0001 | $0.0012 |
| supplyToAave | max | 119,915 | $0.0382 | $0.2440 | $0.0046 | $0.0014 | $0.0002 | $0.0027 |

## 5. USD cost — full matrix

| Operation | Variant | Gas | ethereum | sepolia | arbitrum | base | optimism | polygon |
| erc20_approve | 10usdc | 22,527 | $0.0072 | $0.0458 | $0.0009 | $0.0003 | $0.0000 | $0.0005 |
| deposit | first_10usdc | 74,686 | $0.0238 | $0.1520 | $0.0029 | $0.0009 | $0.0001 | $0.0017 |
| deposit | warm_10usdc | 6,986 | $0.0022 | $0.0142 | $0.0003 | $0.0001 | $0.0000 | $0.0002 |
| deposit | max_100usdc | 6,986 | $0.0022 | $0.0142 | $0.0003 | $0.0001 | $0.0000 | $0.0002 |
| verifyBlock | mock_primary_first | 342,133 | $0.1091 | $0.6961 | $0.0132 | $0.0039 | $0.0007 | $0.0076 |
| verifyBlock | production_primary | 1,283,234 | $0.4091 | $2.6108 | $0.0495 | $0.0148 | $0.0025 | $0.0287 |
| primary_attestation | isolated | 419,985 | $0.1339 | $0.8545 | $0.0162 | $0.0048 | $0.0008 | $0.0094 |
| layer_hashes | isolated | 346,036 | $0.1103 | $0.7040 | $0.0134 | $0.0040 | $0.0007 | $0.0077 |
| fallback_attestation | isolated | 419,985 | $0.1339 | $0.8545 | $0.0162 | $0.0048 | $0.0008 | $0.0094 |
| withdrawal_c4 | isolated | 393,055 | $0.1253 | $0.7997 | $0.0152 | $0.0045 | $0.0008 | $0.0088 |
| withdrawByProof | mock_liquid_only | 53,560 | $0.0171 | $0.1090 | $0.0021 | $0.0006 | $0.0001 | $0.0012 |
| withdrawByProof | mock_with_aave_pull | 63,217 | $0.0202 | $0.1286 | $0.0024 | $0.0007 | $0.0001 | $0.0014 |
| supplyToAave | max | 119,915 | $0.0382 | $0.2440 | $0.0046 | $0.0014 | $0.0002 | $0.0027 |
| harvestYield | 1usdc | 36,589 | $0.0117 | $0.0744 | $0.0014 | $0.0004 | $0.0001 | $0.0008 |
| emergencyWithdrawAll | default | 12,899 | $0.0041 | $0.0262 | $0.0005 | $0.0001 | $0.0000 | $0.0003 |

## 6. Parameter dependencies

| Parameter | Affects | Direction |
| Deposit amount | deposit | ~flat for USDC |
| Warm storage | deposit, verifyBlock, withdraw | 2nd call ~10× cheaper |
| SHPLONK proof size | verifyBlock, withdraw | dominates (~1.28M gas prod) |
| AAVE pull | withdrawByProof | +~18% vs liquid-only mock |
| Gas market | USD columns | re-run script; see section 2 |

## 7. Relayer budgeting snapshot

- **deposit (first)** (74,686 gas): ethereum $0.0238 @ 0.1657 gwei; sepolia $0.1520 @ 1.0574 gwei; arbitrum $0.0029 @ 0.0201 gwei; base $0.0009 @ 0.0060 gwei; optimism $0.0001 @ 0.0010 gwei; polygon $0.0017 @ 277.9716 gwei
- **deposit (warm)** (6,986 gas): ethereum $0.0022 @ 0.1657 gwei; sepolia $0.0142 @ 1.0574 gwei; arbitrum $0.0003 @ 0.0201 gwei; base $0.0001 @ 0.0060 gwei; optimism $0.0000 @ 0.0010 gwei; polygon $0.0002 @ 277.9716 gwei
- **verifyBlock production** (1,283,234 gas): ethereum $0.4091 @ 0.1657 gwei; sepolia $2.6108 @ 1.0574 gwei; arbitrum $0.0495 @ 0.0201 gwei; base $0.0148 @ 0.0060 gwei; optimism $0.0025 @ 0.0010 gwei; polygon $0.0287 @ 277.9716 gwei
- **withdrawal C4 verify** (393,055 gas): ethereum $0.1253 @ 0.1657 gwei; sepolia $0.7997 @ 1.0574 gwei; arbitrum $0.0152 @ 0.0201 gwei; base $0.0045 @ 0.0060 gwei; optimism $0.0008 @ 0.0010 gwei; polygon $0.0088 @ 277.9716 gwei
- **withdrawByProof prod estimate:** 446,615 gas (C4 + bridge overhead; E2E pending)

Raw forge log: `audit/reports/.gas-benchmark-raw.txt`
