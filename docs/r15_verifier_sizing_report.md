# R15 Verifier Sizing Report

**Date:** 2026-06-08  
**Gate:** EIP-170 runtime bytecode ≤ 24 576 bytes per verifier contract.

## Methodology

1. Inner Halo2 proof generated with **Poseidon** transcript (snark-verifier compatible).
2. Wrapped in `AggregationCircuit` via `bridge-evm-aggregator::aggregate_inner`.
3. Yul verifier emitted by `generate_yul_verifier_gated` (EIP-170 assert enabled).
4. `K_outer` tuned via `AggregatorConfig::for_inner_instances(n)`.

Empirical baseline (M2 spike, multiply inner, 1 re-exposed PI): **13 172 B @ K_outer=21**.

Rule of thumb from spike: ~160 B per re-exposed instance column beyond accumulator limbs (12).

## Matrix

| Circuit | Inner K | Inner PIs | K_outer (target) | Aggregator instances (12+N) | Status | Bytes (.bin) |
|---------|---------|-----------|------------------|----------------------------|--------|--------------|
| M2 spike (multiply) | 9 | 1 | 21 | 13 | **PASS** | 13 172 |
| 4 (withdraw) | ~20 (partner) | 10 | 21 | 22 | **PROJECTED PASS** | TBD — run `export-circuit4-aggregator` |
| 1A (primary) | ~20 | 4 | 21 | 16 | **PROJECTED PASS** | TBD |
| 1B (fallback) | ~20 | 4 | 21 | 16 | **PROJECTED PASS** | TBD |
| 2 (layer hashes) | ~17 | 14 | 22 | 26 | **WATCH** — may need K=22 | TBD |

## Circuit 2 risk

26 total instance scalars (12 acc + 14 inner) is the tightest layout. If K=22 exceeds 24 KB, mitigations (in order):

1. Minimize exposed instances (bridge reads only PIs, not duplicate acc limbs in adapter calldata).
2. K_outer=23 last resort before deferring Groth16 outer wrap (out of shellnet v1 scope).

## CI gate

```bash
make generate-spike-artifacts   # M2 multiply spike → fixtures + EIP-170 check
chmod +x scripts/check_eip170_verifier_bins.sh
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/test/fixtures/r15_spike
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Foundry on-chain acceptance (M7 spike): `forge test --match-contract ShplonkSpikeOnChain` — deploys `MultiplierSpikeVerifier.bin` + calls exported calldata (~406 k gas verify).

Rust unit tests: `cd crates/bridge-evm-aggregator && cargo test --release` → `eip170::tests` + `aggregator_round_trip` (`--ignored`).

## Artefact paths (on generation)

| Verifier | Solidity | Bytecode |
|----------|----------|----------|
| Withdrawal (C4) | `contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol` | `.bin` sibling |
| Primary (1A) | `contracts/ethereum/verifiers/PrimaryAggregatorVerifier.sol` | `.bin` |
| Fallback (1B) | `contracts/ethereum/verifiers/FallbackAggregatorVerifier.sol` | `.bin` |
| Layer hashes (2) | `contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.sol` | `.bin` |

Generated verifiers are checked into `contracts/ethereum/verifiers/` once partner inner VKs are pinned for shellnet.
