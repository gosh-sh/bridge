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

| Circuit | Inner K | Inner PIs | K_outer | Universality | Status | Bytes (.bin) |
|---------|---------|-----------|---------|--------------|--------|--------------|
| M2 spike (multiply) | 9 | 1 | 21 | Full | **PASS** | 13 172 |
| 1A (primary) | 20 | 4 | 21 | Full | **PASS** | 21 493 |
| 2 (layer hashes) | 17 | 14 | 22 | Full | **PASS** | 19 100 |
| 1B (fallback) | 20 | 4 | 21 | None / Preprocessed / Full | **FAIL** | ~28 432 (EIP-170) |
| 4 (withdraw) | ~20 (partner) | 10 | 21 | Full | **TBD** | — |

## Circuit 1B (fallback) risk

Measured 2026-06-22 on bound Poseidon snarks: aggregator Yul for Circuit 1B stays
~28.4 KB regardless of `VerifierUniversality` (`None`, `PreprocessedAsWitness`, `Full`)
at `K_outer ∈ {20,21,22,23}` — the inner VK shape (44 advice columns, BLS+SHA) dominates.
Mitigations under investigation:

1. Partner circuit shape reduction (unlikely for shellnet).
2. Split-bytecode / library linker for the Yul verifier (engineering).
3. Retain gnark Groth16 wrapper for 1B only until a sub-24 KB aggregator path exists.

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
