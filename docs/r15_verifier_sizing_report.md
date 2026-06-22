# R15 Verifier Sizing Report

**Date:** 2026-06-08 (fallback updated 2026-06-22)  
**Gate:** EIP-170 runtime bytecode ≤ 24 576 bytes per verifier contract.

## Methodology

1. Inner Halo2 proof generated with **Poseidon** transcript (snark-verifier compatible).
2. Wrapped in `AggregationCircuit` via `bridge-evm-aggregator::aggregate_inner`.
3. Yul verifier emitted by `generate_yul_verifier_gated` (EIP-170 assert enabled).
4. `K_outer` tuned via `AggregatorConfig::for_inner_instances(n)`.

Empirical baseline (M2 spike, multiply inner, 1 re-exposed PI): **13 172 B @ K_outer=21**.

Rule of thumb from spike: ~160 B per re-exposed instance column beyond accumulator limbs (12).

## Matrix

| Circuit | Inner K | Inner PIs | K_outer | Universality | Status | Bytes (.bin) |
|---------|---------|-----------|---------|--------------|--------|--------------|
| M2 spike (multiply) | 9 | 1 | 21 | Full | **PASS** | 13 172 |
| 1A (primary) | 20 | 4 | 21 | Full | **PASS** | 21 493 |
| 1B (fallback) | **21** | 4 | 21 | PreprocessedAsWitness | **PASS** | 21 493 |
| 2 (layer hashes) | 17 | 14 | 22 | Full | **PASS** | 19 100 |
| 4 (withdraw) | ~20 (partner) | 10 | 21 | Full | **TBD** | — |

## Circuit 1B (fallback) — production SHPLONK at inner K=21

Measured 2026-06-22: at the primary path's inner `K=20`, the fallback circuit (which
verifies *two* attestation envelopes — the primary `attestation_bytes` plus the fallback
`attestation_2_bytes`) auto-configures to **44 advice columns**, nearly double primary's 24.
The SHPLONK aggregator Yul size scales with the inner circuit's column count, so the fallback
verifier stayed ~28.4 KB (EIP-170 fail) regardless of `VerifierUniversality` or `K_outer`:

| inner K | K_outer | universality | bytes |
|---------|---------|--------------|-------|
| 20 | 20 | none | 42 632 |
| 20 | 21 | none | 28 432 |
| 20 | 20 | preprocessed | 44 299 |
| 20 | 21 | preprocessed | 28 434 |
| 20 | 20 | full | 44 300 |
| 20 | 21 | full | 28 434 |

Re-keying the inner circuit at **K=21** spreads the same gates over twice the rows, so
halo2-lib auto-configures **22 advice columns** (primary-like) and the aggregator Yul drops to
**21 493 B** — identical to Primary, comfortably under EIP-170. The fallback inner snark shares
the degree-21 Hermez SRS slice (tau-compatible with the degree-20 primary/layer slice), so
on-chip aggregation stays consistent. The earlier gnark Groth16 hybrid for Circuit 1B is
retired. The knob lives in `crates/bridge-prover-orchestrator/src/keys.rs::FALLBACK_K`.

## CI gate

```bash
make generate-spike-artifacts   # M2 multiply spike → fixtures + EIP-170 check
chmod +x scripts/check_eip170_verifier_bins.sh
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/test/fixtures/r15_spike
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Foundry on-chain acceptance (M7 spike): `forge test --match-contract ShplonkSpikeOnChain` — deploys `MultiplierSpikeVerifier.bin` + calls exported calldata (~406 k gas verify).

Production verifyBlock acceptance: `forge test --match-contract AckiNackiBridgeProductionVerifyBlock` — deploys the Primary/Fallback/LayerHashes SHPLONK adapters and verifies the bound calldata (Primary + Fallback isolated paths green; full E2E gated on the tracked Circuit 2 pairing).

Rust unit tests: `cd crates/bridge-evm-aggregator && cargo test --release` → `eip170::tests` + `aggregator_round_trip` (`--ignored`).

## Artefact paths (on generation)

| Verifier | Solidity | Bytecode |
|----------|----------|----------|
| Withdrawal (C4) | `contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol` | `.bin` sibling |
| Primary (1A) | `contracts/ethereum/verifiers/PrimaryAggregatorVerifier.sol` | `.bin` |
| Fallback (1B) | `contracts/ethereum/verifiers/FallbackAggregatorVerifier.sol` | `.bin` (K=21 inner) |
| Layer hashes (2) | `contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.sol` | `.bin` |

Generated verifiers are checked into `contracts/ethereum/verifiers/` once partner inner VKs are pinned for shellnet.
